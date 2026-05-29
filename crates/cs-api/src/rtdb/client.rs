//! Firebase RTDB HTTP+SSE client.
//!
//! Implements the documented Firebase REST surface:
//! - GET    `<base><path>.json?auth=<token>&<params>` — one-shot read
//! - PUT    `<base><path>.json?auth=<token>`           — set (replace)
//! - PATCH  `<base><path>.json?auth=<token>`           — merge fields
//! - DELETE `<base><path>.json?auth=<token>`           — remove
//! - GET    `<base><path>.json` with `Accept: text/event-stream` — subscribe
//!
//! SSE events delivered by Firebase: `put`, `patch`, `cancel`, `auth_revoked`,
//! and periodic `keep-alive` heartbeats.
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_util::io::StreamReader;

const DEFAULT_USER_AGENT: &str = concat!("cs-tui/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, thiserror::Error)]
pub enum RtdbError {
    #[error("invalid rtdbToken JWT: {0}")]
    InvalidJwt(String),

    #[error("rtdb http {status}: {body}")]
    Http { status: u16, body: String },

    #[error("rtdb auth revoked")]
    AuthRevoked,

    #[error("rtdb sse parse: {0}")]
    Sse(String),

    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Kind of SSE event delivered by Firebase RTDB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseEventKind {
    /// `put` — a path's value was set (replaced). Payload: `{"path": "...", "data": <value>}`.
    Put,
    /// `patch` — fields under a path were merged. Payload: `{"path": "...", "data": {...}}`.
    Patch,
    /// `cancel` — the server cancelled the listener (e.g. rules denied the path).
    Cancel,
    /// `auth_revoked` — the auth token was revoked or expired mid-stream.
    AuthRevoked,
    /// `keep-alive` — periodic heartbeat. No useful payload; consumers can ignore.
    KeepAlive,
}

#[derive(Debug, Clone)]
pub struct SseEvent {
    pub kind: SseEventKind,
    /// Raw JSON payload (`null` for keep-alive).
    pub data: serde_json::Value,
}

/// Low-level Firebase RTDB client. Cheap to clone (the underlying reqwest
/// `Client` is `Arc`-shared internally).
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// Construct a new client. `base` is the Firebase project's REST root
    /// (e.g. `https://my-project-default-rtdb.firebaseio.com`); `token` is the
    /// `rtdbToken` returned by `/v1/auth/login`.
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self, RtdbError> {
        let http = reqwest::Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()?;
        let base = base.into();
        let base = base.trim_end_matches('/').to_string();
        Ok(Self {
            http,
            base,
            token: token.into(),
        })
    }

    /// Build the full `.json` URL for a path with the auth token attached.
    pub fn build_url(&self, path: &str, params: &[(&str, &str)]) -> String {
        let path = path.trim_end_matches('/');
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let mut url = format!("{}{}.json?auth={}", self.base, path, self.token);
        for (k, v) in params {
            url.push('&');
            url.push_str(k);
            url.push('=');
            url.push_str(v);
        }
        url
    }

    /// `GET <path>.json` — returns the path's value (or null if absent).
    pub async fn get(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<serde_json::Value, RtdbError> {
        let url = self.build_url(path, params);
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(http_err(status, resp.text().await.unwrap_or_default()));
        }
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// `PUT <path>.json` — replace the value at `path` with `val`.
    pub async fn put<T: Serialize>(&self, path: &str, val: &T) -> Result<(), RtdbError> {
        self.write(Method::PUT, path, val).await
    }

    /// `PATCH <path>.json` — merge fields into the existing value.
    pub async fn patch<T: Serialize>(&self, path: &str, val: &T) -> Result<(), RtdbError> {
        self.write(Method::PATCH, path, val).await
    }

    /// `DELETE <path>.json` — remove the value at `path`.
    pub async fn delete(&self, path: &str) -> Result<(), RtdbError> {
        let url = self.build_url(path, &[]);
        let resp = self.http.delete(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(http_err(status, resp.text().await.unwrap_or_default()));
        }
        Ok(())
    }

    async fn write<T: Serialize>(
        &self,
        method: Method,
        path: &str,
        val: &T,
    ) -> Result<(), RtdbError> {
        let url = self.build_url(path, &[]);
        let resp = self
            .http
            .request(method, url)
            .header("Content-Type", "application/json")
            .json(val)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(http_err(status, resp.text().await.unwrap_or_default()));
        }
        Ok(())
    }

    /// Open an SSE stream on `path`. Returns a receiver that yields `SseEvent`s
    /// as they arrive. The receiver is closed when the stream ends, the server
    /// sends `auth_revoked`/`cancel`, or all senders are dropped.
    pub async fn subscribe(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<mpsc::Receiver<Result<SseEvent, RtdbError>>, RtdbError> {
        let url = self.build_url(path, params);
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(http_err(status, resp.text().await.unwrap_or_default()));
        }

        let stream = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| std::io::Error::other(e.to_string())));
        let reader = BufReader::new(StreamReader::new(stream));

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            read_sse_lines(reader, tx).await;
        });
        Ok(rx)
    }
}

fn http_err(status: StatusCode, body: String) -> RtdbError {
    RtdbError::Http {
        status: status.as_u16(),
        body,
    }
}

/// Read SSE events from a buffered reader, dispatching each to `tx`. The stream
/// ends when the reader hits EOF, a send fails (channel dropped), or an
/// `auth_revoked` event arrives (after which we forward an error and stop).
async fn read_sse_lines<R: tokio::io::AsyncBufRead + Unpin>(
    mut reader: R,
    tx: mpsc::Sender<Result<SseEvent, RtdbError>>,
) {
    let mut buf = String::new();
    let mut current_event: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                let _ = tx.send(Err(RtdbError::Sse(format!("read: {e}")))).await;
                break;
            }
        }

        let line = buf.trim_end_matches('\n').trim_end_matches('\r');

        if line.is_empty() {
            if let Some(event_type) = current_event.take() {
                let data = data_lines.join("\n");
                data_lines.clear();
                let parsed = parse_sse_event(&event_type, &data);
                let is_terminal = matches!(
                    parsed,
                    Ok(SseEvent {
                        kind: SseEventKind::AuthRevoked | SseEventKind::Cancel,
                        ..
                    })
                );
                if tx.send(parsed).await.is_err() {
                    break;
                }
                if is_terminal {
                    break;
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("event:") {
            current_event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim().to_string());
        }
        // Comments (lines starting with ':') and unknown fields are ignored.
    }
}

fn parse_sse_event(event_type: &str, data: &str) -> Result<SseEvent, RtdbError> {
    let kind = match event_type {
        "put" => SseEventKind::Put,
        "patch" => SseEventKind::Patch,
        "cancel" => SseEventKind::Cancel,
        "auth_revoked" => SseEventKind::AuthRevoked,
        "keep-alive" => SseEventKind::KeepAlive,
        other => {
            return Err(RtdbError::Sse(format!("unknown event type {other:?}")));
        }
    };
    let data_value = if data.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(data)
            .map_err(|e| RtdbError::Sse(format!("data not valid JSON: {e}")))?
    };
    Ok(SseEvent {
        kind,
        data: data_value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[test]
    fn build_url_attaches_auth_and_json_suffix() {
        let c = Client::new("https://p-default-rtdb.firebaseio.com", "TKN").unwrap();
        let url = c.build_url("/users/me", &[]);
        assert_eq!(
            url,
            "https://p-default-rtdb.firebaseio.com/users/me.json?auth=TKN"
        );
    }

    #[test]
    fn build_url_prepends_slash_if_missing() {
        let c = Client::new("https://p-default-rtdb.firebaseio.com", "TKN").unwrap();
        let url = c.build_url("users/me", &[]);
        assert!(url.contains("/users/me.json"));
    }

    #[test]
    fn build_url_appends_extra_params() {
        let c = Client::new("https://p-default-rtdb.firebaseio.com", "TKN").unwrap();
        let url = c.build_url("/chat", &[("orderBy", "%22ts%22"), ("limitToLast", "20")]);
        assert!(url.contains("&orderBy=%22ts%22"));
        assert!(url.contains("&limitToLast=20"));
    }

    #[test]
    fn build_url_strips_trailing_slash_from_base() {
        let c = Client::new("https://p-default-rtdb.firebaseio.com/", "T").unwrap();
        let url = c.build_url("/x", &[]);
        assert_eq!(url, "https://p-default-rtdb.firebaseio.com/x.json?auth=T");
    }

    #[test]
    fn parse_sse_event_put() {
        let ev = parse_sse_event("put", r#"{"path":"/a","data":42}"#).unwrap();
        assert_eq!(ev.kind, SseEventKind::Put);
        assert_eq!(ev.data["path"], "/a");
        assert_eq!(ev.data["data"], 42);
    }

    #[test]
    fn parse_sse_event_keep_alive_with_empty_data() {
        let ev = parse_sse_event("keep-alive", "").unwrap();
        assert_eq!(ev.kind, SseEventKind::KeepAlive);
        assert!(ev.data.is_null());
    }

    #[test]
    fn parse_sse_event_rejects_unknown_type() {
        let err = parse_sse_event("invented", "{}").unwrap_err();
        assert!(matches!(err, RtdbError::Sse(_)));
    }

    #[tokio::test]
    async fn read_sse_lines_dispatches_put_and_patch() {
        let stream = "event: put\ndata: {\"path\":\"/a\",\"data\":1}\n\nevent: patch\ndata: {\"path\":\"/a\",\"data\":{\"x\":1}}\n\n";
        let reader = BufReader::new(stream.as_bytes());
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(read_sse_lines(reader, tx));

        let first = rx.recv().await.unwrap().unwrap();
        assert_eq!(first.kind, SseEventKind::Put);
        assert_eq!(first.data["data"], 1);

        let second = rx.recv().await.unwrap().unwrap();
        assert_eq!(second.kind, SseEventKind::Patch);
        assert_eq!(second.data["data"]["x"], 1);
    }

    #[tokio::test]
    async fn read_sse_lines_stops_on_auth_revoked() {
        let stream = "event: auth_revoked\ndata: \"credential is no longer valid\"\n\nevent: put\ndata: 1\n\n";
        let reader = BufReader::new(stream.as_bytes());
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(read_sse_lines(reader, tx));

        let first = rx.recv().await.unwrap().unwrap();
        assert_eq!(first.kind, SseEventKind::AuthRevoked);
        // After auth_revoked, the loop stops — the second event must never arrive.
        let after = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await;
        match after {
            Ok(None) => {}
            other => panic!("expected channel close after auth_revoked, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_sse_lines_handles_multi_line_data() {
        let stream = "event: put\ndata: {\"a\":\ndata: 1}\n\n";
        let reader = BufReader::new(stream.as_bytes());
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(read_sse_lines(reader, tx));

        let ev = rx.recv().await.unwrap().unwrap();
        assert_eq!(ev.kind, SseEventKind::Put);
        assert_eq!(ev.data["a"], 1);
    }

    #[tokio::test]
    async fn read_sse_lines_ignores_comments_and_blank_lines() {
        let stream = ": this is a comment\n\nevent: put\ndata: 7\n\n";
        let reader = BufReader::new(stream.as_bytes());
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(read_sse_lines(reader, tx));

        let ev = rx.recv().await.unwrap().unwrap();
        assert_eq!(ev.kind, SseEventKind::Put);
        assert_eq!(ev.data, 7);
    }
}

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::RwLock;
use url::Url;

use crate::endpoint::EndpointKey;
use crate::envelope::{Data, ErrorEnvelope, Page};
use crate::error::{ApiError, ErrorCode, Result};
use crate::rate_limit::EndpointLimiter;
use crate::tokens::Tokens;
use crate::DEFAULT_BASE_URL;

const DEFAULT_USER_AGENT: &str = concat!("cs-tui/", env!("CARGO_PKG_VERSION"));
const MAX_429_RETRIES: u32 = 3;
/// Cap on image downloads (guards against absurd payloads).
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Async HTTP client for the cyberspace.online REST API.
///
/// `Client` is cheap to clone: internal state (tokens, rate limits, the underlying
/// reqwest client) lives behind `Arc`. Clone freely to share across async tasks.
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    base: Url,
    tokens: RwLock<Tokens>,
    limiter: EndpointLimiter,
}

#[derive(Debug, Clone)]
pub struct ClientBuilder {
    base: Url,
    user_agent: String,
    request_timeout: Duration,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            base: Url::parse(DEFAULT_BASE_URL).expect("compile-time constant URL"),
            user_agent: DEFAULT_USER_AGENT.to_string(),
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl ClientBuilder {
    #[must_use]
    pub fn base_url(mut self, url: Url) -> Self {
        self.base = url;
        self
    }

    /// Parse a string into a base URL. Returns `ApiError::Config` on invalid input.
    pub fn base_url_str(mut self, s: &str) -> Result<Self> {
        self.base =
            Url::parse(s).map_err(|e| ApiError::Config(format!("invalid base URL: {e}")))?;
        Ok(self)
    }

    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = ua.into();
        self
    }

    #[must_use]
    pub fn request_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }

    pub fn build(self) -> Result<Client> {
        let http = reqwest::Client::builder()
            .user_agent(self.user_agent)
            .timeout(self.request_timeout)
            .build()
            .map_err(ApiError::from)?;
        Ok(Client {
            inner: Arc::new(Inner {
                http,
                base: self.base,
                tokens: RwLock::new(Tokens::default()),
                limiter: EndpointLimiter::new(),
            }),
        })
    }
}

impl Client {
    #[must_use]
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Build a client with all defaults (production API base, 30s timeout).
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    /// Returns the configured API base URL.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.base
    }

    /// Returns a clone of the current tokens. Returns the default (empty) tokens
    /// before login.
    pub async fn tokens(&self) -> Tokens {
        self.inner.tokens.read().await.clone()
    }

    /// Replace the entire token bundle. Used at login or when restoring a session.
    pub async fn set_tokens(&self, t: Tokens) {
        *self.inner.tokens.write().await = t;
    }

    /// Update just the `id_token` (and optionally `rtdb_token`). Used by the
    /// refresh flow which doesn't return a new `refresh_token`.
    pub async fn update_id_token(&self, id_token: String, rtdb_token: Option<String>) {
        let mut t = self.inner.tokens.write().await;
        t.id_token = id_token;
        if let Some(rt) = rtdb_token {
            t.rtdb_token = rt;
        }
    }

    /// Discard cached tokens. Does not call the server.
    pub async fn clear_tokens(&self) {
        *self.inner.tokens.write().await = Tokens::default();
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.inner
            .base
            .join(path)
            .map_err(|e| ApiError::Config(format!("invalid path {path}: {e}")))
    }

    /// Make an authenticated request and decode a `{ "data": T }` envelope.
    pub(crate) async fn request<T, B>(
        &self,
        key: EndpointKey,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let raw = self
            .send_with_refresh(key, method, path, query, body)
            .await?;
        let env: Data<T> = serde_json::from_slice(&raw)?;
        Ok(env.data)
    }

    /// Make an authenticated request and decode a `{ "data": [T], "cursor": ? }`
    /// envelope. Returns the items and the next-page cursor (`None` if exhausted).
    pub(crate) async fn request_page<T>(
        &self,
        key: EndpointKey,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(Vec<T>, Option<String>)>
    where
        T: DeserializeOwned,
    {
        let raw = self
            .send_with_refresh::<()>(key, method, path, query, None)
            .await?;
        let env: Page<T> = serde_json::from_slice(&raw)?;
        Ok((env.data, env.cursor))
    }

    /// Make a request that has no response body (e.g. mark-as-read PATCH).
    // TODO(phase-2.1): first caller lands with notification mark-read.
    #[allow(dead_code)]
    pub(crate) async fn request_unit(
        &self,
        key: EndpointKey,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<()> {
        let _ = self
            .send_with_refresh::<()>(key, method, path, query, None)
            .await?;
        Ok(())
    }

    /// Send an authenticated request, transparently refreshing the id_token and
    /// retrying once on 401. The `auth::refresh` method (same crate) is called
    /// when a refresh_token is available.
    async fn send_with_refresh<B>(
        &self,
        key: EndpointKey,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
    ) -> Result<Vec<u8>>
    where
        B: Serialize + ?Sized,
    {
        match self
            .send_raw(key, method.clone(), path, query, body, true)
            .await
        {
            Ok(bytes) => Ok(bytes),
            Err(ApiError::Unauthorized) => {
                // No refresh_token? Bubble the original 401 — the caller must re-login.
                if self.tokens().await.refresh_token.is_empty() {
                    return Err(ApiError::Unauthorized);
                }
                tracing::debug!(endpoint = ?key, "id_token expired — refreshing");
                self.refresh().await?;
                self.send_raw(key, method, path, query, body, true).await
            }
            Err(e) => Err(e),
        }
    }

    /// Make an UN-authenticated request (login, register, check-username) and
    /// decode a `{ "data": T }` envelope.
    pub(crate) async fn request_public<T, B>(
        &self,
        key: EndpointKey,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let raw = self.send_raw(key, method, path, &[], body, false).await?;
        let env: Data<T> = serde_json::from_slice(&raw)?;
        Ok(env.data)
    }

    /// Low-level request: rate-limit → bearer → send → 429 backoff → response bytes.
    ///
    /// 401 handling is deliberately left to the caller (the auth module wraps this
    /// to retry once after `/v1/auth/refresh`). This keeps the dependency direction
    /// clean: rate-limit lives below auth, not the other way around.
    async fn send_raw<B>(
        &self,
        key: EndpointKey,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
        authenticated: bool,
    ) -> Result<Vec<u8>>
    where
        B: Serialize + ?Sized,
    {
        self.inner.limiter.acquire(key).await;

        let url = self.url(path)?;
        let id_token = if authenticated {
            let t = self.inner.tokens.read().await;
            if !t.is_authenticated() {
                return Err(ApiError::Unauthorized);
            }
            Some(t.id_token.clone())
        } else {
            None
        };

        let mut attempt: u32 = 0;
        loop {
            let mut req = self
                .inner
                .http
                .request(method.clone(), url.clone())
                .query(query);
            if let Some(tok) = &id_token {
                req = req.bearer_auth(tok);
            }
            if let Some(b) = body {
                req = req.json(b);
            }

            let resp = req.send().await?;
            let status = resp.status();

            if status == StatusCode::TOO_MANY_REQUESTS && attempt < MAX_429_RETRIES {
                let wait = parse_retry_after(&resp).unwrap_or_else(|| backoff_delay(attempt));
                tracing::warn!(
                    endpoint = ?key,
                    attempt = attempt + 1,
                    wait_ms = wait.as_millis() as u64,
                    "429 — backing off"
                );
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }

            // A 429 that survives our retries is surfaced as the dedicated
            // `RateLimited` variant carrying the server's wait hint, so the UI
            // can show a retry countdown. Parse the header before the response is
            // consumed by `bytes()`.
            let retry_after = if status == StatusCode::TOO_MANY_REQUESTS {
                Some(
                    parse_retry_after(&resp)
                        .unwrap_or_else(|| backoff_delay(attempt))
                        .as_secs(),
                )
            } else {
                None
            };

            let bytes = resp.bytes().await?.to_vec();

            if status.is_success() {
                return Ok(bytes);
            }

            if let Some(retry_after_secs) = retry_after {
                return Err(ApiError::RateLimited { retry_after_secs });
            }

            return Err(parse_error_body(status, &bytes));
        }
    }

    /// Download raw bytes from an arbitrary image URL. Deliberately
    /// **unauthenticated**: image URLs in posts may point at third-party hosts,
    /// so the bearer token must never be attached. The response size is capped.
    pub async fn fetch_image(&self, url: &str) -> Result<Vec<u8>> {
        let mut req = self.inner.http.get(url);
        // Attach auth only for cyberspace-owned hosts (e.g. bunker.cyberspace.online),
        // where uploads may be gated. Never send the token to third-party hosts.
        if is_cyberspace_url(url) {
            let token = self.tokens().await.id_token;
            if !token.is_empty() {
                req = req.bearer_auth(token);
            }
        }
        let resp = req.send().await?.error_for_status()?;
        if let Some(len) = resp.content_length() {
            if len > MAX_IMAGE_BYTES {
                return Err(ApiError::Config(format!("image too large ({len} bytes)")));
            }
        }
        let bytes = resp.bytes().await?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(ApiError::Config("image too large".into()));
        }
        Ok(bytes.to_vec())
    }
}

/// Whether a URL points at a cyberspace.online-owned host (so it's safe to
/// attach the bearer token).
fn is_cyberspace_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .map(|h| h == "cyberspace.online" || h.ends_with(".cyberspace.online"))
        .unwrap_or(false)
}

fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let raw = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    raw.parse::<u64>().ok().map(Duration::from_secs)
}

fn backoff_delay(attempt: u32) -> Duration {
    // 1s, 2s, 4s — capped at 30s.
    let secs = 1u64 << attempt.min(5);
    Duration::from_secs(secs.min(30))
}

fn parse_error_body(status: StatusCode, body: &[u8]) -> ApiError {
    if let Ok(env) = serde_json::from_slice::<ErrorEnvelope>(body) {
        if env.error.code == ErrorCode::Unauthorized {
            return ApiError::Unauthorized;
        }
        return ApiError::Api {
            code: env.error.code,
            message: env.error.message,
            status: status.as_u16(),
        };
    }
    let message = std::str::from_utf8(body)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|_| format!("<{} bytes of non-UTF-8>", body.len()));
    ApiError::Api {
        code: ErrorCode::Unknown,
        message,
        status: status.as_u16(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_to_production_base() {
        let c = Client::new().unwrap();
        assert_eq!(c.base_url().as_str(), "https://api.cyberspace.online/");
    }

    #[test]
    fn builder_accepts_custom_base() {
        let c = Client::builder()
            .base_url_str("https://staging.example.com")
            .unwrap()
            .build()
            .unwrap();
        assert!(c.base_url().as_str().contains("staging.example.com"));
    }

    #[test]
    fn builder_rejects_invalid_base() {
        let err = Client::builder().base_url_str("not a url").unwrap_err();
        assert!(matches!(err, ApiError::Config(_)));
    }

    #[tokio::test]
    async fn tokens_round_trip() {
        let c = Client::new().unwrap();
        assert!(!c.tokens().await.is_authenticated());

        c.set_tokens(Tokens {
            id_token: "id".into(),
            refresh_token: "r".into(),
            rtdb_token: "rt".into(),
        })
        .await;
        assert!(c.tokens().await.is_authenticated());

        c.update_id_token("id2".into(), Some("rt2".into())).await;
        let t = c.tokens().await;
        assert_eq!(t.id_token, "id2");
        assert_eq!(t.refresh_token, "r");
        assert_eq!(t.rtdb_token, "rt2");

        c.clear_tokens().await;
        assert!(!c.tokens().await.is_authenticated());
    }

    #[test]
    fn backoff_grows_exponentially_capped() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(10), Duration::from_secs(30));
    }

    #[test]
    fn parse_error_body_recognizes_envelope() {
        let body = br#"{"error":{"code":"VALIDATION_ERROR","message":"bad"}}"#;
        let err = parse_error_body(StatusCode::BAD_REQUEST, body);
        match err {
            ApiError::Api {
                code,
                message,
                status,
            } => {
                assert_eq!(code, ErrorCode::ValidationError);
                assert_eq!(message, "bad");
                assert_eq!(status, 400);
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn parse_error_body_maps_401_to_unauthorized() {
        let body = br#"{"error":{"code":"UNAUTHORIZED","message":"x"}}"#;
        let err = parse_error_body(StatusCode::UNAUTHORIZED, body);
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[test]
    fn parse_error_body_falls_back_to_raw_text() {
        let err = parse_error_body(StatusCode::BAD_GATEWAY, b"<html>nginx</html>");
        match err {
            ApiError::Api { code, status, .. } => {
                assert_eq!(code, ErrorCode::Unknown);
                assert_eq!(status, 502);
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }
}

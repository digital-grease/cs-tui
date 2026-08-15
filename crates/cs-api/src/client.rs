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

    /// How long until a request to `key` would be allowed by the client-side
    /// rate limiter, without spending anything. Lets the UI show a countdown or
    /// disable a submit instead of letting `acquire` hang silently. Zero when
    /// writable now (or the endpoint is unlimited / untouched).
    ///
    /// The wait is the rolling one § Rate Limits describes, so a spent 2/min
    /// budget reads as a full minute until its oldest request ages out, not as
    /// a half minute until an allowance has trickled back.
    #[must_use]
    pub fn time_until_writable(&self, key: EndpointKey) -> Duration {
        self.time_until_writable_scoped(key, None)
    }

    /// [`time_until_writable`](Client::time_until_writable) for an endpoint with
    /// a per-room or per-conversation budget: pass the `roomId` or
    /// `conversationId` as `scope`. The answer is the longer of the scoped and
    /// the overall wait, because a call has to satisfy both. Endpoints with no
    /// second dimension ignore `scope`.
    #[must_use]
    pub fn time_until_writable_scoped(&self, key: EndpointKey, scope: Option<&str>) -> Duration {
        self.inner.limiter.peek_wait(key, scope)
    }

    /// Take back the rate-limiter grant a request drew, for a call the server
    /// rejected without charging it (§ Poke a User: "A rejected poke
    /// (`400`/`403`/`404`) doesn't count against it"). Pass the same `key` and
    /// `scope` the request was made with. A refund with no grant to take back
    /// does nothing, so it can never mint allowance the server never gave.
    pub(crate) fn refund_rate_limit(&self, key: EndpointKey, scope: Option<&str>) {
        self.inner.limiter.refund(key, scope);
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

    /// Update just the short-lived auth fields returned by `/v1/auth/refresh`.
    /// The `refresh_token` itself is preserved.
    pub async fn update_id_token(
        &self,
        id_token: String,
        rtdb_token: Option<String>,
        rtdb_url: Option<String>,
    ) {
        let mut t = self.inner.tokens.write().await;
        t.id_token = id_token;
        if let Some(rt) = rtdb_token {
            t.rtdb_token = rt;
        }
        if let Some(url) = rtdb_url {
            t.rtdb_url = url;
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
        self.request_scoped(key, None, method, path, query, body)
            .await
    }

    /// [`request`](Client::request) for an endpoint whose rate limit has a
    /// per-room or per-conversation dimension: pass the `roomId` or
    /// `conversationId` as `scope` and both budgets are charged together. Pass
    /// `None` for every other endpoint (which is what [`request`](Client::request)
    /// does).
    pub(crate) async fn request_scoped<T, B>(
        &self,
        key: EndpointKey,
        scope: Option<&str>,
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
            .send_with_refresh(key, scope, method, path, query, body)
            .await?;
        let env: Data<T> = decode_body(key, &raw)?;
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
        self.request_page_scoped(key, None, method, path, query)
            .await
    }

    /// [`request_page`](Client::request_page) with a rate-limit scope; see
    /// [`request_scoped`](Client::request_scoped).
    pub(crate) async fn request_page_scoped<T>(
        &self,
        key: EndpointKey,
        scope: Option<&str>,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(Vec<T>, Option<String>)>
    where
        T: DeserializeOwned,
    {
        let raw = self
            .send_with_refresh::<()>(key, scope, method, path, query, None)
            .await?;
        let env: Page<T> = decode_body(key, &raw)?;
        Ok((env.data, env.cursor))
    }

    /// Make a request whose response body is ignored (e.g. C-Mail mark-as-read).
    pub(crate) async fn request_unit(
        &self,
        key: EndpointKey,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<()> {
        self.request_unit_scoped(key, None, method, path, query)
            .await
    }

    /// [`request_unit`](Client::request_unit) with a rate-limit scope; see
    /// [`request_scoped`](Client::request_scoped).
    pub(crate) async fn request_unit_scoped(
        &self,
        key: EndpointKey,
        scope: Option<&str>,
        method: Method,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<()> {
        let _ = self
            .send_with_refresh::<()>(key, scope, method, path, query, None)
            .await?;
        Ok(())
    }

    /// Send an authenticated request, transparently refreshing the id_token and
    /// retrying once on 401. The `auth::refresh` method (same crate) is called
    /// when a refresh_token is available.
    ///
    /// Each attempt refunds its own rate-limit grant on a 401, the first before
    /// the refresh and the retry before its error is returned. A request
    /// rejected for an expired token never reached the endpoint's handler, so
    /// charging the caller for one action twice would be wrong, and on a budget
    /// with no per-minute window (`UsersPoke` is 1/hour) an unrefunded attempt
    /// strands the whole hour on a request the server never counted.
    ///
    /// A failure of the refresh sub-request is recast by [`refresh_failure`] so
    /// it can never be mistaken for the caller's request being rejected.
    async fn send_with_refresh<B>(
        &self,
        key: EndpointKey,
        scope: Option<&str>,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
    ) -> Result<Vec<u8>>
    where
        B: Serialize + ?Sized,
    {
        match self
            .send_raw(key, scope, method.clone(), path, query, body, true)
            .await
        {
            Ok(bytes) => Ok(bytes),
            Err(ApiError::Unauthorized) => {
                // A 401 is refused before the endpoint's handler runs, so the
                // server charged nothing and neither should the local mirror.
                self.refund_rate_limit(key, scope);
                // No refresh_token? Bubble the original 401, the caller must
                // re-login.
                if self.tokens().await.refresh_token.is_empty() {
                    return Err(ApiError::Unauthorized);
                }
                tracing::debug!(endpoint = ?key, "id_token expired, refreshing");
                if let Err(e) = self.refresh().await {
                    return Err(refresh_failure(e));
                }
                let retried = self
                    .send_raw(key, scope, method, path, query, body, true)
                    .await;
                if matches!(retried, Err(ApiError::Unauthorized)) {
                    // The retry drew its own grant and was refused just as
                    // early as the first attempt, so the server counted it just
                    // as little. Hand it back before the error goes out.
                    self.refund_rate_limit(key, scope);
                }
                retried
            }
            Err(e) => Err(e),
        }
    }

    /// Make an UN-authenticated request (e.g. login) and decode a
    /// `{ "data": T }` envelope.
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
        let raw = self
            .send_raw(key, None, method, path, &[], body, false)
            .await?;
        let env: Data<T> = decode_body(key, &raw)?;
        Ok(env.data)
    }

    /// Low-level request: rate-limit → bearer → send → 429 backoff → response bytes.
    ///
    /// `scope` is the per-room / per-conversation id for the endpoints whose
    /// limit has a second dimension, and `None` everywhere else.
    ///
    /// 401 handling is deliberately left to the caller (the auth module wraps this
    /// to retry once after `/v1/auth/refresh`). This keeps the dependency direction
    /// clean: rate-limit lives below auth, not the other way around.
    // The parameter list is one over clippy's threshold; splitting it into a
    // request struct would churn every helper above for no gain in clarity.
    #[allow(clippy::too_many_arguments)]
    async fn send_raw<B>(
        &self,
        key: EndpointKey,
        scope: Option<&str>,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
        authenticated: bool,
    ) -> Result<Vec<u8>>
    where
        B: Serialize + ?Sized,
    {
        self.inner.limiter.acquire(key, scope).await;

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

            // Every 429 is fed back into the limiter, retried or not. The server
            // has just said its own window is full, which is the one thing the
            // local model cannot derive: nothing else would drain a budget the
            // spec documents as unlimited, and without this the next `peek_wait`
            // would answer "writable now" straight into another 429. Parse the
            // hint before `bytes()` consumes the response.
            let retry_after = if status == StatusCode::TOO_MANY_REQUESTS {
                let wait = parse_retry_after(&resp).unwrap_or_else(|| backoff_delay(attempt));
                self.inner.limiter.penalise(key, wait);
                Some(wait)
            } else {
                None
            };

            if let Some(wait) = retry_after {
                if attempt < MAX_429_RETRIES {
                    tracing::warn!(
                        endpoint = ?key,
                        attempt = attempt + 1,
                        wait_ms = wait.as_millis() as u64,
                        "429, backing off"
                    );
                    attempt += 1;
                    tokio::time::sleep(wait).await;
                    // A retry is another request as far as the server's window
                    // is concerned, so it draws its own grant: one `acquire`
                    // must not put four requests on the wire. Taking it without
                    // waiting also decides when to stop. If the modelled budget
                    // has nothing left, retrying would mean blocking the caller
                    // until it does, which on a 1/hour endpoint (`UsersPoke`) is
                    // the best part of an hour spent hanging. Fall through
                    // instead and surface the 429 with the server's own hint.
                    if self.inner.limiter.try_acquire(key, scope) {
                        continue;
                    }
                }
            }

            let bytes = resp.bytes().await?.to_vec();

            if status.is_success() {
                return Ok(bytes);
            }

            // A 429 that survives our retries is surfaced as the dedicated
            // `RateLimited` variant carrying the server's wait hint, so the UI
            // can show a retry countdown.
            if let Some(wait) = retry_after {
                return Err(ApiError::RateLimited {
                    retry_after_secs: wait.as_secs(),
                });
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

/// Recast a failure of the `/v1/auth/refresh` sub-request into one that cannot
/// be mistaken for the caller's own request being rejected.
///
/// [`Client::send_with_refresh`] runs two different requests and only one of
/// them is the caller's, but the error it returns carries no hint of which.
/// Callers act on the status they are handed: `poke_user` refunds its limiter
/// grant on `400`/`403`/`404` because § Poke a User says a rejected poke isn't
/// charged, and an expired `refreshToken` commonly answers `400`. Letting the
/// refresh endpoint's own status through would earn a second refund for one
/// request, on top of the one the 401 already took, and an unearned refund is
/// allowance the server never granted.
///
/// Every server-side refusal of a refresh means the same thing, the session is
/// over and the user has to sign in again, so they all become `Unauthorized`.
/// Transport and decode failures pass through untouched: they say nothing about
/// the session, and telling "offline" from "signed out" is exactly what the UI
/// uses them for. `RateLimited` passes through for the same reason, it carries a
/// retry hint worth showing and no caller treats it as an uncharged rejection.
fn refresh_failure(e: ApiError) -> ApiError {
    match e {
        ApiError::Api { .. } | ApiError::Unauthorized => ApiError::Unauthorized,
        other => other,
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

/// Decode a response body, logging the raw JSON on failure. Several response
/// shapes are undocumented in the spec, so when a `serde` field/shape mismatch
/// surfaces during live testing this puts the actual body in the log
/// (`RUST_LOG=cs_api=debug`, or the on-disk log) so the wire types can be fixed.
fn decode_body<T: DeserializeOwned>(key: EndpointKey, raw: &[u8]) -> Result<T> {
    serde_json::from_slice(raw).map_err(|e| {
        let snippet: String = String::from_utf8_lossy(raw).chars().take(500).collect();
        tracing::warn!(endpoint = ?key, error = %e, body = %snippet, "response decode failed");
        ApiError::Decode(e)
    })
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

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// Build a canned HTTP/1.1 response. `Connection: close` keeps the test
    /// server one-request-per-connection, which is all it needs to be.
    fn response(status: &str, extra_headers: &[(&str, &str)], body: &str) -> String {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in extra_headers {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.push_str(body);
        out
    }

    fn unauthorized() -> String {
        response(
            "401 Unauthorized",
            &[],
            r#"{"error":{"code":"UNAUTHORIZED","message":"token expired"}}"#,
        )
    }

    /// A `429` carrying the given `Retry-After`, in seconds.
    fn throttled(retry_after_secs: &str) -> String {
        response(
            "429 Too Many Requests",
            &[("Retry-After", retry_after_secs)],
            r#"{"error":{"code":"RATE_LIMITED","message":"slow down"}}"#,
        )
    }

    /// End of the request head, one past the blank line.
    fn header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    /// The declared body length, or zero when the request has no body.
    fn content_length(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0)
    }

    /// Read one whole request, head and body. Draining the body matters: a
    /// socket closed with unread data in it is reset rather than shut down, and
    /// the reset can lose the response that was just written to it.
    async fn read_request(sock: &mut TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            if let Some(end) = header_end(&buf) {
                if buf.len() >= end + content_length(&buf[..end]) {
                    return;
                }
            }
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    /// A `Client` pointed at a throwaway server that answers each request with
    /// the next canned response, in order.
    ///
    /// The crate carries no mock-HTTP dependency, and these tests need real
    /// round trips: the rate-limit accounting they check happens inside the
    /// request layer, between `acquire` and the decoded body.
    async fn client_served_by(responses: Vec<String>) -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in responses {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                read_request(&mut sock).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.flush().await;
                let _ = sock.shutdown().await;
            }
        });
        let c = Client::builder()
            .base_url_str(&format!("http://{addr}"))
            .unwrap()
            .build()
            .unwrap();
        c.set_tokens(Tokens {
            id_token: "id1".into(),
            refresh_token: "r1".into(),
            ..Tokens::default()
        })
        .await;
        c
    }

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
            rtdb_url: "https://db1.example".into(),
        })
        .await;
        assert!(c.tokens().await.is_authenticated());

        c.update_id_token(
            "id2".into(),
            Some("rt2".into()),
            Some("https://db2.example".into()),
        )
        .await;
        let t = c.tokens().await;
        assert_eq!(t.id_token, "id2");
        assert_eq!(t.refresh_token, "r");
        assert_eq!(t.rtdb_token, "rt2");
        assert_eq!(t.rtdb_url, "https://db2.example");

        c.clear_tokens().await;
        assert!(!c.tokens().await.is_authenticated());
    }

    #[test]
    fn scoped_writability_is_reported_per_scope() {
        let c = Client::new().unwrap();
        // Untouched buckets start full, scoped or not.
        assert_eq!(
            c.time_until_writable_scoped(EndpointKey::CircPresence, Some("general")),
            Duration::ZERO
        );
        assert_eq!(
            c.time_until_writable(EndpointKey::CircPresence),
            Duration::ZERO
        );
        // A refund on an untouched endpoint is a no-op, not a way to mint tokens.
        c.refund_rate_limit(EndpointKey::UsersPoke, None);
        assert_eq!(
            c.time_until_writable(EndpointKey::UsersPoke),
            Duration::ZERO
        );
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

    #[tokio::test]
    async fn a_second_401_hands_its_rate_limit_grant_back() {
        // 401, refresh, 401 again. Neither attempt reached the endpoint's
        // handler, so the server counted neither and the local mirror must not
        // strand the whole hourly poke allowance on them.
        let refreshed = response(
            "200 OK",
            &[],
            r#"{"data":{"idToken":"id2","rtdbToken":"rt2","rtdbUrl":"https://db.example"}}"#,
        );
        let c = client_served_by(vec![unauthorized(), refreshed, unauthorized()]).await;

        let err = c
            .send_with_refresh::<()>(
                EndpointKey::UsersPoke,
                None,
                Method::POST,
                "/v1/users/bob/poke",
                &[],
                None,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ApiError::Unauthorized), "got {err:?}");
        assert_eq!(
            c.time_until_writable(EndpointKey::UsersPoke),
            Duration::ZERO,
            "poke is 1/hour, so an unrefunded retry costs an hour of budget the server never charged"
        );
        assert_eq!(
            c.inner
                .limiter
                .live_minute_grants(EndpointKey::EntriesCreate, None),
            None,
            "and nothing else was touched"
        );
    }

    #[tokio::test]
    async fn a_failed_refresh_is_not_reported_as_the_callers_own_rejection() {
        // The refresh sub-request answers 400, which is exactly what an expired
        // refreshToken does. Surfacing that status would look to `poke_user`
        // (users.rs) like the poke itself being refused, and it refunds on
        // 400/403/404, on top of the refund the 401 already took.
        let refresh_refused = response(
            "400 Bad Request",
            &[],
            r#"{"error":{"code":"VALIDATION_ERROR","message":"invalid refresh token"}}"#,
        );
        let c = client_served_by(vec![unauthorized(), refresh_refused]).await;

        let err = c
            .send_with_refresh::<()>(
                EndpointKey::UsersPoke,
                None,
                Method::POST,
                "/v1/users/bob/poke",
                &[],
                None,
            )
            .await
            .unwrap_err();

        assert!(
            !matches!(&err, ApiError::Api { status, .. } if matches!(*status, 400 | 403 | 404)),
            "a refresh failure must not wear the poke's uncharged-rejection statuses: {err:?}"
        );
        assert!(
            matches!(err, ApiError::Unauthorized),
            "a refused refresh is the session ending: {err:?}"
        );
        assert_eq!(
            c.time_until_writable(EndpointKey::UsersPoke),
            Duration::ZERO,
            "the one attempt that went out was refunded exactly once"
        );
    }

    #[tokio::test]
    async fn a_refresh_that_never_reached_the_server_stays_a_transport_error() {
        // The 401 arrives, then the server is gone: the refresh fails with no
        // response at all. That says nothing about the session, so it must not
        // be laundered into "signed out" and cost the user their offline
        // indication.
        let c = client_served_by(vec![unauthorized()]).await;
        let err = c
            .send_with_refresh::<()>(
                EndpointKey::UsersPoke,
                None,
                Method::POST,
                "/v1/users/bob/poke",
                &[],
                None,
            )
            .await
            .unwrap_err();
        assert!(err.is_transport(), "got {err:?}");
    }

    #[tokio::test]
    async fn every_429_reaches_the_limiter_including_the_last() {
        // Four 429s: the three retried ones carry no wait, the final one asks
        // for 30s. Nothing else can teach the limiter about this endpoint,
        // which the spec documents with no limit at all, so if the 429 isn't
        // fed back the next call fires straight into another one.
        let c = client_served_by(vec![
            throttled("0"),
            throttled("0"),
            throttled("0"),
            throttled("30"),
        ])
        .await;
        assert_eq!(
            c.time_until_writable(EndpointKey::EntriesGet),
            Duration::ZERO,
            "the endpoint declares no client-side limit"
        );

        let err = c
            .send_with_refresh::<()>(
                EndpointKey::EntriesGet,
                None,
                Method::GET,
                "/v1/posts/p1",
                &[],
                None,
            )
            .await
            .unwrap_err();

        assert!(
            matches!(
                err,
                ApiError::RateLimited {
                    retry_after_secs: 30
                }
            ),
            "got {err:?}"
        );
        let wait = c.time_until_writable(EndpointKey::EntriesGet);
        assert!(
            wait > Duration::from_secs(25) && wait <= Duration::from_secs(30),
            "the server's Retry-After must gate the next call, got {wait:?}"
        );
    }

    #[tokio::test]
    async fn each_429_retry_draws_its_own_grant() {
        // One acquire must not put two requests on the wire: the retry is
        // another request as far as the server's window is concerned.
        let created = response("201 Created", &[], r#"{"data":{"postId":"p1"}}"#);
        let c = client_served_by(vec![throttled("0"), created]).await;

        c.send_with_refresh::<()>(
            EndpointKey::EntriesCreate,
            None,
            Method::POST,
            "/v1/posts",
            &[],
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            c.inner
                .limiter
                .live_minute_grants(EndpointKey::EntriesCreate, None),
            Some(2),
            "two requests went out, so the 2/min budget owes two grants"
        );
        assert!(
            !c.time_until_writable(EndpointKey::EntriesCreate).is_zero(),
            "and the budget is spent until the window rolls"
        );
    }

    #[tokio::test]
    async fn a_429_retry_stops_at_the_modelled_budget_instead_of_blocking_on_it() {
        // EntriesCreate is 2/min, so the first attempt and one retry spend the
        // window and the next retry has nothing to draw on. Waiting for the
        // window to roll would hang the caller, which on the 1/hour poke budget
        // means the best part of an hour, so the 429 is surfaced instead even
        // though two retries were still allowed.
        let c = client_served_by(vec![throttled("0"); 4]).await;

        let err = c
            .send_with_refresh::<()>(
                EndpointKey::EntriesCreate,
                None,
                Method::POST,
                "/v1/posts",
                &[],
                None,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ApiError::RateLimited { .. }), "got {err:?}");
        assert_eq!(
            c.inner
                .limiter
                .live_minute_grants(EndpointKey::EntriesCreate, None),
            Some(2),
            "only the two requests the budget could pay for went out, not four"
        );
    }

    #[tokio::test]
    async fn the_flag_call_sites_share_one_budget() {
        // § Flag an Entry: the flag endpoints share a single 5/min, 20/hour,
        // 50/day budget. Drive the entry and reply call sites alternately and
        // prove they draw on one budget rather than one each. Split into a key
        // per endpoint, the client would spend a multiple of what the server
        // allows and sail past its own limiter into a 429.
        let flagged = response("200 OK", &[], r#"{"data":{"flagged":true,"flagId":"f1"}}"#);
        let c = client_served_by(vec![flagged; 5]).await;

        c.flag_entry("p1", None).await.unwrap();
        c.flag_reply("r1", None).await.unwrap();
        c.flag_circ_message("general", "m1", None).await.unwrap();
        c.flag_reply("r2", None).await.unwrap();
        c.flag_circ_message("lounge", "m2", None).await.unwrap();

        assert_eq!(
            c.inner.limiter.live_minute_grants(EndpointKey::Flag, None),
            Some(5),
            "five flags across all three endpoints, one budget"
        );
        assert_eq!(
            c.inner
                .limiter
                .live_minute_grants(EndpointKey::Flag, Some("general")),
            None,
            "flagging is unscoped, so a room never gets a budget of its own"
        );
        assert!(
            !c.time_until_writable(EndpointKey::Flag).is_zero(),
            "the shared 5/min budget is spent, so a sixth flag of any kind waits"
        );
    }

    #[tokio::test]
    async fn marking_all_read_keeps_calling_until_the_server_says_it_is_done() {
        // § Mark All as Read marks at most 5,000 per call and reports `hasMore`
        // while unread notifications remain. One call would leave a big inbox
        // partly unread under a name that promises otherwise, so the client
        // calls again until the server clears the flag and returns the total.
        let pass = |body: &str| response("200 OK", &[], body);
        let c = client_served_by(vec![
            pass(r#"{"data":{"updated":5000,"hasMore":true}}"#),
            pass(r#"{"data":{"updated":5000,"hasMore":true}}"#),
            pass(r#"{"data":{"updated":12,"hasMore":false}}"#),
        ])
        .await;

        let updated = c.mark_all_notifications_read().await.unwrap();
        assert_eq!(updated, 10_012, "every pass counts towards the total");
    }

    #[tokio::test]
    async fn marking_all_read_stops_when_a_pass_makes_no_progress() {
        // `hasMore` with nothing marked cannot be made to advance by asking
        // again, so the loop stops instead of hammering the endpoint. The test
        // server has exactly one response to give: a second request would find
        // the listener gone and fail as a transport error, which is what proves
        // the loop stopped rather than merely finished.
        let c = client_served_by(vec![response(
            "200 OK",
            &[],
            r#"{"data":{"updated":0,"hasMore":true}}"#,
        )])
        .await;

        let updated = c.mark_all_notifications_read().await.unwrap();
        assert_eq!(updated, 0);
    }

    #[tokio::test]
    async fn presence_and_typing_call_sites_charge_both_of_their_budgets() {
        // § Rate Limits caps presence at 15 per room and 90 overall, and C-Mail
        // typing at 40 per conversation and 120 overall. The call sites have to
        // pass the roomId / conversationId as the limiter scope, or one room
        // could spend the whole overall allowance without its own budget ever
        // noticing.
        // Both halves of each pair are driven: the announce/set call site and
        // the leave/clear one share a budget, so a scope missing from either
        // leaks the same way.
        let ok = response("200 OK", &[], r#"{"data":{"ok":true}}"#);
        let c = client_served_by(vec![ok.clone(), ok.clone(), ok.clone(), ok]).await;

        c.announce_circ_presence("general", None).await.unwrap();
        c.leave_circ_room("general").await.unwrap();
        c.set_cmail_typing("c1").await.unwrap();
        c.clear_cmail_typing("c1").await.unwrap();

        let limiter = &c.inner.limiter;
        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CircPresence, Some("general")),
            Some(2),
            "the room's own 15/min budget was charged by both call sites"
        );
        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CircPresence, None),
            Some(2),
            "and so was the 90/min overall budget"
        );
        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CircPresence, Some("lounge")),
            None,
            "but no other room's"
        );

        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CmailTyping, Some("c1")),
            Some(2),
            "the conversation's own 40/min budget was charged by both call sites"
        );
        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CmailTyping, None),
            Some(2),
            "and so was the 120/min overall budget"
        );
        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CmailTyping, Some("c2")),
            None,
            "but no other conversation's"
        );
    }
}

//! Authentication endpoints (`/v1/auth/*`).
//!
//! Wraps `POST /v1/auth/login`, `/register`, `/refresh`, `/check-username`, and
//! `/resend-verification`. `login` and `register` store the returned tokens on
//! `Client`; `refresh` updates only the `id_token` (and `rtdb_token`, if present)
//! while preserving the `refresh_token`.
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::tokens::Tokens;

#[derive(Debug, Serialize)]
struct LoginRequest<'a> {
    email: &'a str,
    password: &'a str,
}

#[derive(Debug, Serialize)]
struct RegisterRequest<'a> {
    email: &'a str,
    password: &'a str,
    username: &'a str,
}

#[derive(Debug, Serialize)]
struct RefreshRequest<'a> {
    #[serde(rename = "refreshToken")]
    refresh_token: &'a str,
}

#[derive(Debug, Serialize)]
struct ResendVerificationRequest<'a> {
    #[serde(rename = "idToken")]
    id_token: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct ResendVerificationResponse {
    #[serde(default)]
    pub sent: bool,
}

#[derive(Debug, Serialize)]
struct CheckUsernameRequest<'a> {
    username: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct CheckUsernameResponse {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Client {
    /// `POST /v1/auth/login` — exchange email + password for a token bundle.
    /// On success the bundle is stored on this `Client`.
    pub async fn login(&self, email: &str, password: &str) -> Result<Tokens> {
        let body = LoginRequest { email, password };
        let tokens: Tokens = self
            .request_public(
                EndpointKey::AuthLogin,
                Method::POST,
                "/v1/auth/login",
                Some(&body),
            )
            .await?;
        self.set_tokens(tokens.clone()).await;
        Ok(tokens)
    }

    /// `POST /v1/auth/register` — create an account. Returns the same token
    /// bundle as login; the client is authenticated immediately.
    pub async fn register(&self, email: &str, password: &str, username: &str) -> Result<Tokens> {
        let body = RegisterRequest {
            email,
            password,
            username,
        };
        let tokens: Tokens = self
            .request_public(
                EndpointKey::AuthRegister,
                Method::POST,
                "/v1/auth/register",
                Some(&body),
            )
            .await?;
        self.set_tokens(tokens.clone()).await;
        Ok(tokens)
    }

    /// `POST /v1/auth/refresh` — exchange the stored `refresh_token` for a fresh
    /// `id_token` (and `rtdb_token`). The `refresh_token` itself is preserved.
    pub async fn refresh(&self) -> Result<()> {
        let refresh_token = self.tokens().await.refresh_token;
        if refresh_token.is_empty() {
            return Err(ApiError::Unauthorized);
        }
        let body = RefreshRequest {
            refresh_token: &refresh_token,
        };
        let updated: Tokens = self
            .request_public(
                EndpointKey::AuthRefresh,
                Method::POST,
                "/v1/auth/refresh",
                Some(&body),
            )
            .await?;
        let new_rtdb = if updated.rtdb_token.is_empty() {
            None
        } else {
            Some(updated.rtdb_token)
        };
        self.update_id_token(updated.id_token, new_rtdb).await;
        Ok(())
    }

    /// `POST /v1/auth/check-username` — unauthenticated; returns availability
    /// and an optional reason string when unavailable.
    pub async fn check_username(&self, username: &str) -> Result<CheckUsernameResponse> {
        let body = CheckUsernameRequest { username };
        self.request_public(
            EndpointKey::AuthCheckUsername,
            Method::POST,
            "/v1/auth/check-username",
            Some(&body),
        )
        .await
    }

    /// `POST /v1/auth/resend-verification` — requires a valid `id_token` to be
    /// loaded on this client. Rate limit: 1/min, 5/hour.
    pub async fn resend_verification(&self) -> Result<ResendVerificationResponse> {
        let id_token = self.tokens().await.id_token;
        if id_token.is_empty() {
            return Err(ApiError::Unauthorized);
        }
        let body = ResendVerificationRequest {
            id_token: &id_token,
        };
        self.request_public(
            EndpointKey::AuthResendVerification,
            Method::POST,
            "/v1/auth/resend-verification",
            Some(&body),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_request_serializes_to_documented_shape() {
        let req = LoginRequest {
            email: "a@b.c",
            password: "p",
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"email":"a@b.c","password":"p"}"#);
    }

    #[test]
    fn refresh_request_uses_camel_case() {
        let req = RefreshRequest {
            refresh_token: "AMf-",
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains(r#""refreshToken":"AMf-""#));
    }

    #[test]
    fn check_username_response_handles_available() {
        let raw = r#"{"available":true}"#;
        let r: CheckUsernameResponse = serde_json::from_str(raw).unwrap();
        assert!(r.available);
        assert!(r.reason.is_none());
    }

    #[test]
    fn check_username_response_handles_unavailable_with_reason() {
        let raw = r#"{"available":false,"reason":"taken"}"#;
        let r: CheckUsernameResponse = serde_json::from_str(raw).unwrap();
        assert!(!r.available);
        assert_eq!(r.reason.as_deref(), Some("taken"));
    }

    #[tokio::test]
    async fn refresh_with_no_refresh_token_returns_unauthorized() {
        let c = Client::new().unwrap();
        let err = c.refresh().await.unwrap_err();
        assert!(err.is_unauthorized());
    }

    #[tokio::test]
    async fn resend_verification_with_no_id_token_returns_unauthorized() {
        let c = Client::new().unwrap();
        let err = c.resend_verification().await.unwrap_err();
        assert!(err.is_unauthorized());
    }
}

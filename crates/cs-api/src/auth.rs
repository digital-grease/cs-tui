//! Authentication endpoints (`/v1/auth/*`).
//!
//! Wraps `POST /v1/auth/login` and `/refresh`. `login` stores the returned token
//! bundle on `Client`; `refresh` updates only the short-lived auth/RTDB fields
//! while preserving the `refresh_token`. Account registration is out of scope —
//! this client is for users who already have a cyberspace account.
use reqwest::Method;
use serde::Serialize;

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
struct RefreshRequest<'a> {
    #[serde(rename = "refreshToken")]
    refresh_token: &'a str,
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

    /// `POST /v1/auth/refresh` — exchange the stored `refresh_token` for fresh
    /// `id_token`, `rtdb_token`, and `rtdb_url` fields. The `refresh_token` itself
    /// is preserved.
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
        let new_rtdb_url = if updated.rtdb_url.is_empty() {
            None
        } else {
            Some(updated.rtdb_url)
        };
        self.update_id_token(updated.id_token, new_rtdb, new_rtdb_url)
            .await;
        Ok(())
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

    #[tokio::test]
    async fn refresh_with_no_refresh_token_returns_unauthorized() {
        let c = Client::new().unwrap();
        let err = c.refresh().await.unwrap_err();
        assert!(err.is_unauthorized());
    }
}

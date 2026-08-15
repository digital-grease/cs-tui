//! Authentication endpoints (`/v1/auth/*`).
//!
//! Wraps the four routes v0.8.4 documents: `POST /v1/auth/login`, `/refresh`,
//! `/resend-verification` and `/check-username`. `login` stores the returned
//! token bundle on `Client`; `refresh` updates only the short-lived auth/RTDB
//! fields while preserving the `refresh_token`. Account registration is out of
//! scope, § Access says accounts are created on the website and there is no
//! signup endpoint.
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
struct ResendVerificationResponse {
    #[serde(default)]
    sent: bool,
}

#[derive(Debug, Serialize)]
struct CheckUsernameRequest<'a> {
    username: &'a str,
}

/// Answer to `POST /v1/auth/check-username` (v0.8.4 § Check Username
/// Availability).
#[derive(Debug, Clone, Deserialize)]
pub struct UsernameAvailability {
    /// Whether the handle can be registered.
    pub available: bool,

    /// Why not, when it can't. The spec only shows this alongside
    /// `available: false` (for example "Username is already taken"), and it is
    /// absent on the available answer.
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

    /// `POST /v1/auth/resend-verification`, ask for a fresh verification mail
    /// for the signed-in account (v0.8.4 § Resend Verification Email).
    ///
    /// This is the documented cure for the `403 EMAIL_NOT_VERIFIED` that
    /// § Access says every authenticated request gets until the address is
    /// verified: log in, call this, click the link. The stored `id_token` goes
    /// in the body rather than in an `Authorization` header, which is the shape
    /// the spec gives, so an unverified account can still call it.
    ///
    /// Returns the server's `sent` flag. Rate limit: 1/min, 5/hour, separate
    /// from every other auth route.
    pub async fn resend_verification(&self) -> Result<bool> {
        let id_token = self.tokens().await.id_token;
        if id_token.is_empty() {
            return Err(ApiError::Unauthorized);
        }
        let body = ResendVerificationRequest {
            id_token: &id_token,
        };
        let resp: ResendVerificationResponse = self
            .request_public(
                EndpointKey::AuthResendVerification,
                Method::POST,
                "/v1/auth/resend-verification",
                Some(&body),
            )
            .await?;
        Ok(resp.sent)
    }

    /// `POST /v1/auth/check-username`, is a handle free? (v0.8.4 § Check
    /// Username Availability).
    ///
    /// Needs no authentication, so it works before login. Rate limit: 10/min,
    /// 60/hour, counted per IP.
    pub async fn check_username(&self, username: &str) -> Result<UsernameAvailability> {
        let body = CheckUsernameRequest { username };
        self.request_public(
            EndpointKey::AuthCheckUsername,
            Method::POST,
            "/v1/auth/check-username",
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

    #[tokio::test]
    async fn refresh_with_no_refresh_token_returns_unauthorized() {
        let c = Client::new().unwrap();
        let err = c.refresh().await.unwrap_err();
        assert!(err.is_unauthorized());
    }

    #[test]
    fn resend_verification_request_carries_the_id_token_in_the_body() {
        // § Resend Verification Email documents `{ "idToken": "..." }`, not an
        // Authorization header, so an unverified account can still call it.
        let req = ResendVerificationRequest { id_token: "eyJhb" };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"idToken":"eyJhb"}"#);
    }

    #[test]
    fn resend_verification_response_decodes_the_sent_flag() {
        let ok: ResendVerificationResponse = serde_json::from_str(r#"{"sent":true}"#).unwrap();
        assert!(ok.sent);
        let quiet: ResendVerificationResponse = serde_json::from_str("{}").unwrap();
        assert!(!quiet.sent, "a missing flag is not a send");
    }

    #[tokio::test]
    async fn resend_verification_without_a_session_is_unauthorized() {
        // The body needs an idToken, so there is nothing to send before login.
        let c = Client::new().unwrap();
        let err = c.resend_verification().await.unwrap_err();
        assert!(err.is_unauthorized());
    }

    #[test]
    fn check_username_request_serializes_to_documented_shape() {
        let req = CheckUsernameRequest {
            username: "desired_name",
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"username":"desired_name"}"#);
    }

    #[test]
    fn username_availability_decodes_both_documented_answers() {
        // § Check Username Availability shows exactly these two bodies.
        let free: UsernameAvailability = serde_json::from_str(r#"{"available":true}"#).unwrap();
        assert!(free.available);
        assert!(free.reason.is_none());

        let taken: UsernameAvailability =
            serde_json::from_str(r#"{"available":false,"reason":"Username is already taken"}"#)
                .unwrap();
        assert!(!taken.available);
        assert_eq!(taken.reason.as_deref(), Some("Username is already taken"));
    }
}

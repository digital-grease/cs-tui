use serde::Deserialize;

pub type Result<T> = std::result::Result<T, ApiError>;

/// API-level error codes per the v0.5.0 spec (`docs/api-v0.5.0.md` § Error Codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Unauthorized,
    Forbidden,
    Banned,
    NotFound,
    ValidationError,
    Conflict,
    RateLimited,
    InternalError,
    #[serde(other)]
    Unknown,
}

impl ErrorCode {
    #[must_use]
    pub fn http_status(self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::Forbidden | Self::Banned => 403,
            Self::NotFound => 404,
            Self::ValidationError => 400,
            Self::Conflict => 409,
            Self::RateLimited => 429,
            Self::InternalError | Self::Unknown => 500,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("api {code:?} ({status}): {message}")]
    Api {
        code: ErrorCode,
        message: String,
        status: u16,
    },

    #[error("rate limited; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("unauthorized — token missing, invalid, or expired")]
    Unauthorized,

    #[error("not yet implemented")]
    NotImplemented,

    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),

    #[error("config: {0}")]
    Config(String),
}

impl ApiError {
    #[must_use]
    pub fn is_unauthorized(&self) -> bool {
        matches!(
            self,
            Self::Unauthorized
                | Self::Api {
                    code: ErrorCode::Unauthorized,
                    ..
                }
        )
    }

    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Api {
                    code: ErrorCode::RateLimited,
                    ..
                }
        )
    }

    /// A transport/network-layer failure (DNS, connection refused, TLS, timeout)
    /// — i.e. we never got an HTTP response. Distinct from an error *status*
    /// returned by the server, which means we're online.
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport(_))
    }

    /// The server-advertised wait before retrying, when this is a rate-limit
    /// error that carried a `Retry-After`. `None` for every other error and for
    /// rate-limit errors with no retry hint.
    #[must_use]
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimited { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        }
    }

    /// A short, human-facing message suitable for a toast or status line —
    /// unlike `Display`, it omits status codes and Rust error chains. For
    /// validation/conflict errors the server's own message is usually the most
    /// helpful text, so it's preserved when present.
    #[must_use]
    pub fn user_message(&self) -> String {
        let server_or = |message: &str, fallback: &str| {
            let m = message.trim();
            if m.is_empty() {
                fallback.to_string()
            } else {
                m.to_string()
            }
        };
        match self {
            Self::Api { code, message, .. } => match code {
                ErrorCode::ValidationError => server_or(message, "that didn't pass validation"),
                ErrorCode::Conflict => server_or(message, "that already exists"),
                ErrorCode::NotFound => "not found".to_string(),
                ErrorCode::Forbidden => "you're not allowed to do that".to_string(),
                ErrorCode::Banned => "your account is banned".to_string(),
                ErrorCode::Unauthorized => "session expired — please sign in again".to_string(),
                ErrorCode::RateLimited => "rate limited — slow down a moment".to_string(),
                ErrorCode::InternalError => "the server hit an error — try again".to_string(),
                ErrorCode::Unknown => server_or(message, "something went wrong"),
            },
            Self::RateLimited { retry_after_secs } => {
                format!("rate limited — retry after {retry_after_secs}s")
            }
            Self::Unauthorized => "session expired — please sign in again".to_string(),
            Self::NotImplemented => "not available yet".to_string(),
            Self::Transport(_) => "can't reach the server — check your connection".to_string(),
            Self::Decode(_) => "the server sent something unexpected".to_string(),
            Self::Config(m) => format!("configuration problem: {m}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_deserializes_screaming_snake() {
        let code: ErrorCode = serde_json::from_str("\"VALIDATION_ERROR\"").unwrap();
        assert_eq!(code, ErrorCode::ValidationError);
    }

    #[test]
    fn unknown_error_code_falls_through() {
        let code: ErrorCode = serde_json::from_str("\"SOMETHING_NEW\"").unwrap();
        assert_eq!(code, ErrorCode::Unknown);
    }

    #[test]
    fn http_status_mapping() {
        assert_eq!(ErrorCode::Unauthorized.http_status(), 401);
        assert_eq!(ErrorCode::RateLimited.http_status(), 429);
    }

    #[test]
    fn retry_after_only_set_for_rate_limited() {
        assert_eq!(
            ApiError::RateLimited {
                retry_after_secs: 30
            }
            .retry_after_secs(),
            Some(30)
        );
        assert_eq!(ApiError::Unauthorized.retry_after_secs(), None);
        assert_eq!(
            ApiError::Api {
                code: ErrorCode::RateLimited,
                message: "slow down".into(),
                status: 429,
            }
            .retry_after_secs(),
            None,
            "the envelope-coded form carries no retry hint"
        );
    }

    #[test]
    fn user_message_is_friendly_and_keeps_server_detail() {
        // Transport errors never leak the reqwest chain to users.
        let t = ApiError::Config("x".into()); // stand-in non-transport
        assert!(!t.user_message().contains("api "));

        // Validation/conflict keep the server's specific message when present…
        let v = ApiError::Api {
            code: ErrorCode::ValidationError,
            message: "Content cannot be empty".into(),
            status: 400,
        };
        assert_eq!(v.user_message(), "Content cannot be empty");

        // …and fall back to friendly copy when the server said nothing.
        let v_empty = ApiError::Api {
            code: ErrorCode::ValidationError,
            message: String::new(),
            status: 400,
        };
        assert_eq!(v_empty.user_message(), "that didn't pass validation");

        // Status codes / "api CODE (404)" never appear in the user copy.
        let nf = ApiError::Api {
            code: ErrorCode::NotFound,
            message: "x".into(),
            status: 404,
        };
        assert_eq!(nf.user_message(), "not found");

        // Rate-limit keeps the retry hint (and the "retry after Ns" phrasing).
        assert!(ApiError::RateLimited {
            retry_after_secs: 12
        }
        .user_message()
        .contains("retry after 12s"));
    }

    #[test]
    fn classification_helpers_partition_variants() {
        let rl = ApiError::RateLimited {
            retry_after_secs: 5,
        };
        assert!(rl.is_rate_limited() && !rl.is_transport() && !rl.is_unauthorized());

        let un = ApiError::Unauthorized;
        assert!(un.is_unauthorized() && !un.is_transport() && !un.is_rate_limited());

        let nf = ApiError::Api {
            code: ErrorCode::NotFound,
            message: "missing".into(),
            status: 404,
        };
        assert!(!nf.is_transport() && !nf.is_unauthorized() && !nf.is_rate_limited());
    }
}

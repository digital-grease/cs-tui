use serde::Deserialize;

pub type Result<T> = std::result::Result<T, ApiError>;

/// API-level error codes per the v0.4 spec (`docs/api-v0.4.md` § Error Codes).
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
            ApiError::RateLimited { retry_after_secs: 30 }.retry_after_secs(),
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
    fn classification_helpers_partition_variants() {
        let rl = ApiError::RateLimited { retry_after_secs: 5 };
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

use serde::Deserialize;

pub type Result<T> = std::result::Result<T, ApiError>;

/// API-level error codes per the v0.3.6 spec (`docs/api-v0.3.6.md` § Error Codes).
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
}

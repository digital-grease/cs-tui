//! Response envelope decoding.
//!
//! The v0.3.6 API wraps every payload in one of:
//! - `{ "data": T }` — single object
//! - `{ "data": [T], "cursor": "next|null" }` — paginated list
//! - `{ "error": { "code": "X", "message": "Y" } }` — error
use serde::Deserialize;

use crate::error::ErrorCode;

#[derive(Debug, Deserialize)]
pub(crate) struct Data<T> {
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Page<T> {
    pub data: Vec<T>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ErrorBody {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_envelope_decodes() {
        let raw = r#"{"data":{"hello":"world"}}"#;
        let env: Data<serde_json::Value> = serde_json::from_str(raw).unwrap();
        assert_eq!(env.data["hello"], "world");
    }

    #[test]
    fn page_envelope_decodes_with_null_cursor() {
        let raw = r#"{"data":[1,2,3],"cursor":null}"#;
        let env: Page<i32> = serde_json::from_str(raw).unwrap();
        assert_eq!(env.data, vec![1, 2, 3]);
        assert_eq!(env.cursor, None);
    }

    #[test]
    fn page_envelope_decodes_with_cursor() {
        let raw = r#"{"data":[],"cursor":"abc"}"#;
        let env: Page<serde_json::Value> = serde_json::from_str(raw).unwrap();
        assert_eq!(env.cursor.as_deref(), Some("abc"));
    }

    #[test]
    fn error_envelope_decodes() {
        let raw = r#"{"error":{"code":"VALIDATION_ERROR","message":"bad input"}}"#;
        let env: ErrorEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.error.code, ErrorCode::ValidationError);
        assert_eq!(env.error.message, "bad input");
    }
}

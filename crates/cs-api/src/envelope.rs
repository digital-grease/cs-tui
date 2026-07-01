//! Response envelope decoding.
//!
//! The API wraps every payload in one of:
//! - `{ "data": T }` — single object
//! - `{ "data": [T], "cursor": "next|null" }` — paginated list
//! - `{ "error": { "code": "X", "message": "Y" } }` — error
use serde::{Deserialize, Deserializer};

use crate::error::ErrorCode;

#[derive(Debug, Deserialize)]
pub(crate) struct Data<T> {
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Page<T> {
    pub data: Vec<T>,
    #[serde(default, deserialize_with = "deserialize_cursor")]
    pub cursor: Option<String>,
}

fn deserialize_cursor<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s)),
        serde_json::Value::Number(n) => Ok(Some(n.to_string())),
        other => Ok(Some(other.to_string())),
    }
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
    fn page_envelope_decodes_numeric_cursor_as_string() {
        let raw = r#"{"data":[],"cursor":1719700000000}"#;
        let env: Page<serde_json::Value> = serde_json::from_str(raw).unwrap();
        assert_eq!(env.cursor.as_deref(), Some("1719700000000"));
    }

    #[test]
    fn error_envelope_decodes() {
        let raw = r#"{"error":{"code":"VALIDATION_ERROR","message":"bad input"}}"#;
        let env: ErrorEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.error.code, ErrorCode::ValidationError);
        assert_eq!(env.error.message, "bad input");
    }
}

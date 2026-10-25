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
    pub cursor: Cursor,
}

/// A page's `cursor`, keeping "the server said `null`" apart from "the server
/// sent no cursor at all".
///
/// Most callers only want the next page, which is [`Cursor::next`], and treat
/// both as the end. They differ where a caller would otherwise guess: cIRC
/// derives a cursor from a full page when none is sent, and since v0.9.0
/// § Read a Room says an explicit `null` marks the last page inside the
/// history limit, which a guess would page straight past.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Cursor {
    /// No `cursor` key in the envelope.
    #[default]
    Absent,
    /// `"cursor": null`: no further page.
    Null,
    /// The next page's cursor.
    Next(String),
}

impl Cursor {
    /// The next page's cursor, if any. Absent and `null` both end the list.
    pub(crate) fn next(self) -> Option<String> {
        match self {
            Self::Next(c) => Some(c),
            Self::Absent | Self::Null => None,
        }
    }
}

/// Only called for a key that is present; an absent one takes the `default`.
fn deserialize_cursor<'de, D>(deserializer: D) -> std::result::Result<Cursor, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Null => Cursor::Null,
        serde_json::Value::String(s) => Cursor::Next(s),
        serde_json::Value::Number(n) => Cursor::Next(n.to_string()),
        other => Cursor::Next(other.to_string()),
    })
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
        assert_eq!(env.cursor, Cursor::Null);
        assert_eq!(env.cursor.next(), None);
    }

    #[test]
    fn page_envelope_decodes_with_cursor() {
        let raw = r#"{"data":[],"cursor":"abc"}"#;
        let env: Page<serde_json::Value> = serde_json::from_str(raw).unwrap();
        assert_eq!(env.cursor.next().as_deref(), Some("abc"));
    }

    #[test]
    fn page_envelope_decodes_numeric_cursor_as_string() {
        let raw = r#"{"data":[],"cursor":1719700000000}"#;
        let env: Page<serde_json::Value> = serde_json::from_str(raw).unwrap();
        assert_eq!(env.cursor.next().as_deref(), Some("1719700000000"));
    }

    #[test]
    fn page_envelope_tells_a_missing_cursor_from_a_null_one() {
        let env: Page<i32> = serde_json::from_str(r#"{"data":[1]}"#).unwrap();
        assert_eq!(env.cursor, Cursor::Absent);
        assert_eq!(env.cursor.next(), None);
    }

    #[test]
    fn error_envelope_decodes() {
        let raw = r#"{"error":{"code":"VALIDATION_ERROR","message":"bad input"}}"#;
        let env: ErrorEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.error.code, ErrorCode::ValidationError);
        assert_eq!(env.error.message, "bad input");
    }
}

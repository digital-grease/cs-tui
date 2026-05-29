use serde::{Deserialize, Serialize};

/// Tokens returned by `POST /v1/auth/login`, `/refresh`, and `/register`.
///
/// - `id_token` — Bearer token for REST requests; short-lived.
/// - `refresh_token` — exchanges for a new id_token via `/v1/auth/refresh`.
/// - `rtdb_token` — Firebase JWT used for Realtime Database (chat/DMs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(rename = "idToken", default, skip_serializing_if = "String::is_empty")]
    pub id_token: String,

    #[serde(
        rename = "refreshToken",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub refresh_token: String,

    #[serde(
        rename = "rtdbToken",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub rtdb_token: String,
}

impl Tokens {
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        !self.id_token.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_login_response_shape() {
        let raw = r#"{"idToken":"id","refreshToken":"r","rtdbToken":"rt"}"#;
        let t: Tokens = serde_json::from_str(raw).unwrap();
        assert_eq!(t.id_token, "id");
        assert_eq!(t.refresh_token, "r");
        assert_eq!(t.rtdb_token, "rt");
        assert!(t.is_authenticated());
    }

    #[test]
    fn refresh_response_with_missing_refresh_token_decodes() {
        // /v1/auth/refresh returns only { idToken, rtdbToken }
        let raw = r#"{"idToken":"id2","rtdbToken":"rt2"}"#;
        let t: Tokens = serde_json::from_str(raw).unwrap();
        assert_eq!(t.id_token, "id2");
        assert_eq!(t.refresh_token, "");
        assert_eq!(t.rtdb_token, "rt2");
    }

    #[test]
    fn empty_tokens_omits_empty_fields() {
        let t = Tokens::default();
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s, "{}");
        assert!(!t.is_authenticated());
    }
}

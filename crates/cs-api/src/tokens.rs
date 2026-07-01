use serde::{Deserialize, Serialize};

/// Tokens returned by `POST /v1/auth/login` and `/refresh`.
///
/// - `id_token` — Bearer token for REST requests; short-lived.
/// - `refresh_token` — exchanges for a new id_token via `/v1/auth/refresh`.
/// - `rtdb_token` — optional Firebase custom token for SDK RTDB access.
/// - `rtdb_url` — RTDB endpoint for direct reads with `id_token` auth.
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

    #[serde(rename = "rtdbUrl", default, skip_serializing_if = "String::is_empty")]
    pub rtdb_url: String,
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
        let raw = r#"{"idToken":"id","refreshToken":"r","rtdbToken":"rt","rtdbUrl":"https://cyberspace-cyberspace-default-rtdb.europe-west1.firebasedatabase.app"}"#;
        let t: Tokens = serde_json::from_str(raw).unwrap();
        assert_eq!(t.id_token, "id");
        assert_eq!(t.refresh_token, "r");
        assert_eq!(t.rtdb_token, "rt");
        assert_eq!(
            t.rtdb_url,
            "https://cyberspace-cyberspace-default-rtdb.europe-west1.firebasedatabase.app"
        );
        assert!(t.is_authenticated());
    }

    #[test]
    fn refresh_response_with_missing_refresh_token_decodes() {
        // /v1/auth/refresh returns { idToken, rtdbToken, rtdbUrl }.
        let raw = r#"{"idToken":"id2","rtdbToken":"rt2","rtdbUrl":"https://db.example"}"#;
        let t: Tokens = serde_json::from_str(raw).unwrap();
        assert_eq!(t.id_token, "id2");
        assert_eq!(t.refresh_token, "");
        assert_eq!(t.rtdb_token, "rt2");
        assert_eq!(t.rtdb_url, "https://db.example");
    }

    #[test]
    fn empty_tokens_omits_empty_fields() {
        let t = Tokens::default();
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(s, "{}");
        assert!(!t.is_authenticated());
    }
}

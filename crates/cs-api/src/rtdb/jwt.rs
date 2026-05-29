//! Minimal JWT payload decoder for Firebase RTDB tokens.
//!
//! We do NOT verify the signature — that's Firebase's job server-side. We only
//! need the `aud` claim, which names the Firebase project. The RTDB base URL is
//! then `https://{aud}-default-rtdb.firebaseio.com`.
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

use super::client::RtdbError;

#[derive(Debug, Deserialize)]
struct JwtPayload {
    /// Firebase audience — the project ID.
    #[serde(default)]
    aud: String,
}

/// Decode the payload of a Firebase JWT and return the `aud` claim.
pub fn project_id_from_jwt(jwt: &str) -> Result<String, RtdbError> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err(RtdbError::InvalidJwt(format!(
            "expected 3 dot-separated parts, got {}",
            parts.len()
        )));
    }
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|e| RtdbError::InvalidJwt(format!("base64 decode: {e}")))?;
    let payload: JwtPayload = serde_json::from_slice(&payload_bytes)
        .map_err(|e| RtdbError::InvalidJwt(format!("payload json: {e}")))?;
    if payload.aud.is_empty() {
        return Err(RtdbError::InvalidJwt(
            "missing or empty `aud` claim".to_string(),
        ));
    }
    Ok(payload.aud)
}

/// Construct the Firebase RTDB base URL for the given project ID.
#[must_use]
pub fn base_url_for(project_id: &str) -> String {
    format!("https://{project_id}-default-rtdb.firebaseio.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an unsigned JWT with a JSON payload — `header.payload.signature`.
    /// Header and signature are stub strings (we don't verify).
    fn make_jwt(payload: &str) -> String {
        let encoded = URL_SAFE_NO_PAD.encode(payload.as_bytes());
        format!("HEADER.{encoded}.SIGNATURE")
    }

    #[test]
    fn extracts_aud_from_well_formed_jwt() {
        let jwt = make_jwt(r#"{"aud":"my-project","exp":1234567890}"#);
        assert_eq!(project_id_from_jwt(&jwt).unwrap(), "my-project");
    }

    #[test]
    fn rejects_jwt_without_aud() {
        let jwt = make_jwt(r#"{"exp":1234567890}"#);
        let err = project_id_from_jwt(&jwt).unwrap_err();
        assert!(matches!(err, RtdbError::InvalidJwt(_)));
    }

    #[test]
    fn rejects_jwt_with_empty_aud() {
        let jwt = make_jwt(r#"{"aud":""}"#);
        let err = project_id_from_jwt(&jwt).unwrap_err();
        assert!(matches!(err, RtdbError::InvalidJwt(_)));
    }

    #[test]
    fn rejects_jwt_with_wrong_part_count() {
        let err = project_id_from_jwt("only.two").unwrap_err();
        assert!(matches!(err, RtdbError::InvalidJwt(_)));
    }

    #[test]
    fn rejects_jwt_with_invalid_base64() {
        let err = project_id_from_jwt("HEADER.!!!not-base64!!!.SIG").unwrap_err();
        assert!(matches!(err, RtdbError::InvalidJwt(_)));
    }

    #[test]
    fn base_url_uses_default_database_suffix() {
        assert_eq!(
            base_url_for("cyberspace-prod"),
            "https://cyberspace-prod-default-rtdb.firebaseio.com"
        );
    }
}

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

#[derive(Debug, Deserialize)]
struct UidPayload {
    /// Firebase-specific uid claim.
    #[serde(default)]
    user_id: String,
    /// Standard subject claim (also the uid for Firebase tokens).
    #[serde(default)]
    sub: String,
}

/// Decode the payload of a Firebase JWT and return the caller's uid (`user_id`,
/// falling back to `sub`). Used to address the caller's own RTDB nodes
/// (`user_conversations/<uid>`). The signature is not verified — that's the
/// server's job; we only read a claim.
pub fn uid_from_jwt(jwt: &str) -> Result<String, RtdbError> {
    let payload: UidPayload = decode_jwt_payload(jwt)?;
    let uid = if payload.user_id.is_empty() {
        payload.sub
    } else {
        payload.user_id
    };
    if uid.is_empty() {
        return Err(RtdbError::InvalidJwt(
            "missing `user_id`/`sub` claim".to_string(),
        ));
    }
    Ok(uid)
}

/// Base64url-decode a JWT's payload segment into `T`. Errors if the token isn't
/// three dot-separated parts or the payload isn't the expected JSON.
fn decode_jwt_payload<T: serde::de::DeserializeOwned>(jwt: &str) -> Result<T, RtdbError> {
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
    serde_json::from_slice(&payload_bytes)
        .map_err(|e| RtdbError::InvalidJwt(format!("payload json: {e}")))
}

/// Decode the payload of a Firebase JWT and return the `aud` claim.
pub fn project_id_from_jwt(jwt: &str) -> Result<String, RtdbError> {
    let payload: JwtPayload = decode_jwt_payload(jwt)?;
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
    fn uid_prefers_user_id_then_sub() {
        let jwt = make_jwt(r#"{"user_id":"uid-123","sub":"other"}"#);
        assert_eq!(uid_from_jwt(&jwt).unwrap(), "uid-123");

        let jwt = make_jwt(r#"{"sub":"uid-sub"}"#);
        assert_eq!(uid_from_jwt(&jwt).unwrap(), "uid-sub");
    }

    #[test]
    fn uid_rejects_when_no_uid_claim() {
        let jwt = make_jwt(r#"{"aud":"proj"}"#);
        assert!(matches!(
            uid_from_jwt(&jwt).unwrap_err(),
            RtdbError::InvalidJwt(_)
        ));
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

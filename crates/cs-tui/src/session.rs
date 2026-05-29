//! Persistent login session — saved to `<XDG_CONFIG_HOME>/cs-tui/session.json`.
//!
//! On Unix the file is `chmod 0600` after write. The bundle round-trips the
//! `cs_api::Tokens` fields plus the email used at last login (so the UI can
//! pre-fill it on re-auth).
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cs_api::Tokens;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no home directory — cannot locate XDG config path")]
    NoHome,
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    #[serde(flatten)]
    pub tokens: Tokens,

    /// Email used at last successful login. Optional — pre-fills the login form.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub email: String,
}

impl Session {
    /// Returns the canonical session path, creating the parent directory if needed.
    pub fn default_path() -> Result<PathBuf, SessionError> {
        let dirs =
            ProjectDirs::from("online", "cyberspace", "cs-tui").ok_or(SessionError::NoHome)?;
        let dir = dirs.config_dir();
        fs::create_dir_all(dir)?;
        Ok(dir.join("session.json"))
    }

    /// Reads a session from the default path. Returns `Ok(None)` if no session
    /// has been saved yet.
    pub fn load() -> Result<Option<Self>, SessionError> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Writes the session to the default path with mode 0600 (on Unix).
    pub fn save(&self) -> Result<(), SessionError> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    /// Deletes the saved session if present. No-op if already absent.
    // TODO(phase-4): first caller lands with the logout action.
    #[allow(dead_code)]
    pub fn clear() -> Result<(), SessionError> {
        let path = Self::default_path()?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SessionError::Io(e)),
        }
    }

    /// Lower-level: load from an explicit path.
    pub fn load_from(path: &Path) -> Result<Option<Self>, SessionError> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(SessionError::Io(e)),
        }
    }

    /// Lower-level: save to an explicit path. On Unix the file is chmod 0600
    /// after write; on other platforms the OS default applies.
    pub fn save_to(&self, path: &Path) -> Result<(), SessionError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(path, perms)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cs-tui-test-{label}-{nanos}"))
    }

    #[test]
    fn load_from_nonexistent_returns_none() {
        let path = tmp_path("missing");
        let s = Session::load_from(&path).unwrap();
        assert!(s.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let path = tmp_path("roundtrip").join("session.json");
        let original = Session {
            tokens: Tokens {
                id_token: "id".into(),
                refresh_token: "r".into(),
                rtdb_token: "rt".into(),
            },
            email: "you@example.com".into(),
        };
        original.save_to(&path).unwrap();

        let loaded = Session::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.tokens.id_token, "id");
        assert_eq!(loaded.tokens.refresh_token, "r");
        assert_eq!(loaded.tokens.rtdb_token, "rt");
        assert_eq!(loaded.email, "you@example.com");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_serializes_tokens_in_camel_case() {
        let path = tmp_path("camel").join("session.json");
        let s = Session {
            tokens: Tokens {
                id_token: "id".into(),
                refresh_token: "r".into(),
                rtdb_token: "rt".into(),
            },
            email: String::new(),
        };
        s.save_to(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"idToken\""));
        assert!(text.contains("\"refreshToken\""));
        assert!(text.contains("\"rtdbToken\""));
        // Empty email should be omitted.
        assert!(!text.contains("\"email\""));
        let _ = fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_mode_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_path("perms").join("session.json");
        Session::default().save_to(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0o600, got {mode:#o}");
        let _ = fs::remove_file(&path);
    }
}

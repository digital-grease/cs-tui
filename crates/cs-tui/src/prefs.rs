//! Local UI preferences — saved to `<XDG_CONFIG_HOME>/cs-tui/prefs.json`.
//!
//! Unlike the session, prefs survive logout and are read *before* login, so the
//! chosen theme styles the login screen too. Nothing here is sensitive, so the
//! file keeps default permissions (the session file is the one chmod'd 0600).
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::session::SessionError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Prefs {
    /// Selected theme name (e.g. "cyber", "c64"). Absent → fall back to default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

impl Prefs {
    /// Canonical prefs path, creating the parent directory if needed.
    pub fn default_path() -> Result<PathBuf, SessionError> {
        let dirs =
            ProjectDirs::from("online", "cyberspace", "cs-tui").ok_or(SessionError::NoHome)?;
        let dir = dirs.config_dir();
        fs::create_dir_all(dir)?;
        Ok(dir.join("prefs.json"))
    }

    /// Load prefs, falling back to defaults on any error (a missing or
    /// unreadable prefs file must never block startup).
    pub fn load() -> Self {
        Self::default_path()
            .and_then(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Lower-level: load from an explicit path. Missing file → defaults.
    pub fn load_from(path: &Path) -> Result<Self, SessionError> {
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(SessionError::Io(e)),
        }
    }

    /// Save prefs to the default path.
    pub fn save(&self) -> Result<(), SessionError> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    /// Lower-level: save to an explicit path.
    pub fn save_to(&self, path: &Path) -> Result<(), SessionError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        fs::write(path, bytes)?;
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
        std::env::temp_dir().join(format!("cs-tui-prefs-{label}-{nanos}"))
    }

    #[test]
    fn load_from_missing_returns_default() {
        let p = tmp_path("missing");
        let prefs = Prefs::load_from(&p).unwrap();
        assert!(prefs.theme.is_none());
    }

    #[test]
    fn save_then_load_roundtrips_theme() {
        let p = tmp_path("roundtrip").join("prefs.json");
        let prefs = Prefs {
            theme: Some("c64".into()),
        };
        prefs.save_to(&p).unwrap();
        let loaded = Prefs::load_from(&p).unwrap();
        assert_eq!(loaded.theme.as_deref(), Some("c64"));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn empty_prefs_omit_theme_key() {
        let p = tmp_path("empty").join("prefs.json");
        Prefs::default().save_to(&p).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        assert!(!text.contains("theme"));
        let _ = fs::remove_file(&p);
    }
}

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

    /// Unix seconds of the last update check, successful or not.
    ///
    /// Stamped even when the check fails, so an offline or rate-limited run
    /// waits out the interval like any other instead of retrying on every
    /// launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update_check: Option<i64>,

    /// The newest release the user has already been told about.
    ///
    /// This is what keeps the announcement to once per version: the toast fires
    /// when a release differs from this, and never again for that release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_version: Option<String>,
}

impl Prefs {
    /// Load, apply `edit`, then save back.
    ///
    /// More than one place writes prefs and each knows only its own field, so a
    /// writer that built a fresh `Prefs` from scratch would silently drop
    /// everyone else's (cycling the theme would erase the update-check stamp).
    /// Read-modify-write keeps the writers independent.
    ///
    /// A failed save is logged and otherwise ignored: prefs are a convenience
    /// and must never be load-bearing.
    pub fn edit(edit: impl FnOnce(&mut Self)) {
        let Ok(path) = Self::default_path() else {
            return;
        };
        // Deliberately NOT `load()`, which flattens a damaged file into
        // defaults. Writing those defaults back would silently destroy whatever
        // the file still held (the saved theme, most visibly). A missing file
        // still reads as defaults, which is the case we do want to write.
        let mut prefs = match Self::load_from(&path) {
            Ok(prefs) => prefs,
            Err(e) => {
                tracing::warn!(error = %e, "prefs unreadable; leaving the file untouched");
                return;
            }
        };
        edit(&mut prefs);
        if let Err(e) = prefs.save_to(&path) {
            tracing::warn!(error = %e, "prefs save failed");
        }
    }

    /// Whether an update check is due, given the current unix time.
    ///
    /// Due when none has ever run, when the stamp is in the future (a clock
    /// moved backwards, which would otherwise wedge the check until it caught
    /// up), or when the interval has elapsed.
    #[must_use]
    pub fn update_check_due(&self, now_secs: i64) -> bool {
        match self.last_update_check {
            None => true,
            Some(last) => now_secs < last || now_secs - last >= crate::update::CHECK_INTERVAL_SECS,
        }
    }
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

    /// Save to an explicit path.
    ///
    /// [`Prefs::edit`] is the way to change saved prefs: a bare save from a
    /// value built in memory would drop whatever fields the caller did not know
    /// about, which is how the theme used to be lost.
    ///
    /// Writes a sibling temp file and renames it into place, so a crash or a
    /// second writer can never leave a half-written prefs.json behind. That
    /// matters more than it used to: the theme cycle is no longer the only
    /// writer, and a damaged file used to be harmless but is now something
    /// [`Prefs::edit`] has to refuse to overwrite.
    pub fn save_to(&self, path: &Path) -> Result<(), SessionError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, bytes)?;
        // Same directory, so this is atomic on every platform we target.
        fs::rename(&tmp, path)?;
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
            ..Prefs::default()
        };
        prefs.save_to(&p).unwrap();
        let loaded = Prefs::load_from(&p).unwrap();
        assert_eq!(loaded.theme.as_deref(), Some("c64"));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn the_first_run_is_always_due_for_an_update_check() {
        assert!(Prefs::default().update_check_due(1_000_000));
    }

    #[test]
    fn a_check_is_due_only_once_the_interval_has_elapsed() {
        let now = 1_000_000;
        let prefs = Prefs {
            last_update_check: Some(now),
            ..Prefs::default()
        };
        assert!(!prefs.update_check_due(now), "just checked");
        assert!(
            !prefs.update_check_due(now + crate::update::CHECK_INTERVAL_SECS - 1),
            "a second short of the interval",
        );
        assert!(prefs.update_check_due(now + crate::update::CHECK_INTERVAL_SECS));
    }

    #[test]
    fn a_clock_that_moved_backwards_does_not_wedge_the_check() {
        // Without this the stamp sits in the future and the check would be
        // suppressed until real time caught up with it.
        let prefs = Prefs {
            last_update_check: Some(2_000_000),
            ..Prefs::default()
        };
        assert!(prefs.update_check_due(1_000_000));
    }

    #[test]
    fn writing_one_field_preserves_the_others() {
        // The theme cycle and the update check write prefs independently. A
        // writer that built a fresh Prefs would erase the other's field.
        let p = tmp_path("preserve").join("prefs.json");
        Prefs {
            theme: Some("c64".into()),
            last_update_check: Some(42),
            last_seen_version: Some("9.9.9".into()),
        }
        .save_to(&p)
        .unwrap();

        let mut loaded = Prefs::load_from(&p).unwrap();
        loaded.theme = Some("cyber".into());
        loaded.save_to(&p).unwrap();

        let again = Prefs::load_from(&p).unwrap();
        assert_eq!(again.theme.as_deref(), Some("cyber"));
        assert_eq!(again.last_update_check, Some(42));
        assert_eq!(again.last_seen_version.as_deref(), Some("9.9.9"));
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

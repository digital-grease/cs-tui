//! User configuration — `<XDG_CONFIG_HOME>/cs-tui/config.toml`.
//!
//! Hand-edited and never rewritten by the app (so comments survive). Sets the
//! default theme and an optional custom hex palette. Runtime theme cycling (Esc
//! menu) is a separate, app-written concern — see [`crate::prefs`].
use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use ratatui::style::Color;
use serde::Deserialize;

use crate::ui::theme::Theme;

/// Parsed `config.toml`. Lenient: unknown keys are ignored and a parse error
/// falls back to defaults (never blocks startup).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default theme name: `cyber` | `c64` | `vt320` | `dark` | `custom`.
    pub theme: Option<String>,
    /// Custom palette (used when the theme is `custom`).
    pub colors: Option<Colors>,
}

/// Hex/named overrides for the eight theme colors. Any omitted color keeps the
/// built-in default.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Colors {
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub muted: Option<String>,
    pub accent: Option<String>,
    pub success: Option<String>,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub border: Option<String>,
}

/// Commented starter file written on first launch (all keys commented out, so
/// it changes nothing until the user edits it).
const TEMPLATE: &str = r##"# cs-tui configuration. Edit and restart cs-tui.
# This file is never overwritten by the app, so your comments are safe.

# Default theme: cyber | c64 | vt320 | dark | custom
# (You can also cycle themes at runtime via the Esc menu; that choice is
#  remembered separately and overrides this on the next launch.)
#theme = "custom"

# Custom palette. Set `theme = "custom"` above (or cycle to it in-app) to use it.
# Each value is one of:
#   - a hex color:   "#1e1e2e"  /  "#abc"  /  "1e1e2e"
#   - "reset"        the terminal's own default color
#   - an ANSI index  "0" .. "255"
# Any color you omit falls back to the built-in default.
#[colors]
#background = "reset"
#foreground = "#cdd6f4"
#muted      = "#6c7086"
#accent     = "#89b4fa"
#success    = "#a6e3a1"
#error      = "#f38ba8"
#warning    = "#f9e2af"
#border     = "#585b70"
"##;

impl Config {
    /// `<XDG_CONFIG_HOME>/cs-tui/config.toml` (no directory side effects).
    pub fn default_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("online", "cyberspace", "cs-tui")?;
        Some(dirs.config_dir().join("config.toml"))
    }

    /// Load config, falling back to defaults on a missing/unreadable file or a
    /// parse error (the latter is logged).
    pub fn load() -> Self {
        Self::default_path().map_or_else(Self::default, |p| Self::load_from(&p))
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default(); // missing/unreadable → defaults
        };
        match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "config.toml parse failed; using defaults"
                );
                Self::default()
            }
        }
    }

    /// Write the commented template if no config file exists yet, so the option
    /// is discoverable. Best-effort; failures are ignored.
    pub fn write_template_if_absent() {
        let Some(path) = Self::default_path() else {
            return;
        };
        if path.exists() {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(&path, TEMPLATE).is_ok() {
            tracing::info!(path = %path.display(), "wrote starter config.toml");
        }
    }

    /// Build the custom [`Theme`] from `[colors]` (overriding the default
    /// palette). `None` if there's no `[colors]` section.
    pub fn custom_theme(&self) -> Option<Theme> {
        self.colors.as_ref().map(|c| c.to_theme(Theme::cyber()))
    }
}

impl Colors {
    fn to_theme(&self, base: Theme) -> Theme {
        let mut t = base;
        apply(&mut t.background, self.background.as_deref());
        apply(&mut t.foreground, self.foreground.as_deref());
        apply(&mut t.muted, self.muted.as_deref());
        apply(&mut t.accent, self.accent.as_deref());
        apply(&mut t.success, self.success.as_deref());
        apply(&mut t.error, self.error.as_deref());
        apply(&mut t.warning, self.warning.as_deref());
        apply(&mut t.border, self.border.as_deref());
        t
    }
}

fn apply(field: &mut Color, spec: Option<&str>) {
    if let Some(s) = spec {
        match parse_color(s) {
            Some(c) => *field = c,
            None => {
                tracing::warn!(value = s, "unrecognized color in config.toml; keeping default");
            }
        }
    }
}

/// Parse a color spec: `#rrggbb` / `#rgb` / `rrggbb` hex, `reset`/`default`/`none`
/// (the terminal's own color), or an ANSI index `0`–`255`.
#[must_use]
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_ascii_lowercase();
    if matches!(s.as_str(), "reset" | "default" | "none") {
        return Some(Color::Reset);
    }
    if let Ok(n) = s.parse::<u8>() {
        return Some(Color::Indexed(n));
    }
    let hex = s.strip_prefix('#').unwrap_or(&s);
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb(r, g, b))
        }
        3 => {
            // #abc → #aabbcc
            let nib = |c: &str| u8::from_str_radix(c, 16).ok().map(|v| v * 17);
            Some(Color::Rgb(nib(&hex[0..1])?, nib(&hex[1..2])?, nib(&hex[2..3])?))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_color_handles_hex_reset_and_index() {
        assert_eq!(parse_color("#1e2030"), Some(Color::Rgb(0x1e, 0x20, 0x30)));
        assert_eq!(parse_color("1E2030"), Some(Color::Rgb(0x1e, 0x20, 0x30)));
        assert_eq!(parse_color("#abc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_color("reset"), Some(Color::Reset));
        assert_eq!(parse_color(" Default "), Some(Color::Reset));
        assert_eq!(parse_color("123"), Some(Color::Indexed(123)));
        assert_eq!(parse_color("#nothex"), None);
        assert_eq!(parse_color("blurple"), None);
    }

    #[test]
    fn template_is_valid_toml_and_inert() {
        // The starter template parses and sets nothing (all keys commented).
        let cfg: Config = toml::from_str(TEMPLATE).expect("template parses");
        assert!(cfg.theme.is_none());
        assert!(cfg.colors.is_none());
    }

    #[test]
    fn custom_theme_overrides_only_specified_colors() {
        let cfg: Config = toml::from_str(
            r##"
            theme = "custom"
            [colors]
            accent = "#ff8800"
            background = "reset"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("custom"));
        let t = cfg.custom_theme().expect("has colors");
        assert_eq!(t.accent, Color::Rgb(0xff, 0x88, 0x00));
        assert_eq!(t.background, Color::Reset);
        // Unspecified colors keep the cyber base.
        assert_eq!(t.error, Theme::cyber().error);
    }

    #[test]
    fn no_colors_section_means_no_custom_theme() {
        let cfg: Config = toml::from_str(r#"theme = "dark""#).unwrap();
        assert!(cfg.custom_theme().is_none());
    }

    #[test]
    fn unparseable_color_keeps_base_and_does_not_fail() {
        let cfg: Config = toml::from_str(
            r#"
            [colors]
            accent = "not-a-color"
            "#,
        )
        .unwrap();
        let t = cfg.custom_theme().unwrap();
        assert_eq!(t.accent, Theme::cyber().accent); // kept the default
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // Forward-compat: a future/typo'd key doesn't break loading.
        let cfg: Config = toml::from_str(
            r#"
            theme = "c64"
            wat = 42
            "#,
        )
        .unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("c64"));
    }
}

//! User configuration — `<XDG_CONFIG_HOME>/cs-tui/config.toml` (or `--config`).
//!
//! Hand-edited and never rewritten by the app (so comments survive). Two kinds
//! of settings:
//!   * **Startup**: theme/colors, mouse, images, api_base — resolved in `main`
//!     (CLI flags override these).
//!   * **Runtime**: display + behavior prefs ([`Runtime`]) — installed once into
//!     a process global ([`init`]/[`get`]) so the many render sites can read
//!     them without threading a parameter everywhere. Defaults match the
//!     historical hardcoded behavior exactly.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use directories::ProjectDirs;
use ratatui::style::Color;
use serde::Deserialize;
use time::{OffsetDateTime, UtcOffset};

use crate::ui::nav::RootKind;
use crate::ui::theme::Theme;

/// Parsed `config.toml`. Lenient: unknown keys are ignored and a parse error
/// falls back to defaults (never blocks startup).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    // --- startup ---
    /// Default theme: `cyber` | `c64` | `vt320` | `dark` | `custom`.
    pub theme: Option<String>,
    /// Custom palette (used when the theme is `custom`).
    pub colors: Option<Colors>,
    /// Capture the scroll wheel for in-app scrolling (loses native select/copy).
    pub mouse: Option<bool>,
    /// Render inline images on graphics-capable terminals.
    pub images: Option<bool>,
    /// Override the API base URL.
    pub api_base: Option<String>,

    // --- runtime (see Runtime) ---
    pub time_format: Option<String>,
    pub timezone: Option<String>,
    pub compact: Option<bool>,
    pub preview_length: Option<usize>,
    pub image_height: Option<u16>,
    pub start_section: Option<String>,
    pub nsfw: Option<bool>,
    pub editor: Option<String>,
    pub confirm_deletes: Option<bool>,
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

/// How list timestamps render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormat {
    /// "2h ago".
    Relative,
    /// "2026-05-31 14:30" in the configured timezone.
    Absolute,
}

/// Resolved display + behavior prefs, installed once into a process global.
/// Its `Default` reproduces the historical hardcoded behavior exactly.
#[derive(Debug, Clone)]
pub struct Runtime {
    pub time_format: TimeFormat,
    pub tz_offset: UtcOffset,
    pub compact: bool,
    pub preview_length: usize,
    pub image_height: u16,
    pub start_section: RootKind,
    pub nsfw: bool,
    pub editor: Option<String>,
    pub confirm_deletes: bool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            time_format: TimeFormat::Relative,
            tz_offset: UtcOffset::UTC,
            compact: false,
            preview_length: 200,
            image_height: 20,
            start_section: RootKind::Feed,
            nsfw: false,
            editor: None,
            confirm_deletes: true,
        }
    }
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Install the resolved runtime config. Call once at startup, before any render.
pub fn init(rt: Runtime) {
    let _ = RUNTIME.set(rt);
}

/// The active runtime config (defaults if `init` was never called, e.g. tests).
#[must_use]
pub fn get() -> &'static Runtime {
    RUNTIME.get_or_init(Runtime::default)
}

static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Record the resolved config-file path so the UI can show users where it lives.
/// Call once at startup, alongside [`init`].
pub fn set_config_path(path: PathBuf) {
    let _ = CONFIG_PATH.set(path);
}

/// The active config-file path, if [`set_config_path`] was called (None in tests).
#[must_use]
pub fn config_path() -> Option<&'static Path> {
    CONFIG_PATH.get().map(PathBuf::as_path)
}

const TEMPLATE: &str = r##"# cs-tui configuration. Edit and restart cs-tui.
# This file is never overwritten by the app, so your comments are safe.
# Every option below is shown commented out at its default; uncomment to change.
# Location: this file (override with --config <path> or $CS_TUI_CONFIG).

# ── Appearance ───────────────────────────────────────────────────────────────

# Default theme: cyber | c64 | vt320 | dark | vapor | custom
# (You can also cycle themes at runtime via the Esc menu; that choice is
#  remembered separately and overrides this on the next launch.)
#theme = "cyber"

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

# Compact mode: drop the blank-line / rule separators between list items for a
# denser feed. true | false
#compact = false

# ── Time ─────────────────────────────────────────────────────────────────────

# List timestamps: "relative" ("2h ago") or "absolute" ("2026-05-31 14:30").
#time_format = "relative"

# Timezone for absolute timestamps: "utc" or a fixed UTC offset like
# "-05:00" / "+02:00" / "+0530". (Auto-detecting the local zone isn't reliable
# in a threaded app, so set your offset explicitly.)
#timezone = "utc"

# ── Behavior ─────────────────────────────────────────────────────────────────

# Section to open on launch: feed | notifications | bookmarks | topics |
# profile | journal | settings | guilds
#start_section = "feed"

# Show NSFW posts by default (otherwise they're hidden until toggled).
#nsfw = false

# Require the two-step d → y confirmation before deleting a post/note.
#confirm_deletes = true

# Editor for composing posts/notes. Defaults to $VISUAL, then $EDITOR, then nano.
#editor = "nvim"

# Characters of post content shown in list previews.
#preview_length = 200

# Max rows for the inline image strip in post detail.
#image_height = 20

# ── Input / connection ───────────────────────────────────────────────────────

# Capture the scroll wheel for in-app scrolling. Off keeps native terminal mouse
# behavior (drag to select/copy, click to open links). --mouse forces this on.
#mouse = false

# Render inline images on graphics-capable terminals. --no-images forces off.
#images = true

# API base URL (overrides the built-in default).
#api_base = "https://api.cyberspace.online"
"##;

impl Config {
    /// `<XDG_CONFIG_HOME>/cs-tui/config.toml` (no directory side effects).
    pub fn default_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("online", "cyberspace", "cs-tui")?;
        Some(dirs.config_dir().join("config.toml"))
    }

    /// Load config from `path`, falling back to defaults on a missing/unreadable
    /// file or a parse error (the latter is logged).
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

    /// Write the fully-commented template to `path` if it doesn't exist yet, so
    /// the options are discoverable. Best-effort; failures are ignored.
    pub fn write_template_if_absent(path: &Path) {
        if path.exists() {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::write(path, TEMPLATE).is_ok() {
            tracing::info!(path = %path.display(), "wrote starter config.toml");
        }
    }

    /// Build the custom [`Theme`] from `[colors]`. `None` if there's no section.
    pub fn custom_theme(&self) -> Option<Theme> {
        self.colors.as_ref().map(|c| c.to_theme(Theme::cyber()))
    }

    /// Resolve the runtime display/behavior prefs (strings → enums/offsets),
    /// keeping the default for anything unset or unparseable.
    pub fn to_runtime(&self) -> Runtime {
        let d = Runtime::default();
        Runtime {
            time_format: match self.time_format.as_deref().map(str::to_ascii_lowercase) {
                Some(s) if s == "absolute" => TimeFormat::Absolute,
                Some(s) if s == "relative" => TimeFormat::Relative,
                Some(other) => {
                    tracing::warn!(value = other, "unknown time_format; using relative");
                    d.time_format
                }
                None => d.time_format,
            },
            tz_offset: match self.timezone.as_deref() {
                Some(s) => parse_tz(s).unwrap_or_else(|| {
                    tracing::warn!(value = s, "unparseable timezone; using utc");
                    d.tz_offset
                }),
                None => d.tz_offset,
            },
            compact: self.compact.unwrap_or(d.compact),
            preview_length: self
                .preview_length
                .unwrap_or(d.preview_length)
                .clamp(20, 2000),
            image_height: self.image_height.unwrap_or(d.image_height).clamp(1, 60),
            start_section: match self.start_section.as_deref() {
                Some(s) => parse_section(s).unwrap_or_else(|| {
                    tracing::warn!(value = s, "unknown start_section; using feed");
                    d.start_section
                }),
                None => d.start_section,
            },
            nsfw: self.nsfw.unwrap_or(d.nsfw),
            editor: self.editor.clone().filter(|s| !s.trim().is_empty()),
            confirm_deletes: self.confirm_deletes.unwrap_or(d.confirm_deletes),
        }
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
                tracing::warn!(
                    value = s,
                    "unrecognized color in config.toml; keeping default"
                );
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
    // A bare 1–3 digit number is an ANSI index (0–255). Reject out-of-range
    // values here rather than letting e.g. "256" fall through and be silently
    // reinterpreted as #rgb hex. An explicit `#` still forces hex below.
    if !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse::<u8>().ok().map(Color::Indexed);
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
            let nib = |c: &str| u8::from_str_radix(c, 16).ok().map(|v| v * 17);
            Some(Color::Rgb(
                nib(&hex[0..1])?,
                nib(&hex[1..2])?,
                nib(&hex[2..3])?,
            ))
        }
        _ => None,
    }
}

/// Parse `"utc"` or a fixed offset like `-05:00` / `+02:00` / `+0530` / `+5`.
fn parse_tz(s: &str) -> Option<UtcOffset> {
    let s = s.trim().to_ascii_lowercase();
    if s == "utc" || s == "z" {
        return Some(UtcOffset::UTC);
    }
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1i32, r),
        None => (1i32, s.strip_prefix('+').unwrap_or(&s)),
    };
    // Accept only digits with at most one optional `:` separator — anything else
    // (stray letters, punctuation) is unparseable rather than silently stripped.
    let digits = rest.replace(':', "");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m) = match digits.len() {
        1 | 2 => (digits.parse::<i32>().ok()?, 0),
        3 => (digits[0..1].parse().ok()?, digits[1..3].parse().ok()?),
        4 => (digits[0..2].parse().ok()?, digits[2..4].parse().ok()?),
        _ => return None,
    };
    if m > 59 {
        return None;
    }
    // Reject offsets outside the real-world range (−12:00 … +14:00) so a typo
    // like "+25" falls back to UTC with a warning instead of becoming ±25:59.
    let total = sign * (h * 60 + m);
    if !(-720..=840).contains(&total) {
        return None;
    }
    UtcOffset::from_hms((total / 60) as i8, (total % 60) as i8, 0).ok()
}

fn parse_section(s: &str) -> Option<RootKind> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "feed" => RootKind::Feed,
        "notifications" | "notifs" | "notes" => RootKind::Notifications,
        "bookmarks" => RootKind::Bookmarks,
        "topics" => RootKind::Topics,
        "profile" => RootKind::Profile,
        "journal" => RootKind::Journal,
        "settings" => RootKind::Settings,
        "guilds" => RootKind::Guilds,
        _ => return None,
    })
}

/// Format a list timestamp per the active `time_format`.
#[must_use]
pub fn format_list_timestamp(t: OffsetDateTime) -> String {
    match get().time_format {
        TimeFormat::Relative => format_relative(t),
        TimeFormat::Absolute => format_absolute(t),
    }
}

/// Absolute "YYYY-MM-DD HH:MM" in the configured timezone.
#[must_use]
pub fn format_absolute(t: OffsetDateTime) -> String {
    let t = t.to_offset(get().tz_offset);
    let (d, tt) = (t.date(), t.time());
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        d.year(),
        u8::from(d.month()),
        d.day(),
        tt.hour(),
        tt.minute()
    )
}

fn format_relative(t: OffsetDateTime) -> String {
    let secs = (OffsetDateTime::now_utc() - t).whole_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else if secs < 30 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else {
        let d = t.to_offset(get().tz_offset).date();
        format!("{}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
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
        // Out-of-range ANSI index is rejected, not silently read as #rgb hex.
        assert_eq!(parse_color("256"), None);
        assert_eq!(parse_color("999"), None);
        // An explicit `#` still forces hex even for all-digit input.
        assert_eq!(parse_color("#123"), Some(Color::Rgb(0x11, 0x22, 0x33)));
    }

    #[test]
    fn parse_tz_handles_utc_and_offsets() {
        assert_eq!(parse_tz("utc"), Some(UtcOffset::UTC));
        assert_eq!(
            parse_tz("-05:00"),
            Some(UtcOffset::from_hms(-5, 0, 0).unwrap())
        );
        assert_eq!(
            parse_tz("+0530"),
            Some(UtcOffset::from_hms(5, 30, 0).unwrap())
        );
        assert_eq!(parse_tz("+2"), Some(UtcOffset::from_hms(2, 0, 0).unwrap()));
        assert_eq!(
            parse_tz("+14:00"),
            Some(UtcOffset::from_hms(14, 0, 0).unwrap())
        );
        assert_eq!(parse_tz("nonsense"), None);
        // Out-of-range offsets fall back (None → caller uses UTC), not ±25:59.
        assert_eq!(parse_tz("+25"), None);
        assert_eq!(parse_tz("+1559"), None);
        assert_eq!(parse_tz("-13:00"), None);
        // Garbage-with-digits no longer slips through the old digit filter.
        assert_eq!(parse_tz("ab05cd"), None);
    }

    #[test]
    fn template_is_valid_toml_and_inert() {
        let cfg: Config = toml::from_str(TEMPLATE).expect("template parses");
        assert!(cfg.theme.is_none());
        assert!(cfg.colors.is_none());
        // All commented → resolving to a runtime gives the defaults.
        let rt = cfg.to_runtime();
        let d = Runtime::default();
        assert_eq!(rt.time_format, d.time_format);
        assert_eq!(rt.preview_length, d.preview_length);
        assert_eq!(rt.start_section, d.start_section);
    }

    #[test]
    fn to_runtime_resolves_and_clamps() {
        let cfg: Config = toml::from_str(
            r#"
            time_format = "absolute"
            timezone = "-05:00"
            compact = true
            preview_length = 5
            image_height = 999
            start_section = "topics"
            nsfw = true
            confirm_deletes = false
            editor = "nvim"
            "#,
        )
        .unwrap();
        let rt = cfg.to_runtime();
        assert_eq!(rt.time_format, TimeFormat::Absolute);
        assert_eq!(rt.tz_offset, UtcOffset::from_hms(-5, 0, 0).unwrap());
        assert!(rt.compact);
        assert_eq!(rt.preview_length, 20); // clamped up from 5
        assert_eq!(rt.image_height, 60); // clamped down from 999
        assert_eq!(rt.start_section, RootKind::Topics);
        assert!(rt.nsfw);
        assert!(!rt.confirm_deletes);
        assert_eq!(rt.editor.as_deref(), Some("nvim"));
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
        let t = cfg.custom_theme().expect("has colors");
        assert_eq!(t.accent, Color::Rgb(0xff, 0x88, 0x00));
        assert_eq!(t.background, Color::Reset);
        assert_eq!(t.error, Theme::cyber().error);
    }

    #[test]
    fn unknown_keys_and_bad_values_are_tolerated() {
        let cfg: Config = toml::from_str(
            r#"
            theme = "c64"
            wat = 42
            time_format = "sideways"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("c64"));
        // bad time_format falls back to the default
        assert_eq!(cfg.to_runtime().time_format, TimeFormat::Relative);
    }
}

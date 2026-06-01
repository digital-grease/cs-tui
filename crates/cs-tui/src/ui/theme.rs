use ratatui::style::{Color, Modifier, Style};

/// Visual theme. The default is `cyber` (bright green on black), matching the
/// retro aesthetic of cyberspace.online. Alternate palettes ship as `c64` and
/// `vt320`.
#[derive(Debug, Clone)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub muted: Color,
    pub accent: Color,
    /// Confirmation toasts (e.g. "bookmarked").
    pub success: Color,
    pub error: Color,
    /// Caution accent — drives the rate-limit toast (and any future warnings).
    pub warning: Color,
    pub border: Color,
}

impl Theme {
    /// Bright green-on-black — the default Cyberspace look.
    pub fn cyber() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::White,
            muted: Color::DarkGray,
            accent: Color::LightGreen,
            success: Color::Green,
            error: Color::LightRed,
            warning: Color::LightYellow,
            border: Color::Green,
        }
    }

    /// Commodore 64-inspired light blue on dark blue.
    pub fn c64() -> Self {
        Self {
            background: Color::Indexed(17),  // dark blue
            foreground: Color::Indexed(153), // very light blue
            muted: Color::Indexed(75),       // medium blue
            accent: Color::Indexed(159),     // pale cyan
            success: Color::LightCyan,
            error: Color::LightRed,
            warning: Color::Indexed(227), // light yellow
            border: Color::Indexed(75),
        }
    }

    /// VT320 amber on black.
    pub fn vt320() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::Indexed(214), // amber
            muted: Color::Indexed(94),       // dim amber
            accent: Color::Indexed(220),     // bright amber
            success: Color::Yellow,
            error: Color::LightRed,
            warning: Color::Indexed(214), // amber
            border: Color::Indexed(94),
        }
    }

    /// Legacy neutral dark theme (no longer the default; kept for tests).
    pub fn dark() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::Gray,
            muted: Color::DarkGray,
            accent: Color::LightGreen,
            success: Color::Green,
            error: Color::LightRed,
            warning: Color::LightYellow,
            border: Color::DarkGray,
        }
    }

    pub fn base(&self) -> Style {
        Style::default().fg(self.foreground).bg(self.background)
    }

    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn accent_style(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn error_style(&self) -> Style {
        Style::default().fg(self.error).add_modifier(Modifier::BOLD)
    }

    pub fn warning_style(&self) -> Style {
        Style::default()
            .fg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    pub fn success_style(&self) -> Style {
        Style::default()
            .fg(self.success)
            .add_modifier(Modifier::BOLD)
    }

    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::cyber()
    }
}

/// The selectable palettes. `Custom` is user-defined (from `config.toml`) and
/// only offered in the cycle when the config provides one — so it lives outside
/// the built-in `ALL`; the App resolves and cycles it (see `App::resolve_theme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Cyber,
    C64,
    Vt320,
    Dark,
    Custom,
}

impl ThemeKind {
    /// The built-in palettes, in cycle order. (`Custom` is appended by the App
    /// when configured.)
    pub const ALL: [ThemeKind; 4] = [Self::Cyber, Self::C64, Self::Vt320, Self::Dark];

    /// Stable lowercase name — matches the `--theme` flag and the persisted
    /// prefs value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Cyber => "cyber",
            Self::C64 => "c64",
            Self::Vt320 => "vt320",
            Self::Dark => "dark",
            Self::Custom => "custom",
        }
    }

    /// Parse a name (case-insensitive); unknown names fall back to `Cyber`.
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "c64" => Self::C64,
            "vt320" => Self::Vt320,
            "dark" => Self::Dark,
            "custom" => Self::Custom,
            _ => Self::Cyber,
        }
    }

    /// Resolve to the concrete palette. `Custom` has no built-in colors, so it
    /// falls back to `cyber` here; the App supplies the real custom palette via
    /// `resolve_theme`.
    pub fn theme(self) -> Theme {
        match self {
            Self::Cyber | Self::Custom => Theme::cyber(),
            Self::C64 => Theme::c64(),
            Self::Vt320 => Theme::vt320(),
            Self::Dark => Theme::dark(),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_cyber() {
        let t = Theme::default();
        assert_eq!(t.accent, Theme::cyber().accent);
    }

    #[test]
    fn custom_kind_round_trips() {
        assert_eq!(ThemeKind::from_name("custom"), ThemeKind::Custom);
        assert_eq!(ThemeKind::Custom.name(), "custom");
    }

    #[test]
    fn theme_kind_names_round_trip() {
        for k in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_name(k.name()), k);
        }
        assert_eq!(ThemeKind::from_name("CYBER"), ThemeKind::Cyber);
        assert_eq!(ThemeKind::from_name("unknown"), ThemeKind::Cyber);
    }

    #[test]
    fn theme_kind_resolves_to_matching_palette() {
        assert_eq!(ThemeKind::C64.theme().accent, Theme::c64().accent);
    }
}

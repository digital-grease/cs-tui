use ratatui::style::{Color, Modifier, Style};

/// Visual theme. The default is `cyber` (bright green on black), matching the
/// retro aesthetic of cyberspace.online. Alternate palettes ship as `c64`,
/// `vt320`, `dark`, and `vapor`.
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

    /// Vaporwave / cyberpunk neon — hot pink and electric cyan over deep indigo.
    /// Tuned for readability: body text stays a near-white lavender (high contrast
    /// on the dark background), so the neon is reserved for accents, borders, and
    /// status colors. `error` (neon red), `accent` (pink), `success` (mint), and
    /// `warning` (gold) are kept visibly distinct so a toast's meaning reads at a
    /// glance. Uses truecolor; modern terminals downsample gracefully.
    pub fn vapor() -> Self {
        Self {
            background: Color::Rgb(0x1a, 0x12, 0x2e), // deep indigo
            foreground: Color::Rgb(0xf2, 0xec, 0xff), // near-white lavender
            muted: Color::Rgb(0x9a, 0x8c, 0xc4),      // muted lavender-gray
            accent: Color::Rgb(0xff, 0x5f, 0xd1),     // hot pink
            success: Color::Rgb(0x5d, 0xff, 0xbf),    // mint
            error: Color::Rgb(0xff, 0x3b, 0x6b),      // neon red
            warning: Color::Rgb(0xff, 0xe1, 0x6b),    // pale gold
            border: Color::Rgb(0x00, 0xe0, 0xff),     // electric cyan
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
    Vapor,
    Custom,
}

impl ThemeKind {
    /// The built-in palettes, in cycle order. (`Custom` is appended by the App
    /// when configured.)
    pub const ALL: [ThemeKind; 5] =
        [Self::Cyber, Self::C64, Self::Vt320, Self::Dark, Self::Vapor];

    /// Stable lowercase name — matches the `--theme` flag and the persisted
    /// prefs value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Cyber => "cyber",
            Self::C64 => "c64",
            Self::Vt320 => "vt320",
            Self::Dark => "dark",
            Self::Vapor => "vapor",
            Self::Custom => "custom",
        }
    }

    /// Parse a name (case-insensitive); unknown names fall back to `Cyber`.
    pub fn from_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "c64" => Self::C64,
            "vt320" => Self::Vt320,
            "dark" => Self::Dark,
            "vapor" => Self::Vapor,
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
            Self::Vapor => Theme::vapor(),
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

    #[test]
    fn vapor_is_a_built_in_in_the_cycle() {
        assert!(ThemeKind::ALL.contains(&ThemeKind::Vapor));
        assert_eq!(ThemeKind::from_name("vapor"), ThemeKind::Vapor);
        assert_eq!(ThemeKind::Vapor.name(), "vapor");
        assert_eq!(ThemeKind::Vapor.theme().accent, Theme::vapor().accent);
    }

    #[test]
    fn vapor_keeps_status_colors_distinct() {
        // Usability: a glance at a toast must distinguish meaning, so the accent
        // and the three status colors must not collide.
        let t = Theme::vapor();
        let slots = [t.accent, t.success, t.error, t.warning];
        for (i, a) in slots.iter().enumerate() {
            for b in &slots[i + 1..] {
                assert_ne!(a, b, "vapor status colors must be distinct");
            }
        }
    }
}

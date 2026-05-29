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
    /// Used by future confirmation toasts (Phase 7.3).
    #[allow(dead_code)]
    pub success: Color,
    pub error: Color,
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
            border: Color::DarkGray,
        }
    }

    /// Look up a theme by name (case-insensitive). Unknown names fall back to
    /// `cyber`.
    pub fn by_name(name: &str) -> Self {
        match name.to_lowercase().as_str() {
            "c64" => Self::c64(),
            "vt320" => Self::vt320(),
            "dark" => Self::dark(),
            _ => Self::cyber(),
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

    pub fn border_style(&self) -> Style {
        Style::default().fg(self.border)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::cyber()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_returns_cyber_by_default() {
        let t = Theme::by_name("anything-unknown");
        assert_eq!(t.accent, Theme::cyber().accent);
    }

    #[test]
    fn by_name_resolves_known_themes() {
        assert_eq!(Theme::by_name("c64").accent, Theme::c64().accent);
        assert_eq!(Theme::by_name("vt320").accent, Theme::vt320().accent);
        assert_eq!(Theme::by_name("CYBER").accent, Theme::cyber().accent);
        assert_eq!(Theme::by_name("Dark").accent, Theme::dark().accent);
    }

    #[test]
    fn default_is_cyber() {
        let t = Theme::default();
        assert_eq!(t.accent, Theme::cyber().accent);
    }
}

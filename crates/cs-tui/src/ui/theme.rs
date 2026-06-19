use ratatui::style::{Color, Modifier, Style};

/// Visual theme. The default is `cyber` (bright green on black), matching the
/// retro aesthetic of cyberspace.online. Alternate palettes ship as `c64`,
/// `vt320`, `dark`, `vapor`, `paper` (the one light theme), and `gruvbox`.
#[derive(Debug, Clone)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub muted: Color,
    pub accent: Color,
    /// Panel/screen titles (the `" cs-tui • feed "` strings that sit on the
    /// border). Defaults to `accent` for most palettes, but `vapor` points it at
    /// its cyan so the title + border read as one frame and cyan co-leads with
    /// the pink content accent.
    pub heading: Color,
    /// Confirmation toasts (e.g. "bookmarked").
    pub success: Color,
    pub error: Color,
    /// Caution accent — drives the rate-limit toast (and any future warnings).
    pub warning: Color,
    pub border: Color,
    /// Background fill for the selected row in lists (the `selection = "fill"`
    /// style). A subtle tint that reads as "you are here" while preserving each
    /// span's own foreground color. Collapses to `Reset` in monochrome mode, where
    /// the `▌` bar + bold carry the selection instead.
    pub selection: Color,
}

impl Theme {
    /// Bright green-on-black — the default Cyberspace look.
    pub fn cyber() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::White,
            // A mid-gray (not DarkGray) so secondary text — timestamps, counts —
            // stays legible against a black background.
            muted: Color::Indexed(245),
            accent: Color::LightGreen,
            heading: Color::LightGreen,
            success: Color::Green,
            error: Color::LightRed,
            warning: Color::LightYellow,
            border: Color::Green,
            selection: Color::Indexed(238), // neutral dark gray
        }
    }

    /// Commodore 64-inspired light blue on dark blue.
    pub fn c64() -> Self {
        Self {
            background: Color::Indexed(17),  // dark blue
            foreground: Color::Indexed(153), // very light blue
            muted: Color::Indexed(75),       // medium blue
            accent: Color::Indexed(159),     // pale cyan
            heading: Color::Indexed(159),    // pale cyan
            success: Color::LightCyan,
            error: Color::LightRed,
            warning: Color::Indexed(227), // light yellow
            border: Color::Indexed(75),
            selection: Color::Indexed(19), // medium blue, lighter than the bg
        }
    }

    /// VT320 amber on black.
    pub fn vt320() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::Indexed(214), // amber
            muted: Color::Indexed(94),       // dim amber
            accent: Color::Indexed(220),     // bright amber
            heading: Color::Indexed(220),    // bright amber
            success: Color::Yellow,
            error: Color::LightRed,
            // Pale gold, distinct from the body amber (214) so a caution toast
            // reads as caution and not just bolded body text.
            warning: Color::Indexed(222),
            border: Color::Indexed(94),
            selection: Color::Indexed(238), // neutral dark gray
        }
    }

    /// Vaporwave / cyberpunk neon — hot pink and electric cyan over deep indigo.
    /// Tuned for readability: body text stays a near-white lavender (high contrast
    /// on the dark background), so the neon is reserved for accents, borders, and
    /// status colors. `error` (neon red), `accent` (pink), `success` (mint), and
    /// `warning` (gold) are kept visibly distinct so a toast's meaning reads at a
    /// glance. Defined in truecolor; on a 256-color terminal the app downsamples
    /// it to the nearest indexed color (see [`ColorMode`]).
    pub fn vapor() -> Self {
        Self {
            background: Color::Rgb(0x12, 0x1a, 0x2e), // deep teal-indigo (cooled)
            foreground: Color::Rgb(0xea, 0xf5, 0xff), // cool ice-white
            muted: Color::Rgb(0x82, 0xa4, 0xc4),      // slate-cyan gray (cooled)
            accent: Color::Rgb(0xff, 0x5f, 0xd1),     // hot pink (the content accent)
            heading: Color::Rgb(0x00, 0xe0, 0xff),    // electric cyan — titles join the frame
            success: Color::Rgb(0x5d, 0xff, 0xbf),    // mint
            error: Color::Rgb(0xff, 0x3b, 0x6b),      // neon red
            warning: Color::Rgb(0xff, 0xe1, 0x6b),    // pale gold
            border: Color::Rgb(0x00, 0xe0, 0xff),     // electric cyan
            selection: Color::Rgb(0x22, 0x30, 0x4f),  // cool indigo, lighter than the bg
        }
    }

    /// Neutral slate dark theme — cool grays with a sky-blue accent. The
    /// deliberate counterpoint to `cyber`'s green-on-black: same dark base, but a
    /// blue (not green) accent and a gray border, so the two read as distinct.
    pub fn dark() -> Self {
        Self {
            background: Color::Reset,
            foreground: Color::Indexed(252), // soft light gray
            muted: Color::Indexed(245),      // medium gray
            accent: Color::Indexed(75),      // sky blue
            heading: Color::Indexed(75),     // sky blue
            success: Color::Indexed(114),    // soft green
            error: Color::Indexed(203),      // soft red
            warning: Color::Indexed(215),    // amber
            border: Color::Indexed(240),     // neutral gray
            selection: Color::Indexed(238),  // dark gray, just under the border
        }
    }

    /// The one light theme: warm paper cream with dark ink, for bright rooms. Like
    /// `vapor`, it follows the "chrome is one color" rule — titles and borders are
    /// the same teal so the frame reads as a unit, while the raspberry `accent`
    /// carries content (usernames, links, the selection bar). Status colors are
    /// darkened from the usual neons so they stay legible on a light background
    /// (a pale yellow warning would vanish on cream). Truecolor; downsamples on a
    /// 256-color terminal.
    pub fn paper() -> Self {
        Self {
            background: Color::Rgb(0xfd, 0xf6, 0xe3), // warm paper cream
            foreground: Color::Rgb(0x3a, 0x35, 0x2c), // dark warm ink
            muted: Color::Rgb(0x76, 0x6c, 0x5c),      // warm gray
            accent: Color::Rgb(0xc1, 0x3b, 0x5b),     // raspberry (content)
            heading: Color::Rgb(0x0e, 0x7c, 0x86),    // teal — titles join the frame
            success: Color::Rgb(0x4c, 0x8a, 0x2e),    // forest green
            error: Color::Rgb(0xc0, 0x2a, 0x2a),      // red
            warning: Color::Rgb(0xb0, 0x76, 0x00),    // ochre (darkened to read on cream)
            border: Color::Rgb(0x0e, 0x7c, 0x86),     // teal (matches heading)
            selection: Color::Rgb(0xec, 0xdf, 0xc2),  // deeper cream, darker than the bg
        }
    }

    /// Gruvbox-inspired: warm, matte, earthy dark — the low-saturation counterpoint
    /// to `vapor`'s neon. Aqua frame (titles + borders) with an orange content
    /// `accent`; green/yellow/red round out the status colors. Truecolor;
    /// downsamples on a 256-color terminal.
    pub fn gruvbox() -> Self {
        Self {
            background: Color::Rgb(0x28, 0x28, 0x28), // dark warm gray (bg0)
            foreground: Color::Rgb(0xeb, 0xdb, 0xb2), // cream
            muted: Color::Rgb(0x92, 0x83, 0x74),      // warm gray
            accent: Color::Rgb(0xfe, 0x80, 0x19),     // orange (content)
            heading: Color::Rgb(0x8e, 0xc0, 0x7c),    // aqua — titles join the frame
            success: Color::Rgb(0xb8, 0xbb, 0x26),    // green
            error: Color::Rgb(0xfb, 0x49, 0x34),      // red
            warning: Color::Rgb(0xfa, 0xbd, 0x2f),    // yellow
            border: Color::Rgb(0x8e, 0xc0, 0x7c),     // aqua (matches heading)
            selection: Color::Rgb(0x3c, 0x38, 0x36),  // bg1, lighter than the bg
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

    /// Bold style for panel/screen titles. Same weight as `accent_style` but its
    /// own color slot, so a palette (e.g. `vapor`) can give titles a distinct
    /// hue from inline emphasis.
    pub fn heading_style(&self) -> Style {
        Style::default()
            .fg(self.heading)
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

    /// Highlight style for the selected list row in the `selection = "fill"`
    /// style: a background fill plus bold. Deliberately sets **only** the
    /// background (not the foreground), so ratatui patches it over each cell and
    /// every span keeps its own color — the row reads as one filled block, not a
    /// recolored monotone. In monochrome mode `selection` is `Reset`, so the fill
    /// disappears and the `▌` bar + bold carry the selection.
    pub fn selection_style(&self) -> Style {
        Style::default()
            .bg(self.selection)
            .add_modifier(Modifier::BOLD)
    }

    /// Adapt the palette to what the terminal can actually display. `Full` is a
    /// no-op; `Indexed256` downsamples any truecolor (`vapor`, `#hex` customs) to
    /// the nearest 256-color index; `Monochrome` drops all color (NO_COLOR),
    /// leaving emphasis to the bold modifiers and layout.
    #[must_use]
    pub fn adapt(&self, mode: ColorMode) -> Self {
        match mode {
            ColorMode::Full => self.clone(),
            ColorMode::Indexed256 => self.map(|c| match c {
                Color::Rgb(r, g, b) => Color::Indexed(rgb_to_ansi256(r, g, b)),
                other => other,
            }),
            ColorMode::Monochrome => self.map(|_| Color::Reset),
        }
    }

    fn map(&self, f: impl Fn(Color) -> Color) -> Self {
        Self {
            background: f(self.background),
            foreground: f(self.foreground),
            muted: f(self.muted),
            accent: f(self.accent),
            heading: f(self.heading),
            success: f(self.success),
            error: f(self.error),
            warning: f(self.warning),
            border: f(self.border),
            selection: f(self.selection),
        }
    }

    /// Force the background per the user's transparency preference. `Some(Reset)`
    /// lets the terminal's own transparency/opacity show through; `Some(opaque)`
    /// paints a solid backdrop; `None` keeps the palette's own background.
    #[must_use]
    pub fn with_background(mut self, bg: Option<Color>) -> Self {
        if let Some(c) = bg {
            self.background = c;
        }
        self
    }
}

/// What the terminal can render, decided once at startup from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit truecolor — render `Rgb` directly.
    Full,
    /// 256-color only — downsample `Rgb` to the nearest indexed color.
    Indexed256,
    /// `NO_COLOR` is set — render no color at all.
    Monochrome,
}

impl ColorMode {
    /// Detect from `NO_COLOR` (the [no-color.org](https://no-color.org)
    /// convention) and `COLORTERM`. Absent a truecolor hint we assume 256-color
    /// and downsample, since sending 24-bit escapes to a 256-color terminal
    /// renders wrong rather than degrading.
    #[must_use]
    pub fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::Monochrome;
        }
        let truecolor = std::env::var("COLORTERM").is_ok_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("truecolor") || v.contains("24bit")
        });
        if truecolor {
            Self::Full
        } else {
            Self::Indexed256
        }
    }
}

/// Nearest xterm-256 index for an RGB color (6×6×6 cube + grayscale ramp).
#[must_use]
fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        // Grayscale: the 24-step ramp (232..=255), with the cube's ends for the
        // extremes.
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + ((u16::from(r) - 8) * 24 / 247) as u8;
    }
    let q = |c: u8| -> u16 {
        if c < 48 {
            0
        } else if c < 115 {
            1
        } else {
            (u16::from(c) - 35) / 40
        }
    };
    (16 + 36 * q(r) + 6 * q(g) + q(b)) as u8
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
    Paper,
    Gruvbox,
    Custom,
}

impl ThemeKind {
    /// The built-in palettes, in cycle order. (`Custom` is appended by the App
    /// when configured.)
    pub const ALL: [ThemeKind; 7] = [
        Self::Cyber,
        Self::C64,
        Self::Vt320,
        Self::Dark,
        Self::Vapor,
        Self::Paper,
        Self::Gruvbox,
    ];

    /// Stable lowercase name — matches the persisted prefs value and the
    /// `config.toml` `theme` key.
    pub fn name(self) -> &'static str {
        match self {
            Self::Cyber => "cyber",
            Self::C64 => "c64",
            Self::Vt320 => "vt320",
            Self::Dark => "dark",
            Self::Vapor => "vapor",
            Self::Paper => "paper",
            Self::Gruvbox => "gruvbox",
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
            "paper" => Self::Paper,
            "gruvbox" => Self::Gruvbox,
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
            Self::Paper => Theme::paper(),
            Self::Gruvbox => Theme::gruvbox(),
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
    fn cyber_and_dark_are_visually_distinct() {
        // They share a dark base, but must differ where the eye lands first:
        // accent and border. (They were near-identical before.)
        let (c, d) = (Theme::cyber(), Theme::dark());
        assert_ne!(c.accent, d.accent, "accent must differ");
        assert_ne!(c.border, d.border, "border must differ");
    }

    #[test]
    fn indexed256_downsamples_truecolor_but_leaves_named_colors() {
        // vapor is all Rgb → every slot becomes an indexed color.
        let v = Theme::vapor().adapt(ColorMode::Indexed256);
        for c in [v.accent, v.background, v.error, v.border] {
            assert!(
                matches!(c, Color::Indexed(_)),
                "expected indexed, got {c:?}"
            );
        }
        // cyber uses named/indexed colors → unchanged by the downsample.
        let c = Theme::cyber();
        assert_eq!(c.adapt(ColorMode::Indexed256).accent, c.accent);
    }

    #[test]
    fn monochrome_drops_all_color() {
        let m = Theme::vapor().adapt(ColorMode::Monochrome);
        for c in [
            m.foreground,
            m.accent,
            m.error,
            m.muted,
            m.border,
            m.selection,
        ] {
            assert_eq!(c, Color::Reset);
        }
    }

    #[test]
    fn with_background_overrides_only_when_some() {
        let base = Theme::cyber();
        // Some(_) forces the background (transparency override).
        assert_eq!(
            base.clone().with_background(Some(Color::Reset)).background,
            Color::Reset
        );
        assert_eq!(
            base.clone().with_background(Some(Color::Black)).background,
            Color::Black
        );
        // None leaves the palette's own background untouched.
        assert_eq!(
            base.clone().with_background(None).background,
            base.background
        );
        // Only the background slot moves; selection (and the rest) are unchanged.
        assert_eq!(
            base.clone().with_background(Some(Color::Black)).selection,
            base.selection
        );
    }

    #[test]
    fn full_mode_is_identity() {
        let v = Theme::vapor();
        assert_eq!(v.adapt(ColorMode::Full).accent, v.accent);
    }

    #[test]
    fn rgb_to_ansi256_maps_primaries_and_grays() {
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16); // black → cube origin
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231); // white → cube corner
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196); // pure red
                                                    // Mid grays land in the 232..=255 ramp.
        assert!((232..=255).contains(&rgb_to_ansi256(128, 128, 128)));
    }

    #[test]
    fn vapor_is_a_built_in_in_the_cycle() {
        assert!(ThemeKind::ALL.contains(&ThemeKind::Vapor));
        assert_eq!(ThemeKind::from_name("vapor"), ThemeKind::Vapor);
        assert_eq!(ThemeKind::Vapor.name(), "vapor");
        assert_eq!(ThemeKind::Vapor.theme().accent, Theme::vapor().accent);
    }

    #[test]
    fn vapor_titles_are_cyan_not_pink() {
        // Cyan's "second job": titles share the cyan border instead of the pink
        // accent, so the frame reads as one color and cyan co-leads with pink.
        let t = Theme::vapor();
        assert_eq!(t.heading, t.border, "vapor titles join the cyan frame");
        assert_ne!(t.heading, t.accent, "titles must not be the pink accent");
    }

    #[test]
    fn legacy_palettes_keep_titles_on_their_accent() {
        // The original four palettes were single-hue (or accent-titled) before the
        // `heading` slot existed, so titles stay on their accent — unchanged look.
        for t in [Theme::cyber(), Theme::c64(), Theme::vt320(), Theme::dark()] {
            assert_eq!(t.heading, t.accent);
        }
    }

    #[test]
    fn framed_palettes_put_titles_on_the_border() {
        // vapor/paper/gruvbox follow the "chrome is one color" rule: the title
        // shares the border color and is distinct from the content accent.
        for t in [Theme::vapor(), Theme::paper(), Theme::gruvbox()] {
            assert_eq!(t.heading, t.border, "title joins the frame");
            assert_ne!(t.heading, t.accent, "title is not the content accent");
        }
    }

    #[test]
    fn new_palettes_keep_status_colors_distinct() {
        // Same usability guarantee as vapor: a toast's meaning must read at a
        // glance, so accent and the three status colors can't collide.
        for t in [Theme::paper(), Theme::gruvbox()] {
            let slots = [t.accent, t.success, t.error, t.warning];
            for (i, a) in slots.iter().enumerate() {
                for b in &slots[i + 1..] {
                    assert_ne!(a, b, "status colors must be distinct");
                }
            }
        }
    }

    #[test]
    fn warning_never_collides_with_body_text() {
        // A caution toast must read as caution, not as bolded body text. (vt320
        // previously had warning == foreground; this guards the fix.)
        for k in ThemeKind::ALL {
            let t = k.theme();
            assert_ne!(
                t.warning,
                t.foreground,
                "{}: warning == foreground",
                k.name()
            );
        }
    }

    #[test]
    fn paper_is_the_one_light_theme() {
        // Its background must be lighter than its foreground (the only built-in
        // where that holds), and every other built-in stays dark.
        let lum = |c: Color| match c {
            Color::Rgb(r, g, b) => Some(u16::from(r) * 30 + u16::from(g) * 59 + u16::from(b) * 11),
            _ => None,
        };
        let p = Theme::paper();
        assert!(
            lum(p.background) > lum(p.foreground),
            "paper is light-on-dark-ink"
        );
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

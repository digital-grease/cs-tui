//! Text styles for chat messages (API v0.8.4).
//!
//! § Commands lists twelve styles a message can be posted with (`blink`,
//! `l33t`, `comic`, `cursive`, `times`, `rainbow`, `flip`, `quiet`, `slow`,
//! `glitch`, `spoiler`, `wave`), chained with `+`, and says everything chains
//! except `spoiler`. § Message fields adds that they are "purely
//! presentational", that it is up to the client to decide what `rainbow` or
//! `blink` looks like, and that a client may ignore them entirely. `art` is the
//! one style that is *not* presentational, since it changes how `content` should
//! be read; that lives in [`super::art`].
//!
//! What this module renders, and why:
//!
//! | style | rendering |
//! |-------|-----------|
//! | `rainbow` | one span per character, cycling the six normal ANSI hues |
//! | `quiet` | the dim modifier |
//! | `spoiler` | every glyph masked with `▒` until the screen reveals it |
//! | `blink` | bold plus the theme's warning hue, a static "look at me" |
//! | `wave` | italic, a lean standing in for motion |
//! | `slow` | italic and dim, unhurried |
//! | `glitch` | reversed video, which reads as corruption while standing still |
//! | everything else | plain text |
//!
//! Two deliberate constraints:
//!
//! **No animation clock.** `blink`, `wave`, `slow` and `glitch` are static
//! approximations. Driving them would mean redrawing chat panes on a timer for
//! decoration alone, which costs a wakeup per frame on an idle client and buys
//! very little.
//!
//! **`l33t`, `flip`, `comic`, `cursive` and `times` are treated as no-ops.**
//! These are ambiguous in the spec: it does not say whether the server already
//! transformed the text or expects the client to. A leet or flip transform
//! *changes the text itself*, which contradicts § Message fields calling styles
//! "purely presentational", and the server is documented as expanding commands
//! itself ("resolved server-side, so any client gets it for free"). So the
//! assumption here is that the server sends the final text and these five carry
//! no client-side rendering. No substitution tables are shipped, because a
//! wrong one would corrupt the message rather than merely decorate it. If a
//! live server ever proves otherwise, this is the paragraph to revisit and
//! [`TextStyles`] is where a flag for them would go.
//!
//! Anything unrecognized, including [`cs_api::MessageStyle::Other`] (the
//! catch-all for a `style` field whose JSON shape this client cannot read),
//! degrades to plain text in silence. A style name is never printed at the
//! reader, and neither is raw JSON.
use cs_api::MessageStyle;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

use super::theme::Theme;

/// The glyph an unrevealed `spoiler` is masked with.
///
/// A medium shade block rather than a full block, so a masked run still reads
/// as "text is hidden here" instead of as a solid rule.
pub const SPOILER_GLYPH: char = '▒';

/// The `rainbow` cycle.
///
/// The six *normal* ANSI hues, not their bright variants: every terminal maps
/// these to colors chosen to be legible against its own background, so the
/// cycle survives both a black and a cream terminal. Bright red or bright
/// yellow would wash out on the light `paper` palette. They are fixed rather
/// than pulled from the [`Theme`], because most palettes here are near
/// monochrome (the `cyber` accent and heading are the same green) and a rainbow
/// built from them would not be one.
const RAINBOW: [Color; 6] = [
    Color::Red,
    Color::Yellow,
    Color::Green,
    Color::Cyan,
    Color::Blue,
    Color::Magenta,
];

/// The styles decoded off one message's `style` field.
///
/// Styles chain, so this is a set of flags rather than an enum. Every unknown
/// name is dropped on the floor, which is what makes an unrecognized style
/// render as plain text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextStyles {
    /// `art`: `content` is base64 and must go through [`super::art`] first.
    pub art: bool,
    /// `rainbow`: per-character color cycling.
    pub rainbow: bool,
    /// `quiet`: dimmed.
    pub quiet: bool,
    /// `spoiler`: hidden until the reader reveals it. Does not chain.
    pub spoiler: bool,
    /// `blink`: rendered statically, as bold plus the warning hue.
    pub blink: bool,
    /// `wave`: rendered statically, as italic.
    pub wave: bool,
    /// `slow`: rendered statically, as dim italic.
    pub slow: bool,
    /// `glitch`: rendered statically, as reversed video.
    pub glitch: bool,
}

impl TextStyles {
    /// Decode a message's `style` field.
    ///
    /// Handles both wire shapes ([`MessageStyle::One`] and
    /// [`MessageStyle::Many`]), matches names case-insensitively, and also
    /// splits on `+` in case a server ever echoes a chain back as the literal
    /// `"comic+rainbow"` rather than as an array.
    ///
    /// ```ignore
    /// let styles = TextStyles::from_message(message.extras.style.as_ref());
    /// ```
    #[must_use]
    pub fn from_message(style: Option<&MessageStyle>) -> Self {
        let mut out = Self::default();
        let Some(style) = style else {
            return out;
        };
        for name in style.names() {
            for part in name.split('+') {
                match part.trim().to_ascii_lowercase().as_str() {
                    "art" => out.art = true,
                    "rainbow" => out.rainbow = true,
                    "quiet" => out.quiet = true,
                    "spoiler" => out.spoiler = true,
                    "blink" => out.blink = true,
                    "wave" => out.wave = true,
                    "slow" => out.slow = true,
                    "glitch" => out.glitch = true,
                    // `l33t`, `flip`, `comic`, `cursive` and `times` land here
                    // with everything unrecognized: see the module doc, this
                    // client assumes the server already applied them to the
                    // text and so renders them as they arrived.
                    _ => {}
                }
            }
        }
        out
    }
}

/// Render one already-wrapped row of message text as styled spans.
///
/// `base` is the style the row would have had unstyled (the caller's body or
/// action style), so chat keeps its own colors and this only decorates them.
/// `revealed` is owned by the screen, not by this module: it says whether the
/// reader has pressed the reveal key on this message, and only matters for
/// `spoiler`.
///
/// Returns one span for the whole row in the common case, and one span per
/// character for `rainbow`.
///
/// ```ignore
/// let spans = styles::styled_spans(row, styles, revealed, theme.base(), theme);
/// ```
#[must_use]
pub fn styled_spans(
    text: &str,
    styles: TextStyles,
    revealed: bool,
    base: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let style = decorate(styles, base, theme);
    if styles.spoiler && !revealed {
        return vec![Span::styled(mask(text), style.fg(theme.muted))];
    }
    if styles.rainbow && !is_monochrome(theme) {
        return text
            .chars()
            .enumerate()
            .map(|(i, c)| Span::styled(c.to_string(), style.fg(RAINBOW[i % RAINBOW.len()])))
            .collect();
    }
    vec![Span::styled(text.to_string(), style)]
}

/// Mask a row for an unrevealed `spoiler`.
///
/// Each character becomes as many [`SPOILER_GLYPH`]s as it occupied columns, so
/// the masked row has exactly the display width of the real one. That keeps the
/// message the same height and the same shape whether or not it is revealed, so
/// revealing never reflows the pane. It does leak the length of the hidden
/// text, which is the same bargain every inline spoiler makes.
fn mask(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        for _ in 0..UnicodeWidthChar::width(c).unwrap_or(0) {
            out.push(SPOILER_GLYPH);
        }
    }
    out
}

/// Fold the presentational styles into one [`Style`].
///
/// `rainbow` is not applied here: it needs one style per character and is
/// handled by [`styled_spans`], except under `NO_COLOR`, where it has no color
/// to cycle and falls back to bold.
fn decorate(styles: TextStyles, base: Style, theme: &Theme) -> Style {
    let mut style = base;
    if styles.quiet {
        style = style.add_modifier(Modifier::DIM);
    }
    if styles.slow {
        style = style.add_modifier(Modifier::DIM | Modifier::ITALIC);
    }
    if styles.wave {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if styles.glitch {
        style = style.add_modifier(Modifier::REVERSED);
    }
    if styles.blink {
        style = style.add_modifier(Modifier::BOLD).fg(theme.warning);
    }
    if styles.rainbow && is_monochrome(theme) {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

/// Whether the palette has been flattened to no color at all.
///
/// [`Theme::adapt`] maps every slot to [`Color::Reset`] for `NO_COLOR`, and no
/// real palette has its foreground, accent and error all unset, so this
/// recognizes that state without threading the color mode down here. It matters
/// because [`RAINBOW`] is a fixed palette rather than a theme color, so it
/// would otherwise keep emitting color after the user asked for none.
fn is_monochrome(theme: &Theme) -> bool {
    theme.foreground == Color::Reset && theme.accent == Color::Reset && theme.error == Color::Reset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::{ColorMode, ThemeKind};

    fn one(name: &str) -> Option<MessageStyle> {
        Some(MessageStyle::One(name.to_string()))
    }

    fn many(names: &[&str]) -> Option<MessageStyle> {
        Some(MessageStyle::Many(
            names.iter().map(|s| (*s).to_string()).collect(),
        ))
    }

    fn text_of(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_single_style_name_decodes() {
        let styles = TextStyles::from_message(one("rainbow").as_ref());
        assert_eq!(
            styles,
            TextStyles {
                rainbow: true,
                ..TextStyles::default()
            },
        );
    }

    #[test]
    fn names_are_matched_case_insensitively() {
        assert!(TextStyles::from_message(one("RAINBOW").as_ref()).rainbow);
        assert!(TextStyles::from_message(one(" Quiet ").as_ref()).quiet);
    }

    #[test]
    fn a_chain_decodes_from_an_array_and_from_a_plus_joined_name() {
        // § Commands: "Styles chain with `+`". The server sends an array, but a
        // literal chain must not be mistaken for one unknown name.
        let from_array = TextStyles::from_message(many(&["quiet", "rainbow"]).as_ref());
        assert!(from_array.rainbow && from_array.quiet);

        let from_literal = TextStyles::from_message(one("quiet+rainbow").as_ref());
        assert_eq!(from_literal, from_array);
    }

    #[test]
    fn every_rendered_style_in_the_spec_table_decodes() {
        // The names § Commands lists that this client draws differently, plus
        // `art`, which changes how the content is read.
        for name in [
            "blink", "rainbow", "quiet", "slow", "glitch", "spoiler", "wave", "art",
        ] {
            let styles = TextStyles::from_message(one(name).as_ref());
            assert_ne!(
                styles,
                TextStyles::default(),
                "{name} should set at least one flag",
            );
        }
    }

    #[test]
    fn no_style_the_ornamental_five_and_unknown_names_all_decode_to_nothing() {
        // The five text-transforming names are assumed to have been applied by
        // the server (see the module doc), so they carry no client rendering and
        // must be indistinguishable from an unrecognized name.
        assert_eq!(TextStyles::from_message(None), TextStyles::default());
        for name in ["l33t", "flip", "comic", "cursive", "times", "sparkle"] {
            assert_eq!(
                TextStyles::from_message(one(name).as_ref()),
                TextStyles::default(),
                "{name} should render as plain text",
            );
        }
    }

    #[test]
    fn an_unreadable_style_shape_is_plain() {
        // MessageStyle::Other is the catch-all for a `style` field whose JSON
        // shape this client cannot read. It must never reach the reader.
        let other = MessageStyle::Other(serde_json::json!({"name": "rainbow"}));
        let styles = TextStyles::from_message(Some(&other));
        assert_eq!(styles, TextStyles::default());

        let theme = Theme::cyber();
        let spans = styled_spans("hello", styles, false, theme.base(), &theme);
        assert_eq!(text_of(&spans), "hello");
    }

    #[test]
    fn an_unknown_style_name_never_leaks_into_the_output() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("hologram").as_ref());
        let spans = styled_spans("hi there", styles, false, theme.base(), &theme);
        let text = text_of(&spans);
        assert_eq!(text, "hi there");
        assert!(!text.contains("hologram"), "the style name must not print");
    }

    #[test]
    fn quiet_dims_the_row() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("quiet").as_ref());
        let spans = styled_spans("psst", styles, false, theme.base(), &theme);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn the_static_styles_each_pick_up_their_decoration() {
        let theme = Theme::cyber();
        for (name, modifier) in [
            ("blink", Modifier::BOLD),
            ("wave", Modifier::ITALIC),
            ("slow", Modifier::DIM),
            ("glitch", Modifier::REVERSED),
        ] {
            let styles = TextStyles::from_message(one(name).as_ref());
            let spans = styled_spans("x", styles, false, theme.base(), &theme);
            assert!(
                spans[0].style.add_modifier.contains(modifier),
                "{name} should render with {modifier:?}",
            );
        }
    }

    #[test]
    fn chained_styles_combine_their_decorations() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(many(&["quiet", "wave"]).as_ref());
        let spans = styled_spans("both", styles, false, theme.base(), &theme);
        assert!(spans[0].style.add_modifier.contains(Modifier::DIM));
        assert!(spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn rainbow_colors_each_character_in_turn() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("rainbow").as_ref());
        let spans = styled_spans("abcdefg", styles, false, theme.base(), &theme);
        assert_eq!(spans.len(), 7, "one span per character");
        assert_eq!(text_of(&spans), "abcdefg", "the text is untouched");
        assert_eq!(spans[0].style.fg, Some(RAINBOW[0]));
        assert_eq!(spans[1].style.fg, Some(RAINBOW[1]));
        assert_eq!(spans[6].style.fg, Some(RAINBOW[0]), "the cycle wraps");
    }

    #[test]
    fn rainbow_uses_hues_that_read_on_light_and_dark_terminals() {
        // The normal ANSI hues, not the bright ones: a bright yellow vanishes on
        // the cream `paper` palette.
        for color in RAINBOW {
            assert!(
                !matches!(
                    color,
                    Color::LightRed
                        | Color::LightYellow
                        | Color::LightGreen
                        | Color::LightCyan
                        | Color::LightBlue
                        | Color::LightMagenta
                ),
                "{color:?} is a bright variant",
            );
        }
    }

    #[test]
    fn rainbow_emits_no_color_under_no_color() {
        // NO_COLOR flattens the palette; a fixed rainbow palette would otherwise
        // keep painting after the user asked for none.
        let theme = Theme::cyber().adapt(ColorMode::Monochrome);
        let styles = TextStyles::from_message(one("rainbow").as_ref());
        let spans = styled_spans("abc", styles, false, theme.base(), &theme);
        assert_eq!(spans.len(), 1, "collapses to one plain span");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[0].style.fg, Some(Color::Reset));
    }

    #[test]
    fn a_spoiler_is_masked_until_it_is_revealed() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("spoiler").as_ref());

        let hidden = styled_spans("the butler", styles, false, theme.base(), &theme);
        let masked = text_of(&hidden);
        assert!(!masked.contains("butler"), "the text must not be readable");
        assert!(masked.chars().all(|c| c == SPOILER_GLYPH));

        let shown = styled_spans("the butler", styles, true, theme.base(), &theme);
        assert_eq!(text_of(&shown), "the butler");
    }

    #[test]
    fn a_masked_spoiler_keeps_the_display_width_of_the_text() {
        // Revealing must not reflow the pane, so the mask has to occupy exactly
        // the columns the text did, including for wide glyphs.
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("spoiler").as_ref());
        for text in ["plain text", "你好世界", "wide 👍 emoji"] {
            let width: usize = text
                .chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            let masked = text_of(&styled_spans(text, styles, false, theme.base(), &theme));
            assert_eq!(masked.chars().count(), width, "mask width for {text:?}");
        }
    }

    #[test]
    fn art_is_flagged_but_carries_no_decoration_of_its_own() {
        assert_eq!(
            TextStyles::from_message(one("art").as_ref()),
            TextStyles {
                art: true,
                ..TextStyles::default()
            },
            "art changes how the content is decoded, not how it is decorated",
        );
    }

    #[test]
    fn styling_never_changes_the_text_except_for_a_hidden_spoiler() {
        let theme = Theme::cyber();
        for name in [
            "blink", "l33t", "comic", "cursive", "times", "rainbow", "flip", "quiet", "slow",
            "glitch", "wave",
        ] {
            let styles = TextStyles::from_message(one(name).as_ref());
            let spans = styled_spans("hello world", styles, false, theme.base(), &theme);
            assert_eq!(text_of(&spans), "hello world", "{name} altered the text");
        }
    }

    #[test]
    fn monochrome_detection_does_not_misfire_on_real_palettes() {
        // `cyber` has a Reset background but real foreground colors.
        for kind in ThemeKind::ALL {
            assert!(!is_monochrome(&kind.theme()), "{}", kind.name());
        }
        assert!(is_monochrome(&Theme::cyber().adapt(ColorMode::Monochrome)));
    }
}

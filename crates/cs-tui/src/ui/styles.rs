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
//! Styles split into two kinds, and the split is the thing to understand here.
//!
//! **Attribute styles** decorate the text without changing it, and are applied
//! at render time by [`styled_spans`]:
//!
//! | style | rendering |
//! |-------|-----------|
//! | `rainbow` | one span per character, cycling the six normal ANSI hues |
//! | `quiet` | the dim modifier |
//! | `spoiler` | every glyph masked with `▒` until the screen reveals it |
//! | `blink` | bold plus the theme's warning hue, a static "look at me" |
//! | `wave` | italic when static; a travelling bright crest when animated |
//! | `slow` | italic and dim, unhurried |
//! | `glitch` | reversed video when static; the glyphs decaying under combining marks when animated |
//!
//! **Substitution styles** rewrite the characters themselves, and are applied
//! by [`TextStyles::transform`] *before* the text is wrapped:
//!
//! | style | transform |
//! |-------|-----------|
//! | `l33t` | `a`→`4`, `e`→`3`, `i`→`1`, `o`→`0`, `s`→`5`, `t`→`7`, either case |
//! | `cursive` | ASCII letters to Unicode Mathematical Script |
//! | `flip` | lowercased, reversed, mapped to upside-down lookalikes |
//!
//! `art` is neither: it means `content` is base64 and belongs to [`super::art`].
//! `comic` and `times` are no-ops, and anything unrecognized is plain text.
//!
//! Three deliberate constraints:
//!
//! **No animation clock.** `blink`, `wave`, `slow` and `glitch` are static
//! approximations. Driving them would mean redrawing chat panes on a timer for
//! decoration alone, which costs a wakeup per frame on an idle client and buys
//! very little.
//!
//! **`comic` and `times` stay no-ops.** Unlike the three substitutions, these
//! name *font families*. A terminal renders one font for the whole application,
//! so there is nothing faithful to do with them, and inventing a substitution
//! would misrepresent what the sender wrote.
//!
//! **A substituted body loses its in-body `@mention` highlight.** [`super::chat`]
//! finds mentions by byte range over the row text, and a substitution moves
//! those bytes (`@ragnar` becomes `@r4gn4r`; `flip` reverses the row outright),
//! so the ranges stop matching. The row-level marker is unaffected, because
//! [`super::chat::mentions`] reads the raw `content` rather than the rendered
//! text, so a message naming the reader is still marked in the gutter. That is
//! the trade accepted here: the cheap signal survives, the expensive one does
//! not.
//!
//! An earlier version of this module treated `l33t`, `flip` and `cursive` as
//! no-ops too, reasoning that § Message fields calls styles "purely
//! presentational" and that the server therefore sent already-transformed text.
//! That was wrong, confirmed against the live server on 2026-08-24: the server
//! sends raw text and the client must transform it. The sentence that reading
//! came from names only `rainbow` and `blink`, and does not generalize to the
//! substitution styles.
//!
//! Anything unrecognized, including [`cs_api::MessageStyle::Other`] (the
//! catch-all for a `style` field whose JSON shape this client cannot read),
//! degrades to plain text in silence. A style name is never printed at the
//! reader, and neither is raw JSON.
use std::borrow::Cow;

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
    /// `glitch`: reversed video when static, glyph corruption when animated.
    ///
    /// The corruption is a separate pass, [`corrupt_spans`], because it has to
    /// run after everything that indexes the row.
    pub glitch: bool,
    /// `l33t`: vowel-and-consonant digit substitution. Rewrites the text.
    pub l33t: bool,
    /// `cursive`: ASCII letters to Unicode Mathematical Script. Rewrites the text.
    pub cursive: bool,
    /// `flip`: lowercased, reversed, upside-down lookalikes. Rewrites the text.
    pub flip: bool,
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
                    "l33t" => out.l33t = true,
                    "cursive" => out.cursive = true,
                    "flip" => out.flip = true,
                    // `comic` and `times` land here with everything
                    // unrecognized. They name font families, which a terminal
                    // cannot honor; see the module doc.
                    _ => {}
                }
            }
        }
        out
    }
}

/// `l33t`: the six substitutions the website makes, applied to either case.
const L33T: [(char, char); 6] = [
    ('a', '4'),
    ('e', '3'),
    ('i', '1'),
    ('o', '0'),
    ('s', '5'),
    ('t', '7'),
];

/// `flip`: lowercase ASCII to its conventional upside-down lookalike.
///
/// Uppercase is folded to lowercase before lookup. A faithful per-case table
/// depends on glyphs many terminal fonts lack, so this takes the lowercase-only
/// rendering rather than produce tofu for half an alphabet. `l`, `o`, `s`, `x`
/// and `z` map to themselves, which is correct: they already read the same
/// inverted.
const FLIP: [(char, char); 26] = [
    ('a', '\u{250}'),
    ('b', 'q'),
    ('c', '\u{254}'),
    ('d', 'p'),
    ('e', '\u{1dd}'),
    ('f', '\u{25f}'),
    ('g', '\u{183}'),
    ('h', '\u{265}'),
    ('i', '\u{1d09}'),
    ('j', '\u{27e}'),
    ('k', '\u{29e}'),
    ('l', 'l'),
    ('m', '\u{26f}'),
    ('n', 'u'),
    ('o', 'o'),
    ('p', 'd'),
    ('q', 'b'),
    ('r', '\u{279}'),
    ('s', 's'),
    ('t', '\u{287}'),
    ('u', 'n'),
    ('v', '\u{28c}'),
    ('w', '\u{28d}'),
    ('x', 'x'),
    ('y', '\u{28e}'),
    ('z', 'z'),
];

/// `cursive`: the eleven code points missing from Mathematical Script.
///
/// The block at `U+1D49C` has holes where Unicode had already assigned the
/// letter in Letterlike Symbols, so a naive `base + offset` lands on unassigned
/// code points for these. Each is aliased to its real home instead.
const SCRIPT_HOLES: [(char, char); 11] = [
    ('B', '\u{212c}'),
    ('E', '\u{2130}'),
    ('F', '\u{2131}'),
    ('H', '\u{210b}'),
    ('I', '\u{2110}'),
    ('L', '\u{2112}'),
    ('M', '\u{2133}'),
    ('R', '\u{211b}'),
    ('e', '\u{212f}'),
    ('g', '\u{210a}'),
    ('o', '\u{2134}'),
];

impl TextStyles {
    /// Whether any style here rewrites the text rather than decorating it.
    #[must_use]
    pub fn substitutes(self) -> bool {
        self.l33t || self.cursive || self.flip
    }

    /// Apply the substitution styles, in the order the website applies them.
    ///
    /// Must run **before** the text is wrapped, for two reasons: `flip`
    /// reverses the whole string, so wrapping first would reverse each row
    /// independently and scramble the reading order across rows; and `cursive`
    /// changes which code points are present, which the width math has to see.
    /// It must also run before [`mask`], so an unrevealed `spoiler` masks the
    /// substituted text and keeps the pane the same shape either way.
    ///
    /// Returns [`Cow::Borrowed`] untouched when no substitution applies, which
    /// is the overwhelmingly common case.
    ///
    /// ```ignore
    /// let text = styles.transform(text);
    /// let rows = word_wrap(&text, width);
    /// ```
    #[must_use]
    pub fn transform<'a>(self, text: &'a str) -> Cow<'a, str> {
        if !self.substitutes() {
            return Cow::Borrowed(text);
        }
        let mut out = text.to_string();
        if self.l33t {
            out = map_chars(&out, &L33T);
        }
        if self.cursive {
            out = out.chars().map(to_script).collect();
        }
        if self.flip {
            // Lowercase first, then reverse: reading upside-down text means
            // reading it back to front, so the last character has to land
            // first. Chained after the others deliberately, matching the
            // website, so `l33t+flip` flips the digits rather than leeting
            // characters that are no longer Latin letters.
            out = map_chars(&out.to_lowercase(), &FLIP)
                .chars()
                .rev()
                .collect();
        }
        Cow::Owned(out)
    }
}

/// Substitute through a table, leaving anything absent from it alone.
///
/// Case-preserving: an uppercase letter takes its lowercase entry's
/// replacement, so `l33t` leets `A` and `a` alike without a second table.
fn map_chars(text: &str, table: &[(char, char)]) -> String {
    text.chars()
        .map(|c| {
            let key = c.to_ascii_lowercase();
            table
                .iter()
                .find(|(from, _)| *from == key)
                .map_or(c, |(_, to)| *to)
        })
        .collect()
}

/// One character into Mathematical Script, or unchanged if it is not ASCII alpha.
fn to_script(c: char) -> char {
    if let Some((_, alias)) = SCRIPT_HOLES.iter().find(|(from, _)| *from == c) {
        return *alias;
    }
    let base = match c {
        'A'..='Z' => 0x1D49C - u32::from(b'A'),
        'a'..='z' => 0x1D4B6 - u32::from(b'a'),
        _ => return c,
    };
    char::from_u32(base + u32::from(c)).unwrap_or(c)
}

/// Whether these styles have anything to animate.
///
/// Only `blink`, `wave` and `glitch` do. `slow` reads as a static dim italic by
/// design, and animating a rainbow would be motion sickness rather than colour.
impl TextStyles {
    #[must_use]
    pub fn animates(self) -> bool {
        self.blink || self.wave || self.glitch
    }
}

/// How many animation frames one blink phase lasts.
///
/// At [`ANIM_TICK`] that is roughly 600ms visible, 600ms hidden, close to a
/// terminal cursor's own blink.
const BLINK_PHASE_FRAMES: usize = 4;

/// How often the animation clock advances.
const ANIM_TICK: std::time::Duration = std::time::Duration::from_millis(150);

/// The animation tick interval, for the screen driving the clock.
#[must_use]
pub fn anim_tick() -> std::time::Duration {
    ANIM_TICK
}

/// Render one already-wrapped row of message text as styled spans, at
/// animation frame `frame`.
///
/// `None` renders the static approximation, which is what an unanimated client
/// sees and what `animate_styles = false` keeps. `Some(n)` advances `blink`,
/// `wave` and `glitch`; every other style renders identically either way, so a
/// message with nothing to animate looks the same at every frame.
#[must_use]
pub fn styled_spans_at(
    text: &str,
    styles: TextStyles,
    revealed: bool,
    base: Style,
    theme: &Theme,
    frame: Option<usize>,
) -> Vec<Span<'static>> {
    // Blink is a visibility toggle rather than an attribute: the ANSI blink SGR
    // is ignored or disabled by a good many terminals and multiplexers, so
    // driving it ourselves is the only way it behaves the same everywhere.
    if let Some(f) = frame {
        // A masked spoiler is left alone: blanking an already-hidden run adds
        // nothing, and un-blanking it half the time would flicker the mask.
        let hidden_spoiler = styles.spoiler && !revealed;
        if styles.blink && !hidden_spoiler && (f / BLINK_PHASE_FRAMES) % 2 == 1 {
            // Blank rather than absent, so the row keeps its height and the
            // pane does not reflow twice a second. Padded to the row's *display
            // width* rather than its character count: a wide glyph occupies two
            // columns, so one space per character would make the blank phase
            // narrower than the visible one and the row would twitch.
            let cols: usize = text
                .chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            return vec![Span::styled(" ".repeat(cols), base)];
        }
    }
    // Both `wave` and `glitch` once animated by flipping the case of scattered
    // characters, which was wrong twice over. The owner's verdict from a live
    // run was that they "do the same thing, glitch just has a highlighted
    // background": two shimmers of case changes, told apart only by `glitch`'s
    // whole-row reverse showing through underneath. And rewriting text here
    // meant a character whose case mapping is a different width shifted every
    // later byte offset, which is what `super::chat::highlight_runs` measures
    // `@mention` ranges in. `the_kelvin_sign_does_not_slide_a_mention_range`
    // is that bug, kept.
    //
    // They are now different in kind. A wave restyles cells: a bright crest
    // travelling along the row, text untouched, handled below. A glitch
    // corrupts the glyphs themselves, in `corrupt_spans`, which the renderer
    // runs last precisely so it cannot slide anything.
    let animating = frame.is_some() && (styles.wave || styles.glitch);
    let style = decorate(styles, base, theme, animating);
    if styles.spoiler && !revealed {
        return vec![Span::styled(mask(text), style.fg(theme.muted))];
    }
    let rainbow = styles.rainbow && !is_monochrome(theme);
    // Only a wave needs the one-span-per-character treatment. A glitch is
    // animating too, but its animation is not a styling one.
    let per_cell = animating && styles.wave;
    if !rainbow && !per_cell {
        return vec![Span::styled(text.to_string(), style)];
    }
    // One span per character, so per-cell colour and per-cell motion compose:
    // a `rainbow+wave` message keeps its colours and gains the travelling
    // crest.
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            let mut st = style;
            if rainbow {
                st = st.fg(RAINBOW[i % RAINBOW.len()]);
            }
            if let Some(f) = frame {
                if styles.wave && wave_crest(i, f) {
                    st = st.add_modifier(Modifier::BOLD);
                }
            }
            Span::styled(c.to_string(), st)
        })
        .collect()
}

/// Combining marks that sit *above* a glyph.
///
/// Split from the other two by where they land, not for tidiness: see
/// [`corrupt_spans`] for why a glyph takes at most one from each set.
const ABOVE: [char; 20] = [
    '\u{0300}', // grave
    '\u{0301}', // acute
    '\u{0302}', // circumflex
    '\u{0303}', // tilde
    '\u{0304}', // macron
    '\u{0306}', // breve
    '\u{0307}', // dot above
    '\u{0308}', // diaeresis
    '\u{0309}', // hook above
    '\u{030a}', // ring above
    '\u{030b}', // double acute
    '\u{030c}', // caron
    '\u{030d}', // vertical line above
    '\u{030f}', // double grave
    '\u{0310}', // candrabindu
    '\u{0311}', // inverted breve
    '\u{0313}', // comma above
    '\u{033d}', // x above
    '\u{033e}', // vertical tilde
    '\u{0350}', // right arrowhead above
];

/// Combining marks that sit *below* a glyph.
const BELOW: [char; 18] = [
    '\u{0316}', // grave below
    '\u{0317}', // acute below
    '\u{0318}', // left tack below
    '\u{031e}', // down tack below
    '\u{031f}', // plus sign below
    '\u{0320}', // minus sign below
    '\u{0323}', // dot below
    '\u{0324}', // diaeresis below
    '\u{0325}', // ring below
    '\u{0327}', // cedilla
    '\u{0328}', // ogonek
    '\u{032a}', // bridge below
    '\u{032c}', // caron below
    '\u{032d}', // circumflex below
    '\u{032e}', // breve below
    '\u{0330}', // tilde below
    '\u{0331}', // macron below
    '\u{0353}', // x below
];

/// Combining marks drawn *through* a glyph.
///
/// The ones that read as genuine corruption rather than as an accent, which is
/// why they are a category of their own and why a hit can take one on top of
/// anything else.
const THROUGH: [char; 5] = [
    '\u{0334}', // tilde overlay
    '\u{0335}', // short stroke overlay
    '\u{0336}', // long stroke overlay
    '\u{0337}', // short solidus overlay
    '\u{0338}', // long solidus overlay
];

/// Hang combining marks off the characters `glitch` is corrupting this frame.
///
/// **Must run last, after every pass that indexes the row.** Inline-markdown
/// marks are held per character and `super::chat::highlight_runs` measures
/// `@mention` runs in bytes, both against the row as written; adding characters
/// before either would slide them. Applied to already-styled spans, appending
/// to a character rather than inserting one, so every original glyph keeps the
/// style it was given.
///
/// A corrupted glyph takes **at most one mark from each of [`ABOVE`], [`BELOW`]
/// and [`THROUGH`]**, so at most three, and never two from the same set. That
/// bound is the whole design. Marks in one set stack *vertically*: two above a
/// glyph draw one atop the other and reach up into the row above, which in a
/// terminal grid means overwriting the previous message. One from each set
/// reaches exactly as far as a single mark does, so the corruption can get
/// visibly deeper without ever leaving its own row.
///
/// Every mark is zero-width, so the row's *column* count is unchanged however
/// many land, nothing reflows, and the pane's height maths still holds.
///
/// Every frame corrupts at least one character, so a short message glitches as
/// visibly as a long one; see the body for why that is not left to chance.
///
/// The one real cost: a terminal that does not compose combining marks draws
/// them as separate cells, and the row gets wider than the layout budgeted.
/// Every terminal that can draw the inline images this client also renders does
/// compose them, so it is a narrow risk.
#[must_use]
pub fn corrupt_spans(spans: Vec<Span<'static>>, frame: usize) -> Vec<Span<'static>> {
    // One character in five is hit, which is a good rate for a row of prose and
    // a bad one for a short row: at that rate a three-character message renders
    // completely clean about half of all frames, so a glitched "hi" would sit
    // still. Pick one character up front that is hit regardless. On a long row
    // it is almost always one that would have been hit anyway; on a short one
    // it is the difference between an effect and a twitch.
    let live: usize = spans
        .iter()
        .flat_map(|s| s.content.chars())
        .filter(|c| !c.is_whitespace())
        .count();
    if live == 0 {
        return spans;
    }
    let forced = (noise(0, frame, 0x77) % live as u64) as usize;
    let mut at = 0usize;
    let mut nth = 0usize;
    spans
        .into_iter()
        .map(|span| {
            let style = span.style;
            let mut out = String::with_capacity(span.content.len());
            for ch in span.content.chars() {
                out.push(ch);
                // Never decorate whitespace: a mark floating in a gap between
                // words reads as a rendering fault rather than as corruption.
                if !ch.is_whitespace() {
                    if glitch_hit(at, frame) || nth == forced {
                        corrupt_one(&mut out, at, frame);
                    }
                    nth += 1;
                }
                at += 1;
            }
            Span::styled(out, style)
        })
        .collect()
}

/// Append this frame's marks for the character at `at`, which has been hit.
///
/// Each set fires on its own roll, so most hits take a single mark and a few
/// take two or three. Weighted so the light case dominates: an evenly spread
/// three-mark corruption reads as a font problem, while an occasional deep one
/// among many shallow ones reads as decay.
///
/// A hit that rolled nothing still takes a mark from [`ABOVE`], because a hit
/// that renders identically to a miss would just thin the effect out.
fn corrupt_one(out: &mut String, at: usize, frame: usize) {
    let pick = |set: &[char], salt: u64| set[(noise(at, frame, salt) % set.len() as u64) as usize];
    let above = noise(at, frame, 0x11) % 10 < 7;
    let below = noise(at, frame, 0x22) % 10 < 4;
    let through = noise(at, frame, 0x33) % 10 < 2;
    if above || !(below || through) {
        out.push(pick(&ABOVE, 0x44));
    }
    if below {
        out.push(pick(&BELOW, 0x55));
    }
    if through {
        out.push(pick(&THROUGH, 0x66));
    }
}

/// A deterministic value for `(character, frame, salt)`.
///
/// Deterministic so the same frame always renders identically and a redraw for
/// any other reason does not reshuffle the message. Salted so the several
/// independent choices a corrupted glyph needs do not correlate, which they
/// would if they all read the same number.
fn noise(i: usize, frame: usize, salt: u64) -> u64 {
    let mut h = (i as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((frame as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(salt.wrapping_mul(0x94D0_49BB_1331_11EB));
    // A round of avalanche, so neighbouring characters do not land on
    // neighbouring marks and draw a visible gradient along the row.
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h
}

/// Distance between wave crests, in characters.
const WAVE_SPACING: usize = 5;

/// Whether character `i` is under the wave's crest at `frame`.
///
/// The crest travels along the row and bounces at the end, so the effect reads
/// as a swell moving through the text.
fn wave_crest(i: usize, frame: usize) -> bool {
    let period = 2 * (WAVE_SPACING - 1);
    let t = frame % period;
    let phase = if t > WAVE_SPACING - 1 { period - t } else { t };
    i % WAVE_SPACING == phase
}

/// Whether character `i` is corrupted this `frame`.
///
/// Deterministic in `(i, frame)`, so the same frame always renders identically
/// and a redraw for any other reason does not reshuffle the message.
fn glitch_hit(i: usize, frame: usize) -> bool {
    noise(i, frame, 0) % 5 == 0
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
fn decorate(styles: TextStyles, base: Style, theme: &Theme, animating: bool) -> Style {
    let mut style = base;
    if styles.quiet {
        style = style.add_modifier(Modifier::DIM);
    }
    if styles.slow {
        style = style.add_modifier(Modifier::DIM | Modifier::ITALIC);
    }
    // Static stand-ins, drawn only when there is no clock. While animating the
    // real effect is applied per cell, and leaving the stand-in underneath is
    // what made the two styles look alike.
    if styles.wave && !animating {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if styles.glitch && !animating {
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
    use unicode_width::UnicodeWidthChar;

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
    fn only_blink_wave_and_glitch_animate() {
        for (name, want) in [
            ("blink", true),
            ("wave", true),
            ("glitch", true),
            ("slow", false),
            ("rainbow", false),
            ("quiet", false),
            ("l33t", false),
        ] {
            let st = TextStyles::from_message(one(name).as_ref());
            assert_eq!(st.animates(), want, "{name}");
        }
    }

    #[test]
    fn wave_and_glitch_are_different_kinds_of_effect() {
        // The owner's report from a live run: they were indistinguishable, one
        // just had a highlighted background. They now differ in kind, not
        // degree: a wave restyles cells and leaves the text alone; a glitch
        // corrupts the glyphs and leaves the styling alone.
        let theme = Theme::default();
        let text = "the quick brown fox jumps";
        let wave = TextStyles::from_message(one("wave").as_ref());
        let glitch = TextStyles::from_message(one("glitch").as_ref());

        let wave_spans = styled_spans_at(text, wave, false, theme.base(), &theme, Some(0));
        assert_eq!(text_of(&wave_spans), text, "a wave does not touch the text");
        assert!(
            wave_spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
            "it moves a bright crest through the row instead",
        );

        // Corruption is a later pass, over already-styled spans; see
        // `corrupt_spans` for why it has to be last.
        let plain = styled_spans_at(text, glitch, false, theme.base(), &theme, Some(0));
        let corrupted = corrupt_spans(plain, 0);
        assert_ne!(
            text_of(&corrupted),
            text,
            "a glitch corrupts the glyphs themselves",
        );
        assert!(
            !corrupted
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::REVERSED)),
            "and does not fall back to inverting the row, which is the static \
             stand-in and is what made the two look alike",
        );
    }

    #[test]
    fn corruption_never_costs_a_column() {
        // The property that makes this safe to do at all: combining marks are
        // zero-width, so a corrupted row occupies exactly the columns the layout
        // budgeted and nothing reflows.
        let theme = Theme::default();
        let text = "the quick brown fox";
        let glitch = TextStyles::from_message(one("glitch").as_ref());
        let width = |s: &str| -> usize {
            s.chars()
                .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
                .sum()
        };
        for frame in 0..24 {
            let spans = styled_spans_at(text, glitch, false, theme.base(), &theme, Some(frame));
            let out = text_of(&corrupt_spans(spans, frame));
            assert_eq!(width(&out), width(text), "frame {frame} changed the width");
        }
    }

    #[test]
    fn a_glyph_never_takes_two_marks_from_the_same_set() {
        // The bound that lets marks stack at all. Two from ABOVE draw one atop
        // the other and reach into the row above, which in a terminal grid
        // overwrites the previous message. One from each set reaches exactly as
        // far as a single mark does.
        let theme = Theme::default();
        let text = "corruption spreads through the whole of the signal";
        let glitch = TextStyles::from_message(one("glitch").as_ref());
        let mut seen_stacked = false;
        for frame in 0..64 {
            let spans = styled_spans_at(text, glitch, false, theme.base(), &theme, Some(frame));
            let out = text_of(&corrupt_spans(spans, frame));
            let mut run: Vec<char> = Vec::new();
            let flush = |run: &Vec<char>, seen: &mut bool| {
                for set in [&ABOVE[..], &BELOW[..], &THROUGH[..]] {
                    let n = run.iter().filter(|c| set.contains(c)).count();
                    assert!(n <= 1, "frame {frame} stacked {n} from one set: {out:?}");
                }
                if run.len() > 1 {
                    *seen = true;
                }
            };
            for c in out.chars() {
                if is_combining(c) {
                    run.push(c);
                } else {
                    flush(&run, &mut seen_stacked);
                    run.clear();
                }
            }
            flush(&run, &mut seen_stacked);
        }
        assert!(
            seen_stacked,
            "and the weights should still produce stacked marks, or the sets \
             are pointless",
        );
    }

    #[test]
    fn a_corrupted_glyph_always_shows_it() {
        // A hit whose rolls all came up empty would render identically to a
        // miss, quietly thinning the effect below the rate `glitch_hit` sets.
        for at in 0..512usize {
            for frame in 0..8usize {
                if !glitch_hit(at, frame) {
                    continue;
                }
                let mut out = String::new();
                corrupt_one(&mut out, at, frame);
                assert!(!out.is_empty(), "hit at {at} frame {frame} drew nothing");
            }
        }
    }

    #[test]
    fn even_a_two_letter_message_glitches_every_frame() {
        // At one hit in five, a short row renders clean most frames and the
        // effect reads as a twitch. Every frame corrupts at least one
        // character, whatever the length.
        let theme = Theme::default();
        let glitch = TextStyles::from_message(one("glitch").as_ref());
        for text in ["h", "hi", "hey", "ok then", "a much longer line of prose"] {
            for frame in 0..64 {
                let spans = styled_spans_at(text, glitch, false, theme.base(), &theme, Some(frame));
                let out = text_of(&corrupt_spans(spans, frame));
                assert!(
                    out.chars().any(is_combining),
                    "{text:?} frame {frame} rendered clean",
                );
            }
        }
    }

    #[test]
    fn a_row_with_nothing_to_corrupt_is_left_alone() {
        // `blink` blanks a row to spaces on its off frames, and a glitched
        // blinking message hands that row straight to this. Forcing a hit would
        // have to pick a character, and there is none.
        let theme = Theme::default();
        let glitch = TextStyles::from_message(one("glitch").as_ref());
        for text in ["", "   "] {
            let spans = styled_spans_at(text, glitch, false, theme.base(), &theme, Some(3));
            let out = text_of(&corrupt_spans(spans, 3));
            assert_eq!(out, text, "{text:?} should come back untouched");
        }
    }

    #[test]
    fn corruption_leaves_whitespace_alone() {
        // A mark floating in the gap between words reads as a rendering fault
        // rather than as corruption.
        let theme = Theme::default();
        let glitch = TextStyles::from_message(one("glitch").as_ref());
        for frame in 0..24 {
            let spans = styled_spans_at(
                "a b c d e f g h",
                glitch,
                false,
                theme.base(),
                &theme,
                Some(frame),
            );
            let out = text_of(&corrupt_spans(spans, frame));
            let mut chars = out.chars().peekable();
            while let Some(c) = chars.next() {
                if c == ' ' {
                    if let Some(&next) = chars.peek() {
                        assert!(
                            !is_combining(next),
                            "frame {frame} decorated a space: {out:?}",
                        );
                    }
                }
            }
        }
    }

    /// Whether `c` is one of the marks `corrupt_spans` hangs off a glyph.
    fn is_combining(c: char) -> bool {
        ('\u{0300}'..='\u{036f}').contains(&c)
    }

    #[test]
    fn the_wave_crest_actually_travels() {
        // A crest that sat still would just be a highlight.
        let seen: std::collections::HashSet<Vec<bool>> = (0..8)
            .map(|f| (0..12).map(|i| wave_crest(i, f)).collect())
            .collect();
        assert!(
            seen.len() > 2,
            "the crest occupies different cells over time"
        );
    }

    #[test]
    fn a_wave_never_touches_the_text() {
        // The byte-length hazard is gone by construction now: nothing rewrites
        // the row, so `chat::highlight_runs`' byte ranges cannot slide.
        let theme = Theme::default();
        // Wave only: `glitch` deliberately adds zero-width marks, in a later
        // pass that runs after everything which indexes the row.
        let styles = TextStyles::from_message(one("wave").as_ref());
        let hostile = "hey @neo \u{df}\u{130}\u{131} stra\u{df}e caf\u{e9}";
        for frame in 0..24 {
            let rendered = text_of(&styled_spans_at(
                hostile,
                styles,
                false,
                theme.base(),
                &theme,
                Some(frame),
            ));
            assert_eq!(rendered, hostile, "frame {frame} rewrote the text");
        }
    }

    #[test]
    fn a_frame_never_changes_how_wide_a_row_is() {
        // The property that lets the pane animate without reflowing: the
        // animation only flips case and visibility, so every frame occupies
        // exactly the columns the layout was computed for.
        let theme = Theme::default();
        let styles = TextStyles::from_message(many(&["wave", "glitch", "blink"]).as_ref());
        let text = "Hello there, World";
        let base_width: usize = text.chars().map(|c| c.width().unwrap_or(0)).sum();
        for frame in 0..24 {
            let spans = styled_spans_at(text, styles, false, theme.base(), &theme, Some(frame));
            let rendered: String = spans.iter().map(|s| s.content.as_ref()).collect();
            let w: usize = rendered.chars().map(|c| c.width().unwrap_or(0)).sum();
            assert_eq!(w, base_width, "frame {frame} changed the row width");
        }
    }

    #[test]
    fn blink_alternates_between_showing_and_hiding() {
        let theme = Theme::default();
        let styles = TextStyles::from_message(one("blink").as_ref());
        let shown: Vec<bool> = (0..16)
            .map(|f| {
                let spans = styled_spans_at("hi", styles, false, theme.base(), &theme, Some(f));
                text_of(&spans).trim().is_empty()
            })
            .collect();
        assert!(
            shown.iter().any(|b| *b) && shown.iter().any(|b| !*b),
            "blink must actually alternate: {shown:?}",
        );
    }

    #[test]
    fn the_same_frame_always_renders_identically() {
        // Glitch is deterministic per (position, frame), so a redraw for some
        // other reason does not reshuffle the message under the reader.
        let theme = Theme::default();
        let styles = TextStyles::from_message(one("glitch").as_ref());
        let once = text_of(&styled_spans_at(
            "steady",
            styles,
            false,
            theme.base(),
            &theme,
            Some(7),
        ));
        let twice = text_of(&styled_spans_at(
            "steady",
            styles,
            false,
            theme.base(),
            &theme,
            Some(7),
        ));
        assert_eq!(once, twice);
    }

    #[test]
    fn without_a_frame_the_static_approximation_is_unchanged() {
        // What `animate_styles = false` keeps, and what a client that never
        // animates has always shown.
        let theme = Theme::default();
        let styles = TextStyles::from_message(many(&["wave", "glitch"]).as_ref());
        let spans = styled_spans_at("Hello", styles, false, theme.base(), &theme, None);
        assert_eq!(text_of(&spans), "Hello", "the text is untouched");
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
    fn no_style_the_font_names_and_unknown_names_all_decode_to_nothing() {
        // `comic` and `times` name font families, which a terminal cannot
        // honor, so they stay indistinguishable from an unrecognized name.
        // `l33t`, `flip` and `cursive` used to be in this list on the theory
        // that the server pre-applied them; it does not, and they now decode to
        // real flags (see the module doc and the substitution tests below).
        assert_eq!(TextStyles::from_message(None), TextStyles::default());
        for name in ["comic", "times", "sparkle"] {
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
        let spans = styled_spans_at("hello", styles, false, theme.base(), &theme, None);
        assert_eq!(text_of(&spans), "hello");
    }

    #[test]
    fn an_unknown_style_name_never_leaks_into_the_output() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("hologram").as_ref());
        let spans = styled_spans_at("hi there", styles, false, theme.base(), &theme, None);
        let text = text_of(&spans);
        assert_eq!(text, "hi there");
        assert!(!text.contains("hologram"), "the style name must not print");
    }

    #[test]
    fn l33t_substitutes_both_cases_and_leaves_the_rest() {
        let styles = TextStyles::from_message(one("l33t").as_ref());
        assert!(styles.l33t && styles.substitutes());
        assert_eq!(styles.transform("Ates oi"), "4735 01");
        // Punctuation, digits and non-ASCII are not in the table.
        assert_eq!(styles.transform("!? 9 \u{e9}"), "!? 9 \u{e9}");
    }

    #[test]
    fn flip_lowercases_reverses_and_maps() {
        let styles = TextStyles::from_message(one("flip").as_ref());
        assert!(styles.flip);
        // "Hi" -> lowercase "hi" -> map to "\u{265}\u{1d09}" -> reverse.
        assert_eq!(styles.transform("Hi"), "\u{1d09}\u{265}");
        // The self-mapping letters still reverse.
        assert_eq!(styles.transform("sox"), "xos");
    }

    #[test]
    fn cursive_maps_ascii_letters_and_aliases_the_block_holes() {
        let styles = TextStyles::from_message(one("cursive").as_ref());
        assert!(styles.cursive);
        // 'A' is the base of the block; 'a' is the lowercase base.
        assert_eq!(styles.transform("Aa"), "\u{1d49c}\u{1d4b6}");
        // Every hole aliases into Letterlike Symbols, not an unassigned point.
        assert_eq!(
            styles.transform("BEFHILMRego"),
            "\u{212c}\u{2130}\u{2131}\u{210b}\u{2110}\u{2112}\u{2133}\u{211b}\u{212f}\u{210a}\u{2134}"
        );
        // Non-letters pass through.
        assert_eq!(styles.transform("a1!"), "\u{1d4b6}1!");
    }

    #[test]
    fn every_script_substitution_lands_on_an_assigned_code_point() {
        let styles = TextStyles::from_message(one("cursive").as_ref());
        let alphabet: String = ('A'..='Z').chain('a'..='z').collect();
        for c in styles.transform(&alphabet).chars() {
            // The unassigned holes in the Mathematical Script block. Landing on
            // one of these means `to_script` did base+offset where it should
            // have aliased, which renders as tofu rather than a letter.
            let cp = u32::from(c);
            assert!(
                !matches!(
                    cp,
                    0x1D49D
                        | 0x1D4A0
                        | 0x1D4A1
                        | 0x1D4A3
                        | 0x1D4A4
                        | 0x1D4A7
                        | 0x1D4A8
                        | 0x1D4AD
                        | 0x1D4BA
                        | 0x1D4BC
                        | 0x1D4C4
                ),
                "U+{cp:04X} is an unassigned hole in Mathematical Script"
            );
        }
    }

    #[test]
    fn the_ornamental_two_and_unknown_names_never_substitute() {
        for name in ["comic", "times", "banana"] {
            let styles = TextStyles::from_message(one(name).as_ref());
            assert!(
                !styles.substitutes(),
                "{name} must not rewrite the text: it has no terminal equivalent"
            );
            assert_eq!(styles.transform("hello"), "hello");
        }
    }

    #[test]
    fn a_message_with_no_substitution_style_borrows_rather_than_copies() {
        let styles = TextStyles::from_message(one("rainbow").as_ref());
        assert!(matches!(
            styles.transform("hello"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn substitutions_chain_in_a_fixed_order() {
        let styles = TextStyles::from_message(many(&["l33t", "flip"]).as_ref());
        assert!(styles.l33t && styles.flip);
        // l33t first ("test" -> "7357"), then flip reverses. The digits have no
        // flip mapping, so they survive as themselves, reversed.
        assert_eq!(styles.transform("test"), "7537");
    }

    #[test]
    fn quiet_dims_the_row() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("quiet").as_ref());
        let spans = styled_spans_at("psst", styles, false, theme.base(), &theme, None);
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
            let spans = styled_spans_at("x", styles, false, theme.base(), &theme, None);
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
        let spans = styled_spans_at("both", styles, false, theme.base(), &theme, None);
        assert!(spans[0].style.add_modifier.contains(Modifier::DIM));
        assert!(spans[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn rainbow_colors_each_character_in_turn() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("rainbow").as_ref());
        let spans = styled_spans_at("abcdefg", styles, false, theme.base(), &theme, None);
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
        let spans = styled_spans_at("abc", styles, false, theme.base(), &theme, None);
        assert_eq!(spans.len(), 1, "collapses to one plain span");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[0].style.fg, Some(Color::Reset));
    }

    #[test]
    fn a_spoiler_is_masked_until_it_is_revealed() {
        let theme = Theme::cyber();
        let styles = TextStyles::from_message(one("spoiler").as_ref());

        let hidden = styled_spans_at("the butler", styles, false, theme.base(), &theme, None);
        let masked = text_of(&hidden);
        assert!(!masked.contains("butler"), "the text must not be readable");
        assert!(masked.chars().all(|c| c == SPOILER_GLYPH));

        let shown = styled_spans_at("the butler", styles, true, theme.base(), &theme, None);
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
            let masked = text_of(&styled_spans_at(
                text,
                styles,
                false,
                theme.base(),
                &theme,
                None,
            ));
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
            let spans = styled_spans_at("hello world", styles, false, theme.base(), &theme, None);
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

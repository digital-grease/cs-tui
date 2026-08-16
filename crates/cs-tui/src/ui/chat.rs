//! Shared chat-message rendering for cIRC and C-Mail (API v0.8.4).
//!
//! Both chat systems expand the same slash commands server-side, so both come
//! back with the same optional extras beyond `content` (§ Message fields), and
//! both want the same rendering: wrapped text, decoded art, a text style, an
//! attachment chip, the third-person action form, and cIRC's deletion
//! tombstone. That is built once here so the two screens cannot drift apart.
//!
//! What a body looks like, top to bottom:
//!
//! 1. the message text, wrapped to the pane, styled per [`super::styles`], or
//!    the decoded picture when the style is `art` (§ Commands),
//! 2. an 8-ball answer or a fortune, on its own highlighted row, but only when
//!    the text does not already contain it,
//! 3. one compact chip per attachment.
//!
//! A deleted message (cIRC only) renders as [`TOMBSTONE`] and nothing else,
//! which is what § Message fields asks for: "Render it as a tombstone rather
//! than as text".
//!
//! **Attachments are chips, not pictures.** An `imageUrl` renders as `[image]`,
//! a `gifUrl` as `[gif]`, an `audioAttachment` as `[♪ artist - title]`, each
//! carrying an OSC 8 hyperlink so the chip is clickable in terminals that
//! support it. Inline terminal graphics are deliberately not threaded into the
//! chat panes: a chat pane redraws on every keystroke and on every live
//! message, and image protocol overlays there would fight the message list for
//! the same cells. The audio chip feeds the existing jukebox player instead
//! (see [`open_action`]).
//!
//! **Text is never trusted to be printable.** Every span here goes through
//! ratatui, which strips control characters out of the text it draws, and the
//! only raw escape sequence written into the buffer is built by
//! [`super::hyperlink::osc8`], which strips control characters out of both the
//! URL and the label first.
use cs_api::{AudioAttachment, CircMessage, CmailMessage, MessageExtras};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::art;
use super::audio::JukeboxTrack;
use super::hyperlink::osc8;
use super::styles::{self, TextStyles};
use super::theme::Theme;

/// The indent both chat screens put in front of every body row, so the text
/// lines up under the speaker's name.
pub const INDENT: &str = "  ";

/// What a deleted cIRC message renders as (§ Message fields).
///
/// The wire `content` of a deleted message is the literal `[DELETED]` and every
/// other field is stripped, so there is nothing to render but a marker.
pub const TOMBSTONE: &str = "⌫ message deleted";

/// The chip label for an attached image.
const IMAGE_CHIP: &str = "[image]";

/// The chip label for an attached GIF.
const GIF_CHIP: &str = "[gif]";

/// Fallback title for a jukebox chip whose track named neither artist nor
/// title, matching [`super::audio`]'s card.
const UNTITLED_TRACK: &str = "jukebox";

/// One chat message, borrowed from either wire type.
///
/// [`cs_api::CircMessage`] and [`cs_api::CmailMessage`] carry the same
/// [`MessageExtras`] but name their author differently, so this is the small
/// shared view the renderer works from.
#[derive(Debug, Clone, Copy)]
pub struct ChatMessage<'a> {
    /// The author, used for the `* username action` form.
    pub username: &'a str,
    /// The raw wire `content`. Pass it through unchanged; the rules about empty
    /// and duplicated content are applied here.
    pub content: &'a str,
    /// The optional attachment, style and command fields.
    pub extras: &'a MessageExtras,
}

impl<'a> ChatMessage<'a> {
    /// Build a view from parts, for a caller that has them separately (an
    /// optimistic outgoing message, say, or a test).
    ///
    /// ```ignore
    /// let msg = chat::ChatMessage::new("neo", "hello", &extras);
    /// ```
    #[must_use]
    pub fn new(username: &'a str, content: &'a str, extras: &'a MessageExtras) -> Self {
        Self {
            username,
            content,
            extras,
        }
    }
}

impl<'a> From<&'a CircMessage> for ChatMessage<'a> {
    fn from(m: &'a CircMessage) -> Self {
        Self::new(&m.username, &m.content, &m.extras)
    }
}

impl<'a> From<&'a CmailMessage> for ChatMessage<'a> {
    fn from(m: &'a CmailMessage) -> Self {
        Self::new(&m.sender_username, &m.content, &m.extras)
    }
}

/// How a message body is laid out in a pane.
///
/// `width` is the column budget for the text *after* the indent, so a caller
/// sizes it the way cIRC already does: the pane width less the list's highlight
/// gutter and less the indent.
#[derive(Debug, Clone, Copy)]
pub struct BodyLayout<'a> {
    /// Put in front of every body row. [`INDENT`] unless a screen wants its own.
    pub indent: &'a str,
    /// Columns available to the text itself.
    pub width: usize,
    /// Whether the reader has revealed this message's spoiler. Owned by the
    /// screen, since it is per-message reader state and not part of the message.
    pub revealed: bool,
    /// Hard cap on body rows, when the screen has one.
    ///
    /// ratatui's `List` cannot render an item taller than its viewport: it
    /// paints NOTHING at all, not a clipped top, and settles its offset onto the
    /// offending item so the pane stays blank on later frames too. One
    /// over-tall message therefore hides the entire conversation, not just
    /// itself. A decoded `/art` picture reaches 25 lines on an 80 column canvas
    /// (§ Commands), which wraps to about 50 rows in a standard terminal, so
    /// this is routine rather than exotic.
    ///
    /// Capping here rather than at the render site is what keeps
    /// [`body_height`] and [`body_lines`] agreeing, since both derive from
    /// [`body_rows`].
    pub max_rows: Option<usize>,
}

impl<'a> BodyLayout<'a> {
    /// A layout `width` columns wide, with the standard [`INDENT`] and nothing
    /// revealed.
    ///
    /// ```ignore
    /// let layout = chat::BodyLayout::new(body_width);
    /// ```
    #[must_use]
    pub fn new(width: usize) -> Self {
        Self {
            indent: INDENT,
            width,
            revealed: false,
            max_rows: None,
        }
    }

    /// Cap the body at `rows` rows, replacing the overflow with a marker.
    ///
    /// A screen drawing into a `List` must set this to the pane's height, or a
    /// single tall message blanks the whole pane. See [`BodyLayout::max_rows`].
    ///
    /// ```ignore
    /// let layout = chat::BodyLayout::new(width).with_max_rows(pane_rows);
    /// ```
    #[must_use]
    pub fn with_max_rows(mut self, rows: usize) -> Self {
        self.max_rows = Some(rows);
        self
    }

    /// Mark this message's spoiler as revealed.
    ///
    /// ```ignore
    /// let layout = chat::BodyLayout::new(width).with_revealed(self.revealed.contains(&m.id));
    /// ```
    #[must_use]
    pub fn with_revealed(mut self, revealed: bool) -> Self {
        self.revealed = revealed;
        self
    }
}

/// A chip drawn under a message, and the URL it links to.
///
/// Produced by [`message_chips`] and consumed by [`apply_chip_links`]. `label`
/// is the chip exactly as it was drawn, including any truncation, because the
/// link overlay finds the chip on screen by matching that text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChipLink {
    /// The visible chip text, e.g. `[image]`.
    pub label: String,
    /// The link target.
    pub url: String,
}

/// What the "open" key should do for a message.
///
/// One value rather than two lookups, so both chat screens make the same
/// decision: a track plays, anything else opens in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAction {
    /// Hand this track to the jukebox player.
    Play(JukeboxTrack),
    /// Open this URL with the desktop handler.
    Open(String),
    /// The message carries nothing openable.
    None,
}

/// The body rows of a message, before any styling is applied.
///
/// [`body_lines`] and [`body_height`] both derive from this, so a message can
/// never be measured at one height and drawn at another.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BodyRow {
    /// A wrapped row of message text.
    Text(String),
    /// A hard-wrapped row of decoded `/art`.
    Art(String),
    /// A wrapped row of the `* username action` form.
    Action(String),
    /// An 8-ball answer or a fortune, surfaced on its own row.
    Highlight(String),
    /// An attachment chip.
    Chip(ChipLink),
    /// The deletion tombstone.
    Tombstone,
    /// Stands in for the rows a [`BodyLayout::max_rows`] cap removed, carrying
    /// how many they were, so a clipped message says so instead of just ending.
    Truncated(usize),
}

/// Render a message body into styled lines, indent included.
///
/// Returns no lines at all for a message with neither text nor attachments,
/// which is legal on the wire; the caller's own header row still stands.
///
/// ```ignore
/// let lines = chat::body_lines(m.into(), chat::BodyLayout::new(body_width), theme);
/// ```
#[must_use]
pub fn body_lines(
    msg: ChatMessage<'_>,
    layout: BodyLayout<'_>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let text_styles = TextStyles::from_message(msg.extras.style.as_ref());
    let base = theme.base();
    let action = action_style(msg.extras, theme);
    body_rows(msg, layout)
        .into_iter()
        .map(|row| {
            let mut spans = vec![Span::styled(layout.indent.to_string(), base)];
            match row {
                BodyRow::Text(text) | BodyRow::Art(text) => spans.extend(styles::styled_spans(
                    &text,
                    text_styles,
                    layout.revealed,
                    base,
                    theme,
                )),
                BodyRow::Action(text) => spans.extend(styles::styled_spans(
                    &text,
                    text_styles,
                    layout.revealed,
                    action,
                    theme,
                )),
                BodyRow::Highlight(text) => spans.push(Span::styled(text, theme.accent_style())),
                BodyRow::Chip(chip) => spans.push(Span::styled(chip.label, chip_style(theme))),
                BodyRow::Tombstone => spans.push(Span::styled(
                    TOMBSTONE.to_string(),
                    theme.muted_style().add_modifier(Modifier::ITALIC),
                )),
                BodyRow::Truncated(n) => spans.push(Span::styled(
                    format!("… {n} more lines, too tall for this pane"),
                    theme.muted_style().add_modifier(Modifier::ITALIC),
                )),
            }
            Line::from(spans)
        })
        .collect()
}

/// How many rows [`body_lines`] will produce for the same arguments.
///
/// Saturates rather than wrapping, so a pathologically tall message clips
/// instead of measuring as short.
///
/// ```ignore
/// let rows = chat::body_height(m.into(), chat::BodyLayout::new(body_width));
/// ```
#[must_use]
pub fn body_height(msg: ChatMessage<'_>, layout: BodyLayout<'_>) -> u16 {
    u16::try_from(body_rows(msg, layout).len()).unwrap_or(u16::MAX)
}

/// Rendered height of a whole message: the caller's `header_rows` plus its body.
///
/// cIRC and C-Mail both draw one header row (name, admin star, timestamp), so
/// both pass 1. A screen that folds its header into the `* username action`
/// form for a message with `extras.is_action` set passes 0 for those.
///
/// ```ignore
/// let height = chat::message_height(m.into(), chat::BodyLayout::new(body_width), 1);
/// ```
#[must_use]
pub fn message_height(msg: ChatMessage<'_>, layout: BodyLayout<'_>, header_rows: u16) -> u16 {
    header_rows.saturating_add(body_height(msg, layout))
}

/// The chips [`body_lines`] will draw for this message, in the order it draws
/// them.
///
/// Hand these to [`apply_chip_links`] after the pane is rendered. Empty for a
/// deleted message, which renders only its tombstone.
///
/// ```ignore
/// let chips = chat::message_chips(m.into(), chat::BodyLayout::new(body_width));
/// ```
#[must_use]
pub fn message_chips(msg: ChatMessage<'_>, layout: BodyLayout<'_>) -> Vec<ChipLink> {
    // Derived from the same rows the renderer draws, so the two cannot disagree
    // about which chips exist. That matters because [`apply_chip_links`] pairs
    // the chips it is given against the chip runs it finds on screen: a chip
    // listed here but cut by a `max_rows` cap, or suppressed by a tombstone,
    // would shift every later chip onto another message's URL.
    body_rows(msg, layout)
        .into_iter()
        .filter_map(|row| match row {
            BodyRow::Chip(chip) => Some(chip),
            _ => None,
        })
        .collect()
}

/// The chips of a run of messages, in the order they are drawn.
///
/// The pane-wide list [`apply_chip_links`] expects: pass every message the pane
/// holds, in display order.
///
/// ```ignore
/// let chips = chat::collect_chips(messages.items.iter().map(Into::into), layout);
/// ```
#[must_use]
pub fn collect_chips<'a>(
    messages: impl IntoIterator<Item = ChatMessage<'a>>,
    layout: BodyLayout<'_>,
) -> Vec<ChipLink> {
    messages
        .into_iter()
        .flat_map(|msg| message_chips(msg, layout))
        .collect()
}

/// Overlay OSC 8 hyperlinks onto the chips drawn in `area`, returning how many
/// were linked.
///
/// Call it after the message pane has been rendered, passing the chips produced
/// by the messages the pane actually drew, starting at the list's scroll offset
/// and in display order. Screens cannot compute the exact screen row of a chip
/// (the list widget owns its scroll), so chips are located by matching their
/// label in the buffer and then paired with `chips` TOP DOWN.
///
/// Top down is what makes a scrolled pane safe. The list draws from its offset
/// downwards and truncates whatever does not fit, so the visible chips are a
/// PREFIX of the chips belonging to the messages from that offset on. Pairing
/// from the bottom instead would assume the visible chips are a suffix of the
/// whole thread, which only holds while the pane sits at the very bottom: scroll
/// up one row and every chip would take the URL of a later message's attachment.
///
/// Fails safe twice over. A run only counts as a chip if the buffer drew it in
/// [`chip_style`], so body text that merely reads `[image]` cannot masquerade as
/// one and shift the pairing. And if the labels stop agreeing the walk stops
/// rather than pointing a chip at another message's attachment. A chip whose
/// label a wrapped row cut short, or that contains a double-width glyph the
/// buffer pads, will not match and is left as plain text.
///
/// ```ignore
/// chat::apply_chip_links(frame.buffer_mut(), messages_area, &chips, theme);
/// ```
pub fn apply_chip_links(buf: &mut Buffer, area: Rect, chips: &[ChipLink], theme: &Theme) -> usize {
    if chips.is_empty() {
        return 0;
    }
    let runs = find_chip_runs(buf, area, chips, chip_style(theme));
    let mut linked = 0;
    for (chip, run) in chips.iter().zip(runs.iter()) {
        if run.label != chip.label {
            break;
        }
        if linkify_chip(buf, run, &chip.url) {
            linked += 1;
        }
    }
    linked
}

/// What the "open" key should do for a message: play its track, open its
/// picture, or nothing.
///
/// A track wins over a picture, since a message that carries both is really a
/// jukebox post.
///
/// ```ignore
/// match chat::open_action(&m.extras) {
///     chat::OpenAction::Play(track) => CircIntent::PlayJukebox(Some(track)),
///     chat::OpenAction::Open(url) => CircIntent::OpenUrl(url),
///     chat::OpenAction::None => CircIntent::None,
/// }
/// ```
#[must_use]
pub fn open_action(extras: &MessageExtras) -> OpenAction {
    if let Some(track) = jukebox_track(extras) {
        return OpenAction::Play(track);
    }
    match attachment_url(extras) {
        Some(url) => OpenAction::Open(url),
        None => OpenAction::None,
    }
}

/// The picture a message links to: its image, else its GIF.
///
/// ```ignore
/// let url = chat::attachment_url(&m.extras);
/// ```
#[must_use]
pub fn attachment_url(extras: &MessageExtras) -> Option<String> {
    [extras.image_url.as_deref(), extras.gif_url.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|url| !url.is_empty())
        .map(str::to_string)
}

/// The jukebox track a message carries, ready for the player.
///
/// `None` when there is no `audioAttachment` or it names no source, so a blank
/// track never reaches the now-playing bar.
///
/// ```ignore
/// let track = chat::jukebox_track(&m.extras);
/// ```
#[must_use]
pub fn jukebox_track(extras: &MessageExtras) -> Option<JukeboxTrack> {
    let audio = extras.audio_attachment.as_ref()?;
    let url = audio.src.trim();
    if url.is_empty() {
        return None;
    }
    Some(JukeboxTrack {
        url: url.to_string(),
        artist: audio.artist.trim().to_string(),
        title: audio.title.trim().to_string(),
    })
}

/// Whether the message is hidden behind a spoiler, so the reveal key has
/// something to do.
///
/// ```ignore
/// let can_reveal = chat::has_spoiler(&m.extras);
/// ```
#[must_use]
pub fn has_spoiler(extras: &MessageExtras) -> bool {
    TextStyles::from_message(extras.style.as_ref()).spoiler
}

/// The message's text as a one-line preview would want it: the same text the
/// body renders, with `/art` decoded first.
///
/// § Message fields is explicit that `style: "art"` means `content` is
/// base64 and must be decoded before display, so any caller previewing a
/// message (a status line, a confirmation prompt) has to decode it too, or it
/// puts a base64 blob on screen where the reader is trying to identify which
/// message they picked. Sharing this with [`body_lines`] is what keeps the two
/// from drifting.
///
/// Returns `None` when the message has no text to show, which per
/// `display_content` covers both an empty body and a caption that merely
/// repeats the attachment URL.
///
/// ```ignore
/// let preview = one_line_preview(&chat::preview_text(&m.extras, &m.content).unwrap_or_default(), 40);
/// ```
#[must_use]
pub fn preview_text(extras: &MessageExtras, content: &str) -> Option<String> {
    let text = extras.display_content(content)?;
    if TextStyles::from_message(extras.style.as_ref()).art {
        Some(art::decode_art(text))
    } else {
        Some(text.to_string())
    }
}

/// A one-line summary of a message for a list row, never empty.
///
/// [`preview_text`] answers `None` for a message whose attachment IS the whole
/// message, which is a blank row in a conversation list rather than useful
/// information. This falls back to the attachment's chip label, and renders a
/// deleted message as the [`TOMBSTONE`] instead of the literal `[DELETED]` the
/// wire carries.
///
/// ```ignore
/// let row = one_line_preview(&chat::summary_text(&m.extras, &m.content), 48);
/// ```
#[must_use]
pub fn summary_text(extras: &MessageExtras, content: &str) -> String {
    if extras.deleted {
        return TOMBSTONE.to_string();
    }
    if let Some(text) = preview_text(extras, content) {
        if !text.trim().is_empty() {
            return text;
        }
    }
    chips_of(extras, usize::MAX)
        .first()
        .map(|c| c.label.clone())
        .unwrap_or_default()
}

/// Display width of `c` in terminal columns, counting an unprintable character
/// as zero.
///
/// ```ignore
/// let cols = chat::char_width('世');
/// ```
#[must_use]
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Word-wrap `content` to `width` display columns (unicode-width aware). Words
/// longer than a line are hard-broken; embedded newlines start a new line;
/// always returns at least one (possibly empty) line.
///
/// Promoted from the cIRC screen so C-Mail wraps message bodies the same way.
///
/// ```ignore
/// let rows = chat::word_wrap(&m.content, body_width);
/// ```
#[must_use]
pub fn word_wrap(content: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    for para in content.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0usize;
        for word in para.split_whitespace() {
            let ww: usize = word.chars().map(char_width).sum();
            let sep = usize::from(!cur.is_empty());
            if cur_w + sep + ww <= width {
                if sep == 1 {
                    cur.push(' ');
                }
                cur.push_str(word);
                cur_w += sep + ww;
                continue;
            }
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            if ww <= width {
                cur.push_str(word);
                cur_w = ww;
            } else {
                // A word wider than the whole line: hard-break it by columns.
                for ch in word.chars() {
                    let cw = char_width(ch);
                    if cur_w + cw > width && !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                        cur_w = 0;
                    }
                    cur.push(ch);
                    cur_w += cw;
                }
            }
        }
        // Push the trailing line (empty for a blank paragraph) to preserve breaks.
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Break `content` at exactly `width` display columns without touching its
/// spaces.
///
/// The wrapper for `/art`: § Commands says leading spaces are the picture, and
/// [`word_wrap`] would collapse every run of them. Embedded newlines still
/// start a new line, and the result always has at least one (possibly empty)
/// line.
///
/// ```ignore
/// let rows = chat::hard_wrap(&art::decode_art(&m.content), body_width);
/// ```
#[must_use]
pub fn hard_wrap(content: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    for para in content.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0usize;
        for ch in para.chars() {
            let cw = char_width(ch);
            if cur_w + cw > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur.push(ch);
            cur_w += cw;
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Cut `s` down to `width` display columns, marking the cut with an ellipsis.
///
/// Used on chip labels so a long track title cannot push a chip past the pane
/// and out of reach of the link overlay. The list previews need exactly the
/// same cut, so this is [`super::text::truncate_to_width`] under a name the
/// chat primitives can be read against.
///
/// ```ignore
/// let label = chat::truncate_to_width("[♪ a very long title]", 12);
/// ```
#[must_use]
pub fn truncate_to_width(s: &str, width: usize) -> String {
    super::text::truncate_to_width(s, width)
}

/// Lay a message out into rows, applying the § Message fields rules for empty,
/// duplicated, action and deleted content.
fn body_rows(msg: ChatMessage<'_>, layout: BodyLayout<'_>) -> Vec<BodyRow> {
    if msg.extras.deleted {
        return vec![BodyRow::Tombstone];
    }
    let width = layout.width.max(1);
    let text_styles = TextStyles::from_message(msg.extras.style.as_ref());
    let mut rows = Vec::new();

    // `display_content` carries the spec's two content rules: the text may be
    // empty because the attachment is the whole message, and a message posted
    // from the website may repeat the attachment URL as its caption.
    if let Some(text) = msg.extras.display_content(msg.content) {
        if text_styles.art {
            rows.extend(
                hard_wrap(&art::decode_art(text), width)
                    .into_iter()
                    .map(BodyRow::Art),
            );
        } else if msg.extras.is_action {
            let line = format!("* {} {}", author_of(msg.username), text.trim());
            rows.extend(word_wrap(&line, width).into_iter().map(BodyRow::Action));
        } else {
            rows.extend(word_wrap(text, width).into_iter().map(BodyRow::Text));
        }
    }

    for highlight in highlights(msg) {
        rows.extend(
            word_wrap(&highlight, width)
                .into_iter()
                .map(BodyRow::Highlight),
        );
    }
    rows.extend(chips_of(msg.extras, width).into_iter().map(BodyRow::Chip));

    // Last, so the cap counts every row the message actually produced.
    if let Some(max) = layout.max_rows {
        let max = max.max(1);
        if rows.len() > max {
            let hidden = rows.len() - max + 1;
            rows.truncate(max - 1);
            rows.push(BodyRow::Truncated(hidden));
        }
    }
    rows
}

/// The 8-ball answer and fortune worth surfacing on their own row.
///
/// § Message fields offers both "on its own for clients that want to highlight
/// it", but the expanded command usually leaves them in `content` too, so one
/// that is already in the text is skipped rather than printed twice.
fn highlights(msg: ChatMessage<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |flag: bool, label: &str, value: Option<&String>| {
        let Some(value) = value.map(|v| v.trim()).filter(|v| !v.is_empty()) else {
            return;
        };
        if flag && !msg.content.contains(value) {
            out.push(format!("{label}: {value}"));
        }
    };
    push(
        msg.extras.is_eightball,
        "8-ball",
        msg.extras.eightball_answer.as_ref(),
    );
    push(
        msg.extras.is_fortune,
        "fortune",
        msg.extras.fortune_text.as_ref(),
    );
    out
}

/// The attachment chips a message carries, already truncated to `width`.
fn chips_of(extras: &MessageExtras, width: usize) -> Vec<ChipLink> {
    let mut chips = Vec::new();
    for (label, url) in [
        (IMAGE_CHIP, extras.image_url.as_deref()),
        (GIF_CHIP, extras.gif_url.as_deref()),
    ] {
        if let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) {
            chips.push(ChipLink {
                label: truncate_to_width(label, width),
                url: url.to_string(),
            });
        }
    }
    if let Some(audio) = extras.audio_attachment.as_ref() {
        let url = audio.src.trim();
        if !url.is_empty() {
            chips.push(ChipLink {
                label: truncate_to_width(&audio_chip_label(audio), width),
                url: url.to_string(),
            });
        }
    }
    chips
}

/// The label of a jukebox chip: `[♪ artist - title · genre]`, dropping whatever
/// the track did not name.
fn audio_chip_label(audio: &AudioAttachment) -> String {
    let artist = audio.artist.trim();
    let title = audio.title.trim();
    let mut label = match (artist.is_empty(), title.is_empty()) {
        (false, false) => format!("♪ {artist} - {title}"),
        (false, true) => format!("♪ {artist}"),
        (true, false) => format!("♪ {title}"),
        (true, true) => format!("♪ {UNTITLED_TRACK}"),
    };
    if let Some(genre) = audio
        .genre
        .as_deref()
        .map(str::trim)
        .filter(|g| !g.is_empty())
    {
        label.push_str(" · ");
        label.push_str(genre);
    }
    format!("[{label}]")
}

/// The author's name, or a placeholder when the server sent none, so an action
/// never renders as `*  waves`.
fn author_of(username: &str) -> &str {
    let name = username.trim();
    if name.is_empty() {
        "?"
    } else {
        name
    }
}

/// Style for the `* username action` form: italic, and accented when the action
/// was a dice roll, an 8-ball or a fortune, which § Message fields flags
/// precisely so a client can pick them out.
fn action_style(extras: &MessageExtras, theme: &Theme) -> Style {
    let base = theme.base().add_modifier(Modifier::ITALIC);
    if extras.is_dice || extras.is_eightball || extras.is_fortune {
        base.fg(theme.accent)
    } else {
        base
    }
}

/// Style for an attachment chip: the content accent, underlined so it reads as
/// something to click even where OSC 8 is not supported.
fn chip_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::UNDERLINED)
}

/// One occurrence of a chip label found on screen.
#[derive(Debug, Clone)]
struct ChipRun {
    x: u16,
    y: u16,
    cells: u16,
    label: String,
}

/// Every occurrence of any chip's label inside `area` that was drawn in
/// `style`, in draw order (rows top to bottom, columns left to right).
///
/// The style check is what separates a real chip from body text that happens to
/// read like one: a message whose content is literally `[image]` would otherwise
/// register as a run and shift every subsequent chip onto the wrong URL.
fn find_chip_runs(buf: &Buffer, area: Rect, chips: &[ChipLink], style: Style) -> Vec<ChipRun> {
    let mut labels: Vec<&str> = chips.iter().map(|c| c.label.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();

    let bounds = buf.area;
    let x0 = area.x.max(bounds.x);
    let y0 = area.y.max(bounds.y);
    let x1 = area
        .x
        .saturating_add(area.width)
        .min(bounds.x.saturating_add(bounds.width));
    let y1 = area
        .y
        .saturating_add(area.height)
        .min(bounds.y.saturating_add(bounds.height));

    let mut out = Vec::new();
    for y in y0..y1 {
        let mut x = x0;
        while x < x1 {
            // Compare the attributes that define a chip rather than the whole
            // Style: a painted cell reports concrete colors where `chip_style`
            // leaves fields unset, so a straight equality never holds. The
            // selected row recolors the background only, so the foreground and
            // the underline both survive selection.
            let styled_as_chip = buf.cell((x, y)).is_some_and(|c| {
                let fg_matches = match style.fg {
                    Some(fg) => c.fg == fg,
                    None => true,
                };
                fg_matches && c.modifier.contains(style.add_modifier)
            });
            let hit = if styled_as_chip {
                labels.iter().find_map(|label| {
                    match_label_at(buf, x, y, x1, label).map(|cells| (cells, *label))
                })
            } else {
                None
            };
            match hit {
                Some((cells, label)) => {
                    out.push(ChipRun {
                        x,
                        y,
                        cells,
                        label: label.to_string(),
                    });
                    x = x.saturating_add(cells.max(1));
                }
                None => x += 1,
            }
        }
    }
    out
}

/// How many cells `label` occupies starting at (`x`, `y`), or `None` if it is
/// not drawn there.
fn match_label_at(buf: &Buffer, x: u16, y: u16, limit: u16, label: &str) -> Option<u16> {
    let mut seen = String::new();
    let mut cx = x;
    while cx < limit && seen.len() < label.len() {
        seen.push_str(buf.cell((cx, y))?.symbol());
        cx += 1;
    }
    if seen == label {
        Some(cx - x)
    } else {
        None
    }
}

/// Turn one found chip run into an OSC 8 hyperlink.
///
/// The whole `open, label, close` triple goes into the run's first cell and the
/// rest of the run is flagged `skip`, which is the arrangement
/// [`super::hyperlink`] documents: the escape sequence stays atomic, so a
/// partial redraw can never leave a link open over unrelated cells.
fn linkify_chip(buf: &mut Buffer, run: &ChipRun, url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || run.cells == 0 {
        return false;
    }
    let sequence = osc8(url, &run.label);
    let Some(cell) = buf.cell_mut((run.x, run.y)) else {
        return false;
    };
    cell.set_symbol(&sequence);
    for cx in (run.x + 1)..run.x.saturating_add(run.cells) {
        if let Some(cell) = buf.cell_mut((cx, run.y)) {
            cell.set_skip(true);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use cs_api::MessageStyle;
    use ratatui::style::Color;

    fn extras() -> MessageExtras {
        MessageExtras::default()
    }

    fn styled(name: &str) -> MessageExtras {
        MessageExtras {
            style: Some(MessageStyle::One(name.to_string())),
            ..MessageExtras::default()
        }
    }

    fn track(artist: &str, title: &str, genre: Option<&str>) -> AudioAttachment {
        AudioAttachment {
            src: "https://youtu.be/dQw4w9WgXcQ".into(),
            origin: "youtube".into(),
            artist: artist.into(),
            title: title.into(),
            genre: genre.map(str::to_string),
        }
    }

    fn layout(width: usize) -> BodyLayout<'static> {
        BodyLayout::new(width)
    }

    /// The rendered text of each body row, indent included.
    fn rows(msg: ChatMessage<'_>, width: usize) -> Vec<String> {
        let theme = Theme::cyber();
        body_lines(msg, layout(width), &theme)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn word_wrap_breaks_words_newlines_and_long_tokens() {
        // Behaviour promoted verbatim from the cIRC screen.
        assert_eq!(
            word_wrap("the quick brown fox", 9),
            vec!["the quick", "brown fox"],
        );
        assert_eq!(word_wrap("a\n\nb", 10), vec!["a", "", "b"]);
        assert_eq!(word_wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(word_wrap("", 5), vec![String::new()]);
    }

    #[test]
    fn word_wrap_measures_cjk_and_emoji_by_display_width() {
        // Each CJK ideograph is two columns wide, so four of them fill a
        // four-column line two at a time, not four at a time.
        assert_eq!(word_wrap("你好世界", 4), vec!["你好", "世界"]);
        assert_eq!(word_wrap("你好世界", 5), vec!["你好", "世界"]);
        // Emoji are two columns as well.
        assert_eq!(word_wrap("👍👍👍", 4), vec!["👍👍", "👍"]);
        // A wide word that does fit is packed with a narrow one.
        assert_eq!(word_wrap("hi 世界", 8), vec!["hi 世界"]);
        assert_eq!(word_wrap("hi 世界", 6), vec!["hi", "世界"]);
    }

    #[test]
    fn char_width_counts_columns_not_characters() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('世'), 2);
        assert_eq!(char_width('\u{200b}'), 0, "zero-width space");
    }

    #[test]
    fn hard_wrap_preserves_runs_of_spaces() {
        // § Commands: leading spaces are the picture, so art must not be
        // reflowed the way prose is.
        assert_eq!(hard_wrap("    /\\", 10), vec!["    /\\"]);
        assert_eq!(hard_wrap("a    b", 10), vec!["a    b"]);
        // Compare with the prose wrapper, which collapses them.
        assert_eq!(word_wrap("a    b", 10), vec!["a b"]);
    }

    #[test]
    fn hard_wrap_breaks_at_the_column_budget_and_keeps_blank_lines() {
        assert_eq!(hard_wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(hard_wrap("a\n\nb", 4), vec!["a", "", "b"]);
        assert_eq!(hard_wrap("你好世界", 4), vec!["你好", "世界"]);
        assert_eq!(hard_wrap("", 4), vec![String::new()]);
    }

    #[test]
    fn truncate_to_width_counts_columns_and_marks_the_cut() {
        assert_eq!(truncate_to_width("[image]", 20), "[image]");
        assert_eq!(truncate_to_width("[image]", 4), "[im…");
        // A wide glyph that would straddle the budget is dropped whole.
        assert_eq!(truncate_to_width("你好世界", 5), "你好…");
        assert_eq!(truncate_to_width("[image]", 0), "");
    }

    #[test]
    fn a_plain_message_wraps_with_the_indent() {
        let e = extras();
        let msg = ChatMessage::new("neo", "the quick brown fox", &e);
        assert_eq!(rows(msg, 9), vec!["  the quick", "  brown fox"]);
    }

    #[test]
    fn body_height_always_matches_the_rows_drawn() {
        let theme = Theme::cyber();
        let audio = MessageExtras {
            audio_attachment: Some(track("Art of Noise", "Paranoimia", Some("electronic"))),
            ..MessageExtras::default()
        };
        let art_extras = styled("art");
        let deleted = MessageExtras {
            deleted: true,
            ..MessageExtras::default()
        };
        let picture = STANDARD.encode(" /\\_/\\\n( o.o )");
        let cases: Vec<(&MessageExtras, &str)> = vec![
            (&audio, "listen to this"),
            (&art_extras, picture.as_str()),
            (&deleted, "[DELETED]"),
            (&audio, ""),
        ];
        for (e, content) in cases {
            let msg = ChatMessage::new("neo", content, e);
            let drawn = body_lines(msg, layout(24), &theme).len();
            assert_eq!(
                usize::from(body_height(msg, layout(24))),
                drawn,
                "height disagreed with the render for {content:?}",
            );
        }
    }

    #[test]
    fn message_height_adds_the_callers_header_rows() {
        let e = extras();
        let msg = ChatMessage::new("neo", "hello", &e);
        assert_eq!(message_height(msg, layout(20), 1), 2);
        assert_eq!(message_height(msg, layout(20), 0), 1);
    }

    #[test]
    fn an_attachment_with_no_caption_still_renders_as_its_chip() {
        // § Message fields: "content may be empty. An attachment can be the
        // entire message."
        let e = MessageExtras {
            image_url: Some("https://cdn.example/pic.png".into()),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "", &e);
        assert_eq!(rows(msg, 30), vec!["  [image]"]);
    }

    #[test]
    fn a_caption_that_duplicates_the_attachment_url_is_not_printed_twice() {
        let e = MessageExtras {
            gif_url: Some("https://cdn.example/a.gif".into()),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "https://cdn.example/a.gif", &e);
        assert_eq!(rows(msg, 40), vec!["  [gif]"]);

        // A real caption survives alongside the chip.
        let msg = ChatMessage::new("neo", "look at this", &e);
        assert_eq!(rows(msg, 40), vec!["  look at this", "  [gif]"]);
    }

    #[test]
    fn a_jukebox_chip_names_the_track_and_its_genre() {
        let e = MessageExtras {
            audio_attachment: Some(track("Art of Noise", "Paranoimia", Some("electronic"))),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "", &e);
        assert_eq!(
            rows(msg, 60),
            vec!["  [♪ Art of Noise - Paranoimia · electronic]"]
        );
    }

    #[test]
    fn a_jukebox_chip_degrades_when_the_track_is_unnamed() {
        for (artist, title, want) in [
            ("", "Paranoimia", "  [♪ Paranoimia]"),
            ("Art of Noise", "", "  [♪ Art of Noise]"),
            ("", "", "  [♪ jukebox]"),
        ] {
            let e = MessageExtras {
                audio_attachment: Some(track(artist, title, None)),
                ..MessageExtras::default()
            };
            let msg = ChatMessage::new("neo", "", &e);
            assert_eq!(rows(msg, 60), vec![want]);
        }
    }

    #[test]
    fn a_track_with_no_source_is_neither_a_chip_nor_playable() {
        let e = MessageExtras {
            audio_attachment: Some(AudioAttachment {
                src: "   ".into(),
                ..track("A", "T", None)
            }),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "hi", &e);
        assert_eq!(rows(msg, 40), vec!["  hi"]);
        assert_eq!(jukebox_track(&e), None);
        assert_eq!(open_action(&e), OpenAction::None);
    }

    #[test]
    fn a_deleted_message_renders_as_a_tombstone_and_nothing_else() {
        // § Message fields: content is `[DELETED]` and every other field is
        // gone, so the literal must never reach the reader.
        let e = MessageExtras {
            deleted: true,
            image_url: Some("https://cdn.example/pic.png".into()),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "[DELETED]", &e);
        let drawn = rows(msg, 40);
        assert_eq!(drawn, vec![format!("  {TOMBSTONE}")]);
        assert!(!drawn[0].contains("[DELETED]"));
        assert!(message_chips(msg, layout(40)).is_empty());
    }

    #[test]
    fn an_action_renders_in_the_third_person() {
        // § Message fields: "conventionally rendered as `* username content`".
        let e = MessageExtras {
            is_action: true,
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "waves", &e);
        assert_eq!(rows(msg, 40), vec!["  * neo waves"]);
    }

    #[test]
    fn an_action_from_a_nameless_sender_still_reads() {
        let e = MessageExtras {
            is_action: true,
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("  ", "waves", &e);
        assert_eq!(rows(msg, 40), vec!["  * ? waves"]);
    }

    #[test]
    fn dice_eightball_and_fortune_actions_are_accented() {
        let theme = Theme::cyber();
        for e in [
            MessageExtras {
                is_action: true,
                is_dice: true,
                ..MessageExtras::default()
            },
            MessageExtras {
                is_action: true,
                is_fortune: true,
                ..MessageExtras::default()
            },
        ] {
            let msg = ChatMessage::new("neo", "rolls 4d6", &e);
            let lines = body_lines(msg, layout(40), &theme);
            assert_eq!(lines[0].spans[1].style.fg, Some(theme.accent));
        }
        // A plain action keeps the body colour.
        let plain = MessageExtras {
            is_action: true,
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "waves", &plain);
        let lines = body_lines(msg, layout(40), &theme);
        assert_eq!(lines[0].spans[1].style.fg, Some(theme.foreground));
    }

    #[test]
    fn an_eightball_answer_is_surfaced_only_when_the_text_omits_it() {
        // Already in the content: no duplicate row.
        let e = MessageExtras {
            is_action: true,
            is_eightball: true,
            eightball_answer: Some("Ask again later".into()),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "asks the 8-ball: Ask again later", &e);
        assert_eq!(rows(msg, 60).len(), 1);

        // Missing from the content: surfaced on its own row.
        let msg = ChatMessage::new("neo", "asks the 8-ball", &e);
        assert_eq!(
            rows(msg, 60),
            vec!["  * neo asks the 8-ball", "  8-ball: Ask again later"],
        );
    }

    #[test]
    fn a_fortune_is_surfaced_only_when_the_text_omits_it() {
        let e = MessageExtras {
            is_action: true,
            is_fortune: true,
            fortune_text: Some("You will ship it".into()),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "cracks open a fortune cookie", &e);
        assert_eq!(
            rows(msg, 60),
            vec![
                "  * neo cracks open a fortune cookie",
                "  fortune: You will ship it",
            ],
        );
    }

    #[test]
    fn art_is_decoded_with_its_leading_spaces_intact() {
        // § Commands: the `/art` example, base64 as § Message fields describes.
        let picture = " /\\_/\\\n( o.o )\n > ^ <";
        let e = styled("art");
        let encoded = STANDARD.encode(picture);
        let msg = ChatMessage::new("neo", &encoded, &e);
        assert_eq!(
            rows(msg, 40),
            vec!["   /\\_/\\", "  ( o.o )", "   > ^ <"],
            "the indent is two spaces, then the picture's own leading space",
        );
    }

    #[test]
    fn undecodable_art_still_shows_something() {
        let e = styled("art");
        let msg = ChatMessage::new("neo", "not base64!!", &e);
        assert_eq!(rows(msg, 40), vec!["  not base64!!"]);
    }

    #[test]
    fn an_unknown_style_renders_as_plain_text() {
        let e = styled("hologram");
        let msg = ChatMessage::new("neo", "hello", &e);
        assert_eq!(rows(msg, 40), vec!["  hello"]);
    }

    #[test]
    fn an_unreadable_style_shape_never_prints_raw_json() {
        // MessageStyle::Other holds the JSON verbatim; none of it may surface.
        let e = MessageExtras {
            style: Some(MessageStyle::Other(serde_json::json!({"name": "rainbow"}))),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "hello", &e);
        let drawn = rows(msg, 40);
        assert_eq!(drawn, vec!["  hello"]);
        assert!(!drawn[0].contains('{'), "no JSON: {drawn:?}");
    }

    #[test]
    fn a_spoiler_is_hidden_until_the_screen_says_it_is_revealed() {
        let theme = Theme::cyber();
        let e = styled("spoiler");
        let msg = ChatMessage::new("neo", "the butler did it", &e);
        assert!(has_spoiler(&e));

        let hidden: String = body_lines(msg, layout(40), &theme)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(!hidden.contains("butler"), "{hidden:?}");

        let shown: String = body_lines(msg, layout(40).with_revealed(true), &theme)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(shown.contains("the butler did it"), "{shown:?}");
    }

    #[test]
    fn revealing_a_spoiler_does_not_change_the_height() {
        let e = styled("spoiler");
        let msg = ChatMessage::new("neo", "the butler did it in the library", &e);
        assert_eq!(
            body_height(msg, layout(12)),
            body_height(msg, layout(12).with_revealed(true)),
        );
    }

    #[test]
    fn a_rainbow_message_colours_each_character() {
        let theme = Theme::cyber();
        let e = styled("rainbow");
        let msg = ChatMessage::new("neo", "abc", &e);
        let lines = body_lines(msg, layout(40), &theme);
        // One span for the indent, then one per character.
        assert_eq!(lines[0].spans.len(), 4);
        let colours: Vec<Option<Color>> = lines[0].spans[1..].iter().map(|s| s.style.fg).collect();
        assert_eq!(colours.len(), 3);
        assert!(colours[0] != colours[1] && colours[1] != colours[2]);
    }

    #[test]
    fn the_chips_reported_are_the_chips_drawn() {
        // apply_chip_links finds chips by their label, so the two must agree.
        let e = MessageExtras {
            image_url: Some("https://cdn.example/pic.png".into()),
            audio_attachment: Some(track("Art of Noise", "Paranoimia", None)),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "both", &e);
        let drawn = rows(msg, 60);
        let chips = message_chips(msg, layout(60));
        assert_eq!(chips.len(), 2);
        for chip in &chips {
            assert!(
                drawn.iter().any(|row| row.contains(&chip.label)),
                "{:?} was not drawn in {drawn:?}",
                chip.label,
            );
        }
        assert_eq!(chips[0].url, "https://cdn.example/pic.png");
        assert_eq!(chips[1].url, "https://youtu.be/dQw4w9WgXcQ");
    }

    #[test]
    fn collect_chips_keeps_the_messages_in_display_order() {
        let first = MessageExtras {
            image_url: Some("https://cdn.example/1.png".into()),
            ..MessageExtras::default()
        };
        let second = MessageExtras {
            gif_url: Some("https://cdn.example/2.gif".into()),
            ..MessageExtras::default()
        };
        let chips = collect_chips(
            [
                ChatMessage::new("a", "", &first),
                ChatMessage::new("b", "", &second),
            ],
            layout(40),
        );
        assert_eq!(chips[0].url, "https://cdn.example/1.png");
        assert_eq!(chips[1].url, "https://cdn.example/2.gif");
    }

    #[test]
    fn open_action_plays_a_track_and_otherwise_opens_the_picture() {
        let audio = MessageExtras {
            audio_attachment: Some(track("Art of Noise", "Paranoimia", None)),
            image_url: Some("https://cdn.example/cover.png".into()),
            ..MessageExtras::default()
        };
        assert_eq!(
            open_action(&audio),
            OpenAction::Play(JukeboxTrack {
                url: "https://youtu.be/dQw4w9WgXcQ".into(),
                artist: "Art of Noise".into(),
                title: "Paranoimia".into(),
            }),
            "a track outranks a picture",
        );

        let gif = MessageExtras {
            gif_url: Some("https://cdn.example/a.gif".into()),
            ..MessageExtras::default()
        };
        assert_eq!(
            open_action(&gif),
            OpenAction::Open("https://cdn.example/a.gif".into()),
        );
        assert_eq!(open_action(&extras()), OpenAction::None);
    }

    #[test]
    fn attachment_url_prefers_the_image_over_the_gif() {
        let e = MessageExtras {
            image_url: Some(" https://cdn.example/pic.png ".into()),
            gif_url: Some("https://cdn.example/a.gif".into()),
            ..MessageExtras::default()
        };
        assert_eq!(
            attachment_url(&e).as_deref(),
            Some("https://cdn.example/pic.png"),
            "and the URL is trimmed",
        );
        assert_eq!(attachment_url(&extras()), None);
    }

    #[test]
    fn the_wire_types_both_convert_into_a_chat_message() {
        let circ = CircMessage {
            username: "neo".into(),
            content: "hi".into(),
            ..CircMessage::default()
        };
        let view: ChatMessage<'_> = (&circ).into();
        assert_eq!((view.username, view.content), ("neo", "hi"));

        let cmail = CmailMessage {
            sender_username: "trinity".into(),
            content: "yo".into(),
            ..CmailMessage::default()
        };
        let view: ChatMessage<'_> = (&cmail).into();
        assert_eq!((view.username, view.content), ("trinity", "yo"));
    }

    /// Paint `rows` into a buffer of `width` columns, one string per row, as
    /// ordinary body text.
    fn painted(rows: &[&str], width: u16) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, rows.len() as u16));
        for (y, row) in rows.iter().enumerate() {
            buf.set_string(0, y as u16, row, Style::default());
        }
        buf
    }

    /// Paint `rows` the way the renderer paints a chip: the leading indent stays
    /// plain and the label itself carries [`chip_style`], which is what
    /// [`find_chip_runs`] keys on.
    fn painted_as_chips(rows: &[&str], width: u16) -> Buffer {
        let theme = Theme::cyber();
        let mut buf = Buffer::empty(Rect::new(0, 0, width, rows.len() as u16));
        for (y, row) in rows.iter().enumerate() {
            let indent = row.len() - row.trim_start().len();
            buf.set_string(0, y as u16, &row[..indent], Style::default());
            buf.set_string(
                indent as u16,
                y as u16,
                row.trim_start(),
                chip_style(&theme),
            );
        }
        buf
    }

    fn chip(label: &str, url: &str) -> ChipLink {
        ChipLink {
            label: label.to_string(),
            url: url.to_string(),
        }
    }

    #[test]
    fn apply_chip_links_wraps_the_chip_and_skips_its_trailing_cells() {
        let mut buf = painted_as_chips(&["  hello", "  [image]"], 20);
        let area = buf.area;
        let chips = vec![chip(IMAGE_CHIP, "https://cdn.example/pic.png")];
        assert_eq!(apply_chip_links(&mut buf, area, &chips, &Theme::cyber()), 1);

        let first = buf.cell((2, 1)).unwrap().symbol().to_string();
        assert_eq!(
            first,
            osc8("https://cdn.example/pic.png", IMAGE_CHIP),
            "the first cell carries the whole atomic sequence",
        );
        for x in 3..(2 + IMAGE_CHIP.len() as u16) {
            assert!(buf.cell((x, 1)).unwrap().skip, "cell {x} is skipped");
        }
        assert!(!buf.cell((1, 1)).unwrap().skip, "the indent is untouched");
    }

    #[test]
    fn apply_chip_links_matches_each_chip_to_its_own_url() {
        let mut buf = painted_as_chips(&["  [image]", "  [gif]", "  [image]"], 20);
        let area = buf.area;
        let chips = vec![
            chip(IMAGE_CHIP, "https://cdn.example/1.png"),
            chip(GIF_CHIP, "https://cdn.example/a.gif"),
            chip(IMAGE_CHIP, "https://cdn.example/2.png"),
        ];
        assert_eq!(apply_chip_links(&mut buf, area, &chips, &Theme::cyber()), 3);
        for (y, url) in [
            (0, "https://cdn.example/1.png"),
            (1, "https://cdn.example/a.gif"),
            (2, "https://cdn.example/2.png"),
        ] {
            let cell = buf.cell((2, y)).unwrap().symbol().to_string();
            assert!(cell.contains(url), "row {y} should link {url}: {cell:?}");
        }
    }

    #[test]
    fn a_body_is_capped_so_it_can_never_blank_its_pane() {
        // ratatui's List paints NOTHING for an item taller than the viewport,
        // so one tall message would hide the entire conversation. A decoded
        // /art picture (80x25 canvas, § Commands) reaches that height on a
        // normal terminal, which is what makes this routine.
        let extras = MessageExtras::default();
        let long = (0..60)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let msg = ChatMessage::new("neo", &long, &extras);

        let capped = BodyLayout::new(40).with_max_rows(10);
        let height = body_height(msg, capped);
        assert!(height <= 10, "body must fit the cap, got {height}");
        assert_eq!(
            body_lines(msg, capped, &Theme::cyber()).len(),
            usize::from(height),
            "measured and drawn heights must agree",
        );

        let rendered: String = body_lines(msg, capped, &Theme::cyber())
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            rendered.contains("more lines"),
            "a clipped message must say so: {rendered:?}",
        );

        // Uncapped is still uncapped, so screens that do not draw into a List
        // are unaffected.
        assert!(body_height(msg, BodyLayout::new(40)) > 10);
    }

    #[test]
    fn a_chip_cut_by_the_cap_is_not_offered_for_linking() {
        // apply_chip_links pairs the chips it is given against the runs it finds
        // on screen. A chip listed but not drawn would shift every later chip
        // onto another message's URL.
        let extras = MessageExtras {
            image_url: Some("https://cdn.example/pic.png".into()),
            ..MessageExtras::default()
        };
        let long = (0..60)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let msg = ChatMessage::new("neo", &long, &extras);

        assert!(
            message_chips(msg, BodyLayout::new(40)).len() == 1,
            "uncapped, the chip is there",
        );
        assert!(
            message_chips(msg, BodyLayout::new(40).with_max_rows(5)).is_empty(),
            "capped away, it must not be offered",
        );
    }

    #[test]
    fn summary_text_never_renders_a_blank_or_raw_row() {
        // A list row that shows nothing at all, or the wire's literal
        // "[DELETED]", or base64, is worse than useless.
        let attachment_only = MessageExtras {
            image_url: Some("https://cdn.example/pic.png".into()),
            ..MessageExtras::default()
        };
        assert_eq!(summary_text(&attachment_only, ""), IMAGE_CHIP);

        // § Message fields: a website post may repeat the URL as its caption.
        assert_eq!(
            summary_text(&attachment_only, "https://cdn.example/pic.png"),
            IMAGE_CHIP,
            "the URL must not be printed as if it were a caption",
        );

        let deleted = MessageExtras {
            deleted: true,
            ..MessageExtras::default()
        };
        assert_eq!(summary_text(&deleted, "[DELETED]"), TOMBSTONE);

        let plain = MessageExtras::default();
        assert_eq!(summary_text(&plain, "hello"), "hello");
    }

    #[test]
    fn apply_chip_links_pairs_from_the_top_so_a_scrolled_pane_stays_correct() {
        // The caller hands over the chips of the messages the list drew, starting
        // at its scroll offset, so the visible chips are a PREFIX of that slice
        // and the bottom row may be cut off. Pairing from the bottom instead
        // would give row 0 the URL of a message further down the thread.
        let mut buf = painted_as_chips(&["  [image]", "  [image]"], 20);
        let area = buf.area;
        let chips = vec![
            chip(IMAGE_CHIP, "https://cdn.example/1.png"),
            chip(IMAGE_CHIP, "https://cdn.example/2.png"),
            chip(IMAGE_CHIP, "https://cdn.example/cut-off.png"),
        ];
        assert_eq!(apply_chip_links(&mut buf, area, &chips, &Theme::cyber()), 2);
        assert!(buf
            .cell((2, 0))
            .unwrap()
            .symbol()
            .contains("https://cdn.example/1.png"));
        assert!(buf
            .cell((2, 1))
            .unwrap()
            .symbol()
            .contains("https://cdn.example/2.png"));
    }

    #[test]
    fn apply_chip_links_ignores_body_text_that_merely_looks_like_a_chip() {
        // A message whose content is literally "[image]" is drawn as plain body
        // text. Counting it as a run would shift the real chip below it onto the
        // wrong URL, so the style check has to reject it.
        let theme = Theme::cyber();
        let mut buf = painted(&["  [image]"], 20);
        buf.set_string(0, 0, "  ", Style::default());
        let area = buf.area;
        let chips = vec![chip(IMAGE_CHIP, "https://cdn.example/real.png")];
        assert_eq!(apply_chip_links(&mut buf, area, &chips, &theme), 0);
        assert_eq!(
            buf.cell((2, 0)).unwrap().symbol(),
            "[",
            "plain text is left exactly as drawn",
        );
    }

    #[test]
    fn apply_chip_links_stops_rather_than_linking_the_wrong_url() {
        // The buffer disagrees with the chip list, so nothing may be linked to a
        // target the reader cannot see.
        let mut buf = painted_as_chips(&["  [gif]"], 20);
        let area = buf.area;
        let chips = vec![chip(IMAGE_CHIP, "https://cdn.example/pic.png")];
        assert_eq!(apply_chip_links(&mut buf, area, &chips, &Theme::cyber()), 0);
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "[");
    }

    #[test]
    fn apply_chip_links_ignores_a_chip_outside_the_pane() {
        let mut buf = painted_as_chips(&["  [image]", "  [image]"], 20);
        // Only the first row is the message pane.
        let area = Rect::new(0, 0, 20, 1);
        let chips = vec![chip(IMAGE_CHIP, "https://cdn.example/1.png")];
        assert_eq!(apply_chip_links(&mut buf, area, &chips, &Theme::cyber()), 1);
        assert_eq!(
            buf.cell((2, 1)).unwrap().symbol(),
            "[",
            "the row below the pane is untouched",
        );
    }

    #[test]
    fn apply_chip_links_is_a_noop_without_chips_or_with_an_empty_url() {
        let theme = Theme::cyber();
        let mut buf = painted_as_chips(&["  [image]"], 20);
        let area = buf.area;
        assert_eq!(apply_chip_links(&mut buf, area, &[], &theme), 0);
        assert_eq!(
            apply_chip_links(&mut buf, area, &[chip(IMAGE_CHIP, "   ")], &theme),
            0
        );
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "[");
    }

    #[test]
    fn a_message_pane_renders_end_to_end() {
        // The whole path through a real terminal buffer: wrapped text, a chip,
        // and the hyperlink overlay on top.
        let theme = Theme::cyber();
        let e = MessageExtras {
            image_url: Some("https://cdn.example/pic.png".into()),
            ..MessageExtras::default()
        };
        let msg = ChatMessage::new("neo", "look at this thing I found", &e);
        let width = 20u16;
        let body = layout(usize::from(width) - INDENT.len());
        let lines = body_lines(msg, body, &theme);
        let chips = message_chips(msg, body);

        let backend = ratatui::backend::TestBackend::new(width, lines.len() as u16);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(ratatui::widgets::Paragraph::new(lines.clone()), area);
                apply_chip_links(f.buffer_mut(), area, &chips, &theme);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let text: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("look at this"), "{text:?}");
        assert!(
            text.contains("\u{1b}]8;;https://cdn.example/pic.png\u{1b}\\"),
            "the chip must carry an OSC 8 link: {text:?}",
        );
    }
}

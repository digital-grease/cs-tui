//! The always-on inline chat composer, shared by cIRC and C-Mail.
//!
//! Both chat screens put the same strip under the conversation: a prompt, the
//! draft, and a caret. A message is long enough to wrap, so the strip grows
//! downwards as the draft outgrows the pane and scrolls once it has taken every
//! row it may — never running off the right edge, which is the one failure the
//! reader cannot see happening. Building it once here is what keeps the two
//! screens from drifting apart, the same rule [`super::chat`] follows for the
//! message bodies themselves.
//!
//! cIRC owns a [`Composer`] outright (its room input is focused the whole time,
//! so it carries the caret keys); C-Mail keeps a plain draft string and lays it
//! out with the caret at the end. Both go through [`layout`] and [`lines`], so
//! both wrap and scroll identically.
//!
//! **A message may hold line breaks** (v0.9.1 § Send a Message, both surfaces),
//! so a newline in the draft starts a new row here exactly as it will in the
//! conversation once it is sent. [`is_newline_key`] is how one gets typed, and
//! it is shared so the two screens cannot bind different keys for it.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::editor::Segment;
use super::theme::Theme;

/// Whether `key` puts a line break into the draft instead of sending it.
///
/// Enter sends, so a line break needs a key of its own, and no single key
/// reaches the client in every terminal:
///
/// - `Alt+Enter`, the usual chord for this in chat clients. Unix terminals send
///   it as Esc then a carriage return. Windows Terminal, WezTerm and mintty
///   keep it for full screen by default, and on macOS Option is not Alt unless
///   the terminal is set to use it as Meta.
/// - `Ctrl+J`, a bare line feed, which crossterm reports as `Ctrl+J` once the
///   terminal is in raw mode. On Windows crossterm rebuilds the letter from the
///   keyboard layout and cases it by Shift and Caps Lock, so `J` counts too.
///   `Ctrl+Alt+J` does not: Windows reports AltGr as Ctrl+Alt.
/// - Enter with Ctrl or Shift held, for a terminal that reports the modifier.
///   Windows does, and so does a Unix terminal set up to send a distinct
///   sequence for them. Most Unix terminals send both as a plain Enter, which
///   still sends: there is nothing to tell the two apart by.
pub(super) fn is_newline_key(key: &KeyEvent) -> bool {
    let mods = key.modifiers;
    match key.code {
        KeyCode::Enter => {
            mods.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        }
        KeyCode::Char('j' | 'J') => {
            mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT)
        }
        _ => false,
    }
}

/// Most rows the composer may grow to for `draft`.
///
/// [`MAX_ROWS`] for ordinary text, which soft-wraps and then scrolls. A draft
/// with line breaks gets a row per line, up to [`cs_api::MAX_MESSAGE_LINES`],
/// so a message can be read back whole before it is sent. That matters most in
/// C-Mail, whose caret stays at the end and whose arrows scroll the thread: a
/// row the composer does not show there cannot be reached at all. The screen's
/// own budget still applies on top, so the conversation is never squeezed out.
pub(super) fn max_rows(draft: &str) -> u16 {
    let lines = draft.split('\n').count().min(cs_api::MAX_MESSAGE_LINES);
    MAX_ROWS.max(u16::try_from(lines).unwrap_or(MAX_ROWS))
}

/// The warning to raise instead of sending `content`, when it is longer than a
/// message may be.
///
/// [`cs_api::MAX_MESSAGE_LEN`] is a hard limit, and the API crate refuses a
/// longer message itself. Left to that, though, the refusal arrives as a failed
/// send: the draft has already been cleared into the outgoing strip, which then
/// offers `Ctrl+R` to retry a message that can never go. Checked here first, the
/// reader gets told the limit and how far over it they are, and the draft stays
/// in the composer to be cut down.
///
/// Measured exactly as the API crate measures it ([`cs_api::message_len`]), so
/// the two can never disagree about a draft. That is UTF-16 units, in which
/// most emoji count as two, and the warning says so when the draft holds any:
/// otherwise the count it gives would not match the characters on screen.
/// Applies to everything, `/art` and text styles included: the length limit
/// has no exemptions.
///
/// The wording is kept short because it is shown in a toast, which is only as
/// wide as the terminal.
pub(super) fn length_limit_notice(content: &str) -> Option<String> {
    let len = cs_api::message_len(content);
    let max = cs_api::MAX_MESSAGE_LEN;
    (len > max).then(|| {
        let over = len - max;
        let doubled = if len == content.chars().count() {
            ""
        } else {
            " (emoji count as 2)"
        };
        format!("message too long: {len} of {max} characters{doubled}, cut {over}")
    })
}

/// What to tell the reader instead of sending `content`, when it holds more
/// lines than a message keeps.
///
/// A message keeps [`cs_api::MAX_MESSAGE_LINES`] (v0.9.1 § Send a Message), but
/// the server does not refuse a longer one: it joins every line past the tenth
/// onto the tenth with spaces and posts that. Sending it anyway would put up
/// something other than what was typed without a word, which is the reason an
/// unknown command is refused locally too (see [`super::commands`]). Callers
/// keep the draft on a refusal, for the same reason they do there: so it can
/// be fixed rather than retyped.
///
/// `content` is what will be sent, already trimmed. In a cIRC room a
/// text-styled message is exempt, since there its line breaks all become spaces
/// anyway (§ Send a Message, cIRC). C-Mail's § Send a Message says no such
/// thing, so a styled C-Mail message is held to the rule like any other. `/art`
/// is exempt as well, but that is the cIRC caller's check: the picture has a
/// 25-line canvas of its own (§ Commands), and only cIRC has it.
pub(super) fn line_limit_notice(
    content: &str,
    surface: super::commands::Surface,
) -> Option<String> {
    if surface == super::commands::Surface::Circ && super::commands::is_styled(content) {
        return None;
    }
    let lines = cs_api::message_line_count(content);
    let max = cs_api::MAX_MESSAGE_LINES;
    (lines > max).then(|| format!("too many lines: {lines} of {max}, lines past {max} get joined"))
}

/// Most rows the composer may grow to as a long draft soft-wraps. Past this it
/// scrolls to keep the caret in view instead: the conversation above it is the
/// point of the screen, and `Ctrl+E` opens a whole editor for a long message.
/// A draft with line breaks may grow further, one row per line (see
/// [`max_rows`]).
pub(super) const MAX_ROWS: u16 = 4;

/// Rows of conversation the composer may never take, however long the draft is.
pub(super) const MIN_CHAT_ROWS: u16 = 3;

/// The composer's prompt, and the gutter every wrapped continuation row indents
/// by so the draft stays in one column. Both are two cells wide.
pub(super) const PROMPT: &str = "› ";
const GUTTER: &str = "  ";
/// Marker for the top row when the draft has grown past [`MAX_ROWS`] and rows
/// have scrolled off above it.
const MORE: &str = "… ";

/// A draft with a caret inside it: cIRC's composer, which owns both.
///
/// Every other text field in the client is a short value that scrolls sideways
/// under [`super::input::windowed_line`]. A chat composer is neither: a message
/// is long enough to wrap, and it is the field you live in while a cIRC room is
/// open, so it gets the caret keys (`←`/`→`, Home/End, Delete) and soft-wraps
/// downwards instead of running off the right edge. C-Mail's composer keeps a
/// plain string instead (its arrows scroll the thread), and lays out through
/// [`layout`] all the same.
#[derive(Debug, Default)]
pub(super) struct Composer {
    /// The draft so far. It may hold newlines: typed with [`is_newline_key`],
    /// pasted, or handed back by the `Ctrl+E` editor, which `/art` needs.
    pub(super) text: String,
    /// Caret as a char index into `text` (`0..=` its char count).
    pub(super) cursor: usize,
}

impl Composer {
    /// Characters typed so far (the caret is a char index, never a byte offset).
    pub(super) fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Replace the whole draft, leaving the caret at the end: a prefill (the
    /// editor handing its content back) is something you keep typing after.
    pub(super) fn set(&mut self, text: String) {
        self.text = text;
        self.cursor = self.len();
    }

    pub(super) fn insert(&mut self, c: char) {
        let at = super::input::byte_index(&self.text, self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// Insert text at the caret, which lands just after it. Line breaks
    /// survive, unlike in the single-line fields, and arrive as `\n` however the
    /// terminal sent them (see [`super::input::normalize_line_breaks`]).
    ///
    /// One insert for the whole string rather than one per character: a paste
    /// can be large, and re-finding the byte offset for every character made a
    /// 100,000-character paste freeze the screen for seconds.
    pub(super) fn insert_str(&mut self, text: &str) {
        let text = super::input::normalize_line_breaks(text);
        let at = super::input::byte_index(&self.text, self.cursor);
        self.text.insert_str(at, &text);
        self.cursor += text.chars().count();
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = super::input::byte_index(&self.text, self.cursor - 1);
        self.text.remove(at);
        self.cursor -= 1;
    }

    pub(super) fn delete(&mut self) {
        if self.cursor < self.len() {
            let at = super::input::byte_index(&self.text, self.cursor);
            self.text.remove(at);
        }
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.len());
    }

    pub(super) fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub(super) fn move_end(&mut self) {
        self.cursor = self.len();
    }
}

/// A draft laid out for the composer strip: the wrapped rows, which of them are
/// on screen, and where the caret landed.
///
/// Derived from `(draft, width, cap)` every frame and never stored, so a resize
/// is correct by construction — the same rule [`super::editor`] follows.
pub(super) struct View {
    /// The draft as display characters (see [`display`]).
    chars: Vec<char>,
    /// Every row, as a range into `chars`: each line of the draft soft-wrapped
    /// on its own (see [`wrap_lines`]). No range ever covers a newline, so a
    /// newline is never drawn.
    segs: Vec<Segment>,
    /// The caret's index among `chars`, and the row holding it.
    caret: usize,
    caret_row: usize,
    /// First visible row, and how many rows are visible (`1..=cap`).
    first: usize,
    pub(super) rows: usize,
    /// Index in `chars` where the mention ghost begins, when one is showing.
    ///
    /// Everything from here on is a preview, not text: it is styled as such,
    /// and it is not in the draft. Building it into `chars` rather than
    /// appending it after the fact is what lets the ghost take part in wrapping
    /// and scrolling like anything else, instead of needing its own width math.
    ghost_from: Option<usize>,
}

/// The draft as the characters the composer actually draws, plus where the caret
/// sits among them.
///
/// Newlines stay in as themselves. Until v0.9.1 a message could not keep one,
/// so the composer drew each as an inline `⏎` marker on a single flowing line;
/// now a line break is part of what gets posted (§ Send a Message), and the
/// draft shows it the way the conversation will. [`wrap_lines`] breaks the row
/// at each one.
fn display(text: &str, cursor: usize, ghost: &str) -> (Vec<char>, usize, Option<usize>) {
    let mut chars: Vec<char> = text.chars().collect();
    let caret = cursor.min(chars.len());
    // The ghost sits exactly at the caret, so the caret cell lands on its first
    // character. Deliberate: the preview then reads as a continuation of what
    // you are typing rather than something parked after a gap. A handle holds
    // no newline, but one is filtered anyway: a ghost that broke the row would
    // be a preview of text that could not be typed by completing it.
    let ghost_from = (!ghost.is_empty()).then(|| {
        let tail: Vec<char> = chars.split_off(caret);
        chars.extend(ghost.chars().filter(|&c| c != '\n'));
        chars.extend(tail);
        caret
    });
    (chars, caret, ghost_from)
}

/// Soft-wrap each line of `chars` to `width` on its own, as ranges into the
/// whole of `chars`, and find the row the caret is on.
///
/// A newline closes the last row of its line and belongs to no row, so a draft
/// ending in one has a final empty row for the caret to sit on, which is where
/// the next character will go. A caret just before a newline sits at the end of
/// that line, not at the start of the next.
fn wrap_lines(chars: &[char], width: usize, caret: usize) -> (Vec<Segment>, usize) {
    let mut segs = Vec::new();
    let mut caret_row = 0;
    let mut start = 0;
    loop {
        let end = chars[start..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |i| start + i);
        let line = &chars[start..end];
        let line_segs = super::editor::wrap_line(line, width);
        if (start..=end).contains(&caret) {
            let (k, _) = super::editor::caret_in_line(line, &line_segs, caret - start);
            caret_row = segs.len() + k;
        }
        segs.extend(line_segs.into_iter().map(|s| Segment {
            start: s.start + start,
            end: s.end + start,
        }));
        if end == chars.len() {
            return (segs, caret_row);
        }
        start = end + 1;
    }
}

/// Wrap `text` to `width` content columns, showing at most `cap` rows, with the
/// caret at char index `cursor`.
///
/// A draft that outgrows `cap` scrolls rather than growing further, and the
/// window always holds the caret: it rides the bottom row while you type, and
/// Home takes both it and the window back to the start.
///
/// Takes the draft as `(text, cursor)` rather than as a [`Composer`] so a screen
/// that keeps its draft as a plain string (C-Mail) lays out through exactly the
/// same code, with the caret at the end.
pub(super) fn layout(text: &str, cursor: usize, width: usize, cap: usize, ghost: &str) -> View {
    let cap = cap.max(1);
    let (chars, caret, ghost_from) = display(text, cursor, ghost);
    // `wrap_line` always yields at least one segment per line, so `segs` is
    // never empty.
    let (segs, caret_row) = wrap_lines(&chars, width.max(1), caret);
    let rows = segs.len().min(cap);
    let first = caret_row
        .saturating_sub(rows - 1)
        .min(segs.len().saturating_sub(rows));
    View {
        chars,
        segs,
        caret,
        caret_row,
        first,
        rows,
        ghost_from,
    }
}

/// Content columns the draft has, once the prompt gutter is paid for.
pub(super) fn width(area_width: u16) -> usize {
    (area_width as usize)
        .saturating_sub(PROMPT.chars().count())
        .max(1)
}

/// Draw a laid-out draft: the prompt on its first row, an aligned gutter on the
/// wrapped continuations, and a reverse-video caret, the same block the shared
/// single-line fields use.
pub(super) fn lines(view: &View, theme: &Theme) -> Vec<Line<'static>> {
    let ghost_style = theme.muted_style();
    // The caret sits on the ghost's first character, so it has to carry the
    // ghost's colour too. Reversed in the draft's own colour there would read
    // as a character you had actually typed.
    let caret_style = if view.ghost_from == Some(view.caret) {
        ghost_style.add_modifier(Modifier::REVERSED)
    } else {
        theme.base().add_modifier(Modifier::REVERSED)
    };
    let style_at = |i: usize| match view.ghost_from {
        Some(from) if i >= from => ghost_style,
        _ => theme.base(),
    };
    // A run may straddle the ghost boundary, so it is emitted per style rather
    // than as one span.
    let run_spans = |range: std::ops::Range<usize>| -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut buf = String::new();
        let mut current: Option<Style> = None;
        for i in range {
            let st = style_at(i);
            if current != Some(st) {
                if let Some(prev) = current.take() {
                    spans.push(Span::styled(std::mem::take(&mut buf), prev));
                }
                current = Some(st);
            }
            buf.push(view.chars[i]);
        }
        if let Some(st) = current {
            spans.push(Span::styled(buf, st));
        }
        spans
    };
    (view.first..view.first + view.rows)
        .map(|r| {
            let seg = view.segs[r];
            let gutter = if r == 0 {
                Span::styled(PROMPT, theme.accent_style())
            } else if r == view.first {
                // The draft scrolled: say so, rather than silently cutting it.
                Span::styled(MORE, theme.muted_style())
            } else {
                Span::styled(GUTTER, theme.base())
            };
            let mut spans = vec![gutter];
            if r != view.caret_row {
                spans.extend(run_spans(seg.start..seg.end));
                return Line::from(spans);
            }
            let at = view.caret.clamp(seg.start, seg.end);
            spans.extend(run_spans(seg.start..at));
            if at < seg.end {
                spans.push(Span::styled(view.chars[at].to_string(), caret_style));
                spans.extend(run_spans(at + 1..seg.end));
            } else {
                // Caret past the last character of the row.
                spans.push(Span::styled(" ", caret_style));
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::commands::Surface;
    use super::*;

    /// The rows a draft lays out to, as the text each one draws, prompt and
    /// gutter stripped.
    fn rows(text: &str, cursor: usize, width: usize, cap: usize) -> Vec<String> {
        let view = layout(text, cursor, width, cap, "");
        lines(&view, &Theme::cyber())
            .iter()
            .map(|l| {
                let row: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                row.chars().skip(PROMPT.chars().count()).collect()
            })
            .collect()
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn a_line_break_starts_a_new_row_and_is_never_drawn() {
        // v0.9.1 § Send a Message: the message keeps its line breaks, so the
        // draft shows them as rows rather than as a `⏎` marker on one line.
        let text = "hello\nworld";
        let view = layout(text, text.chars().count(), 40, 4, "");
        assert_eq!(view.rows, 2);
        let drawn = rows(text, text.chars().count(), 40, 4);
        assert_eq!(drawn[0].trim_end(), "hello");
        assert_eq!(drawn[1].trim_end(), "world");
        assert!(drawn.iter().all(|r| !r.contains('⏎')));
    }

    #[test]
    fn each_line_soft_wraps_on_its_own() {
        // "abcdef" overflows a 4-column pane; the break after it must not pull
        // "gh" up onto the wrapped tail.
        let text = "abcdef\ngh";
        let drawn = rows(text, 0, 4, 8);
        let trimmed: Vec<&str> = drawn.iter().map(|r| r.trim_end()).collect();
        assert_eq!(trimmed, vec!["abcd", "ef", "gh"]);
    }

    #[test]
    fn a_trailing_line_break_gives_the_caret_an_empty_row() {
        let text = "hi\n";
        let view = layout(text, 3, 40, 4, "");
        assert_eq!(
            view.rows, 2,
            "the new line exists before anything is typed on it"
        );
        assert_eq!(view.caret_row, 1, "the caret waits on the new line");
    }

    #[test]
    fn a_caret_before_the_break_stays_at_the_end_of_its_line() {
        let view = layout("ab\ncd", 2, 40, 4, "");
        assert_eq!(view.caret_row, 0);
        let view = layout("ab\ncd", 3, 40, 4, "");
        assert_eq!(view.caret_row, 1, "just after the break is the next line");
    }

    #[test]
    fn blank_lines_are_rows_too() {
        assert_eq!(layout("a\n\nb", 0, 40, 8, "").rows, 3);
    }

    #[test]
    fn a_tall_draft_scrolls_to_keep_the_caret_in_view() {
        let text = (1..=10)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let end = text.chars().count();
        let view = layout(&text, end, 40, MAX_ROWS as usize, "");
        assert_eq!(view.rows, MAX_ROWS as usize);
        let drawn = rows(&text, end, 40, MAX_ROWS as usize);
        assert_eq!(drawn.last().map(|r| r.trim_end()), Some("10"));
        // And Home takes the window back to the top.
        let drawn = rows(&text, 0, 40, MAX_ROWS as usize);
        assert_eq!(drawn[0].trim_end(), "1");
    }

    #[test]
    fn the_mention_ghost_never_breaks_the_row() {
        let view = layout("@ne", 3, 40, 4, "o\nx");
        assert_eq!(view.rows, 1);
    }

    #[test]
    fn a_draft_past_the_line_limit_is_refused_with_a_reason() {
        let max = cs_api::MAX_MESSAGE_LINES;
        let at_limit = vec!["x"; max].join("\n");
        assert_eq!(line_limit_notice(&at_limit, Surface::Circ), None);
        let over = format!("{at_limit}\ny");
        let notice = line_limit_notice(&over, Surface::Circ).expect("one line too many");
        assert!(
            notice.contains(&format!("{} of {max}", max + 1)),
            "{notice}"
        );
        // Blank-line runs collapse to one line (§ Send a Message), so a run
        // counts once however long it is: eight lines, a run of blanks, and one
        // more is ten.
        let eight = vec!["x"; max - 2].join("\n");
        let eight_then_gap = format!("{eight}\n\n\n\n\ny");
        assert_eq!(line_limit_notice(&eight_then_gap, Surface::Circ), None);
        let nine = vec!["x"; max - 1].join("\n");
        let nine_then_gap = format!("{nine}\n\n\n\n\ny");
        assert!(line_limit_notice(&nine_then_gap, Surface::Circ).is_some());
    }

    #[test]
    fn a_draft_past_the_character_limit_is_refused_with_how_far_over_it_is() {
        let max = cs_api::MAX_MESSAGE_LEN;
        assert_eq!(
            length_limit_notice(&"x".repeat(max)),
            None,
            "at the limit sends"
        );
        let notice = length_limit_notice(&"x".repeat(max + 55)).expect("55 over");
        assert!(
            notice.contains(&format!("{} of {max} characters", max + 55)),
            "{notice}"
        );
        assert!(notice.contains("cut 55"), "{notice}");
    }

    #[test]
    fn the_character_limit_counts_utf16_units_as_the_api_crate_does() {
        let max = cs_api::MAX_MESSAGE_LEN;
        // Kana is one unit each though three bytes, so a full message of it
        // sends: counting bytes would refuse it at a third of the length.
        assert_eq!(length_limit_notice(&"\u{3042}".repeat(max)), None);
        // An emoji is two units, so half as many fill the message...
        let rockets = "\u{1F680}".repeat(max / 2);
        assert_eq!(length_limit_notice(&rockets), None);
        // ...and one more character tips it over, with the reason given.
        let notice = length_limit_notice(&format!("{rockets}x")).expect("one over");
        assert!(notice.contains("2049 of 2048 characters"), "{notice}");
        assert!(notice.contains("emoji count as 2"), "{notice}");
        // Plain text over the limit does not mention emoji.
        let plain = length_limit_notice(&"x".repeat(max + 1)).expect("one over");
        assert!(!plain.contains("emoji"), "{plain}");
    }

    #[test]
    fn the_character_limit_has_no_exemptions() {
        let max = cs_api::MAX_MESSAGE_LEN;
        let long = "x".repeat(max);
        assert!(length_limit_notice(&format!("/rainbow {long}")).is_some());
        assert!(length_limit_notice(&format!("/art\n{long}")).is_some());
    }

    #[test]
    fn a_styled_draft_is_exempt_from_the_line_limit_only_in_circ() {
        // cIRC turns a styled message's line breaks into spaces (v0.9.1 § Send
        // a Message, cIRC); C-Mail's section says no such thing.
        let lines = vec!["x"; cs_api::MAX_MESSAGE_LINES + 5].join("\n");
        let styled = format!("/rainbow {lines}");
        assert_eq!(line_limit_notice(&styled, Surface::Circ), None);
        assert!(line_limit_notice(&styled, Surface::Cmail).is_some());
        let action = format!("/me {lines}");
        assert!(line_limit_notice(&action, Surface::Circ).is_some());
    }

    #[test]
    fn the_newline_keys_are_every_reportable_chord_but_never_plain_enter() {
        let newline = [
            key(KeyCode::Enter, KeyModifiers::ALT),
            key(KeyCode::Enter, KeyModifiers::CONTROL),
            key(KeyCode::Enter, KeyModifiers::SHIFT),
            key(KeyCode::Char('j'), KeyModifiers::CONTROL),
            // Windows with Caps Lock on, and Ctrl+Shift+J.
            key(KeyCode::Char('J'), KeyModifiers::CONTROL),
            key(
                KeyCode::Char('J'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ];
        for k in newline {
            assert!(is_newline_key(&k), "{k:?}");
        }
        let not_newline = [
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Char('j'), KeyModifiers::NONE),
            key(KeyCode::Char('J'), KeyModifiers::SHIFT),
            // AltGr on Windows arrives as Ctrl+Alt; it types, it does not break.
            key(
                KeyCode::Char('j'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
        ];
        for k in not_newline {
            assert!(!is_newline_key(&k), "{k:?}");
        }
    }

    #[test]
    fn a_draft_with_line_breaks_may_grow_a_row_per_line_up_to_the_limit() {
        assert_eq!(max_rows("one long paragraph"), MAX_ROWS);
        assert_eq!(
            max_rows("a\nb"),
            MAX_ROWS,
            "never shrinks below the usual cap"
        );
        let ten = ["x"; cs_api::MAX_MESSAGE_LINES].join("\n");
        assert_eq!(usize::from(max_rows(&ten)), cs_api::MAX_MESSAGE_LINES);
        let fifteen = ["x"; 15].join("\n");
        assert_eq!(
            usize::from(max_rows(&fifteen)),
            cs_api::MAX_MESSAGE_LINES,
            "a draft over the limit scrolls past ten rows"
        );
    }

    #[test]
    fn a_pasted_carriage_return_becomes_a_line_and_the_caret_follows() {
        let mut c = Composer::default();
        c.insert_str("ab");
        c.move_left();
        c.insert_str("1\r2\r\n3");
        assert_eq!(c.text, "a1\n2\n3b");
        assert_eq!(c.cursor, 6, "just after the pasted text");
    }
}

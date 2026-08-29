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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::editor::Segment;
use super::theme::Theme;

/// Most rows the composer may grow to as a long draft soft-wraps. Past this it
/// scrolls to keep the caret in view instead: the conversation above it is the
/// point of the screen, and `Ctrl+E` opens a whole editor for a long message.
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
    /// The draft so far. It may hold newlines — the `Ctrl+E` editor is the only
    /// way to put them there, since Enter sends, and `/art` needs them.
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

    /// Insert text at the caret, which lands just after it. Newlines survive:
    /// unlike the single-line fields, this one can hold them.
    pub(super) fn insert_str(&mut self, text: &str) {
        for c in text.chars() {
            self.insert(c);
        }
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
    /// Every soft-wrapped row, as a range into `chars`.
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
/// A newline only reaches the draft through the `Ctrl+E` editor, and the
/// composer is a strip under the conversation rather than an editor of its own,
/// so a newline stays the inline `⏎` marker it has always been. Only the width
/// starts a new row.
fn display(text: &str, cursor: usize, ghost: &str) -> (Vec<char>, usize, Option<usize>) {
    let mut chars: Vec<char> = Vec::with_capacity(text.len());
    let mut caret = 0;
    for (i, c) in text.chars().enumerate() {
        if i == cursor {
            caret = chars.len();
        }
        if c == '\n' {
            chars.extend([' ', '⏎', ' ']);
        } else {
            chars.push(c);
        }
    }
    if cursor >= text.chars().count() {
        caret = chars.len();
    }
    // The ghost sits exactly at the caret, so the caret cell lands on its first
    // character. Deliberate: the preview then reads as a continuation of what
    // you are typing rather than something parked after a gap.
    let ghost_from = (!ghost.is_empty()).then(|| {
        let at = caret;
        let tail: Vec<char> = chars.split_off(at);
        chars.extend(ghost.chars());
        chars.extend(tail);
        at
    });
    (chars, caret, ghost_from)
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
    // `wrap_line` always yields at least one segment, so `segs` is never empty.
    let segs = super::editor::wrap_line(&chars, width.max(1));
    let (caret_row, _) = super::editor::caret_in_line(&chars, &segs, caret);
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

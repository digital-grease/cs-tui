//! Built-in multiline text editor for composing posts, replies, notes, and
//! guild threads, replacing the dependency on an external `$EDITOR`.
//!
//! Three layers, smallest to largest:
//!
//! - [`TextBuffer`] is the pure logical model: a `Vec<Vec<char>>` of lines plus
//!   a char-indexed cursor. Every edit and horizontal cursor move lives here and
//!   is unit-tested with no rendering dependency. A "column" here is a CHAR index
//!   (`0..=line.len()`), never a byte offset or a display width.
//!
//! - The free `wrap_*` / `cursor_to_visual` / `visual_to_cursor` / `scroll_follow`
//!   functions are the pure display geometry. They map the logical buffer to
//!   wrapped *visual* rows at a given content width and back. Width handling
//!   matches `text.rs` (`UnicodeWidthChar::width(ch).unwrap_or(0)`); tabs expand
//!   to [`TAB_WIDTH`]. They own no state, so a terminal resize is correct by
//!   construction: everything is re-derived from `(lines, cursor, width)` each
//!   frame.
//!
//! - [`EditorScreen`] ties a `TextBuffer` to the TUI: it owns the scroll offset,
//!   the last rendered body size, the editor's [`EditorPurpose`], and an inline
//!   error. Vertical movement (Up/Down/PageUp/PageDown) is *visual* (it steps by
//!   wrapped rows, tracking a sticky display column) so long, soft-wrapped lines
//!   navigate the way a user expects.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use super::compose::ComposeKind;
use super::theme::Theme;

/// Display columns a tab advances to. Fixed (not elastic tab stops) so a glyph's
/// width depends only on the glyph, which the wrap mapper relies on.
const TAB_WIDTH: usize = 4;

// ===========================================================================
// TextBuffer — pure logical model (Task 1.1)
// ===========================================================================

/// A multiline text buffer with a single cursor. Pure logic, no rendering deps.
///
/// Edits and cursor positions are indexed by `char`. A wide CJK/emoji glyph is
/// one `char` element and the cursor steps over it as a single unit; its display
/// width is exclusively a concern of the wrap/render layer below.
#[derive(Debug, Clone)]
pub struct TextBuffer {
    /// Logical lines. INVARIANT: never empty; an empty document is one empty
    /// line (`vec![Vec::new()]`). No element is ever `'\r'` or `'\n'`.
    lines: Vec<Vec<char>>,
    /// Cursor line index. INVARIANT: `cursor_row < lines.len()`.
    cursor_row: usize,
    /// Cursor column as a CHAR index. INVARIANT: `cursor_col <= line.len()`
    /// (inclusive: the cursor may sit just past the last char).
    cursor_col: usize,
}

impl TextBuffer {
    /// Build from initial text: split on `\n`, strip every `\r` (so CRLF and a
    /// lone CR both normalize). Empty input yields one empty line. The cursor
    /// starts at the END of the buffer, ready to keep typing after a prefill.
    pub fn new(initial: &str) -> Self {
        let cleaned: String = initial.chars().filter(|&c| c != '\r').collect();
        let lines: Vec<Vec<char>> = cleaned.split('\n').map(|l| l.chars().collect()).collect();
        // `split('\n')` always yields at least one segment.
        let cursor_row = lines.len() - 1;
        let cursor_col = lines[cursor_row].len();
        Self {
            lines,
            cursor_row,
            cursor_col,
        }
    }

    /// Join the lines with `\n` (no trailing newline). Round-trips with [`new`]
    /// for any `\r`-free input.
    #[must_use]
    pub fn content(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// True when the document is empty or only whitespace. Drives the Save guard.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.lines
            .iter()
            .all(|l| l.iter().all(|c| c.is_whitespace()))
    }

    #[must_use]
    pub fn lines(&self) -> &[Vec<char>] {
        &self.lines
    }

    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// Place the cursor at `(row, col)`, clamping both into range. Used by the
    /// visual vertical-movement layer after it resolves a target.
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.lines.len() - 1);
        self.cursor_col = col.min(self.lines[self.cursor_row].len());
    }

    /// Insert one char at the cursor. `'\n'` splits the line; `'\r'` is ignored.
    pub fn insert_char(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => {}
            _ => {
                self.lines[self.cursor_row].insert(self.cursor_col, c);
                self.cursor_col += 1;
            }
        }
    }

    /// Split the current line at the cursor; the tail moves to a new line below
    /// and the cursor lands at its start.
    pub fn newline(&mut self) {
        let tail = self.lines[self.cursor_row].split_off(self.cursor_col);
        self.lines.insert(self.cursor_row + 1, tail);
        self.cursor_row += 1;
        self.cursor_col = 0;
    }

    /// Delete the char before the cursor. At column 0 (row > 0) join this line
    /// onto the previous one, landing at the seam. At `(0, 0)` it is a no-op.
    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.lines[self.cursor_row].remove(self.cursor_col - 1);
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            let cur = self.lines.remove(self.cursor_row);
            let prev_len = self.lines[self.cursor_row - 1].len();
            self.lines[self.cursor_row - 1].extend(cur);
            self.cursor_row -= 1;
            self.cursor_col = prev_len;
        }
    }

    /// Delete the char at the cursor. At end-of-line on a non-last line, pull the
    /// next line up. At the end of the last line it is a no-op.
    pub fn delete_forward(&mut self) {
        let len = self.lines[self.cursor_row].len();
        if self.cursor_col < len {
            self.lines[self.cursor_row].remove(self.cursor_col);
        } else if self.cursor_row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].extend(next);
        }
    }

    /// Insert arbitrary text at the cursor. `\n` splits lines; `\r` is dropped so
    /// CRLF pastes cleanly. The cursor ends just after the inserted text.
    pub fn paste(&mut self, text: &str) {
        let cleaned: String = text.chars().filter(|&c| c != '\r').collect();
        let segs: Vec<&str> = cleaned.split('\n').collect();
        if segs.len() == 1 {
            for c in segs[0].chars() {
                self.lines[self.cursor_row].insert(self.cursor_col, c);
                self.cursor_col += 1;
            }
            return;
        }
        let row = self.cursor_row;
        // Chars after the cursor get reattached to the final pasted line.
        let tail = self.lines[row].split_off(self.cursor_col);
        self.lines[row].extend(segs[0].chars());
        let mut insert_at = row + 1;
        for seg in &segs[1..segs.len() - 1] {
            self.lines.insert(insert_at, seg.chars().collect());
            insert_at += 1;
        }
        let last = segs[segs.len() - 1];
        let last_len = last.chars().count();
        let mut last_line: Vec<char> = last.chars().collect();
        last_line.extend(tail);
        self.lines.insert(insert_at, last_line);
        self.cursor_row = insert_at;
        self.cursor_col = last_len;
    }

    /// Left one char, wrapping to the end of the previous line at column 0.
    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    /// Right one char, wrapping to the start of the next line at end-of-line.
    pub fn move_right(&mut self) {
        let len = self.lines[self.cursor_row].len();
        if self.cursor_col < len {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    /// Cursor to the start of the current line.
    pub fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    /// Cursor to the end of the current line.
    pub fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_row].len();
    }
}

// ===========================================================================
// Display geometry — soft-wrap, cursor mapping, scroll (Task 1.2)
// ===========================================================================

/// Display width of one char, matching `text.rs` (`unwrap_or(0)` for controls),
/// with tabs expanded to a fixed [`TAB_WIDTH`].
fn cell_width(ch: char) -> usize {
    match ch {
        '\t' => TAB_WIDTH,
        _ => UnicodeWidthChar::width(ch).unwrap_or(0),
    }
}

/// Total display width of a char slice.
fn display_width(chars: &[char]) -> usize {
    chars.iter().map(|&c| cell_width(c)).sum()
}

/// One visual row of a logical line, as a half-open char range `chars[start..end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    start: usize,
    end: usize,
}

/// Hard-wrap one logical line to content width `w`.
///
/// Always returns at least one segment (an empty line -> one empty segment).
/// Breaks *before* a glyph that would overflow, so a wide glyph never straddles
/// the right edge. Guarantees forward progress (every segment advances the char
/// index by >= 1), so degenerate widths (`w == 0`, or `w == 1` facing a 2-wide
/// glyph) terminate.
fn wrap_line(line: &[char], w: usize) -> Vec<Segment> {
    let w = w.max(1);
    let mut segs = Vec::new();
    let mut seg_start = 0;
    let mut seg_width = 0;
    let mut i = 0;
    while i < line.len() {
        let cw = cell_width(line[i]);
        // Overflow only forces a break when the segment already holds a char,
        // otherwise an over-wide glyph could never be placed (infinite loop).
        if seg_width + cw > w && i > seg_start {
            segs.push(Segment {
                start: seg_start,
                end: i,
            });
            seg_start = i;
            seg_width = 0;
            continue; // re-test line[i] against the fresh segment
        }
        seg_width += cw;
        i += 1;
    }
    segs.push(Segment {
        start: seg_start,
        end: line.len(),
    });
    segs
}

/// Wrapped segments for every line, plus the global visual-row index at which
/// each logical line starts. `vrow_base` has length `lines.len() + 1`; its last
/// element is the total visual-row count.
struct WrapMap {
    segs: Vec<Vec<Segment>>,
    vrow_base: Vec<usize>,
}

fn wrap_buffer(lines: &[Vec<char>], w: usize) -> WrapMap {
    let mut segs = Vec::with_capacity(lines.len());
    let mut vrow_base = Vec::with_capacity(lines.len() + 1);
    let mut acc = 0;
    for line in lines {
        vrow_base.push(acc);
        let s = wrap_line(line, w);
        acc += s.len();
        segs.push(s);
    }
    vrow_base.push(acc);
    WrapMap { segs, vrow_base }
}

fn total_visual_rows(m: &WrapMap) -> usize {
    *m.vrow_base.last().unwrap()
}

/// Logical row owning global visual row `vrow` (largest `r` with
/// `vrow_base[r] <= vrow`). `vrow_base` is strictly increasing, so this is exact.
fn logical_row_of(m: &WrapMap, vrow: usize) -> usize {
    m.vrow_base.partition_point(|&b| b <= vrow) - 1
}

/// Map a logical caret `(row, col)` (col in `0..=line.len()`) to a global visual
/// `(vrow, vcol)`.
///
/// At a wrap boundary the caret renders at the START of the next visual row
/// (where the next glyph will land), except at true end-of-line, which has no
/// next segment and renders at the right edge of the last segment.
fn cursor_to_visual(m: &WrapMap, lines: &[Vec<char>], row: usize, col: usize) -> (usize, usize) {
    let segs = &m.segs[row];
    let base = m.vrow_base[row];
    let line = &lines[row];
    for (k, seg) in segs.iter().enumerate() {
        let is_last = k + 1 == segs.len();
        if col < seg.end || (is_last && col == seg.end) {
            return (base + k, display_width(&line[seg.start..col]));
        }
    }
    let last = segs.len() - 1;
    let seg = segs[last];
    (
        base + last,
        display_width(&line[seg.start..col.min(line.len())]),
    )
}

/// Map a visual `(vrow, target_vcol)` back to the nearest logical `(row, col)`,
/// snapping to the left edge of a wide glyph that straddles `target_vcol`. Used
/// by visual Up/Down/PageUp/PageDown.
fn visual_to_cursor(
    m: &WrapMap,
    lines: &[Vec<char>],
    vrow: usize,
    target_vcol: usize,
) -> (usize, usize) {
    let vrow = vrow.min(total_visual_rows(m).saturating_sub(1));
    let row = logical_row_of(m, vrow);
    let k = vrow - m.vrow_base[row];
    let seg = m.segs[row][k];
    let line = &lines[row];
    let mut col = seg.start;
    let mut wsum = 0;
    while col < seg.end {
        let cw = cell_width(line[col]);
        if wsum + cw > target_vcol {
            break;
        }
        wsum += cw;
        col += 1;
    }
    // On a soft-wrap (non-last) segment, `col == seg.end` is the wrap boundary,
    // which `cursor_to_visual` renders at the START of the *next* visual row. If
    // we returned it here, a single Up/Down would visually skip a row. Snap back
    // to the segment's last glyph so the caret stays on the row we navigated to.
    let is_last = k + 1 == m.segs[row].len();
    if !is_last && col == seg.end && col > seg.start {
        col -= 1;
    }
    (row, col)
}

/// New scroll (top visual row) keeping `caret_vrow` within `[scroll, scroll+h)`,
/// moving as little as possible and never leaving a window of blank rows below
/// the content.
fn scroll_follow(prev_scroll: usize, caret_vrow: usize, h: usize, total: usize) -> usize {
    let h = h.max(1);
    let mut scroll = prev_scroll;
    if caret_vrow < scroll {
        scroll = caret_vrow;
    } else if caret_vrow >= scroll + h {
        scroll = caret_vrow + 1 - h;
    }
    scroll.min(total.saturating_sub(h))
}

/// Expand tabs to spaces for rendering (the buffer keeps the literal `\t`).
fn expand_tabs(chars: &[char]) -> String {
    let mut s = String::with_capacity(chars.len());
    for &c in chars {
        if c == '\t' {
            for _ in 0..TAB_WIDTH {
                s.push(' ');
            }
        } else {
            s.push(c);
        }
    }
    s
}

// ===========================================================================
// EditorScreen — the TUI screen (Task 2.1)
// ===========================================================================

/// What the editor produces on save, i.e. where its content flows next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorPurpose {
    /// First-step body composition. On save the app builds a `ComposeScreen` of
    /// `kind` (prefilling `prefill_topics`) and replaces the editor with it.
    NewBody {
        kind: ComposeKind,
        prefill_topics: Vec<String>,
    },
    /// Ctrl+E re-edit of an existing `ComposeScreen` body. On save the app pops
    /// back to that compose screen and overwrites its content.
    ReEditBody,
}

/// Outcome of a key for the surrounding app to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorIntent {
    None,
    Save,
    Cancel,
}

/// The full-screen multiline editor.
#[derive(Debug)]
pub struct EditorScreen {
    buffer: TextBuffer,
    purpose: EditorPurpose,
    /// Inline error (e.g. a blank-content save attempt), shown in the footer.
    pub error: Option<String>,
    /// Top visual row. Interior-mutable because `render` (which takes `&self`,
    /// like every screen) is where we have the real viewport size, mirroring how
    /// `list.rs` keeps its scroll offset in a `Cell`.
    scroll: Cell<usize>,
    /// Body content size captured at the last render, so `handle_key` can do
    /// visual vertical movement before the next frame.
    last_body_w: Cell<u16>,
    last_body_h: Cell<u16>,
    /// Sticky *visual* target column for vertical movement, in display cells.
    /// `Some` while walking Up/Down so the caret tracks a vertical screen line
    /// across short and wide lines; cleared by any horizontal move or edit.
    desired_vcol: Option<usize>,
}

impl EditorScreen {
    pub fn new(purpose: EditorPurpose, initial: &str) -> Self {
        Self {
            buffer: TextBuffer::new(initial),
            purpose,
            error: None,
            scroll: Cell::new(0),
            // Sane defaults until the first render measures the real viewport.
            last_body_w: Cell::new(80),
            last_body_h: Cell::new(20),
            desired_vcol: None,
        }
    }

    #[must_use]
    pub fn content(&self) -> String {
        self.buffer.content()
    }

    #[must_use]
    pub fn purpose(&self) -> &EditorPurpose {
        &self.purpose
    }

    /// Insert pasted text (bracketed paste). Routed here from the event loop.
    pub fn paste(&mut self, text: &str) {
        self.buffer.paste(text);
        self.desired_vcol = None;
        self.error = None;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('s') {
            if self.buffer.is_blank() {
                self.error = Some("nothing to save yet · type something · esc cancels".into());
                return EditorIntent::None;
            }
            return EditorIntent::Save;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            return EditorIntent::Cancel;
        }
        // Ignore other control-modified keys so they don't insert stray glyphs.
        if ctrl {
            return EditorIntent::None;
        }
        match key.code {
            KeyCode::Char(c) => self.edit(|b| b.insert_char(c)),
            KeyCode::Enter => self.edit(TextBuffer::newline),
            KeyCode::Tab => self.edit(|b| b.insert_char('\t')),
            KeyCode::Backspace => self.edit(TextBuffer::backspace),
            KeyCode::Delete => self.edit(TextBuffer::delete_forward),
            KeyCode::Left => self.horizontal(TextBuffer::move_left),
            KeyCode::Right => self.horizontal(TextBuffer::move_right),
            KeyCode::Home => self.horizontal(TextBuffer::move_home),
            KeyCode::End => self.horizontal(TextBuffer::move_end),
            KeyCode::Up => self.move_vertical(-1),
            KeyCode::Down => self.move_vertical(1),
            KeyCode::PageUp => {
                let h = self.last_body_h.get().max(1) as isize;
                self.move_vertical(-h);
            }
            KeyCode::PageDown => {
                let h = self.last_body_h.get().max(1) as isize;
                self.move_vertical(h);
            }
            // Esc never actually reaches here (the app's global handler pops the
            // screen first); kept as a defensive no-op.
            _ => {}
        }
        EditorIntent::None
    }

    /// Run an edit, then reset sticky vertical state and clear the error.
    fn edit(&mut self, f: impl FnOnce(&mut TextBuffer)) {
        f(&mut self.buffer);
        self.desired_vcol = None;
        self.error = None;
    }

    /// Run a horizontal move, which also drops the sticky vertical column.
    fn horizontal(&mut self, f: impl FnOnce(&mut TextBuffer)) {
        f(&mut self.buffer);
        self.desired_vcol = None;
    }

    /// Move the caret by `delta` *visual* rows (negative = up), preserving the
    /// sticky display column.
    fn move_vertical(&mut self, delta: isize) {
        let w = self.last_body_w.get().max(1) as usize;
        let lines = self.buffer.lines();
        let m = wrap_buffer(lines, w);
        let (row, col) = self.buffer.cursor();
        let (vrow, vcol) = cursor_to_visual(&m, lines, row, col);
        let target_vcol = *self.desired_vcol.get_or_insert(vcol);
        let total = total_visual_rows(&m);
        let target_vrow = if delta < 0 {
            vrow.saturating_sub(delta.unsigned_abs())
        } else {
            (vrow + delta as usize).min(total.saturating_sub(1))
        };
        let (nr, nc) = visual_to_cursor(&m, lines, target_vrow, target_vcol);
        self.buffer.set_cursor(nr, nc);
    }

    fn title(&self) -> String {
        match &self.purpose {
            EditorPurpose::NewBody { kind, .. } => kind.title(),
            EditorPurpose::ReEditBody => " cs-tui • edit body ".to_string(),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(self.title(), theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let body = layout[0];
        let footer = layout[1];

        let w = body.width.max(1) as usize;
        let h = body.height.max(1) as usize;
        self.last_body_w.set(body.width.max(1));
        self.last_body_h.set(body.height.max(1));

        let lines = self.buffer.lines();
        let m = wrap_buffer(lines, w);
        let (row, col) = self.buffer.cursor();
        let (cvrow, cvcol) = cursor_to_visual(&m, lines, row, col);
        let total = total_visual_rows(&m);
        let scroll = scroll_follow(self.scroll.get(), cvrow, h, total);
        self.scroll.set(scroll);

        let mut out: Vec<Line> = Vec::with_capacity(h);
        for vr in scroll..(scroll + h).min(total) {
            let lr = logical_row_of(&m, vr);
            let seg = m.segs[lr][vr - m.vrow_base[lr]];
            let text = expand_tabs(&lines[lr][seg.start..seg.end]);
            out.push(Line::from(Span::styled(text, theme.base())));
        }
        frame.render_widget(Paragraph::new(out), body);

        let footer_line = if let Some(msg) = &self.error {
            Line::from(Span::styled(msg.clone(), theme.error_style()))
        } else {
            Line::from(Span::styled(
                "ctrl+s save · esc cancel · enter newline · paste ok",
                theme.muted_style(),
            ))
        };
        frame.render_widget(Paragraph::new(footer_line), footer);

        // Show the real terminal cursor only for the editor frame. Clamp the
        // column into the body so the caret at the end of a full-width row stays
        // inside the box.
        if cvrow >= scroll && cvrow < scroll + h {
            let cx = body.x.saturating_add(cvcol.min(w.saturating_sub(1)) as u16);
            let cy = body.y.saturating_add((cvrow - scroll) as u16);
            frame.set_cursor_position(Position::new(cx, cy));
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    /// Build a buffer and place the cursor at `(row, col)` (which `new` can't,
    /// since it always lands at the end). Asserts the placement is valid.
    fn at(initial: &str, row: usize, col: usize) -> TextBuffer {
        let mut b = TextBuffer::new(initial);
        assert!(row < b.lines.len() && col <= b.lines[row].len());
        b.cursor_row = row;
        b.cursor_col = col;
        b
    }

    // ---- construction / content ----

    #[test]
    fn new_empty_is_single_blank_line() {
        let b = TextBuffer::new("");
        assert_eq!(b.lines, vec![Vec::<char>::new()]);
        assert_eq!(b.cursor(), (0, 0));
        assert_eq!(b.content(), "");
        assert!(b.is_blank());
    }

    #[test]
    fn new_splits_on_newline_cursor_at_end() {
        let b = TextBuffer::new("a\nb");
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.cursor(), (1, 1));
    }

    #[test]
    fn new_keeps_trailing_blank_line() {
        let b = TextBuffer::new("a\n");
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.cursor(), (1, 0));
        assert_eq!(b.content(), "a\n");
    }

    #[test]
    fn new_strips_carriage_returns() {
        let b = TextBuffer::new("a\r\nb");
        assert_eq!(b.content(), "a\nb");
        assert!(b.lines.iter().all(|l| !l.contains(&'\r')));
    }

    #[test]
    fn content_round_trips_through_new() {
        for s in ["", "a", "a\nb\nc", "a\n\nb", "\nb", "héllo 日本 😀\nsecond"] {
            assert_eq!(TextBuffer::new(s).content(), s, "round-trip {s:?}");
        }
    }

    #[test]
    fn is_blank_detects_whitespace_only() {
        assert!(TextBuffer::new("   \n\t\n  ").is_blank());
        assert!(!TextBuffer::new("  x ").is_blank());
    }

    // ---- insert ----

    #[test]
    fn insert_char_mid_line() {
        let mut b = at("abc", 0, 2);
        b.insert_char('x');
        assert_eq!(b.content(), "abxc");
        assert_eq!(b.cursor(), (0, 3));
    }

    #[test]
    fn insert_wide_char_is_one_element() {
        let mut b = TextBuffer::new("");
        b.insert_char('日');
        assert_eq!(b.lines[0], vec!['日']);
        assert_eq!(b.cursor(), (0, 1));
    }

    #[test]
    fn insert_newline_char_splits_line() {
        let mut b = at("abc", 0, 2);
        b.insert_char('\n');
        assert_eq!(b.content(), "ab\nc");
        assert_eq!(b.cursor(), (1, 0));
    }

    #[test]
    fn insert_carriage_return_is_ignored() {
        let mut b = at("ab", 0, 1);
        b.insert_char('\r');
        assert_eq!(b.content(), "ab");
        assert_eq!(b.cursor(), (0, 1));
    }

    // ---- newline ----

    #[test]
    fn newline_at_end_appends_blank_line() {
        let mut b = at("ab", 0, 2);
        b.newline();
        assert_eq!(b.content(), "ab\n");
        assert_eq!(b.cursor(), (1, 0));
    }

    #[test]
    fn newline_carries_tail_down() {
        let mut b = at("helloworld", 0, 5);
        b.newline();
        assert_eq!(b.content(), "hello\nworld");
        assert_eq!(b.cursor(), (1, 0));
    }

    // ---- backspace ----

    #[test]
    fn backspace_mid_line_removes_prev_char() {
        let mut b = at("abc", 0, 2);
        b.backspace();
        assert_eq!(b.content(), "ac");
        assert_eq!(b.cursor(), (0, 1));
    }

    #[test]
    fn backspace_at_col_zero_joins_previous_line() {
        let mut b = at("ab\ncd", 1, 0);
        b.backspace();
        assert_eq!(b.content(), "abcd");
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn backspace_at_origin_is_noop() {
        let mut b = at("ab", 0, 0);
        b.backspace();
        assert_eq!(b.content(), "ab");
        assert_eq!(b.cursor(), (0, 0));
    }

    #[test]
    fn backspace_collapses_blank_middle_line() {
        let mut b = at("a\n\nb", 1, 0);
        b.backspace();
        assert_eq!(b.content(), "a\nb");
        assert_eq!(b.cursor(), (0, 1));
    }

    #[test]
    fn backspace_deletes_one_wide_char() {
        let mut b = at("日", 0, 1);
        b.backspace();
        assert_eq!(b.content(), "");
        assert_eq!(b.cursor(), (0, 0));
    }

    // ---- delete forward ----

    #[test]
    fn delete_forward_mid_line() {
        let mut b = at("abc", 0, 1);
        b.delete_forward();
        assert_eq!(b.content(), "ac");
        assert_eq!(b.cursor(), (0, 1));
    }

    #[test]
    fn delete_forward_at_line_end_pulls_next_up() {
        let mut b = at("ab\ncd", 0, 2);
        b.delete_forward();
        assert_eq!(b.content(), "abcd");
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn delete_forward_at_buffer_end_is_noop() {
        let mut b = at("ab", 0, 2);
        b.delete_forward();
        assert_eq!(b.content(), "ab");
        assert_eq!(b.cursor(), (0, 2));
    }

    // ---- paste ----

    #[test]
    fn paste_without_newline_inserts_inline() {
        let mut b = at("ad", 0, 1);
        b.paste("bc");
        assert_eq!(b.content(), "abcd");
        assert_eq!(b.cursor(), (0, 3));
    }

    #[test]
    fn paste_two_lines_splits_and_reattaches_tail() {
        let mut b = at("ad", 0, 1);
        b.paste("b\nc");
        assert_eq!(b.content(), "ab\ncd");
        assert_eq!(b.cursor(), (1, 1));
    }

    #[test]
    fn paste_three_lines() {
        let mut b = TextBuffer::new("");
        b.paste("a\nb\nc");
        assert_eq!(b.content(), "a\nb\nc");
        assert_eq!(b.cursor(), (2, 1));
    }

    #[test]
    fn paste_crlf_pastes_cleanly() {
        let mut b = TextBuffer::new("");
        b.paste("x\r\ny");
        assert_eq!(b.content(), "x\ny");
        assert!(b.lines.iter().all(|l| !l.contains(&'\r')));
    }

    #[test]
    fn paste_trailing_newline_adds_blank_line() {
        let mut b = TextBuffer::new("");
        b.paste("x\n");
        assert_eq!(b.content(), "x\n");
        assert_eq!(b.cursor(), (1, 0));
    }

    #[test]
    fn paste_matches_keystroke_typing() {
        let mut pasted = TextBuffer::new("");
        pasted.paste("a\nb\nc");
        let mut typed = TextBuffer::new("");
        for c in ['a', '\n', 'b', '\n', 'c'] {
            typed.insert_char(c);
        }
        assert_eq!(pasted.content(), typed.content());
        assert_eq!(pasted.cursor(), typed.cursor());
    }

    // ---- horizontal movement ----

    #[test]
    fn move_left_wraps_to_end_of_previous_line() {
        let mut b = at("ab\ncd", 1, 0);
        b.move_left();
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn move_right_wraps_to_next_line_start() {
        let mut b = at("ab\ncd", 0, 2);
        b.move_right();
        assert_eq!(b.cursor(), (1, 0));
    }

    #[test]
    fn move_left_at_origin_is_noop() {
        let mut b = at("ab", 0, 0);
        b.move_left();
        assert_eq!(b.cursor(), (0, 0));
    }

    #[test]
    fn move_right_at_buffer_end_is_noop() {
        let mut b = at("ab", 0, 2);
        b.move_right();
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn home_and_end_jump_to_line_bounds() {
        let mut b = at("abcd", 0, 2);
        b.move_home();
        assert_eq!(b.cursor(), (0, 0));
        b.move_end();
        assert_eq!(b.cursor(), (0, 4));
    }

    #[test]
    fn set_cursor_clamps_into_range() {
        let mut b = TextBuffer::new("ab\ncde");
        b.set_cursor(9, 9);
        assert_eq!(b.cursor(), (1, 3));
        b.set_cursor(0, 9);
        assert_eq!(b.cursor(), (0, 2));
    }

    #[test]
    fn full_delete_leaves_one_empty_line() {
        let mut b = at("a", 0, 1);
        b.backspace();
        assert_eq!(b.lines.len(), 1);
        assert!(b.lines[0].is_empty());
        assert_eq!(b.cursor(), (0, 0));
    }

    // ---- wrap geometry ----

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn seg_widths(line: &[char], w: usize) -> Vec<usize> {
        wrap_line(line, w)
            .iter()
            .map(|s| display_width(&line[s.start..s.end]))
            .collect()
    }

    #[test]
    fn wrap_empty_line_is_one_segment() {
        assert_eq!(wrap_line(&[], 5), vec![Segment { start: 0, end: 0 }]);
    }

    #[test]
    fn wrap_line_exactly_w_is_one_row() {
        assert_eq!(wrap_line(&chars("hello"), 5).len(), 1);
    }

    #[test]
    fn wrap_line_one_over_w_splits() {
        let segs = wrap_line(&chars("hellos"), 5);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1], Segment { start: 5, end: 6 });
    }

    #[test]
    fn wrap_breaks_before_wide_glyph_no_straddle() {
        // "ABCD日" at w=5: ABCD=4 cells, 日 would make 6 -> breaks before it.
        let line = chars("ABCD日");
        let segs = wrap_line(&line, 5);
        assert_eq!(segs.len(), 2);
        assert_eq!(seg_widths(&line, 5), vec![4, 2]);
    }

    #[test]
    fn wrap_fits_wide_glyph_exactly_at_boundary() {
        // "AB日X" = 1+1+2+1 = 5 cells -> one row.
        assert_eq!(wrap_line(&chars("AB日X"), 5).len(), 1);
    }

    #[test]
    fn wrap_zero_width_combining_rides_along() {
        // 'e' + combining acute + 'x' = width 2.
        let line = vec!['e', '\u{301}', 'x'];
        assert_eq!(wrap_line(&line, 5).len(), 1);
    }

    #[test]
    fn wrap_tab_expands_and_can_force_break() {
        // ['a','\t','b'] at w=4: a=1, \t=4 -> break before \t, then b breaks too.
        let line = vec!['a', '\t', 'b'];
        assert_eq!(wrap_line(&line, 4).len(), 3);
    }

    #[test]
    fn wrap_w_zero_does_not_panic() {
        assert_eq!(wrap_line(&chars("ab"), 0).len(), 2);
    }

    #[test]
    fn wrap_w_one_with_cjk_terminates() {
        let line = chars("日日");
        let segs = wrap_line(&line, 1);
        assert_eq!(segs.len(), 2);
        // Every segment advances by at least one char.
        assert!(segs.iter().all(|s| s.end > s.start));
    }

    #[test]
    fn wrap_long_line_segment_count() {
        let line = chars(&"x".repeat(23));
        let segs = wrap_line(&line, 5);
        assert_eq!(segs.len(), 5);
        assert_eq!(seg_widths(&line, 5), vec![5, 5, 5, 5, 3]);
    }

    // ---- cursor_to_visual / WrapMap ----

    fn wmap(lines: &[Vec<char>], w: usize) -> WrapMap {
        wrap_buffer(lines, w)
    }

    #[test]
    fn vrow_base_totals_match_segment_counts() {
        let lines = vec![chars("aaaaaa"), chars("bb"), Vec::new()];
        let m = wmap(&lines, 5);
        for r in 0..lines.len() {
            assert_eq!(m.vrow_base[r + 1] - m.vrow_base[r], m.segs[r].len());
        }
        assert_eq!(total_visual_rows(&m), 2 + 1 + 1);
    }

    #[test]
    fn cursor_empty_buffer_top_left() {
        let lines = vec![Vec::new()];
        let m = wmap(&lines, 10);
        assert_eq!(cursor_to_visual(&m, &lines, 0, 0), (0, 0));
    }

    #[test]
    fn cursor_end_of_full_width_line_is_end_of_line() {
        let lines = vec![chars("hello")];
        let m = wmap(&lines, 5);
        assert_eq!(cursor_to_visual(&m, &lines, 0, 5), (0, 5));
    }

    #[test]
    fn cursor_at_wrap_boundary_is_start_of_next() {
        let lines = vec![chars("ABCDE")];
        let m = wmap(&lines, 3); // segs [0..3],[3..5]
        assert_eq!(cursor_to_visual(&m, &lines, 0, 3), (1, 0));
        // Just before the boundary stays on the first row.
        assert_eq!(cursor_to_visual(&m, &lines, 0, 2), (0, 2));
    }

    #[test]
    fn cursor_end_of_long_wrapped_line() {
        let lines = vec![chars(&"x".repeat(23))];
        let m = wmap(&lines, 5);
        assert_eq!(cursor_to_visual(&m, &lines, 0, 23), (4, 3));
    }

    #[test]
    fn cursor_on_blank_middle_line() {
        let lines = vec![chars("a"), Vec::new(), chars("b")];
        let m = wmap(&lines, 10);
        assert_eq!(cursor_to_visual(&m, &lines, 1, 0), (1, 0));
    }

    #[test]
    fn cursor_vcol_accounts_for_wide_glyphs() {
        let lines = vec![chars("日本x")];
        let m = wmap(&lines, 10);
        // logical col 2 (before 'x') -> 2 CJK = 4 cells.
        assert_eq!(cursor_to_visual(&m, &lines, 0, 2), (0, 4));
    }

    // ---- visual_to_cursor / round-trip ----

    #[test]
    fn visual_to_cursor_snaps_to_left_of_straddling_glyph() {
        let lines = vec![chars("a日b")];
        let m = wmap(&lines, 10);
        // target vcol 2 sits inside the wide glyph (cells 1..3) -> snap to col 1.
        assert_eq!(visual_to_cursor(&m, &lines, 0, 2), (0, 1));
    }

    #[test]
    fn visual_to_cursor_round_trips_non_boundary() {
        let lines = vec![chars(&"abcdefghij".repeat(3))];
        let m = wmap(&lines, 7);
        for col in [0usize, 3, 7, 12, 20, 29] {
            let (vr, vc) = cursor_to_visual(&m, &lines, 0, col);
            // Skip exact boundary columns (deliberate start-of-next asymmetry).
            let seg = m.segs[0][vr];
            if col == seg.start && col != 0 {
                continue;
            }
            assert_eq!(visual_to_cursor(&m, &lines, vr, vc), (0, col), "col {col}");
        }
    }

    // ---- scroll_follow ----

    #[test]
    fn scroll_follows_caret_down_past_window() {
        assert_eq!(scroll_follow(0, 4, 3, 5), 2);
    }

    #[test]
    fn scroll_follows_caret_up_above_window() {
        assert_eq!(scroll_follow(2, 0, 3, 5), 0);
    }

    #[test]
    fn scroll_unchanged_when_caret_in_window() {
        assert_eq!(scroll_follow(4, 6, 5, 30), 4);
    }

    #[test]
    fn scroll_clamps_when_buffer_shorter_than_window() {
        assert_eq!(scroll_follow(3, 0, 4, 1), 0);
    }

    #[test]
    fn scroll_never_underflows_at_top() {
        assert_eq!(scroll_follow(0, 0, 1, 1), 0);
    }

    #[test]
    fn scroll_height_one_keeps_caret_visible() {
        let s = scroll_follow(0, 3, 1, 10);
        assert_eq!(s, 3);
    }

    // ---- EditorScreen handle_key / visual nav ----

    fn screen(initial: &str) -> EditorScreen {
        EditorScreen::new(EditorPurpose::ReEditBody, initial)
    }

    #[test]
    fn typing_inserts_and_updates_content() {
        let mut s = screen("");
        for c in "hi".chars() {
            s.handle_key(key(KeyCode::Char(c), KeyModifiers::empty()));
        }
        assert_eq!(s.content(), "hi");
    }

    #[test]
    fn ctrl_s_saves_when_non_blank() {
        let mut s = screen("hello");
        assert_eq!(
            s.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            EditorIntent::Save
        );
    }

    #[test]
    fn ctrl_s_on_blank_sets_error_and_stays() {
        let mut s = screen("   \n  ");
        assert_eq!(
            s.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            EditorIntent::None
        );
        assert!(s.error.is_some());
    }

    #[test]
    fn ctrl_c_cancels() {
        let mut s = screen("hello");
        assert_eq!(
            s.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            EditorIntent::Cancel
        );
    }

    #[test]
    fn editing_clears_a_previous_error() {
        let mut s = screen("");
        s.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(s.error.is_some());
        s.handle_key(key(KeyCode::Char('x'), KeyModifiers::empty()));
        assert!(s.error.is_none());
    }

    #[test]
    fn esc_key_is_a_noop_in_handle_key() {
        // The app pops the screen before Esc reaches here, but it must not edit.
        let mut s = screen("abc");
        assert_eq!(
            s.handle_key(key(KeyCode::Esc, KeyModifiers::empty())),
            EditorIntent::None
        );
        assert_eq!(s.content(), "abc");
    }

    #[test]
    fn down_then_up_visual_returns_to_start_within_wrapped_line() {
        // One logical line wrapping to several visual rows; visual Down/Up move
        // by wrapped row, not by logical line.
        let mut s = screen(&"x".repeat(20));
        s.last_body_w.set(5);
        s.last_body_h.set(10);
        s.buffer.set_cursor(0, 2); // visual row 0, col 2
        s.handle_key(key(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(s.buffer.cursor(), (0, 7)); // next visual row, same vcol
        s.handle_key(key(KeyCode::Up, KeyModifiers::empty()));
        assert_eq!(s.buffer.cursor(), (0, 2));
    }

    #[test]
    fn vertical_desired_col_sticks_across_short_visual_row() {
        // Lines of differing length: desired column is preserved across a short
        // line and restored on the long one below it.
        let mut s = screen("longline\nx\nlongline");
        s.last_body_w.set(40); // no wrapping; visual rows == logical lines
        s.last_body_h.set(10);
        s.buffer.set_cursor(0, 6);
        s.handle_key(key(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(s.buffer.cursor(), (1, 1)); // clamped to short line
        s.handle_key(key(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(s.buffer.cursor(), (2, 6)); // desired col restored
    }

    #[test]
    fn down_onto_full_width_wrapped_row_does_not_skip_a_row() {
        // Regression: caret at the end of a line that exactly fills the width
        // (desired_vcol == w). Down must land on the FIRST wrapped row of the
        // next line, not skip to the second (the visual_to_cursor boundary snap).
        let mut s = screen("AAAAA\nBBBBBBBBBB");
        s.last_body_w.set(5);
        s.last_body_h.set(10);
        s.buffer.set_cursor(0, 5); // end of line 0, fills the width
        s.handle_key(key(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(s.buffer.cursor(), (1, 4));
        // And cursor_to_visual confirms it renders on line1's first wrapped row.
        let m = wrap_buffer(s.buffer.lines(), 5);
        let (vrow, _) = cursor_to_visual(&m, s.buffer.lines(), 1, 4);
        assert_eq!(vrow, 1);
    }

    #[test]
    fn horizontal_move_resets_desired_col() {
        let mut s = screen("longline\nx\nlongline");
        s.last_body_w.set(40);
        s.last_body_h.set(10);
        s.buffer.set_cursor(0, 6);
        s.handle_key(key(KeyCode::Left, KeyModifiers::empty())); // now col 5, desired reset
        s.handle_key(key(KeyCode::Down, KeyModifiers::empty()));
        s.handle_key(key(KeyCode::Down, KeyModifiers::empty()));
        assert_eq!(s.buffer.cursor(), (2, 5));
    }

    #[test]
    fn paste_event_inserts_multiline() {
        let mut s = screen("");
        s.paste("a\nb");
        assert_eq!(s.content(), "a\nb");
        assert_eq!(s.buffer.cursor(), (1, 1));
    }

    #[test]
    fn title_uses_compose_kind() {
        let s = EditorScreen::new(
            EditorPurpose::NewBody {
                kind: ComposeKind::NewEntry,
                prefill_topics: vec![],
            },
            "",
        );
        assert_eq!(s.title(), " cs-tui • new post ");
    }
}

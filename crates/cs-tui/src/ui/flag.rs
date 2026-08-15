//! The shared "why are you reporting this?" field behind every `F` binding.
//!
//! Four screens can file a report (the feed, a topic feed, a post's detail view
//! and cIRC), and v0.8.4 gives all of them the same optional free-text `reason`
//! (§ Flag an Entry, § Flag a Reply, § Flag a Message). This module owns the one
//! single-line field they share so the typing experience cannot drift apart:
//! the same cap, the same caret keys, the same paste handling, and the same rule
//! that a blank reason travels as an absent field rather than as `""`.
//!
//! What each screen keeps for itself is the *target* (which is why
//! [`FlagPrompt`] is generic over it) and the *presentation*, since the feeds
//! replace their status line with a two-row prompt while the post detail already
//! names its target on a status line it draws anyway.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme::Theme;

/// Maximum length of a flag reason, in characters (v0.8.4 § Flag an Entry:
/// "`reason` is optional, max 500 characters"). Typing stops at the cap rather
/// than letting a whole report bounce off the server.
pub const MAX_FLAG_REASON: usize = 500;

/// What a key press did to an open [`FlagPrompt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagPromptKey {
    /// The prompt took the key and stays open.
    Consumed,
    /// The user cancelled: close the prompt and file nothing.
    Cancelled,
    /// The user submitted: close the prompt and send the report.
    Submitted,
}

/// An open flag-reason prompt: what is being reported, and the reason so far.
///
/// The reason is optional, so submitting an empty prompt is a valid report with
/// no reason attached. One short sentence does not justify a whole editor
/// screen, so this is a single-line field drawn with
/// [`super::input::windowed_line`].
///
/// `T` is whatever the screen needs to identify the report's subject, captured
/// when `F` was pressed so the report can never drift onto another item while
/// the reason is being typed: a post id on the feeds, a post-or-reply choice on
/// the post detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagPrompt<T> {
    /// What the report is about.
    pub target: T,
    /// The reason typed so far, capped at [`MAX_FLAG_REASON`] characters.
    pub reason: String,
    /// Caret position as a char index into `reason` (`0..=` its char count).
    pub cursor: usize,
}

impl<T> FlagPrompt<T> {
    /// An empty prompt reporting `target`.
    #[must_use]
    pub fn new(target: T) -> Self {
        Self {
            target,
            reason: String::new(),
            cursor: 0,
        }
    }

    /// Characters typed so far (the cap is on characters, not bytes).
    #[must_use]
    pub fn len(&self) -> usize {
        self.reason.chars().count()
    }

    /// Whether nothing has been typed yet. Submitting in this state is valid.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reason.is_empty()
    }

    /// The reason to put on the wire: trimmed, or `None` when blank so the
    /// field is omitted rather than sent as an empty string.
    #[must_use]
    pub fn reason_to_send(&self) -> Option<String> {
        let reason = self.reason.trim();
        if reason.is_empty() {
            None
        } else {
            Some(reason.to_string())
        }
    }

    /// Insert bracketed-paste text at the caret, with newlines collapsed to
    /// spaces (the field is single-line, and a pasted newline must not submit).
    pub fn paste(&mut self, text: &str) {
        for c in super::input::collapse_newlines(text).chars() {
            self.insert(c);
        }
    }

    /// Apply a key press, reporting whether the prompt should stay open.
    pub fn handle_key(&mut self, key: KeyEvent) -> FlagPromptKey {
        match key.code {
            KeyCode::Enter => return FlagPromptKey::Submitted,
            KeyCode::Esc => return FlagPromptKey::Cancelled,
            KeyCode::Backspace if self.cursor > 0 => {
                let at = byte_index(&self.reason, self.cursor - 1);
                self.reason.remove(at);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < self.len() => {
                let at = byte_index(&self.reason, self.cursor);
                self.reason.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            // A modified letter is a shortcut, never text (Ctrl+C already quit
            // before we got here).
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.insert(c);
            }
            _ => {}
        }
        FlagPromptKey::Consumed
    }

    /// Insert one character at the caret, silently ignored once the reason is
    /// at [`MAX_FLAG_REASON`].
    fn insert(&mut self, c: char) {
        if self.len() >= MAX_FLAG_REASON {
            return;
        }
        let at = byte_index(&self.reason, self.cursor);
        self.reason.insert(at, c);
        self.cursor += 1;
    }
}

/// Byte offset of char index `at` in `s`, or `s.len()` when `at` is past the
/// end (so it can address the position just after the last character).
fn byte_index(s: &str, at: usize) -> usize {
    s.char_indices().nth(at).map_or(s.len(), |(i, _)| i)
}

/// Draw an open flag-reason prompt into `area`: a hint row carrying a live
/// character count, then the windowed single-line input.
///
/// This is the feeds' presentation, where the prompt takes over the status row.
/// The post detail draws its own single row instead, because the status line it
/// already has is where it names which of the post or the selected reply is
/// being reported.
pub fn render_flag_prompt<T>(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    prompt: &FlagPrompt<T>,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);
    // Spell out what a blank submit does: the reason is optional (v0.8.4 § Flag
    // an Entry), which an empty field on its own doesn't say.
    let submit = if prompt.is_empty() {
        "enter submits with no reason"
    } else {
        "enter submit"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                "flag reason (optional · {}/{MAX_FLAG_REASON}) · {submit} · esc cancel",
                prompt.len()
            ),
            theme.muted_style(),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(super::input::windowed_line(
            &prompt.reason,
            prompt.cursor,
            rows[1].width as usize,
            theme,
        )),
        rows[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn typed(prompt: &mut FlagPrompt<&'static str>, text: &str) {
        for c in text.chars() {
            prompt.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_blank_reason_travels_as_an_absent_field() {
        let mut p = FlagPrompt::new("post");
        assert!(p.is_empty());
        assert_eq!(p.reason_to_send(), None);
        typed(&mut p, "   ");
        assert_eq!(p.reason_to_send(), None, "whitespace only is still absent");
    }

    #[test]
    fn a_typed_reason_is_trimmed_before_it_is_sent() {
        let mut p = FlagPrompt::new("post");
        typed(&mut p, "  spam  ");
        assert_eq!(p.reason_to_send(), Some("spam".to_string()));
    }

    #[test]
    fn typing_stops_at_the_servers_cap() {
        let mut p = FlagPrompt::new("post");
        for _ in 0..(MAX_FLAG_REASON + 20) {
            p.handle_key(key(KeyCode::Char('x')));
        }
        assert_eq!(p.len(), MAX_FLAG_REASON);
        assert_eq!(p.cursor, MAX_FLAG_REASON);
    }

    #[test]
    fn the_caret_keys_edit_in_the_middle_of_the_reason() {
        let mut p = FlagPrompt::new("post");
        typed(&mut p, "spm");
        p.handle_key(key(KeyCode::Left));
        typed(&mut p, "a");
        assert_eq!(p.reason, "spam");
        p.handle_key(key(KeyCode::Home));
        typed(&mut p, ">");
        assert_eq!(p.reason, ">spam");
        p.handle_key(key(KeyCode::End));
        typed(&mut p, "!");
        assert_eq!(p.reason, ">spam!");
    }

    #[test]
    fn backspace_and_delete_act_on_opposite_sides_of_the_caret() {
        let mut p = FlagPrompt::new("post");
        typed(&mut p, "abcd");
        p.handle_key(key(KeyCode::Left));
        p.handle_key(key(KeyCode::Backspace));
        assert_eq!(p.reason, "abd", "backspace takes the char before the caret");
        p.handle_key(key(KeyCode::Delete));
        assert_eq!(p.reason, "ab", "delete takes the char under the caret");
    }

    #[test]
    fn multibyte_text_is_edited_by_character_not_by_byte() {
        let mut p = FlagPrompt::new("post");
        typed(&mut p, "héllo");
        assert_eq!(p.len(), 5);
        p.handle_key(key(KeyCode::Home));
        p.handle_key(key(KeyCode::Right));
        p.handle_key(key(KeyCode::Delete));
        assert_eq!(p.reason, "hllo");
    }

    #[test]
    fn paste_collapses_newlines_and_respects_the_caret() {
        let mut p = FlagPrompt::new("post");
        typed(&mut p, "ab");
        p.handle_key(key(KeyCode::Left));
        p.paste("x\ny");
        assert_eq!(p.reason, "ax yb");
    }

    #[test]
    fn enter_submits_and_escape_cancels() {
        let mut p = FlagPrompt::new("post");
        assert_eq!(p.handle_key(key(KeyCode::Enter)), FlagPromptKey::Submitted);
        assert_eq!(p.handle_key(key(KeyCode::Esc)), FlagPromptKey::Cancelled);
        assert_eq!(
            p.handle_key(key(KeyCode::Char('q'))),
            FlagPromptKey::Consumed,
            "a bare letter is text, not a shortcut"
        );
    }

    #[test]
    fn modified_letters_are_shortcuts_not_text() {
        let mut p = FlagPrompt::new("post");
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let mut k = key(KeyCode::Char('r'));
            k.modifiers = modifier;
            assert_eq!(p.handle_key(k), FlagPromptKey::Consumed);
        }
        assert!(p.is_empty(), "neither modifier inserted a character");
        let mut shifted = key(KeyCode::Char('R'));
        shifted.modifiers = KeyModifiers::SHIFT;
        p.handle_key(shifted);
        assert_eq!(p.reason, "R", "a shifted letter is still text");
    }
}

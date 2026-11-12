//! Shared single-line text-input rendering.
//!
//! Login, compose, and edit-profile all edit a short value inline. Without
//! windowing, a value longer than its field overflows and the caret can scroll
//! off-screen. [`windowed_line`] slides a fixed-width window so the caret is
//! always visible, drawing it as a reverse-video cell. Indexing is by `char`
//! (matching the screens' char-aware edit models); display-width handling for
//! wide glyphs (CJK/emoji) is a separate concern.
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// Collapse newlines and carriage returns to single spaces. Used when a
/// bracketed-paste (which may carry a multi-line clipboard) is inserted into a
/// single-line field, so the field stays on one line and a pasted newline can't
/// trigger an Enter/submit.
#[must_use]
pub fn collapse_newlines(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// Turn every CRLF and every lone CR into a plain `\n`, for text that keeps
/// its line breaks (the chat composers and the built-in editor).
///
/// A paste does not arrive with the line endings it was copied with. VTE
/// terminals (GNOME Terminal and its kin), Konsole and iTerm2 send each pasted
/// line break as a CR, which is what the Return key sends, and a Windows
/// clipboard can carry CRLF. crossterm hands the bytes over as they came. Left
/// alone, a CR is drawn as nothing, so the lines run together on one row, and
/// it is not the `\n` the API counts as a line break.
#[must_use]
pub fn normalize_line_breaks(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Byte offset of char index `at` in `s`, or `s.len()` when `at` is past the
/// end (so it can address the position just after the last character).
///
/// Every caret in the client is a char index, and every edit has to become a
/// byte offset before `String` will take it, so the conversion lives here rather
/// than once per field.
#[must_use]
pub fn byte_index(s: &str, at: usize) -> usize {
    s.char_indices().nth(at).map_or(s.len(), |(i, _)| i)
}

/// Render a focused input value into a `Line`, windowed to `width` cells so the
/// caret at char index `cursor` (`0..=len`) stays in view, with a reverse-video
/// block caret.
#[must_use]
pub fn windowed_line(text: &str, cursor: usize, width: usize, theme: &Theme) -> Line<'static> {
    let chars: Vec<char> = text.chars().collect();
    let w = width.max(1);
    let cur = cursor.min(chars.len());
    // Slide the window so the caret sits within [offset, offset + w).
    let offset = if cur < w { 0 } else { cur - w + 1 };
    let end = (offset + w).min(chars.len());

    let before: String = chars[offset..cur].iter().collect();
    let caret = theme.base().add_modifier(Modifier::REVERSED);
    let mut spans = vec![Span::styled(before, theme.base())];
    if cur < end {
        spans.push(Span::styled(chars[cur].to_string(), caret));
        let after: String = chars[cur + 1..end].iter().collect();
        if !after.is_empty() {
            spans.push(Span::styled(after, theme.base()));
        }
    } else {
        // Caret sits just past the last visible char.
        spans.push(Span::styled(" ", caret));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a line's spans into (text, is_caret) where caret = reversed.
    fn render(line: &Line<'_>) -> (String, usize) {
        let mut text = String::new();
        let mut caret_col = 0;
        for span in &line.spans {
            if span.style.add_modifier.contains(Modifier::REVERSED) {
                caret_col = text.chars().count();
            }
            text.push_str(span.content.as_ref());
        }
        (text, caret_col)
    }

    #[test]
    fn every_line_ending_becomes_a_plain_newline() {
        assert_eq!(normalize_line_breaks("a\rb\r\nc\nd"), "a\nb\nc\nd");
        // CRLF is one break, not two: the pair is replaced before lone CRs.
        assert_eq!(normalize_line_breaks("a\r\n\r\nb"), "a\n\nb");
        assert_eq!(normalize_line_breaks("plain"), "plain");
    }

    #[test]
    fn short_value_shows_everything_with_caret_at_cursor() {
        let theme = Theme::cyber();
        let (text, caret) = render(&windowed_line("hello", 2, 20, &theme));
        assert_eq!(text, "hello");
        assert_eq!(caret, 2); // caret over the 'l'
    }

    #[test]
    fn caret_past_end_appends_a_block() {
        let theme = Theme::cyber();
        let (text, caret) = render(&windowed_line("hi", 2, 20, &theme));
        assert_eq!(text, "hi ");
        assert_eq!(caret, 2);
    }

    #[test]
    fn long_value_windows_to_keep_the_caret_visible() {
        let theme = Theme::cyber();
        // 30 chars, width 10, cursor at the end → only the tail is shown and the
        // caret stays on-screen.
        let value: String = ('a'..='z').chain('A'..='Z').take(30).collect();
        let (text, caret) = render(&windowed_line(&value, value.chars().count(), 10, &theme));
        assert!(
            text.chars().count() <= 10,
            "windowed to field width: {text:?}"
        );
        assert!(caret < 10, "caret stays within the window");
        assert!(
            value.ends_with(text.trim_end()),
            "shows the tail near the caret"
        );
    }

    #[test]
    fn caret_in_the_middle_of_a_long_value_is_visible() {
        let theme = Theme::cyber();
        let value: String = "x".repeat(40);
        let (_, caret) = render(&windowed_line(&value, 25, 10, &theme));
        assert!(caret < 10, "mid-text caret is windowed into view");
    }
}

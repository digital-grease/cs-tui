//! Display-width-aware text truncation, shared by the list previews.
//!
//! Truncating by `char` count misplaces the cut for wide glyphs (CJK, many
//! emoji) — two-column characters make a "max chars" string render up to twice
//! as wide as intended, misaligning columns. These helpers budget by *display
//! width* instead.
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Truncate `s` to at most `max` display columns, appending `…` when it's cut.
///
/// A `max` of 0 yields an empty string: there is not even room for the
/// ellipsis, and returning one would overflow the caller's budget.
#[must_use]
pub fn truncate_to_width(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - 1; // leave a column for the ellipsis
    let mut out = String::new();
    let mut width = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > budget {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out.push('…');
    out
}

/// The first line of `s`, trimmed and width-truncated to `max` columns.
#[must_use]
pub fn first_line_truncated(s: &str, max: usize) -> String {
    truncate_to_width(s.lines().next().unwrap_or("").trim(), max)
}

/// The trailing marker for an entry or reply whose author has revised it, or
/// `""` when it is untouched.
///
/// v0.8.4 § Edit Entry and § Edit Reply both stamp `editedAt` on the item and
/// both leave `createdAt` alone, so the relative time beside it still reads as
/// the publish time and this marker is the only thing saying the text has moved
/// since. It rides in the muted metadata run right after that time, on every
/// screen that lists entries or replies, rather than spending width on a second
/// date. Shared so the four screens showing it cannot drift apart.
#[must_use]
pub fn edited_marker(edited_at: Option<time::OffsetDateTime>) -> &'static str {
    if edited_at.is_some() {
        " · (edited)"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_ascii_is_unchanged() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn long_ascii_truncates_with_ellipsis() {
        let out = truncate_to_width(&"x".repeat(20), 10);
        assert_eq!(out.chars().count(), 10); // 9 chars + …
        assert!(out.ends_with('…'));
    }

    #[test]
    fn wide_glyphs_budget_by_column_not_char_count() {
        // Each CJK char is 2 columns wide. With max=5 columns we fit 2 of them
        // (4 cols) plus the ellipsis — never overflowing the column budget.
        let out = truncate_to_width("日本語テスト", 5);
        assert!(
            out.width() <= 5,
            "must not exceed the column budget: {out:?}"
        );
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().filter(|c| *c != '…').count(), 2);
    }

    #[test]
    fn first_line_takes_only_the_first_line() {
        assert_eq!(first_line_truncated("first\nsecond", 100), "first");
        assert_eq!(first_line_truncated("  spaced  \nx", 100), "spaced");
    }

    #[test]
    fn the_edited_marker_tracks_the_edited_at_stamp() {
        assert_eq!(edited_marker(None), "");
        assert_eq!(
            edited_marker(Some(time::OffsetDateTime::UNIX_EPOCH)),
            " · (edited)"
        );
    }
}

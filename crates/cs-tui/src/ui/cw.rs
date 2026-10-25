//! Content warnings on entries (v0.9.0 § Create Entry).
//!
//! An entry with a `contentWarning` is shown as `CW: <warning>`, and its title,
//! body and attachments are revealed only on request. Every list that previews
//! entries goes through these helpers, so the feed, a topic, a profile, a
//! guild's threads and the bookmarks all mask the same way: the warning in
//! place of the title and the
//! snippet, no `[image]`/`[jukebox]` markers, no picture band, and no track for
//! `p`/`o` or the shuffle pool to pick up.
//!
//! The one place a warned entry can be revealed is the post detail screen
//! (`v`), which keeps its own reveal state. Opening a post is the request; a
//! list never shows what is behind a warning.
use cs_api::{Attachment, Entry};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// Whether a list should mask `entry` behind its content warning.
#[must_use]
pub fn masked(entry: &Entry) -> bool {
    entry.cw().is_some()
}

/// The attachments a list may act on for `entry`: all of them, or none while a
/// content warning masks it. Feed the jukebox helpers from this rather than
/// from `entry.attachments`, so `p`, `o` and the shuffle pool cannot play a
/// track the reader has not chosen to see.
#[must_use]
pub fn listed_attachments(entry: &Entry) -> &[Attachment] {
    if masked(entry) {
        &[]
    } else {
        &entry.attachments
    }
}

/// The `CW: <warning>` row a list draws in place of a masked entry's title and
/// snippet. `max` is a display-width budget for the warning text.
#[must_use]
pub fn cw_line(warning: &str, max: usize, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("CW: ", theme.warning_style().add_modifier(Modifier::BOLD)),
        Span::styled(
            super::text::first_line_truncated(warning, max),
            theme.warning_style(),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Attachment {
        Attachment::audio("https://youtu.be/abc", "A", "T", "g")
    }

    #[test]
    fn an_unwarned_entry_lists_its_attachments() {
        let e = Entry {
            attachments: vec![track()],
            ..Default::default()
        };
        assert!(!masked(&e));
        assert_eq!(listed_attachments(&e).len(), 1);
    }

    #[test]
    fn a_warned_entry_lists_nothing_to_play() {
        let e = Entry {
            content_warning: Some("spiders".into()),
            attachments: vec![track()],
            ..Default::default()
        };
        assert!(masked(&e));
        assert!(listed_attachments(&e).is_empty());
        assert!(super::super::audio::jukebox_track(listed_attachments(&e)).is_none());
    }

    #[test]
    fn a_blank_warning_does_not_mask() {
        let e = Entry {
            content_warning: Some("   ".into()),
            ..Default::default()
        };
        assert!(!masked(&e));
    }

    #[test]
    fn the_cw_line_reads_as_the_spec_puts_it() {
        let line = cw_line("spiders", 60, &Theme::cyber());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "CW: spiders");
    }
}

//! Top-level navigation: a tab bar and number-key shortcuts for switching between
//! root screens (Feed / Notifications / Bookmarks / Topics; Profile / Journal /
//! Settings join when their phases land).
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RootKind {
    Feed,
    Notifications,
    Bookmarks,
    Topics,
    Profile,
    Journal,
    Settings,
}

impl RootKind {
    #[must_use]
    pub fn all() -> &'static [RootKind] {
        &[
            Self::Feed,
            Self::Notifications,
            Self::Bookmarks,
            Self::Topics,
            Self::Profile,
            Self::Journal,
            Self::Settings,
        ]
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Notifications => "notif",
            Self::Bookmarks => "bookm",
            Self::Topics => "topic",
            Self::Profile => "me",
            Self::Journal => "journ",
            Self::Settings => "sett",
        }
    }

    #[must_use]
    pub fn shortcut(self) -> char {
        match self {
            Self::Feed => '1',
            Self::Notifications => '2',
            Self::Bookmarks => '3',
            Self::Topics => '4',
            Self::Profile => '5',
            Self::Journal => '6',
            Self::Settings => '7',
        }
    }

    #[must_use]
    pub fn from_shortcut(c: char) -> Option<Self> {
        match c {
            '1' => Some(Self::Feed),
            '2' => Some(Self::Notifications),
            '3' => Some(Self::Bookmarks),
            '4' => Some(Self::Topics),
            '5' => Some(Self::Profile),
            '6' => Some(Self::Journal),
            '7' => Some(Self::Settings),
            _ => None,
        }
    }
}

/// Render the top tab bar. `current` is highlighted; `unread_count` (>0) shows
/// next to the notifications tab.
pub fn render_tab_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    current: RootKind,
    unread_count: u32,
    theme: &Theme,
) {
    let mut spans: Vec<Span<'_>> = Vec::new();
    for (i, kind) in RootKind::all().iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", theme.muted_style()));
        }
        let active = current == *kind;
        let style = if active {
            theme.accent_style()
        } else {
            theme.muted_style()
        };
        let badge = if *kind == RootKind::Notifications && unread_count > 0 {
            format!(" ({unread_count})")
        } else {
            String::new()
        };
        spans.push(Span::styled(
            format!("{}·{}{}", kind.shortcut(), kind.label(), badge),
            style,
        ));
    }
    spans.push(Span::styled("    ", theme.muted_style()));
    spans.push(Span::styled("esc menu", theme.muted_style()));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_round_trips_for_all_kinds() {
        for kind in RootKind::all() {
            let c = kind.shortcut();
            assert_eq!(RootKind::from_shortcut(c), Some(*kind));
        }
    }

    #[test]
    fn unknown_shortcut_returns_none() {
        assert_eq!(RootKind::from_shortcut('x'), None);
        assert_eq!(RootKind::from_shortcut('0'), None);
        assert_eq!(RootKind::from_shortcut('9'), None);
    }

    #[test]
    fn labels_are_short() {
        for kind in RootKind::all() {
            assert!(kind.label().len() <= 6, "label {:?} too long", kind.label());
        }
    }
}

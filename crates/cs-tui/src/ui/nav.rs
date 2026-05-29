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
    Guilds,
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
            Self::Guilds,
        ]
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Feed => "Feed",
            Self::Notifications => "Notifications",
            Self::Bookmarks => "Bookmarks",
            Self::Topics => "Topics",
            Self::Profile => "Profile",
            Self::Journal => "Journal",
            Self::Settings => "Settings",
            Self::Guilds => "Guilds",
        }
    }

    /// The next section in tab-cycle order (wraps around).
    #[must_use]
    pub fn next(self) -> Self {
        let all = Self::all();
        let i = all.iter().position(|k| *k == self).unwrap_or(0);
        all[(i + 1) % all.len()]
    }

    /// The previous section in tab-cycle order (wraps around).
    #[must_use]
    pub fn prev(self) -> Self {
        let all = Self::all();
        let i = all.iter().position(|k| *k == self).unwrap_or(0);
        all[(i + all.len() - 1) % all.len()]
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
            Self::Guilds => '8',
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
            '8' => Some(Self::Guilds),
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
    fn labels_are_nonempty() {
        for kind in RootKind::all() {
            assert!(!kind.label().is_empty());
        }
    }

    #[test]
    fn next_and_prev_cycle_and_wrap() {
        assert_eq!(RootKind::Feed.next(), RootKind::Notifications);
        assert_eq!(RootKind::Guilds.next(), RootKind::Feed); // wraps
        assert_eq!(RootKind::Feed.prev(), RootKind::Guilds); // wraps
        assert_eq!(RootKind::Notifications.prev(), RootKind::Feed);
    }
}

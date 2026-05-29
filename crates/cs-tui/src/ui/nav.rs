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

const TAB_SEP: &str = " │ ";

fn styled_token(text: &str, active: bool, theme: &Theme) -> Span<'static> {
    let style = if active {
        theme.accent_style()
    } else {
        theme.muted_style()
    };
    Span::styled(text.to_string(), style)
}

/// Choose a contiguous window of section indices to show so that `current`
/// stays visible within `avail` columns. Returns `(lo, hi, clipped_left,
/// clipped_right)`; the window is `lo..hi`. Grows rightward first so upcoming
/// sections are revealed as you tab forward, then leftward near the end.
fn tab_window(widths: &[usize], sep_w: usize, cur: usize, avail: usize) -> (usize, usize, bool, bool) {
    let len = widths.len();
    if len == 0 {
        return (0, 0, false, false);
    }
    let cur = cur.min(len - 1);
    // Leave room for the ‹ / › scroll markers.
    let budget = avail.saturating_sub(4);
    let mut lo = cur;
    let mut hi = cur + 1;
    let mut used = widths[cur];
    loop {
        let can_right = hi < len && used + sep_w + widths[hi] <= budget;
        let can_left = lo > 0 && used + sep_w + widths[lo - 1] <= budget;
        if can_right {
            used += sep_w + widths[hi];
            hi += 1;
        } else if can_left {
            lo -= 1;
            used += sep_w + widths[lo];
        } else {
            break;
        }
    }
    (lo, hi, lo > 0, hi < len)
}

/// Render the top tab bar. `current` is highlighted; `unread_count` (>0) shows
/// next to the notifications tab. When the full bar doesn't fit, it scrolls
/// horizontally to keep the current section in view, marking clipped ends with
/// `‹`/`›`.
pub fn render_tab_bar(
    frame: &mut Frame<'_>,
    area: Rect,
    current: RootKind,
    unread_count: u32,
    theme: &Theme,
) {
    let kinds = RootKind::all();
    let tokens: Vec<String> = kinds
        .iter()
        .map(|k| {
            let badge = if *k == RootKind::Notifications && unread_count > 0 {
                format!(" ({unread_count})")
            } else {
                String::new()
            };
            format!("{}·{}{}", k.shortcut(), k.label(), badge)
        })
        .collect();
    let widths: Vec<usize> = tokens.iter().map(|t| t.chars().count()).collect();
    let sep_w = TAB_SEP.chars().count();
    let total = widths.iter().sum::<usize>() + sep_w * widths.len().saturating_sub(1);
    let avail = area.width as usize;
    let hint = "    esc menu";
    let hint_w = hint.chars().count();
    let cur = kinds.iter().position(|k| *k == current).unwrap_or(0);

    // Window of sections to show, and whether to append the hint.
    let (lo, hi, clip_l, clip_r) = if total <= avail {
        (0, tokens.len(), false, false)
    } else {
        tab_window(&widths, sep_w, cur, avail)
    };
    let with_hint = total + hint_w <= avail;

    let mut spans: Vec<Span<'_>> = Vec::new();
    if clip_l {
        spans.push(Span::styled("‹ ", theme.muted_style()));
    }
    for (offset, token) in tokens[lo..hi].iter().enumerate() {
        if offset > 0 {
            spans.push(Span::styled(TAB_SEP, theme.muted_style()));
        }
        spans.push(styled_token(token, lo + offset == cur, theme));
    }
    if clip_r {
        spans.push(Span::styled(" ›", theme.muted_style()));
    }
    if with_hint {
        spans.push(Span::styled(hint, theme.muted_style()));
    }
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

    #[test]
    fn tab_window_shows_all_when_it_fits() {
        let widths = vec![5usize; 4];
        let (lo, hi, l, r) = tab_window(&widths, 3, 2, 1000);
        assert_eq!((lo, hi), (0, 4));
        assert!(!l && !r);
    }

    #[test]
    fn tab_window_keeps_current_visible_when_narrow() {
        let widths = vec![10usize; 8];
        for cur in 0..8 {
            let (lo, hi, clip_l, clip_r) = tab_window(&widths, 3, cur, 40);
            assert!(lo <= cur && cur < hi, "current {cur} must be in {lo}..{hi}");
            assert!(hi - lo < 8, "should be a partial window when narrow");
            assert_eq!(clip_l, lo > 0);
            assert_eq!(clip_r, hi < 8);
        }
    }

    #[test]
    fn tab_window_marks_both_ends_when_scrolled_to_middle() {
        let widths = vec![10usize; 8];
        let (lo, hi, clip_l, clip_r) = tab_window(&widths, 3, 4, 30);
        assert!(lo <= 4 && 4 < hi);
        assert!(clip_l && clip_r, "middle window should clip both ends");
    }
}

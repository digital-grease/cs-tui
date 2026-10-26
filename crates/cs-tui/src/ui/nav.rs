//! Top-level navigation: the tab bar and the keys that move between root
//! screens.
//!
//! Sections are cycled with the left and right arrows, and nothing else. They
//! used to carry number keys too, `1`-`0` for ten sections, which stopped
//! scaling the moment there were eleven: the digits ran out, and picking which
//! section went without one would have been arbitrary. Dropping them altogether
//! keeps one way to move rather than one-and-a-partial, and hands every digit
//! back to the screens, where they can mean something local.
use cs_api::UnreadCount;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RootKind {
    Feed,
    Notifications,
    Cmail,
    Circ,
    Bookmarks,
    Topics,
    Profile,
    Journal,
    Guilds,
    /// The program registry (v0.8.10 § Programs).
    Programs,
    Settings,
}

impl RootKind {
    #[must_use]
    pub fn all() -> &'static [RootKind] {
        &[
            Self::Feed,
            Self::Notifications,
            Self::Cmail,
            Self::Circ,
            Self::Bookmarks,
            Self::Topics,
            Self::Profile,
            Self::Journal,
            Self::Guilds,
            Self::Programs,
            Self::Settings,
        ]
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Feed => "Feed",
            Self::Notifications => "Notifications",
            Self::Cmail => "C-Mail",
            Self::Circ => "cIRC",
            Self::Bookmarks => "Bookmarks",
            Self::Topics => "Topics",
            Self::Profile => "Profile",
            Self::Journal => "Journal",
            Self::Guilds => "Guilds",
            Self::Programs => "Programs",
            Self::Settings => "Settings",
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
fn tab_window(
    widths: &[usize],
    sep_w: usize,
    cur: usize,
    avail: usize,
) -> (usize, usize, bool, bool) {
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

/// Render the top tab bar. `current` is highlighted; unread counts (>0) show
/// next to their tabs. When the full bar doesn't fit, it scrolls horizontally to
/// keep the current section in view, marking clipped ends with `‹`/`›`. When
/// `offline` is set, a red marker replaces the right-hand hint so the connection
/// loss is always visible. Otherwise a newer release, when one is known, keeps a
/// badge at the right-hand end for the whole session.
#[derive(Debug, Clone, Copy)]
pub struct TabBarStatus<'a> {
    pub current: RootKind,
    /// Unread notifications (API v0.8.6 § Unread Count). The whole figure, not
    /// just the number: past 100 the server counts only the 100 most recent and
    /// the badge has to say "99+", which [`UnreadCount::badge`] decides.
    pub unread_count: UnreadCount,
    pub cmail_unread_count: u32,
    /// Messages that arrived in a backgrounded cIRC room.
    ///
    /// Scoped to the one room the reader left open, not to every room they
    /// could join: `GET /v1/circ` carries no per-room unread count, so an
    /// all-rooms badge is not something the API can support yet.
    pub circ_unread_count: usize,
    pub can_go_back: bool,
    pub offline: bool,
    /// The version of a newer cs-tui release, when the update check found one.
    pub update: Option<&'a str>,
}

/// The update badge's two sizes: the full one when the bar has room for it
/// beside the hint, the short one otherwise.
fn update_badges(version: &str) -> (String, String) {
    (format!(" ↑ {version} available "), format!(" ↑ {version} "))
}

/// The parenthesised badge for a tab, or nothing when there is nothing to
/// report. `text` is already rendered, so a capped notification count arrives
/// here as "99+" rather than as a number to format.
fn badge(any: bool, text: &str) -> String {
    if any {
        format!(" ({text})")
    } else {
        String::new()
    }
}

pub fn render_tab_bar(frame: &mut Frame<'_>, area: Rect, status: TabBarStatus<'_>, theme: &Theme) {
    let kinds = RootKind::all();
    let tokens: Vec<String> = kinds
        .iter()
        .map(|k| {
            let unread = status.unread_count;
            let cmail = status.cmail_unread_count;
            let badge = match *k {
                RootKind::Notifications => badge(unread.any(), &unread.badge()),
                RootKind::Cmail => badge(cmail > 0, &cmail.to_string()),
                RootKind::Circ => badge(
                    status.circ_unread_count > 0,
                    &status.circ_unread_count.to_string(),
                ),
                _ => String::new(),
            };
            format!("{}{}", k.label(), badge)
        })
        .collect();
    let widths: Vec<usize> = tokens.iter().map(|t| t.chars().count()).collect();
    let sep_w = TAB_SEP.chars().count();
    let total = widths.iter().sum::<usize>() + sep_w * widths.len().saturating_sub(1);
    let avail = area.width as usize;
    let hint = if status.can_go_back {
        "    esc back · ? help"
    } else {
        "    esc menu · ? help"
    };
    let hint_w = hint.chars().count();
    let cur = kinds.iter().position(|k| *k == status.current).unwrap_or(0);

    // The update badge takes its width off the end before the tabs are laid
    // out, so it never covers one. Offline wins the corner outright, and a bar
    // too narrow for the badge and the current tab drops the badge.
    let update_badge = status.update.filter(|_| !status.offline).and_then(|v| {
        let (long, short) = update_badges(v);
        let pick = if total + hint_w + long.chars().count() <= avail {
            long
        } else {
            short
        };
        (pick.chars().count() + widths[cur] + 4 <= avail).then_some(pick)
    });
    let badge_w = update_badge.as_ref().map_or(0, |b| b.chars().count());
    let avail = avail - badge_w;

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
    if with_hint && !status.offline {
        spans.push(Span::styled(hint, theme.muted_style()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    // A dropped connection is the most important thing on the bar, so it's
    // painted as a right-aligned overlay *over* the tabs — guaranteeing it shows
    // even when the tab list already fills the width.
    if status.offline {
        let marker = " ⚠ offline ";
        let mw = (marker.chars().count() as u16).min(area.width);
        let mrect = Rect::new(area.x + area.width - mw, area.y, mw, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(marker, theme.error_style()))),
            mrect,
        );
    } else if let Some(badge) = update_badge {
        let bw = badge_w as u16;
        let brect = Rect::new(area.x + area.width - bw, area.y, bw, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(badge, theme.warning_style()))),
            brect,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_reachable_by_cycling() {
        // The arrows are the only way between sections now, so `next` has to
        // visit every one of them before coming back round. A section left out
        // of `all()` would be unreachable rather than merely unlabelled.
        let mut seen = vec![RootKind::Feed];
        let mut kind = RootKind::Feed;
        for _ in 0..RootKind::all().len() {
            kind = kind.next();
            if kind == RootKind::Feed {
                break;
            }
            seen.push(kind);
        }
        assert_eq!(
            seen.len(),
            RootKind::all().len(),
            "cycling visited {seen:?}, which is not every section"
        );
        assert_eq!(kind, RootKind::Feed, "the cycle has to close");
    }

    #[test]
    fn no_section_label_carries_a_key_prefix() {
        // The rendered bar is asserted in app.rs
        // (`the_tab_bar_draws_section_names_and_no_key_prefixes`), which is
        // where the `N·` prefix actually lived. This only pins the labels.
        for kind in RootKind::all() {
            assert!(
                !kind.label().contains('·'),
                "{kind:?} still carries a key prefix"
            );
            assert!(
                !kind.label().chars().any(|c| c.is_ascii_digit()),
                "{kind:?} still carries a digit"
            );
        }
    }

    #[test]
    fn a_capped_unread_count_badges_as_99_plus() {
        // § Unread Count: past 100 unread the server counts only the 100 most
        // recent, so the number would read as an exact total it is not.
        let capped = UnreadCount {
            count: 100,
            exact: false,
        };
        assert_eq!(badge(capped.any(), &capped.badge()), " (99+)");

        let exact = UnreadCount {
            count: 7,
            exact: true,
        };
        assert_eq!(badge(exact.any(), &exact.badge()), " (7)");

        let none = UnreadCount::default();
        assert_eq!(
            badge(none.any(), &none.badge()),
            "",
            "an empty inbox gets no badge at all"
        );
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
        assert_eq!(RootKind::Settings.next(), RootKind::Feed); // wraps
        assert_eq!(RootKind::Feed.prev(), RootKind::Settings); // wraps
        assert_eq!(RootKind::Notifications.prev(), RootKind::Feed);
        // Programs sits between Guilds and Settings.
        assert_eq!(RootKind::Guilds.next(), RootKind::Programs);
        assert_eq!(RootKind::Settings.prev(), RootKind::Programs);
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

    fn bar_text(width: u16, status: TabBarStatus<'_>) -> String {
        let backend = ratatui::backend::TestBackend::new(width, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        let theme = Theme::default();
        terminal
            .draw(|f| render_tab_bar(f, f.area(), status, &theme))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn status(update: Option<&str>, offline: bool) -> TabBarStatus<'_> {
        TabBarStatus {
            current: RootKind::Feed,
            unread_count: UnreadCount::default(),
            cmail_unread_count: 0,
            circ_unread_count: 0,
            can_go_back: false,
            offline,
            update,
        }
    }

    #[test]
    fn a_known_update_badges_the_right_hand_end() {
        let wide = bar_text(400, status(Some("9.9.9"), false));
        assert!(
            wide.trim_end().ends_with("↑ 9.9.9 available"),
            "full badge at the end: {wide}"
        );
        assert!(
            wide.contains("esc menu"),
            "the hint stays beside it: {wide}"
        );

        let narrow = bar_text(60, status(Some("9.9.9"), false));
        assert!(
            narrow.trim_end().ends_with("↑ 9.9.9"),
            "short badge when there is no room for the long one: {narrow}"
        );
        assert!(
            narrow.contains("Feed"),
            "the current tab is not covered: {narrow}"
        );

        let none = bar_text(400, status(None, false));
        assert!(!none.contains('↑'), "no release, no badge: {none}");
    }

    #[test]
    fn offline_takes_the_corner_from_the_update_badge() {
        let bar = bar_text(400, status(Some("9.9.9"), true));
        assert!(bar.contains("offline"), "{bar}");
        assert!(!bar.contains('↑'), "{bar}");
    }

    #[test]
    fn a_bar_too_narrow_for_the_badge_drops_it() {
        let bar = bar_text(12, status(Some("9.9.9"), false));
        assert!(!bar.contains('↑'), "{bar}");
        assert!(bar.contains("Feed"), "{bar}");
    }

    #[test]
    fn tab_window_marks_both_ends_when_scrolled_to_middle() {
        let widths = vec![10usize; 8];
        let (lo, hi, clip_l, clip_r) = tab_window(&widths, 3, 4, 30);
        assert!(lo <= 4 && 4 < hi);
        assert!(clip_l && clip_r, "middle window should clip both ends");
    }
}

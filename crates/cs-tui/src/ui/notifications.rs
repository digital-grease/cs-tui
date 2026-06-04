//! Notifications screen.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Notification, NotificationType, NotificationsFilter};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::list::{self, TabState};
use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationsIntent {
    /// Load next cursor page.
    LoadMore,
    /// Re-fetch from scratch.
    Refresh,
    /// Cycle the read filter (all → unread → read → all).
    ToggleFilter,
    /// Mark the selected notification as read.
    MarkSelectedRead {
        notification_id: String,
    },
    /// Mark every unread notification as read.
    MarkAllRead,
    /// Navigate to the post referenced by the selected notification (if any).
    OpenSelected {
        post_id: String,
        highlight_reply_id: Option<String>,
    },
    Quit,
    None,
}

/// Server-side type buckets, cycled with `t`. Each maps to a set of
/// [`NotificationType`]s passed to the list endpoint's `type` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifTypeFilter {
    All,
    Mentions,
    Replies,
    Social,
    System,
}

impl NotifTypeFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Mentions,
            Self::Mentions => Self::Replies,
            Self::Replies => Self::Social,
            Self::Social => Self::System,
            Self::System => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mentions => "mentions",
            Self::Replies => "replies",
            Self::Social => "social",
            Self::System => "system",
        }
    }

    /// The notification types this bucket selects (empty = no type filter).
    fn types(self) -> Vec<NotificationType> {
        use NotificationType::*;
        match self {
            Self::All => vec![],
            Self::Mentions => vec![PostMention, ReplyMention, ChatMention, DmMessage],
            Self::Replies => vec![Reply, ThreadReply],
            Self::Social => vec![
                NewFollower,
                Unfollowed,
                Poke,
                NewPostFollowing,
                NewPostFriend,
                Bookmark,
                GuildNewThread,
            ],
            Self::System => vec![
                SupporterGranted,
                SupporterRemoved,
                HackerGranted,
                HackerRemoved,
                ImagePermissionGranted,
                ImagePermissionRemoved,
                AttachmentPermissionGranted,
                AttachmentPermissionRemoved,
                SystemBan,
            ],
        }
    }
}

#[derive(Debug)]
pub struct NotificationsScreen {
    pub list: TabState<Notification>,
    pub filter: NotificationsFilter,
    pub type_filter: NotifTypeFilter,
}

impl NotificationsScreen {
    pub fn new() -> Self {
        Self {
            list: TabState::loading(),
            filter: NotificationsFilter::All,
            type_filter: NotifTypeFilter::All,
        }
    }

    /// The notification types currently selected by the `t` filter.
    #[must_use]
    pub fn selected_types(&self) -> Vec<NotificationType> {
        self.type_filter.types()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> NotificationsIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return NotificationsIntent::Quit;
        }

        // Always-available actions (don't gate on loading): filter, mark, open.
        match key.code {
            KeyCode::Char('f') => {
                self.filter = match self.filter {
                    NotificationsFilter::All => NotificationsFilter::Unread,
                    NotificationsFilter::Unread => NotificationsFilter::Read,
                    NotificationsFilter::Read => NotificationsFilter::All,
                };
                self.list.items.clear();
                self.list.next_cursor = None;
                self.list.selected = 0;
                self.list.loading = true;
                self.list.error = None;
                return NotificationsIntent::ToggleFilter;
            }
            KeyCode::Char('t') => {
                // Cycle the server-side type bucket; reload like the read filter.
                self.type_filter = self.type_filter.next();
                self.list.items.clear();
                self.list.next_cursor = None;
                self.list.selected = 0;
                self.list.loading = true;
                self.list.error = None;
                return NotificationsIntent::ToggleFilter;
            }
            KeyCode::Char('m') => {
                // Only mark when actually unread: marking an already-read item
                // would burn a rate-limited write and wrongly decrement the
                // global unread count.
                if let Some(n) = self.list.items.get(self.list.selected) {
                    if !n.read {
                        return NotificationsIntent::MarkSelectedRead {
                            notification_id: n.notification_id.clone(),
                        };
                    }
                }
            }
            KeyCode::Char('M') => return NotificationsIntent::MarkAllRead,
            KeyCode::Enter => {
                if let Some(n) = self.list.items.get(self.list.selected) {
                    if let Some(post_id) = &n.target_id {
                        return NotificationsIntent::OpenSelected {
                            post_id: post_id.clone(),
                            highlight_reply_id: n.reply_id.clone(),
                        };
                    }
                }
            }
            _ => {}
        }

        // Movement + load keys are gated on not-currently-loading so a single
        // press doesn't queue duplicate fetches.
        if self.list.loading {
            return NotificationsIntent::None;
        }
        match super::list_nav::navigate(
            key.code,
            &mut self.list.selected,
            self.list.items.len(),
            self.list.next_cursor.is_some(),
        ) {
            super::list_nav::ListNav::LoadMore => {
                self.list.loading = true;
                return NotificationsIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return NotificationsIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        if key.code == KeyCode::Char('r') {
            self.list.items.clear();
            self.list.next_cursor = None;
            self.list.selected = 0;
            self.list.loading = true;
            self.list.error = None;
            return NotificationsIntent::Refresh;
        }
        NotificationsIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Notification>, Option<String>), String>) {
        self.list.apply_initial(result);
    }

    pub fn apply_more(&mut self, result: Result<(Vec<Notification>, Option<String>), String>) {
        self.list.apply_more(result);
    }

    /// Optimistically mark a single notification as read in local state.
    pub fn mark_local(&mut self, notification_id: &str) {
        for n in &mut self.list.items {
            if n.notification_id == notification_id {
                n.read = true;
                break;
            }
        }
    }

    /// Undo a `mark_local` when the server rejected the mark.
    pub fn unmark_local(&mut self, notification_id: &str) {
        for n in &mut self.list.items {
            if n.notification_id == notification_id {
                n.read = false;
                break;
            }
        }
    }

    /// Optimistically mark every notification as read in local state.
    pub fn mark_all_local(&mut self) {
        for n in &mut self.list.items {
            n.read = true;
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let read = match self.filter {
            NotificationsFilter::All => "",
            NotificationsFilter::Unread => " · unread",
            NotificationsFilter::Read => " · read",
        };
        let typ = if self.type_filter == NotifTypeFilter::All {
            String::new()
        } else {
            format!(" · {}", self.type_filter.label())
        };
        let title = format!(" cs-tui • notifications{read}{typ} ");
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let visible: Vec<usize> = (0..self.list.items.len()).collect();
        list::render_body(
            frame,
            layout[0],
            theme,
            &self.list,
            &visible,
            "no notifications",
            |n| notification_item(n, theme),
        );

        let status = status_line(self, theme);
        frame.render_widget(status, layout[1]);
    }
}

impl Default for NotificationsScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn notification_item(n: &Notification, theme: &Theme) -> ListItem<'static> {
    let actor = n
        .actor
        .as_ref()
        .map(|a| a.username.as_str())
        .unwrap_or("system");
    let when = n
        .created_at
        .map(crate::config::format_list_timestamp)
        .unwrap_or_default();
    let unread_marker = if n.read {
        Span::styled("  ", theme.muted_style())
    } else {
        Span::styled("● ", theme.accent_style())
    };
    let summary = summarize(n, actor);
    let header = Line::from(vec![
        unread_marker,
        Span::styled(summary, theme.base()),
        Span::styled(format!(" · {when}"), theme.muted_style()),
    ]);
    let mut lines = vec![header];
    if !crate::config::get().compact {
        lines.push(Line::from(""));
    }
    ListItem::new(lines)
}

fn summarize(n: &Notification, actor: &str) -> String {
    use NotificationType::*;
    match n.kind {
        Bookmark => format!("@{actor} bookmarked your post"),
        Reply => format!("@{actor} replied to your post"),
        ThreadReply => {
            let thread = n.thread_author_username.as_deref().unwrap_or("a thread");
            format!("@{actor} replied in @{thread}'s thread")
        }
        ReplyMention => format!("@{actor} mentioned you in a reply"),
        PostMention => format!("@{actor} mentioned you in a post"),
        ChatMention => format!("@{actor} mentioned you in chat"),
        DmMessage => format!("@{actor} sent you a DM"),
        NewFollower => format!("@{actor} followed you"),
        Unfollowed => format!("@{actor} unfollowed you"),
        NewPostFollowing => format!("@{actor} posted (from following)"),
        NewPostFriend => format!("@{actor} posted (from friends)"),
        Poke => format!("@{actor} poked you"),
        GuildNewThread => {
            let guild = n.guild_name.as_deref().unwrap_or("a guild");
            format!("new thread in {guild} by @{actor}")
        }
        SupporterGranted => "supporter status granted".to_string(),
        SupporterRemoved => "supporter status removed".to_string(),
        HackerGranted => "hacker status granted".to_string(),
        HackerRemoved => "hacker status removed".to_string(),
        ImagePermissionGranted => "image-upload permission granted".to_string(),
        ImagePermissionRemoved => "image-upload permission removed".to_string(),
        AttachmentPermissionGranted => "attachment permission granted".to_string(),
        AttachmentPermissionRemoved => "attachment permission removed".to_string(),
        SystemBan => "your account has been banned".to_string(),
        Unknown => format!("notification from @{actor}"),
    }
}

fn status_line<'a>(s: &'a NotificationsScreen, theme: &Theme) -> Paragraph<'a> {
    if let Some(msg) = list::load_more_error(&s.list) {
        return Paragraph::new(Line::from(Span::styled(msg, theme.error_style())));
    }
    let text = if s.list.loading {
        "loading… · enter open · m mark · M mark-all · f read · t type · r refresh · esc menu"
            .to_string()
    } else if s.list.next_cursor.is_some() {
        format!(
            "{} items · scroll down for more · enter open · m mark · M mark-all · f read · t type · r refresh · esc menu",
            s.list.items.len()
        )
    } else {
        format!(
            "{} items · end · enter open · m mark · M mark-all · f read · t type · r refresh · esc menu",
            s.list.items.len()
        )
    };
    Paragraph::new(Line::from(Span::styled(text, theme.muted_style())))
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
            state: KeyEventState::empty(),
        }
    }

    fn notif(
        id: &str,
        kind: NotificationType,
        target: Option<&str>,
        reply: Option<&str>,
    ) -> Notification {
        Notification {
            notification_id: id.into(),
            kind,
            read: false,
            created_at: None,
            actor: None,
            target_id: target.map(String::from),
            target_type: target.map(|_| "post".to_string()),
            reply_id: reply.map(String::from),
            thread_author_username: None,
            guild_name: None,
        }
    }

    #[test]
    fn new_starts_loading() {
        let s = NotificationsScreen::new();
        assert!(s.list.loading);
        assert!(s.list.items.is_empty());
    }

    #[test]
    fn apply_initial_populates_and_clears_loading() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![notif("n1", NotificationType::Reply, Some("p1"), Some("r1"))],
            Some("cur".into()),
        )));
        assert!(!s.list.loading);
        assert_eq!(s.list.items.len(), 1);
        assert_eq!(s.list.next_cursor.as_deref(), Some("cur"));
    }

    #[test]
    fn j_advances_selection_bounded() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![
                notif("a", NotificationType::Poke, None, None),
                notif("b", NotificationType::Poke, None, None),
            ],
            None,
        )));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.list.selected, 1);
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.list.selected, 1);
    }

    #[test]
    fn j_at_bottom_auto_loads() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![notif("a", NotificationType::Poke, None, None)],
            Some("next".into()),
        )));
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, NotificationsIntent::LoadMore);
        assert!(s.list.loading);
    }

    #[test]
    fn enter_opens_when_target_present() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![notif("n1", NotificationType::Reply, Some("p1"), Some("r1"))],
            None,
        )));
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            NotificationsIntent::OpenSelected {
                post_id: "p1".into(),
                highlight_reply_id: Some("r1".into()),
            }
        );
    }

    #[test]
    fn enter_with_no_target_yields_none() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![notif("n1", NotificationType::Poke, None, None)],
            None,
        )));
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, NotificationsIntent::None);
    }

    #[test]
    fn m_marks_selected() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![notif("n1", NotificationType::Reply, None, None)],
            None,
        )));
        let intent = s.handle_key(key(KeyCode::Char('m')));
        assert_eq!(
            intent,
            NotificationsIntent::MarkSelectedRead {
                notification_id: "n1".into()
            }
        );
    }

    #[test]
    fn m_on_already_read_is_a_noop() {
        // Marking an already-read item would waste a rate-limited write and
        // wrongly decrement the global unread count.
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![notif("n1", NotificationType::Reply, None, None)],
            None,
        )));
        s.mark_local("n1"); // now read
        assert_eq!(s.handle_key(key(KeyCode::Char('m'))), NotificationsIntent::None);
    }

    #[test]
    fn capital_m_marks_all() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((vec![], None)));
        let key_m = KeyEvent {
            code: KeyCode::Char('M'),
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(s.handle_key(key_m), NotificationsIntent::MarkAllRead);
    }

    #[test]
    fn f_cycles_filter() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((vec![], None)));
        assert!(matches!(s.filter, NotificationsFilter::All));
        s.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(s.filter, NotificationsFilter::Unread));
        s.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(s.filter, NotificationsFilter::Read));
        s.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(s.filter, NotificationsFilter::All));
    }

    #[test]
    fn t_cycles_type_filter_and_reloads() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((vec![], None)));
        assert!(matches!(s.type_filter, NotifTypeFilter::All));
        assert!(s.selected_types().is_empty());

        let intent = s.handle_key(key(KeyCode::Char('t')));
        assert_eq!(intent, NotificationsIntent::ToggleFilter);
        assert!(matches!(s.type_filter, NotifTypeFilter::Mentions));
        assert!(s.selected_types().contains(&NotificationType::PostMention));
        assert!(s.list.loading, "changing the type filter triggers a reload");
    }

    #[test]
    fn type_filter_wraps_back_to_all() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((vec![], None)));
        for _ in 0..5 {
            s.handle_key(key(KeyCode::Char('t')));
            s.list.loading = false; // let the next press through
        }
        assert!(matches!(s.type_filter, NotifTypeFilter::All));
    }

    #[test]
    fn mark_local_flips_read_flag() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![
                notif("a", NotificationType::Poke, None, None),
                notif("b", NotificationType::Poke, None, None),
            ],
            None,
        )));
        s.mark_local("a");
        assert!(s.list.items[0].read);
        assert!(!s.list.items[1].read);
    }

    #[test]
    fn mark_all_local_flips_every_record() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![
                notif("a", NotificationType::Poke, None, None),
                notif("b", NotificationType::Poke, None, None),
            ],
            None,
        )));
        s.mark_all_local();
        assert!(s.list.items.iter().all(|n| n.read));
    }

    #[test]
    fn summary_includes_actor_for_reply() {
        let actor_n = Notification {
            notification_id: "n".into(),
            kind: NotificationType::Reply,
            read: false,
            created_at: None,
            actor: Some(cs_api::NotificationActor {
                id: "u".into(),
                username: "alice".into(),
            }),
            target_id: None,
            target_type: None,
            reply_id: None,
            thread_author_username: None,
            guild_name: None,
        };
        let s = summarize(&actor_n, "alice");
        assert!(s.contains("@alice"));
        assert!(s.contains("replied"));
    }
}

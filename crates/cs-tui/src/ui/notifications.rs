//! Notifications screen.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Notification, NotificationType, NotificationsFilter};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

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

#[derive(Debug)]
pub struct NotificationsScreen {
    pub items: Vec<Notification>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub filter: NotificationsFilter,
}

impl NotificationsScreen {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: true,
            error: None,
            filter: NotificationsFilter::All,
        }
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
                self.items.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return NotificationsIntent::ToggleFilter;
            }
            KeyCode::Char('m') => {
                if let Some(n) = self.items.get(self.selected) {
                    return NotificationsIntent::MarkSelectedRead {
                        notification_id: n.notification_id.clone(),
                    };
                }
            }
            KeyCode::Char('M') => return NotificationsIntent::MarkAllRead,
            KeyCode::Enter => {
                if let Some(n) = self.items.get(self.selected) {
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
        if self.loading {
            return NotificationsIntent::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down
                if !self.items.is_empty() && self.selected < self.items.len() - 1 =>
            {
                self.selected += 1;
            }
            // At the bottom, scrolling down pulls the next page automatically.
            KeyCode::Char('j') | KeyCode::Down if self.next_cursor.is_some() => {
                self.loading = true;
                return NotificationsIntent::LoadMore;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End if !self.items.is_empty() => {
                self.selected = self.items.len() - 1;
            }
            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::PageDown
                if self.next_cursor.is_some() =>
            {
                self.loading = true;
                return NotificationsIntent::LoadMore;
            }
            KeyCode::Char('r') => {
                self.items.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return NotificationsIntent::Refresh;
            }
            _ => {}
        }
        NotificationsIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Notification>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((items, cursor)) => {
                self.items = items;
                self.next_cursor = cursor;
                if self.selected >= self.items.len() {
                    self.selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_more(&mut self, result: Result<(Vec<Notification>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((mut items, cursor)) => {
                self.items.append(&mut items);
                self.next_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    /// Optimistically mark a single notification as read in local state.
    pub fn mark_local(&mut self, notification_id: &str) {
        for n in &mut self.items {
            if n.notification_id == notification_id {
                n.read = true;
                break;
            }
        }
    }

    /// Optimistically mark every notification as read in local state.
    pub fn mark_all_local(&mut self) {
        for n in &mut self.items {
            n.read = true;
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let title = match self.filter {
            NotificationsFilter::All => " cs-tui • notifications ",
            NotificationsFilter::Unread => " cs-tui • notifications · unread ",
            NotificationsFilter::Read => " cs-tui • notifications · read ",
        };
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

        if self.loading && self.items.is_empty() {
            let para = Paragraph::new(Line::from(Span::styled(
                "loading notifications…",
                theme.accent_style(),
            )));
            frame.render_widget(para, layout[0]);
        } else if let Some(msg) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style()))),
                layout[0],
            );
        } else if self.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "no notifications",
                    theme.muted_style(),
                ))),
                layout[0],
            );
        } else {
            let items: Vec<ListItem<'_>> = self
                .items
                .iter()
                .map(|n| notification_item(n, theme))
                .collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(self.selected.min(self.items.len().saturating_sub(1))));
            frame.render_stateful_widget(list, layout[0], &mut state);
        }

        let status = status_line(self, theme);
        frame.render_widget(status, layout[1]);
    }
}

impl Default for NotificationsScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn notification_item<'a>(n: &'a Notification, theme: &Theme) -> ListItem<'a> {
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
    ListItem::new(vec![header])
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
    let text = if s.loading {
        "loading… · enter open · m mark · M mark-all · f filter · r refresh · esc menu"
            .to_string()
    } else if s.next_cursor.is_some() {
        format!(
            "{} items · scroll down for more · enter open · m mark · M mark-all · f filter · r refresh · esc menu",
            s.items.len()
        )
    } else {
        format!(
            "{} items · end · enter open · m mark · M mark-all · f filter · r refresh · esc menu",
            s.items.len()
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
        assert!(s.loading);
        assert!(s.items.is_empty());
    }

    #[test]
    fn apply_initial_populates_and_clears_loading() {
        let mut s = NotificationsScreen::new();
        s.apply_initial(Ok((
            vec![notif("n1", NotificationType::Reply, Some("p1"), Some("r1"))],
            Some("cur".into()),
        )));
        assert!(!s.loading);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.next_cursor.as_deref(), Some("cur"));
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
        assert_eq!(s.selected, 1);
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 1);
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
        assert!(s.loading);
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
        assert!(s.items[0].read);
        assert!(!s.items[1].read);
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
        assert!(s.items.iter().all(|n| n.read));
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

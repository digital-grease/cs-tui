//! C-Mail screen — private 1:1 conversations.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{CmailConversation, CmailMessage};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::list::{self, TabState};
use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmailIntent {
    RefreshConversations,
    OpenConversation {
        conversation_id: String,
    },
    LoadOlder {
        conversation_id: String,
        before: Option<i64>,
    },
    StartNew,
    SubmitNew {
        username: String,
    },
    CancelInput,
    BackToConversations,
    StartCompose {
        conversation_id: String,
    },
    SendMessage {
        conversation_id: String,
        content: String,
    },
    Quit,
    None,
}

#[derive(Debug)]
pub enum CmailMode {
    Conversations,
    Starting {
        username: String,
    },
    Conversation {
        conversation: CmailConversation,
        messages: TabState<CmailMessage>,
    },
    ConfirmSend {
        conversation_id: String,
        conversation: CmailConversation,
        messages: TabState<CmailMessage>,
        content: String,
        error: Option<String>,
    },
}

#[derive(Debug)]
pub struct CmailScreen {
    pub conversations: TabState<CmailConversation>,
    pub mode: CmailMode,
}

impl CmailScreen {
    pub fn new() -> Self {
        Self {
            conversations: TabState::loading(),
            mode: CmailMode::Conversations,
        }
    }

    pub fn is_text_input(&self) -> bool {
        matches!(self.mode, CmailMode::Starting { .. })
    }

    pub fn paste_text(&mut self, text: &str) {
        let cleaned = super::input::collapse_newlines(text);
        if let CmailMode::Starting { username } = &mut self.mode {
            username.push_str(&cleaned);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> CmailIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return CmailIntent::Quit;
        }
        if key.code == KeyCode::Esc {
            if let Some(intent) = self.handle_escape() {
                return intent;
            }
        }

        match &mut self.mode {
            CmailMode::Conversations => self.handle_conversations_key(key),
            CmailMode::Starting { username } => handle_starting_key(key, username),
            CmailMode::Conversation {
                conversation,
                messages,
            } => handle_conversation_key(key, conversation, messages),
            CmailMode::ConfirmSend {
                conversation_id,
                content,
                error,
                ..
            } => handle_confirm_send_key(key, conversation_id, content, error),
        }
    }

    pub fn handle_escape(&mut self) -> Option<CmailIntent> {
        match &mut self.mode {
            CmailMode::Conversations => None,
            CmailMode::Starting { .. } => {
                self.mode = CmailMode::Conversations;
                Some(CmailIntent::CancelInput)
            }
            CmailMode::Conversation { .. } => {
                self.mode = CmailMode::Conversations;
                Some(CmailIntent::BackToConversations)
            }
            CmailMode::ConfirmSend {
                conversation,
                messages,
                ..
            } => {
                self.mode = CmailMode::Conversation {
                    conversation: conversation.clone(),
                    messages: messages.clone(),
                };
                Some(CmailIntent::CancelInput)
            }
        }
    }

    fn handle_conversations_key(&mut self, key: KeyEvent) -> CmailIntent {
        if self.conversations.loading {
            return match key.code {
                KeyCode::Char('n') => {
                    self.mode = CmailMode::Starting {
                        username: String::new(),
                    };
                    CmailIntent::StartNew
                }
                _ => CmailIntent::None,
            };
        }
        match key.code {
            KeyCode::Char('n') => {
                self.mode = CmailMode::Starting {
                    username: String::new(),
                };
                CmailIntent::StartNew
            }
            KeyCode::Char('r') => {
                self.conversations.items.clear();
                self.conversations.selected = 0;
                self.conversations.loading = true;
                self.conversations.error = None;
                CmailIntent::RefreshConversations
            }
            KeyCode::Enter => self
                .conversations
                .items
                .get(self.conversations.selected)
                .map(|c| CmailIntent::OpenConversation {
                    conversation_id: c.conversation_id.clone(),
                })
                .unwrap_or(CmailIntent::None),
            _ => {
                match super::list_nav::navigate(
                    key.code,
                    &mut self.conversations.selected,
                    self.conversations.items.len(),
                    false,
                ) {
                    super::list_nav::ListNav::Moved => CmailIntent::None,
                    _ => CmailIntent::None,
                }
            }
        }
    }

    pub fn apply_conversations(&mut self, result: Result<Vec<CmailConversation>, String>) {
        self.conversations.loading = false;
        self.conversations.loaded = true;
        match result {
            Ok(items) => {
                self.conversations.items = items;
                self.conversations.selected = self
                    .conversations
                    .selected
                    .min(self.conversations.items.len().saturating_sub(1));
                self.conversations.error = None;
            }
            Err(msg) => self.conversations.error = Some(msg),
        }
    }

    pub fn open_conversation(&mut self, conversation_id: &str) {
        if let Some(conversation) = self
            .conversations
            .items
            .iter()
            .find(|c| c.conversation_id == conversation_id)
            .cloned()
        {
            self.mode = CmailMode::Conversation {
                conversation,
                messages: TabState::loading(),
            };
        }
    }

    pub fn apply_started(&mut self, result: Result<CmailConversation, String>) -> Option<String> {
        match result {
            Ok(conversation) => {
                let id = conversation.conversation_id.clone();
                if let Some(existing) = self
                    .conversations
                    .items
                    .iter_mut()
                    .find(|c| c.conversation_id == id)
                {
                    *existing = conversation.clone();
                } else {
                    self.conversations.items.insert(0, conversation.clone());
                    self.conversations.selected = 0;
                }
                self.mode = CmailMode::Conversation {
                    conversation,
                    messages: TabState::loading(),
                };
                Some(id)
            }
            Err(msg) => {
                self.mode = CmailMode::Starting {
                    username: String::new(),
                };
                self.conversations.error = Some(msg);
                None
            }
        }
    }

    pub fn apply_messages(
        &mut self,
        conversation_id: &str,
        result: Result<(Vec<CmailMessage>, Option<String>), String>,
    ) {
        let CmailMode::Conversation {
            conversation,
            messages,
            ..
        } = &mut self.mode
        else {
            return;
        };
        if conversation.conversation_id != conversation_id {
            return;
        }
        if messages.loaded {
            apply_older_messages(messages, result);
        } else {
            messages.apply_initial(result);
            if !messages.items.is_empty() {
                messages.selected = messages.items.len() - 1;
            }
        }
    }

    pub fn confirm_send(&mut self, conversation_id: &str, content: String) {
        let CmailMode::Conversation {
            conversation,
            messages,
        } = &mut self.mode
        else {
            return;
        };
        if conversation.conversation_id != conversation_id {
            return;
        }
        self.mode = CmailMode::ConfirmSend {
            conversation_id: conversation_id.to_string(),
            conversation: conversation.clone(),
            messages: messages.clone(),
            content,
            error: None,
        };
    }

    pub fn finish_send(&mut self, conversation_id: &str, result: Result<(), String>) -> bool {
        match &mut self.mode {
            CmailMode::Conversation {
                conversation,
                messages,
            } if conversation.conversation_id == conversation_id => match result {
                Ok(()) => {
                    messages.loading = true;
                    messages.error = None;
                    true
                }
                Err(msg) => {
                    messages.error = Some(msg);
                    false
                }
            },
            CmailMode::ConfirmSend {
                conversation_id: id,
                conversation,
                messages,
                error,
                ..
            } if id == conversation_id => match result {
                Ok(()) => {
                    messages.loading = true;
                    messages.error = None;
                    self.mode = CmailMode::Conversation {
                        conversation: conversation.clone(),
                        messages: messages.clone(),
                    };
                    true
                }
                Err(msg) => {
                    *error = Some(msg);
                    false
                }
            },
            _ => false,
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        match &self.mode {
            CmailMode::Conversations | CmailMode::Starting { .. } => {
                self.render_conversations(frame, area, theme)
            }
            CmailMode::Conversation { .. } | CmailMode::ConfirmSend { .. } => {
                self.render_conversation(frame, area, theme)
            }
        }
    }

    fn render_conversations(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let title = match &self.mode {
            CmailMode::Starting { .. } => " cs-tui • c-mail • new conversation ",
            _ => " cs-tui • c-mail ",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let input_rows = matches!(self.mode, CmailMode::Starting { .. });
        let constraints = if input_rows {
            vec![
                Constraint::Min(1),
                Constraint::Length(2),
                Constraint::Length(1),
            ]
        } else {
            vec![Constraint::Min(1), Constraint::Length(1)]
        };
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);
        let visible: Vec<usize> = (0..self.conversations.items.len()).collect();
        list::render_body(
            frame,
            layout[0],
            theme,
            &self.conversations,
            &visible,
            "no conversations · n new",
            |c| conversation_item(c, theme),
        );
        let status_idx = if input_rows { 2 } else { 1 };
        if let CmailMode::Starting { username } = &self.mode {
            let prompt = Paragraph::new(Line::from(vec![
                Span::styled("username: ", theme.muted_style()),
                Span::styled(username.clone(), theme.base()),
            ]));
            frame.render_widget(prompt, layout[1]);
        }
        let status = if input_rows {
            "enter start · esc cancel"
        } else {
            "enter open · n new · r refresh · esc menu"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status, theme.muted_style()))),
            layout[status_idx],
        );
    }

    fn render_conversation(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let (conversation, messages, draft, error) = match &self.mode {
            CmailMode::Conversation {
                conversation,
                messages,
            } => (conversation, messages, None, None),
            CmailMode::ConfirmSend {
                conversation,
                messages,
                content,
                error,
                ..
            } => (
                conversation,
                messages,
                Some(content.as_str()),
                error.as_deref(),
            ),
            _ => return,
        };
        let title = format!(" cs-tui • c-mail • @{} ", conversation.other_user.username);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let constraints = if draft.is_some() || error.is_some() {
            vec![
                Constraint::Min(1),
                Constraint::Length(2),
                Constraint::Length(1),
            ]
        } else {
            vec![Constraint::Min(1), Constraint::Length(1)]
        };
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);
        let visible: Vec<usize> = (0..messages.items.len()).collect();
        let messages_area = bottom_aligned_messages_area(layout[0], messages.items.len());
        list::render_body(
            frame,
            messages_area,
            theme,
            messages,
            &visible,
            "no messages yet",
            |m| message_item(m, theme),
        );
        let status_idx = if let Some(draft) = draft {
            let line = if let Some(msg) = error {
                Line::from(Span::styled(msg.to_string(), theme.error_style()))
            } else {
                Line::from(vec![
                    Span::styled("draft: ", theme.muted_style()),
                    Span::styled(draft.to_string(), theme.base()),
                ])
            };
            frame.render_widget(Paragraph::new(line), layout[1]);
            2
        } else {
            1
        };
        let status = if draft.is_some() {
            "enter send · esc cancel"
        } else if messages.next_cursor.is_some() {
            "c compose · r refresh · scroll up older · esc back"
        } else {
            "c compose · r refresh · esc back"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status, theme.muted_style()))),
            layout[status_idx],
        );
    }
}

impl Default for CmailScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn handle_starting_key(key: KeyEvent, username: &mut String) -> CmailIntent {
    match key.code {
        KeyCode::Esc => CmailIntent::CancelInput,
        KeyCode::Enter => {
            let trimmed = username.trim().trim_start_matches('@').to_string();
            if trimmed.is_empty() {
                CmailIntent::None
            } else {
                CmailIntent::SubmitNew { username: trimmed }
            }
        }
        KeyCode::Backspace => {
            username.pop();
            CmailIntent::None
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            username.push(c);
            CmailIntent::None
        }
        _ => CmailIntent::None,
    }
}

fn handle_conversation_key(
    key: KeyEvent,
    conversation: &CmailConversation,
    messages: &mut TabState<CmailMessage>,
) -> CmailIntent {
    if messages.loading {
        return CmailIntent::None;
    }
    match key.code {
        KeyCode::Esc => CmailIntent::BackToConversations,
        KeyCode::Char('c') => CmailIntent::StartCompose {
            conversation_id: conversation.conversation_id.clone(),
        },
        KeyCode::Char('r') => {
            messages.items.clear();
            messages.selected = 0;
            messages.next_cursor = None;
            messages.loading = true;
            messages.loaded = false;
            messages.error = None;
            CmailIntent::OpenConversation {
                conversation_id: conversation.conversation_id.clone(),
            }
        }
        KeyCode::Char('k') | KeyCode::Up | KeyCode::PageUp
            if messages.selected == 0 && messages.next_cursor.is_some() =>
        {
            messages.loading = true;
            let before = messages.next_cursor.as_deref().and_then(|s| s.parse().ok());
            CmailIntent::LoadOlder {
                conversation_id: conversation.conversation_id.clone(),
                before,
            }
        }
        _ => {
            super::list_nav::navigate(
                key.code,
                &mut messages.selected,
                messages.items.len(),
                false,
            );
            CmailIntent::None
        }
    }
}

fn apply_older_messages(
    messages: &mut TabState<CmailMessage>,
    result: Result<(Vec<CmailMessage>, Option<String>), String>,
) {
    messages.loading = false;
    match result {
        Ok((mut older, cursor)) => {
            let added = older.len();
            older.append(&mut messages.items);
            messages.items = older;
            messages.selected = messages.selected.saturating_add(added);
            messages.shift_offset(added);
            messages.next_cursor = cursor;
            messages.error = None;
        }
        Err(msg) => messages.error = Some(msg),
    }
}

fn bottom_aligned_messages_area(area: Rect, message_count: usize) -> Rect {
    if message_count == 0 || area.height == 0 {
        return area;
    }
    let message_rows = message_count.saturating_mul(2).min(u16::MAX as usize) as u16;
    if message_rows >= area.height {
        return area;
    }
    Rect {
        y: area.y + area.height - message_rows,
        height: message_rows,
        ..area
    }
}

fn handle_confirm_send_key(
    key: KeyEvent,
    conversation_id: &str,
    content: &str,
    error: &mut Option<String>,
) -> CmailIntent {
    match key.code {
        KeyCode::Esc => CmailIntent::CancelInput,
        KeyCode::Enter => {
            if content.trim().is_empty() {
                *error = Some("message is empty — esc to cancel".into());
                CmailIntent::None
            } else {
                CmailIntent::SendMessage {
                    conversation_id: conversation_id.to_string(),
                    content: content.to_string(),
                }
            }
        }
        _ => CmailIntent::None,
    }
}

fn conversation_item(c: &CmailConversation, theme: &Theme) -> ListItem<'static> {
    let when = c
        .last_message_at
        .map(format_epoch_millis_list_timestamp)
        .unwrap_or_default();
    let unread = if c.unread_count > 0 {
        format!(" · {} unread", c.unread_count)
    } else {
        String::new()
    };
    let preview = c
        .last_message
        .as_ref()
        .map(|m| m.content.as_str())
        .unwrap_or("no messages yet");
    ListItem::new(vec![
        Line::from(vec![
            Span::styled(format!("@{}", c.other_user.username), theme.base()),
            Span::styled(format!(" · {when}{unread}"), theme.muted_style()),
        ]),
        Line::from(Span::styled(preview.to_string(), theme.muted_style())),
    ])
}

fn message_item(m: &CmailMessage, theme: &Theme) -> ListItem<'static> {
    let when = format_epoch_millis_list_timestamp(m.timestamp);
    ListItem::new(vec![
        Line::from(vec![
            Span::styled(format!("@{}", m.sender_username), theme.base()),
            Span::styled(format!(" · {when}"), theme.muted_style()),
        ]),
        Line::from(Span::styled(m.content.clone(), theme.muted_style())),
    ])
}

fn format_epoch_millis_list_timestamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    time::OffsetDateTime::from_unix_timestamp(secs)
        .map(crate::config::format_list_timestamp)
        .unwrap_or_default()
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

    fn user(username: &str) -> cs_api::CmailUser {
        cs_api::CmailUser {
            user_id: format!("uid-{username}"),
            username: username.into(),
            display_name: None,
            profile_picture_url: None,
        }
    }

    fn convo(id: &str, username: &str) -> CmailConversation {
        CmailConversation {
            conversation_id: id.into(),
            other_user: user(username),
            last_message: None,
            last_message_at: None,
            unread_count: 0,
        }
    }

    fn message(id: &str, content: &str, timestamp: i64) -> CmailMessage {
        CmailMessage {
            id: id.into(),
            sender_id: "uid-alice".into(),
            sender_username: "alice".into(),
            content: content.into(),
            timestamp,
            read: true,
        }
    }

    #[test]
    fn new_starts_loading_conversations() {
        let s = CmailScreen::new();
        assert!(s.conversations.loading);
        assert!(matches!(s.mode, CmailMode::Conversations));
    }

    #[test]
    fn n_starts_username_prompt_and_enter_submits() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![]));
        assert_eq!(s.handle_key(key(KeyCode::Char('n'))), CmailIntent::StartNew);
        assert!(s.is_text_input());
        s.handle_key(key(KeyCode::Char('a')));
        s.handle_key(key(KeyCode::Char('l')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CmailIntent::SubmitNew {
                username: "al".into()
            }
        );
    }

    #[test]
    fn enter_opens_selected_conversation() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CmailIntent::OpenConversation {
                conversation_id: "c1".into()
            }
        );
    }

    #[test]
    fn esc_from_open_conversation_returns_to_conversation_list() {
        let mut s = CmailScreen::new();
        let c = convo("c1", "alice");
        s.mode = CmailMode::Conversation {
            conversation: c,
            messages: TabState::default(),
        };
        assert_eq!(
            s.handle_key(key(KeyCode::Esc)),
            CmailIntent::BackToConversations
        );
        assert!(matches!(s.mode, CmailMode::Conversations));
    }

    #[test]
    fn esc_from_new_conversation_input_cancels_without_opening_global_menu() {
        let mut s = CmailScreen::new();
        s.mode = CmailMode::Starting {
            username: "alice".into(),
        };
        assert_eq!(s.handle_key(key(KeyCode::Esc)), CmailIntent::CancelInput);
        assert!(matches!(s.mode, CmailMode::Conversations));
    }

    #[test]
    fn c_in_conversation_requests_existing_editor_compose() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        if let CmailMode::Conversation { messages, .. } = &mut s.mode {
            messages.apply_initial(Ok((vec![], None)));
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Char('c'))),
            CmailIntent::StartCompose {
                conversation_id: "c1".into(),
            }
        );
        assert!(!s.is_text_input());
    }

    #[test]
    fn opening_conversation_selects_newest_message() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");

        s.apply_messages(
            "c1",
            Ok((
                vec![
                    message("m1", "oldest", 1_000),
                    message("m2", "middle", 2_000),
                    message("m3", "newest", 3_000),
                ],
                Some("1000".into()),
            )),
        );

        let CmailMode::Conversation { messages, .. } = &s.mode else {
            panic!("conversation should remain open");
        };
        assert_eq!(messages.selected, 2);
        assert_eq!(messages.items[messages.selected].content, "newest");
    }

    #[test]
    fn short_conversation_renders_messages_at_bottom() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        s.apply_messages(
            "c1",
            Ok((
                vec![
                    message("m1", "oldest-visible", 1_000),
                    message("m2", "newest-visible", 2_000),
                ],
                None,
            )),
        );

        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| s.render(f, f.area(), &theme)).unwrap();

        let buffer = terminal.backend().buffer();
        let row_text = |y| -> String {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        };
        let rows = (0..buffer.area.height)
            .map(|y| format!("{y}: {}", row_text(y)))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !row_text(1).contains("oldest-visible"),
            "message list should not start at the top when it can sit at the bottom"
        );
        assert!(row_text(6).contains("@alice"), "{rows}");
        assert!(row_text(7).contains("oldest-visible"), "{rows}");
        assert!(row_text(8).contains("@alice"), "{rows}");
        assert!(row_text(9).contains("newest-visible"), "{rows}");
    }

    #[test]
    fn scrolling_up_from_oldest_loads_older_and_prepends_without_jumping() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        s.apply_messages(
            "c1",
            Ok((
                vec![
                    message("m2", "current-oldest", 2_000),
                    message("m3", "newest", 3_000),
                ],
                Some("2000".into()),
            )),
        );

        if let CmailMode::Conversation { messages, .. } = &mut s.mode {
            messages.selected = 0;
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Up)),
            CmailIntent::LoadOlder {
                conversation_id: "c1".into(),
                before: Some(2_000),
            }
        );

        s.apply_messages(
            "c1",
            Ok((vec![message("m1", "prepended-older", 1_000)], None)),
        );

        let CmailMode::Conversation { messages, .. } = &s.mode else {
            panic!("conversation should remain open");
        };
        assert_eq!(
            messages
                .items
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["prepended-older", "current-oldest", "newest"]
        );
        assert_eq!(messages.selected, 1);
        assert_eq!(messages.items[messages.selected].content, "current-oldest");
    }

    #[test]
    fn confirm_send_requires_enter_to_send() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        if let CmailMode::Conversation { messages, .. } = &mut s.mode {
            messages.apply_initial(Ok((vec![], None)));
        }
        s.confirm_send("c1", "hello".into());

        let ctrl_d = KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(s.handle_key(ctrl_d), CmailIntent::None);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CmailIntent::SendMessage {
                conversation_id: "c1".into(),
                content: "hello".into(),
            }
        );
    }
}

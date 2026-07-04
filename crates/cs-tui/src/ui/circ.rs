//! cIRC screen — multi-user chat rooms.
//!
//! Structurally a sibling of [`super::cmail`]: a room list, then a room view with
//! a message list, an inline composer (optimistic send), and live RTDB updates.
//! Shares the small chat-rendering helpers from `cmail`.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{CircMessage, CircRoom};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::cmail::{
    avatar_color, bottom_aligned_messages_area, format_epoch_millis_relative, one_line_preview,
    Outgoing,
};
use super::list::{self, TabState};
use super::theme::Theme;

const MAX_OUTGOING_ROWS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircIntent {
    RefreshRooms,
    OpenRoom {
        room_id: String,
    },
    LoadOlder {
        room_id: String,
        before: Option<i64>,
    },
    StartCompose {
        room_id: String,
        draft: String,
    },
    SendMessage {
        room_id: String,
        content: String,
    },
    RetryFailed {
        room_id: String,
        contents: Vec<String>,
    },
    BackToRooms,
    Quit,
    None,
}

// One `CircMode` exists at a time; the size gap doesn't warrant boxing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CircMode {
    Rooms,
    Room {
        room: CircRoom,
        messages: TabState<CircMessage>,
    },
}

#[derive(Debug)]
pub struct CircScreen {
    pub rooms: TabState<CircRoom>,
    pub mode: CircMode,
    /// Inline composer buffer for the open room.
    draft: String,
    /// Whether the inline composer is focused (capturing text) vs. browse mode.
    composing: bool,
    /// Optimistic outgoing messages awaiting their server echo.
    outgoing: Vec<Outgoing>,
}

impl CircScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rooms: TabState::loading(),
            mode: CircMode::Rooms,
            draft: String::new(),
            composing: false,
            outgoing: Vec::new(),
        }
    }

    pub fn is_text_input(&self) -> bool {
        self.composing && matches!(self.mode, CircMode::Room { .. })
    }

    pub fn paste_text(&mut self, text: &str) {
        if self.composing && matches!(self.mode, CircMode::Room { .. }) {
            self.draft.push_str(text);
        }
    }

    /// Prefill the inline composer with `content` and focus it (editor hand-back).
    pub fn set_draft_and_focus(&mut self, content: String) {
        if matches!(self.mode, CircMode::Room { .. }) {
            self.draft = content;
            self.composing = true;
        }
    }

    fn reset_composer(&mut self) {
        self.draft.clear();
        self.composing = false;
        self.outgoing.clear();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> CircIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return CircIntent::Quit;
        }
        if key.code == KeyCode::Esc {
            if let Some(intent) = self.handle_escape() {
                return intent;
            }
        }
        match &mut self.mode {
            CircMode::Rooms => self.handle_rooms_key(key),
            CircMode::Room { .. } => self.handle_room_key(key),
        }
    }

    pub fn handle_escape(&mut self) -> Option<CircIntent> {
        match &mut self.mode {
            CircMode::Rooms => None,
            CircMode::Room { .. } if self.composing => {
                self.composing = false;
                Some(CircIntent::None)
            }
            CircMode::Room { .. } => {
                self.reset_composer();
                self.mode = CircMode::Rooms;
                Some(CircIntent::BackToRooms)
            }
        }
    }

    fn handle_rooms_key(&mut self, key: KeyEvent) -> CircIntent {
        if self.rooms.loading {
            return CircIntent::None;
        }
        match key.code {
            KeyCode::Char('r') => {
                self.rooms.items.clear();
                self.rooms.selected = 0;
                self.rooms.loading = true;
                self.rooms.error = None;
                CircIntent::RefreshRooms
            }
            KeyCode::Enter => self
                .rooms
                .items
                .get(self.rooms.selected)
                .map(|r| CircIntent::OpenRoom {
                    room_id: r.room_id().to_string(),
                })
                .unwrap_or(CircIntent::None),
            code => {
                super::list_nav::navigate(
                    code,
                    &mut self.rooms.selected,
                    self.rooms.items.len(),
                    false,
                );
                CircIntent::None
            }
        }
    }

    pub fn apply_rooms(&mut self, result: Result<Vec<CircRoom>, String>) {
        self.rooms.loading = false;
        self.rooms.loaded = true;
        match result {
            Ok(items) => {
                self.rooms.items = items;
                self.rooms.selected = self
                    .rooms
                    .selected
                    .min(self.rooms.items.len().saturating_sub(1));
                self.rooms.error = None;
            }
            Err(msg) => self.rooms.error = Some(msg),
        }
    }

    pub fn open_room(&mut self, room_id: &str) {
        if let Some(room) = self
            .rooms
            .items
            .iter()
            .find(|r| r.room_id() == room_id)
            .cloned()
        {
            self.reset_composer();
            self.mode = CircMode::Room {
                room,
                messages: TabState::loading(),
            };
        }
    }

    pub fn apply_messages(
        &mut self,
        room_id: &str,
        initial: bool,
        result: Result<(Vec<CircMessage>, Option<String>), String>,
    ) {
        let CircMode::Room { room, messages } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        if initial {
            messages.apply_initial(result);
            if !messages.items.is_empty() {
                messages.selected = messages.items.len() - 1;
            }
        } else {
            apply_older_messages(messages, result);
        }
    }

    /// Merge live-streamed messages into the open room (de-duped, timestamp
    /// order; follows the tail when pinned to the bottom).
    pub fn apply_live(&mut self, room_id: &str, incoming: Vec<CircMessage>) {
        let messages = match &mut self.mode {
            CircMode::Room { room, messages } if room.room_id() == room_id => messages,
            _ => return,
        };
        let fresh: Vec<CircMessage> = incoming.into_iter().filter(|m| !m.id.is_empty()).collect();
        if fresh.is_empty() {
            return;
        }
        let was_at_bottom =
            messages.items.is_empty() || messages.selected + 1 >= messages.items.len();
        let selected_id = messages.items.get(messages.selected).map(|m| m.id.clone());
        for msg in fresh {
            if let Some(existing) = messages.items.iter_mut().find(|m| m.id == msg.id) {
                *existing = msg;
            } else {
                messages.items.push(msg);
            }
        }
        messages.items.sort_by_key(|m| m.timestamp);
        messages.loading = false;
        messages.loaded = true;
        if was_at_bottom {
            messages.selected = messages.items.len().saturating_sub(1);
        } else if let Some(id) = selected_id {
            if let Some(pos) = messages.items.iter().position(|m| m.id == id) {
                messages.selected = pos;
            }
        }
    }

    /// Resolve a send (mirrors C-Mail): drop the matching optimistic entry on
    /// success (returning `true` to reload), or mark it failed.
    pub fn finish_send(
        &mut self,
        room_id: &str,
        content: &str,
        result: Result<(), String>,
    ) -> bool {
        let CircMode::Room { room, .. } = &self.mode else {
            return false;
        };
        if room.room_id() != room_id {
            return false;
        }
        match result {
            Ok(()) => {
                if let Some(pos) = self
                    .outgoing
                    .iter()
                    .position(|o| !o.failed && o.content == content)
                {
                    self.outgoing.remove(pos);
                }
                if let CircMode::Room { messages, .. } = &mut self.mode {
                    messages.loading = true;
                    messages.error = None;
                }
                true
            }
            Err(_) => {
                if let Some(o) = self
                    .outgoing
                    .iter_mut()
                    .find(|o| !o.failed && o.content == content)
                {
                    o.failed = true;
                }
                false
            }
        }
    }

    fn handle_room_key(&mut self, key: KeyEvent) -> CircIntent {
        let room_id = match &self.mode {
            CircMode::Room { room, .. } => room.room_id().to_string(),
            CircMode::Rooms => return CircIntent::None,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if ctrl && key.code == KeyCode::Char('r') {
            let contents: Vec<String> = self
                .outgoing
                .iter()
                .filter(|o| o.failed)
                .map(|o| o.content.clone())
                .collect();
            if contents.is_empty() {
                return CircIntent::None;
            }
            for o in self.outgoing.iter_mut().filter(|o| o.failed) {
                o.failed = false;
            }
            return CircIntent::RetryFailed { room_id, contents };
        }

        if self.composing {
            self.handle_composing_key(key, &room_id, ctrl)
        } else {
            self.handle_browse_key(key, &room_id)
        }
    }

    fn handle_composing_key(&mut self, key: KeyEvent, room_id: &str, ctrl: bool) -> CircIntent {
        match key.code {
            KeyCode::Char('e') if ctrl => CircIntent::StartCompose {
                room_id: room_id.to_string(),
                draft: self.draft.clone(),
            },
            KeyCode::Enter => {
                let content = self.draft.trim().to_string();
                if content.is_empty() {
                    return CircIntent::None;
                }
                self.outgoing.push(Outgoing {
                    content: content.clone(),
                    failed: false,
                });
                self.draft.clear();
                CircIntent::SendMessage {
                    room_id: room_id.to_string(),
                    content,
                }
            }
            KeyCode::Backspace => {
                self.draft.pop();
                CircIntent::None
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => self.scroll_messages(key.code, room_id),
            KeyCode::Char(c) if !ctrl => {
                self.draft.push(c);
                CircIntent::None
            }
            _ => CircIntent::None,
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent, room_id: &str) -> CircIntent {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('i') | KeyCode::Enter => {
                self.composing = true;
                CircIntent::None
            }
            KeyCode::Char('r') => {
                if let CircMode::Room { messages, .. } = &mut self.mode {
                    messages.items.clear();
                    messages.selected = 0;
                    messages.next_cursor = None;
                    messages.loading = true;
                    messages.loaded = false;
                    messages.error = None;
                }
                CircIntent::OpenRoom {
                    room_id: room_id.to_string(),
                }
            }
            code => self.scroll_messages(code, room_id),
        }
    }

    fn scroll_messages(&mut self, code: KeyCode, room_id: &str) -> CircIntent {
        let CircMode::Room { messages, .. } = &mut self.mode else {
            return CircIntent::None;
        };
        if messages.loading {
            return CircIntent::None;
        }
        match code {
            KeyCode::Home => messages.selected = 0,
            KeyCode::End => messages.selected = messages.items.len().saturating_sub(1),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp
                if messages.selected == 0 && messages.next_cursor.is_some() =>
            {
                messages.loading = true;
                let before = messages.next_cursor.as_deref().and_then(|s| s.parse().ok());
                return CircIntent::LoadOlder {
                    room_id: room_id.to_string(),
                    before,
                };
            }
            other => {
                super::list_nav::navigate(
                    other,
                    &mut messages.selected,
                    messages.items.len(),
                    false,
                );
            }
        }
        CircIntent::None
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        match &self.mode {
            CircMode::Rooms => self.render_rooms(frame, area, theme),
            CircMode::Room { .. } => self.render_room(frame, area, theme),
        }
    }

    fn render_rooms(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • cIRC ", theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let visible: Vec<usize> = (0..self.rooms.items.len()).collect();
        list::render_body(
            frame,
            layout[0],
            theme,
            &self.rooms,
            &visible,
            "no rooms available",
            |r| room_item(r, theme),
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "enter open · r refresh · esc menu",
                theme.muted_style(),
            ))),
            layout[1],
        );
    }

    fn render_room(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let CircMode::Room { room, messages } = &self.mode else {
            return;
        };
        let title = if room.name.is_empty() {
            format!(" cs-tui • cIRC • #{} ", room.room_id())
        } else {
            format!(" cs-tui • cIRC • #{} · {} ", room.room_id(), room.name)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let out_rows = self.outgoing_rows();
        let footer_rows = if self.composing { 2 } else { 1 };
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(out_rows),
                Constraint::Length(footer_rows),
            ])
            .split(inner);

        let visible: Vec<usize> = (0..messages.items.len()).collect();
        let messages_area =
            bottom_aligned_messages_area(layout[0], messages.items.len().saturating_mul(2));
        list::render_body(
            frame,
            messages_area,
            theme,
            messages,
            &visible,
            "no messages yet — c to chat",
            |m| ListItem::new(circ_message_lines(m, theme)),
        );

        if out_rows > 0 {
            self.render_outgoing(frame, layout[1], theme);
        }
        let scrolled_up = messages.selected + 1 < messages.items.len();
        self.render_footer(
            frame,
            layout[2],
            theme,
            messages.next_cursor.is_some(),
            scrolled_up,
        );
    }

    fn outgoing_rows(&self) -> u16 {
        let n = self.outgoing.len();
        if n == 0 {
            return 0;
        }
        (n.min(MAX_OUTGOING_ROWS) + usize::from(n > MAX_OUTGOING_ROWS)) as u16
    }

    fn render_outgoing(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        for o in self.outgoing.iter().take(MAX_OUTGOING_ROWS) {
            let status = if o.failed {
                Span::styled(" ✗ not sent · ctrl+r retry", theme.error_style())
            } else {
                Span::styled(" · sending…", theme.muted_style())
            };
            let preview = one_line_preview(&o.content, 48);
            lines.push(Line::from(vec![
                Span::styled("→ you", theme.accent_style()),
                status,
                Span::styled(format!(": {preview}"), theme.muted_style()),
            ]));
        }
        if self.outgoing.len() > MAX_OUTGOING_ROWS {
            lines.push(Line::from(Span::styled(
                format!("  … +{} more", self.outgoing.len() - MAX_OUTGOING_ROWS),
                theme.muted_style(),
            )));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_footer(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        has_older: bool,
        scrolled_up: bool,
    ) {
        if self.composing {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(area);
            let shown = self.draft.replace('\n', " ⏎ ");
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("› ", theme.accent_style()),
                    Span::styled(shown, theme.base()),
                    Span::styled("▏", theme.accent_style()),
                ])),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "enter send · ctrl+e editor · esc unfocus",
                    theme.muted_style(),
                ))),
                rows[1],
            );
        } else {
            let mut hint = String::from("c chat · ");
            if scrolled_up {
                hint.push_str("end ↓ newest · ");
            }
            if has_older {
                hint.push_str("scroll up for older · ");
            }
            hint.push_str("r refresh · esc back");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(hint, theme.muted_style()))),
                area,
            );
        }
    }
}

fn apply_older_messages(
    messages: &mut TabState<CircMessage>,
    result: Result<(Vec<CircMessage>, Option<String>), String>,
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

fn room_item(r: &CircRoom, theme: &Theme) -> ListItem<'static> {
    let key = r.room_id();
    let header = if r.name.is_empty() {
        Line::from(Span::styled(format!("#{key}"), theme.accent_style()))
    } else {
        Line::from(vec![
            Span::styled(format!("#{key}"), theme.accent_style()),
            Span::styled(format!("  {}", r.name), theme.base()),
        ])
    };
    let sub = match r.last_message_at {
        Some(ts) => format!("  last activity {}", format_epoch_millis_relative(ts)),
        None => "  no activity yet".to_string(),
    };
    ListItem::new(vec![
        header,
        Line::from(Span::styled(sub, theme.muted_style())),
    ])
}

fn circ_message_lines(m: &CircMessage, theme: &Theme) -> Vec<Line<'static>> {
    let when = format_epoch_millis_relative(m.timestamp);
    let name = if m.username.is_empty() {
        "?".to_string()
    } else {
        m.username.clone()
    };
    // Each speaker's name gets their stable per-user colour so a busy room is
    // easy to scan; a ★ marks chat admins.
    let mut header = vec![
        Span::styled("  ", theme.muted_style()),
        Span::styled(
            name,
            Style::default()
                .fg(avatar_color(&m.username))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if m.is_chat_admin {
        header.push(Span::styled(" ★", theme.warning_style()));
    }
    header.push(Span::styled(format!(" · {when}"), theme.muted_style()));
    vec![
        Line::from(header),
        Line::from(Span::styled(format!("  {}", m.content), theme.base())),
    ]
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

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn room(slug: &str) -> CircRoom {
        CircRoom {
            id: format!("id-{slug}"),
            slug: slug.into(),
            name: String::new(),
            last_message_at: None,
            sort_order: 0,
        }
    }

    fn message(id: &str, user: &str, content: &str, ts: i64) -> CircMessage {
        CircMessage {
            id: id.into(),
            user_id: format!("uid-{user}"),
            username: user.into(),
            is_chat_admin: false,
            content: content.into(),
            timestamp: ts,
        }
    }

    fn open(slug: &str) -> CircScreen {
        let mut s = CircScreen::new();
        s.apply_rooms(Ok(vec![room(slug)]));
        s.open_room(slug);
        s.apply_messages(slug, true, Ok((vec![], None)));
        s
    }

    #[test]
    fn enter_opens_selected_room() {
        let mut s = CircScreen::new();
        s.apply_rooms(Ok(vec![room("general")]));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CircIntent::OpenRoom {
                room_id: "general".into()
            }
        );
    }

    #[test]
    fn typing_and_enter_sends_optimistically() {
        let mut s = open("general");
        s.handle_key(key(KeyCode::Char('c'))); // focus composer
        assert!(s.is_text_input());
        for c in "hey".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CircIntent::SendMessage {
                room_id: "general".into(),
                content: "hey".into(),
            }
        );
        assert_eq!(s.outgoing.len(), 1);
        assert_eq!(s.outgoing[0].content, "hey");
    }

    #[test]
    fn ctrl_e_expands_to_editor_with_draft() {
        let mut s = open("general");
        s.handle_key(key(KeyCode::Char('c')));
        for c in "hi".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.handle_key(ctrl(KeyCode::Char('e'))),
            CircIntent::StartCompose {
                room_id: "general".into(),
                draft: "hi".into(),
            }
        );
    }

    #[test]
    fn failed_send_marks_outgoing_and_ctrl_r_retries() {
        let mut s = open("general");
        s.handle_key(key(KeyCode::Char('c')));
        for c in "hey".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        s.handle_key(key(KeyCode::Enter));
        assert!(!s.finish_send("general", "hey", Err("boom".into())));
        assert!(s.outgoing[0].failed);
        assert_eq!(
            s.handle_key(ctrl(KeyCode::Char('r'))),
            CircIntent::RetryFailed {
                room_id: "general".into(),
                contents: vec!["hey".into()],
            }
        );
        assert!(!s.outgoing[0].failed);
    }

    #[test]
    fn live_merge_dedupes_and_follows_tail() {
        let mut s = open("general");
        s.apply_messages(
            "general",
            true,
            Ok((vec![message("m1", "neo", "hi", 1_000)], None)),
        );
        s.apply_live(
            "general",
            vec![
                message("m1", "neo", "hi", 1_000),
                message("m2", "trinity", "yo", 2_000),
            ],
        );
        let CircMode::Room { messages, .. } = &s.mode else {
            panic!("room should stay open");
        };
        assert_eq!(messages.items.len(), 2);
        assert_eq!(messages.items[1].content, "yo");
        assert_eq!(messages.selected, 1);
    }

    #[test]
    fn esc_from_room_returns_to_rooms() {
        let mut s = open("general");
        assert_eq!(s.handle_escape(), Some(CircIntent::BackToRooms));
        assert!(matches!(s.mode, CircMode::Rooms));
    }
}

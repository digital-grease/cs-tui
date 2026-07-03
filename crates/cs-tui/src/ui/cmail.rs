//! C-Mail screen — private 1:1 conversations.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{CmailConversation, CmailLiveUpdate, CmailMessage, CmailUser};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::list::{self, TabState};
use super::theme::Theme;

/// Maximum optimistic-outgoing lines shown before collapsing to a "+N more".
const MAX_OUTGOING_ROWS: usize = 4;

/// Collapse a (possibly multi-line) string to a single truncated line for a
/// compact preview.
fn one_line_preview(s: &str, max_chars: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max_chars {
        let head: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    } else {
        flat
    }
}

/// The display name to show for a C-Mail user: their profile display name when
/// set, otherwise the username.
fn display_name_of(user: &CmailUser) -> &str {
    match &user.display_name {
        Some(name) if !name.trim().is_empty() => name,
        _ => &user.username,
    }
}

/// A stable per-user colour for the avatar initial, hashed from the username so
/// the same person always gets the same colour. Uses the 16-colour ANSI palette
/// so it adapts to the terminal theme.
fn avatar_color(username: &str) -> Color {
    const PALETTE: [Color; 6] = [
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
        Color::Blue,
        Color::Red,
    ];
    let hash = username.bytes().fold(0u32, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(u32::from(b))
    });
    PALETTE[hash as usize % PALETTE.len()]
}

/// A one-character coloured avatar for a user (the first alphanumeric of their
/// display name), a cheap visual anchor that works in any terminal.
fn avatar_span(user: &CmailUser) -> Span<'static> {
    let label = display_name_of(user);
    let initial = label
        .chars()
        .find(|c| c.is_alphanumeric())
        .unwrap_or('?')
        .to_uppercase()
        .next()
        .unwrap_or('?');
    Span::styled(
        format!("{initial} "),
        Style::default()
            .fg(avatar_color(&user.username))
            .add_modifier(Modifier::BOLD),
    )
}

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
        draft: String,
    },
    SendMessage {
        conversation_id: String,
        content: String,
    },
    RetryFailed {
        conversation_id: String,
        contents: Vec<String>,
    },
    Quit,
    None,
}

// One `CmailMode` exists at a time (the screen's current view), so the size gap
// between the open-conversation variant and the small ones isn't worth boxing
// (which would only add derefs on the hot render path).
#[allow(clippy::large_enum_variant)]
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
}

/// An optimistically-shown outgoing message: rendered immediately when you send
/// from the inline composer, before the server echo lands. Cleared on success;
/// marked `failed` if the send errors.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub content: String,
    pub failed: bool,
}

#[derive(Debug)]
pub struct CmailScreen {
    pub conversations: TabState<CmailConversation>,
    pub mode: CmailMode,
    /// Inline composer buffer for the open conversation.
    draft: String,
    /// Whether the inline composer is focused (capturing text) vs. browse mode.
    composing: bool,
    /// Optimistic outgoing messages awaiting their server echo.
    outgoing: Vec<Outgoing>,
    /// Active `/` filter over the conversation list (`Some` while the box is open).
    conv_filter: Option<String>,
}

impl CmailScreen {
    pub fn new() -> Self {
        Self {
            conversations: TabState::loading(),
            mode: CmailMode::Conversations,
            draft: String::new(),
            composing: false,
            outgoing: Vec::new(),
            conv_filter: None,
        }
    }

    /// Whether the `/` conversation-list filter box is open.
    pub fn is_filtering(&self) -> bool {
        self.conv_filter.is_some()
    }

    /// Indices into the conversation list matching the active `/` filter (by
    /// username or display name, case-insensitive). All indices when no filter.
    fn conversation_view(&self) -> Vec<usize> {
        let query = match &self.conv_filter {
            Some(q) if !q.is_empty() => Some(q.to_lowercase()),
            _ => None,
        };
        self.conversations
            .items
            .iter()
            .enumerate()
            .filter(|(_, c)| match &query {
                Some(q) => {
                    c.other_user.username.to_lowercase().contains(q)
                        || display_name_of(&c.other_user).to_lowercase().contains(q)
                }
                None => true,
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Close the `/` filter box. Returns `true` if it was open.
    pub fn clear_conversation_filter(&mut self) -> bool {
        if self.conv_filter.take().is_some() {
            self.conversations.selected = 0;
            true
        } else {
            false
        }
    }

    /// Test helper: a screen already open on `conversation` with an empty thread.
    #[cfg(test)]
    pub fn for_open_conversation(conversation: CmailConversation) -> Self {
        let mut s = Self::new();
        s.conversations.loading = false;
        s.conversations.items.push(conversation.clone());
        s.mode = CmailMode::Conversation {
            conversation,
            messages: TabState::default(),
        };
        s
    }

    /// Test helper: the current inline-composer draft.
    #[cfg(test)]
    pub fn draft_for_test(&self) -> &str {
        &self.draft
    }

    /// Reset the transient conversation-view state (called when a conversation is
    /// opened, so a prior thread's draft/pending sends — and the list filter —
    /// don't leak into it).
    fn reset_composer(&mut self) {
        self.draft.clear();
        self.composing = false;
        self.outgoing.clear();
        self.conv_filter = None;
    }

    pub fn is_text_input(&self) -> bool {
        // The filter box only exists in Conversations mode, so `is_filtering`
        // alone is sufficient there.
        matches!(self.mode, CmailMode::Starting { .. })
            || (self.composing && matches!(self.mode, CmailMode::Conversation { .. }))
            || self.is_filtering()
    }

    /// Prefill the inline composer with `content` and focus it — used when the
    /// full editor hands its text back for a final review + send.
    pub fn set_draft_and_focus(&mut self, content: String) {
        if matches!(self.mode, CmailMode::Conversation { .. }) {
            self.draft = content;
            self.composing = true;
        }
    }

    /// Whether the user is currently looking at the open conversation with
    /// `username` (used to suppress a "new mail" toast for a thread they're
    /// already reading).
    pub fn viewing_conversation_with(&self, username: &str) -> bool {
        match &self.mode {
            CmailMode::Conversation { conversation, .. } => {
                conversation.other_user.username == username
            }
            _ => false,
        }
    }

    pub fn paste_text(&mut self, text: &str) {
        if let CmailMode::Starting { username } = &mut self.mode {
            username.push_str(&super::input::collapse_newlines(text));
        } else if let Some(q) = self.conv_filter.as_mut() {
            q.push_str(&super::input::collapse_newlines(text));
            self.conversations.selected = 0;
        } else if self.composing && matches!(self.mode, CmailMode::Conversation { .. }) {
            // Preserve newlines in a pasted message body (the composer collapses
            // them for display but sends the full text).
            self.draft.push_str(text);
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
            CmailMode::Conversation { .. } => self.handle_conversation_key(key),
        }
    }

    pub fn handle_escape(&mut self) -> Option<CmailIntent> {
        match &mut self.mode {
            // First Esc closes the `/` filter box; otherwise fall through to the
            // app's back/menu behaviour.
            CmailMode::Conversations => self
                .clear_conversation_filter()
                .then_some(CmailIntent::None),
            CmailMode::Starting { .. } => {
                self.mode = CmailMode::Conversations;
                Some(CmailIntent::CancelInput)
            }
            CmailMode::Conversation { .. } if self.composing => {
                // First Esc unfocuses the composer (keeping the draft); a second
                // Esc then leaves the conversation.
                self.composing = false;
                Some(CmailIntent::None)
            }
            CmailMode::Conversation { .. } => {
                self.reset_composer();
                self.mode = CmailMode::Conversations;
                Some(CmailIntent::BackToConversations)
            }
        }
    }

    fn start_new_conversation(&mut self) -> CmailIntent {
        self.conv_filter = None;
        self.mode = CmailMode::Starting {
            username: String::new(),
        };
        CmailIntent::StartNew
    }

    /// Open the conversation currently selected in the (possibly filtered) view.
    fn open_selected_conversation(&self) -> CmailIntent {
        let view = self.conversation_view();
        view.get(self.conversations.selected)
            .and_then(|&i| self.conversations.items.get(i))
            .map(|c| CmailIntent::OpenConversation {
                conversation_id: c.conversation_id.clone(),
            })
            .unwrap_or(CmailIntent::None)
    }

    fn handle_conversations_key(&mut self, key: KeyEvent) -> CmailIntent {
        // Filter box open: printable keys narrow the list, nav moves within it.
        if self.conv_filter.is_some() {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Enter => return self.open_selected_conversation(),
                KeyCode::Backspace => {
                    if let Some(q) = self.conv_filter.as_mut() {
                        q.pop();
                    }
                    self.conversations.selected = 0;
                    return CmailIntent::None;
                }
                KeyCode::Char(c) if !ctrl => {
                    if let Some(q) = self.conv_filter.as_mut() {
                        q.push(c);
                    }
                    self.conversations.selected = 0;
                    return CmailIntent::None;
                }
                code => {
                    let len = self.conversation_view().len();
                    super::list_nav::navigate(code, &mut self.conversations.selected, len, false);
                    return CmailIntent::None;
                }
            }
        }

        if self.conversations.loading {
            return match key.code {
                KeyCode::Char('n') => self.start_new_conversation(),
                _ => CmailIntent::None,
            };
        }
        match key.code {
            KeyCode::Char('n') => self.start_new_conversation(),
            KeyCode::Char('/') => {
                self.conv_filter = Some(String::new());
                self.conversations.selected = 0;
                CmailIntent::None
            }
            KeyCode::Char('r') => {
                self.conversations.items.clear();
                self.conversations.selected = 0;
                self.conversations.loading = true;
                self.conversations.error = None;
                CmailIntent::RefreshConversations
            }
            KeyCode::Enter => self.open_selected_conversation(),
            code => {
                let len = self.conversation_view().len();
                super::list_nav::navigate(code, &mut self.conversations.selected, len, false);
                CmailIntent::None
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
            self.reset_composer();
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
                self.reset_composer();
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

    /// Apply a message fetch. `initial` distinguishes a fresh load / refresh /
    /// post-send reload (`before` was `None`, replaces the list and jumps to the
    /// newest) from a scroll-back page (`before` was set, prepends older
    /// messages). Routing on the caller's intent rather than on `loaded` is what
    /// keeps a post-send reload from being mistaken for an older-page prepend.
    pub fn apply_messages(
        &mut self,
        conversation_id: &str,
        initial: bool,
        result: Result<(Vec<CmailMessage>, Option<String>), String>,
    ) {
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
        if initial {
            messages.apply_initial(result);
            if !messages.items.is_empty() {
                messages.selected = messages.items.len() - 1;
            }
        } else {
            apply_older_messages(messages, result);
        }
    }

    /// Apply changes that arrived over the live RTDB stream to the open
    /// conversation. New/replaced messages are merged (de-duped by id, kept in
    /// timestamp order); `Read` updates flip an existing message's read flag
    /// without touching its content (that's how a read receipt lands live). If
    /// the view was pinned to the newest message it follows the new tail;
    /// otherwise the caller's scroll position is preserved.
    pub fn apply_live(&mut self, conversation_id: &str, updates: Vec<CmailLiveUpdate>) {
        let messages = match &mut self.mode {
            CmailMode::Conversation {
                conversation,
                messages,
            } if conversation.conversation_id == conversation_id => messages,
            _ => return,
        };
        if updates.is_empty() {
            return;
        }
        let was_at_bottom =
            messages.items.is_empty() || messages.selected + 1 >= messages.items.len();
        let selected_id = messages.items.get(messages.selected).map(|m| m.id.clone());
        for update in updates {
            match update {
                CmailLiveUpdate::Message(msg) if !msg.id.is_empty() => {
                    if let Some(existing) = messages.items.iter_mut().find(|m| m.id == msg.id) {
                        *existing = msg;
                    } else {
                        messages.items.push(msg);
                    }
                }
                CmailLiveUpdate::Message(_) => {}
                CmailLiveUpdate::Read { id, read } => {
                    if let Some(existing) = messages.items.iter_mut().find(|m| m.id == id) {
                        existing.read = read;
                    }
                }
            }
        }
        if messages.items.is_empty() {
            return;
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

    /// Resolve a send. On success the matching optimistic `outgoing` entry is
    /// dropped (the real message arrives on the reload/echo) and the caller is
    /// told to reload (`true`). On failure that entry is marked `failed` so it
    /// stays visible with a retry hint. `content` correlates the result to the
    /// optimistic entry (FIFO among equal contents).
    pub fn finish_send(
        &mut self,
        conversation_id: &str,
        content: &str,
        result: Result<(), String>,
    ) -> bool {
        let CmailMode::Conversation { conversation, .. } = &self.mode else {
            return false;
        };
        if conversation.conversation_id != conversation_id {
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
                if let CmailMode::Conversation { messages, .. } = &mut self.mode {
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

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        match &self.mode {
            CmailMode::Conversations | CmailMode::Starting { .. } => {
                self.render_conversations(frame, area, theme)
            }
            CmailMode::Conversation { .. } => self.render_conversation(frame, area, theme),
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
        let visible = self.conversation_view();
        let empty = if self.conv_filter.is_some() {
            "no matches · esc to clear"
        } else {
            "no conversations · n new"
        };
        list::render_body(
            frame,
            layout[0],
            theme,
            &self.conversations,
            &visible,
            empty,
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
        let status_line = if input_rows {
            Line::from(Span::styled(
                "enter start · esc cancel",
                theme.muted_style(),
            ))
        } else if let Some(q) = &self.conv_filter {
            Line::from(vec![
                Span::styled("/", theme.accent_style()),
                Span::styled(q.clone(), theme.base()),
                Span::styled("▏", theme.accent_style()),
                Span::styled("  enter open · esc clear", theme.muted_style()),
            ])
        } else {
            Line::from(Span::styled(
                "enter open · n new · / filter · r refresh · esc menu",
                theme.muted_style(),
            ))
        };
        frame.render_widget(Paragraph::new(status_line), layout[status_idx]);
    }

    fn render_conversation(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let CmailMode::Conversation {
            conversation,
            messages,
        } = &self.mode
        else {
            return;
        };
        let other = &conversation.other_user;
        let name = display_name_of(other);
        let title = if name != other.username {
            format!(" cs-tui • c-mail • {name} (@{}) ", other.username)
        } else {
            format!(" cs-tui • c-mail • @{} ", other.username)
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
            bottom_aligned_messages_area(layout[0], rendered_message_rows(&messages.items, other));
        // `render_body` calls the item closure for every message in order each
        // frame, so a couple of `Cell`s let us inject day separators and a single
        // "new" divider without an extra pass or breaking selection indices.
        let now_local = time::OffsetDateTime::now_utc().to_offset(crate::config::get().tz_offset);
        let last_day: std::cell::Cell<Option<(i32, u16)>> = std::cell::Cell::new(None);
        let divider_placed = std::cell::Cell::new(false);
        list::render_body(
            frame,
            messages_area,
            theme,
            messages,
            &visible,
            "no messages yet — c to compose",
            |m| {
                let mut lines: Vec<Line<'static>> = Vec::new();
                if let Some(t) = local_datetime(m.timestamp) {
                    let key = day_key(t);
                    if last_day.get() != Some(key) {
                        last_day.set(Some(key));
                        lines.push(separator_line(&day_separator_label(t, now_local), theme));
                    }
                }
                if !divider_placed.get() && !m.read && message_from_other(m, other) {
                    divider_placed.set(true);
                    lines.push(separator_line("new", theme));
                }
                lines.extend(message_lines(m, other, theme));
                ListItem::new(lines)
            },
        );

        if out_rows > 0 {
            self.render_outgoing(frame, layout[1], theme);
        }
        let scrolled_up = messages.selected + 1 < messages.items.len();
        self.render_conversation_footer(
            frame,
            layout[2],
            theme,
            messages.next_cursor.is_some(),
            scrolled_up,
        );
    }

    /// Height (rows) the optimistic-outgoing strip needs: one line per pending
    /// message, capped, plus an overflow line.
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

    fn render_conversation_footer(
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
            // Single-line composer view: newlines (from a paste or the expanded
            // editor) collapse to a marker; the full text is still sent.
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
            let mut hint = String::from("c compose · ");
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

    fn handle_conversation_key(&mut self, key: KeyEvent) -> CmailIntent {
        let conversation_id = match &self.mode {
            CmailMode::Conversation { conversation, .. } => conversation.conversation_id.clone(),
            _ => return CmailIntent::None,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Ctrl+R retries failed optimistic sends, from either sub-mode.
        if ctrl && key.code == KeyCode::Char('r') {
            let contents: Vec<String> = self
                .outgoing
                .iter()
                .filter(|o| o.failed)
                .map(|o| o.content.clone())
                .collect();
            if contents.is_empty() {
                return CmailIntent::None;
            }
            for o in self.outgoing.iter_mut().filter(|o| o.failed) {
                o.failed = false;
            }
            return CmailIntent::RetryFailed {
                conversation_id,
                contents,
            };
        }

        if self.composing {
            self.handle_composing_key(key, &conversation_id, ctrl)
        } else {
            self.handle_browse_key(key, &conversation_id)
        }
    }

    fn handle_composing_key(
        &mut self,
        key: KeyEvent,
        conversation_id: &str,
        ctrl: bool,
    ) -> CmailIntent {
        match key.code {
            // Expand into the full editor, prefilled with the current draft.
            KeyCode::Char('e') if ctrl => CmailIntent::StartCompose {
                conversation_id: conversation_id.to_string(),
                draft: self.draft.clone(),
            },
            KeyCode::Enter => {
                let content = self.draft.trim().to_string();
                if content.is_empty() {
                    return CmailIntent::None;
                }
                // Show it immediately; the echo/reload replaces it.
                self.outgoing.push(Outgoing {
                    content: content.clone(),
                    failed: false,
                });
                self.draft.clear();
                CmailIntent::SendMessage {
                    conversation_id: conversation_id.to_string(),
                    content,
                }
            }
            KeyCode::Backspace => {
                self.draft.pop();
                CmailIntent::None
            }
            // Scroll the thread while keeping the composer focused.
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => self.scroll_messages(key.code, conversation_id),
            KeyCode::Char(c) if !ctrl => {
                self.draft.push(c);
                CmailIntent::None
            }
            _ => CmailIntent::None,
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent, conversation_id: &str) -> CmailIntent {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('i') | KeyCode::Enter => {
                self.composing = true;
                CmailIntent::None
            }
            KeyCode::Char('r') => {
                if let CmailMode::Conversation { messages, .. } = &mut self.mode {
                    messages.items.clear();
                    messages.selected = 0;
                    messages.next_cursor = None;
                    messages.loading = true;
                    messages.loaded = false;
                    messages.error = None;
                }
                CmailIntent::OpenConversation {
                    conversation_id: conversation_id.to_string(),
                }
            }
            code => self.scroll_messages(code, conversation_id),
        }
    }

    /// Move the message selection / trigger an older-page load. Shared by browse
    /// and composing modes (composing passes only arrow/page keys).
    fn scroll_messages(&mut self, code: KeyCode, conversation_id: &str) -> CmailIntent {
        let CmailMode::Conversation { messages, .. } = &mut self.mode else {
            return CmailIntent::None;
        };
        if messages.loading {
            return CmailIntent::None;
        }
        match code {
            KeyCode::Home => messages.selected = 0,
            KeyCode::End => messages.selected = messages.items.len().saturating_sub(1),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp
                if messages.selected == 0 && messages.next_cursor.is_some() =>
            {
                messages.loading = true;
                let before = messages.next_cursor.as_deref().and_then(|s| s.parse().ok());
                return CmailIntent::LoadOlder {
                    conversation_id: conversation_id.to_string(),
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
        CmailIntent::None
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

/// Total rendered rows for the message list: two per message, plus one for each
/// injected day separator and the single "new" divider. Lets the bottom-anchor
/// stay exact even though items are variable-height.
fn rendered_message_rows(messages: &[CmailMessage], other: &CmailUser) -> usize {
    let mut rows = 0usize;
    let mut last_day: Option<(i32, u16)> = None;
    let mut divider = false;
    for m in messages {
        if let Some(t) = local_datetime(m.timestamp) {
            let key = day_key(t);
            if last_day != Some(key) {
                last_day = Some(key);
                rows += 1;
            }
        }
        if !divider && !m.read && message_from_other(m, other) {
            divider = true;
            rows += 1;
        }
        rows += 2;
    }
    rows
}

fn bottom_aligned_messages_area(area: Rect, content_rows: usize) -> Rect {
    if content_rows == 0 || area.height == 0 {
        return area;
    }
    let rows = content_rows.min(u16::MAX as usize) as u16;
    if rows >= area.height {
        return area;
    }
    Rect {
        y: area.y + area.height - rows,
        height: rows,
        ..area
    }
}

fn conversation_item(c: &CmailConversation, theme: &Theme) -> ListItem<'static> {
    let when = c
        .last_message_at
        .map(format_epoch_millis_relative)
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
    // Header: avatar + display name, with the @handle muted alongside when a
    // separate display name is set.
    let name = display_name_of(&c.other_user);
    let mut header = vec![
        avatar_span(&c.other_user),
        Span::styled(name.to_string(), theme.base()),
    ];
    if name != c.other_user.username {
        header.push(Span::styled(
            format!(" @{}", c.other_user.username),
            theme.muted_style(),
        ));
    }
    header.push(Span::styled(
        format!(" · {when}{unread}"),
        theme.muted_style(),
    ));
    ListItem::new(vec![
        Line::from(header),
        Line::from(Span::styled(format!("  {preview}"), theme.muted_style())),
    ])
}

/// Whether a message was sent by the other participant (vs. the local user).
fn message_from_other(m: &CmailMessage, other: &CmailUser) -> bool {
    (!other.user_id.is_empty() && m.sender_id == other.user_id)
        || (other.user_id.is_empty() && m.sender_username == other.username)
}

fn message_lines(m: &CmailMessage, other: &CmailUser, theme: &Theme) -> Vec<Line<'static>> {
    let when = format_epoch_millis_relative(m.timestamp);
    // The other side's messages align left with their name; the local user's own
    // outgoing messages are marked "you", accent-coloured and arrow-prefixed, so
    // the two sides of a 1:1 thread are distinct at a glance.
    let from_other = message_from_other(m, other);
    let (prefix, who, who_style) = if from_other {
        // Their name gets their stable avatar colour (same hue as the list
        // avatar), so the sender line stands out from the base-styled body.
        (
            "  ",
            display_name_of(other).to_string(),
            Style::default()
                .fg(avatar_color(&other.username))
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("→ ", "you".to_string(), theme.accent_style())
    };
    let mut header = vec![
        Span::styled(prefix, theme.muted_style()),
        Span::styled(who, who_style),
        Span::styled(format!(" · {when}"), theme.muted_style()),
    ];
    // Read receipt on your own outgoing messages: whether the other side has
    // read it yet.
    if !from_other {
        if m.read {
            header.push(Span::styled(" · ✓ read", theme.accent_style()));
        } else {
            header.push(Span::styled(" · sent", theme.muted_style()));
        }
    }
    vec![
        Line::from(header),
        // The body is the primary content, so it uses the base style; only the
        // metadata line above is muted.
        Line::from(Span::styled(format!("  {}", m.content), theme.base())),
    ]
}

/// A centred-ish separator line like `── Today ──` / `── new ──`, used between
/// day boundaries and before the first unread message.
fn separator_line(label: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("  ── {label} ──"),
        theme.muted_style(),
    ))
}

/// The message timestamp as a local `OffsetDateTime` (config timezone).
fn local_datetime(ms: i64) -> Option<time::OffsetDateTime> {
    let secs = ms.div_euclid(1000);
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .map(|t| t.to_offset(crate::config::get().tz_offset))
}

/// Calendar-day key (year, day-of-year) for grouping messages into date buckets.
fn day_key(t: time::OffsetDateTime) -> (i32, u16) {
    (t.year(), t.ordinal())
}

/// A day-separator label: "Today" / "Yesterday" / "Wed Jul 2".
fn day_separator_label(t: time::OffsetDateTime, now_local: time::OffsetDateTime) -> String {
    if day_key(t) == day_key(now_local) {
        return "Today".to_string();
    }
    if day_key(t) == day_key(now_local - time::Duration::days(1)) {
        return "Yesterday".to_string();
    }
    format!(
        "{} {} {}",
        weekday_abbr(t.weekday()),
        month_abbr(t.month()),
        t.day()
    )
}

fn weekday_abbr(w: time::Weekday) -> &'static str {
    use time::Weekday::*;
    match w {
        Monday => "Mon",
        Tuesday => "Tue",
        Wednesday => "Wed",
        Thursday => "Thu",
        Friday => "Fri",
        Saturday => "Sat",
        Sunday => "Sun",
    }
}

fn month_abbr(m: time::Month) -> &'static str {
    use time::Month::*;
    match m {
        January => "Jan",
        February => "Feb",
        March => "Mar",
        April => "Apr",
        May => "May",
        June => "Jun",
        July => "Jul",
        August => "Aug",
        September => "Sep",
        October => "Oct",
        November => "Nov",
        December => "Dec",
    }
}

/// Format a ms-epoch timestamp as a compact relative age ("now", "5m", "3h",
/// "2d"), falling back to an absolute date for anything older than a week.
fn format_epoch_millis_relative(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let Ok(t) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return String::new();
    };
    let elapsed = (time::OffsetDateTime::now_utc() - t).whole_seconds();
    match elapsed {
        e if e < 0 => "now".to_string(),
        e if e < 60 => "now".to_string(),
        e if e < 3_600 => format!("{}m", e / 60),
        e if e < 86_400 => format!("{}h", e / 3_600),
        e if e < 7 * 86_400 => format!("{}d", e / 86_400),
        _ => crate::config::format_list_timestamp(t),
    }
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
    fn slash_filters_the_conversation_list_and_enter_opens_a_match() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![
            convo("c1", "alice"),
            convo("c2", "bob"),
            convo("c3", "carol"),
        ]));
        // `/` opens the filter; typing narrows to matches.
        assert_eq!(s.handle_key(key(KeyCode::Char('/'))), CmailIntent::None);
        assert!(s.is_filtering());
        assert!(s.is_text_input());
        s.handle_key(key(KeyCode::Char('b')));
        assert_eq!(s.conversation_view(), vec![1]); // only "bob"
                                                    // Enter opens the single match, regardless of the raw item index.
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CmailIntent::OpenConversation {
                conversation_id: "c2".into()
            }
        );
    }

    #[test]
    fn esc_closes_the_conversation_filter_before_leaving() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.handle_key(key(KeyCode::Char('/')));
        s.handle_key(key(KeyCode::Char('z'))); // no matches
        assert!(s.is_filtering());
        // Esc (via the app's handle_escape) clears the filter and stays put.
        assert_eq!(s.handle_escape(), Some(CmailIntent::None));
        assert!(!s.is_filtering());
        // A second Esc now falls through (None → app opens the menu).
        assert_eq!(s.handle_escape(), None);
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

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn open_with_messages(msgs: Vec<CmailMessage>, cursor: Option<String>) -> CmailScreen {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        s.apply_messages("c1", true, Ok((msgs, cursor)));
        s
    }

    #[test]
    fn c_focuses_the_inline_composer_and_ctrl_e_expands_to_the_editor() {
        let mut s = open_with_messages(vec![], None);
        // `c` focuses the inline composer (captures text) without opening the editor.
        assert_eq!(s.handle_key(key(KeyCode::Char('c'))), CmailIntent::None);
        assert!(s.is_text_input());
        s.handle_key(key(KeyCode::Char('h')));
        s.handle_key(key(KeyCode::Char('i')));
        // Ctrl+E expands into the full editor, carrying the draft.
        assert_eq!(
            s.handle_key(ctrl(KeyCode::Char('e'))),
            CmailIntent::StartCompose {
                conversation_id: "c1".into(),
                draft: "hi".into(),
            }
        );
    }

    #[test]
    fn typing_a_message_and_pressing_enter_sends_optimistically() {
        let mut s = open_with_messages(vec![], None);
        s.handle_key(key(KeyCode::Char('c'))); // focus composer
        for c in "yo".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CmailIntent::SendMessage {
                conversation_id: "c1".into(),
                content: "yo".into(),
            }
        );
        // The draft clears and the message shows immediately as pending.
        assert_eq!(s.outgoing.len(), 1);
        assert_eq!(s.outgoing[0].content, "yo");
        assert!(!s.outgoing[0].failed);
    }

    #[test]
    fn a_failed_send_marks_the_outgoing_and_ctrl_r_retries_it() {
        let mut s = open_with_messages(vec![], None);
        s.handle_key(key(KeyCode::Char('c')));
        for c in "yo".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        s.handle_key(key(KeyCode::Enter));
        // Server rejects it.
        assert!(!s.finish_send("c1", "yo", Err("boom".into())));
        assert!(s.outgoing[0].failed);
        // Ctrl+R re-sends the failed message and clears its failed flag.
        assert_eq!(
            s.handle_key(ctrl(KeyCode::Char('r'))),
            CmailIntent::RetryFailed {
                conversation_id: "c1".into(),
                contents: vec!["yo".into()],
            }
        );
        assert!(!s.outgoing[0].failed);
    }

    #[test]
    fn opening_conversation_selects_newest_message() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");

        s.apply_messages(
            "c1",
            true,
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
            true,
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
        // Sender headers now show the display name (username when unset), no "@".
        assert!(row_text(6).contains("alice"), "{rows}");
        assert!(row_text(7).contains("oldest-visible"), "{rows}");
        assert!(row_text(8).contains("alice"), "{rows}");
        assert!(row_text(9).contains("newest-visible"), "{rows}");
    }

    #[test]
    fn conversation_shows_day_separator_and_unread_divider() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        // Incoming + unread → triggers the "new" divider; the first message also
        // gets a day separator.
        let mut unread = message("m1", "hello there", 1_000);
        unread.read = false;
        s.apply_messages("c1", true, Ok((vec![unread], None)));

        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| s.render(f, f.area(), &theme)).unwrap();

        let buffer = terminal.backend().buffer();
        let text: String = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol())
            .collect();
        assert!(
            text.contains("── new ──"),
            "unread divider missing:\n{text}"
        );
        assert!(
            text.contains("hello there"),
            "message body missing:\n{text}"
        );
    }

    #[test]
    fn scrolling_up_from_oldest_loads_older_and_prepends_without_jumping() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        s.apply_messages(
            "c1",
            true,
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
            false,
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
    fn editor_text_returns_to_the_composer_for_a_final_review() {
        // The full editor is an expanded surface: its text comes back to the
        // inline draft (focused), and Enter there sends it.
        let mut s = open_with_messages(vec![], None);
        s.set_draft_and_focus("expanded message".into());
        assert!(s.is_text_input());
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CmailIntent::SendMessage {
                conversation_id: "c1".into(),
                content: "expanded message".into(),
            }
        );
    }

    #[test]
    fn reload_after_send_replaces_the_list_instead_of_prepending_it() {
        // Regression: a post-send reload uses `before = None`, i.e. it re-fetches
        // the newest page. It must replace the thread, not be mistaken for an
        // older-page prepend (which duplicated and mis-ordered every message).
        let mut s = open_with_messages(
            vec![
                message("m1", "first", 1_000),
                message("m2", "second", 2_000),
            ],
            Some("1000".into()),
        );

        // Send "third" inline (optimistic), then the server confirms it.
        s.handle_key(key(KeyCode::Char('c')));
        for c in "third".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        s.handle_key(key(KeyCode::Enter));
        assert!(s.finish_send("c1", "third", Ok(())));
        assert!(
            s.outgoing.is_empty(),
            "confirmed send clears the optimistic entry"
        );

        // The reload (before = None → initial) returns the newest page, now
        // including the just-sent message.
        s.apply_messages(
            "c1",
            true,
            Ok((
                vec![
                    message("m1", "first", 1_000),
                    message("m2", "second", 2_000),
                    message("m3", "third", 3_000),
                ],
                Some("1000".into()),
            )),
        );

        let CmailMode::Conversation { messages, .. } = &s.mode else {
            panic!("conversation should remain open after send");
        };
        assert_eq!(
            messages
                .items
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"],
            "reload must replace, not prepend-and-duplicate"
        );
        assert_eq!(messages.selected, 2);
        assert_eq!(messages.items[messages.selected].content, "third");
    }

    #[test]
    fn apply_live_dedupes_and_follows_the_tail_when_pinned_to_bottom() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        s.apply_messages("c1", true, Ok((vec![message("m1", "hi", 1_000)], None)));

        // The live stream's opening window re-delivers m1 and adds a new m2.
        s.apply_live(
            "c1",
            vec![
                CmailLiveUpdate::Message(message("m1", "hi", 1_000)),
                CmailLiveUpdate::Message(message("m2", "reply", 2_000)),
            ],
        );

        let CmailMode::Conversation { messages, .. } = &s.mode else {
            panic!("conversation should remain open");
        };
        assert_eq!(messages.items.len(), 2, "duplicate m1 must be de-duped");
        assert_eq!(messages.items[1].content, "reply");
        assert_eq!(
            messages.selected, 1,
            "the view follows the new tail when it was pinned to the bottom"
        );
    }

    #[test]
    fn apply_live_read_update_flips_flag_without_touching_content() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice")]));
        s.open_conversation("c1");
        // An outgoing message, not yet read by the other side.
        let mut mine = message("m1", "you there?", 1_000);
        mine.read = false;
        s.apply_messages("c1", true, Ok((vec![mine], None)));

        // A read receipt arrives as a bare read-flag flip.
        s.apply_live(
            "c1",
            vec![CmailLiveUpdate::Read {
                id: "m1".into(),
                read: true,
            }],
        );

        let CmailMode::Conversation { messages, .. } = &s.mode else {
            panic!("conversation should remain open");
        };
        assert_eq!(messages.items.len(), 1, "no phantom message is created");
        assert_eq!(messages.items[0].content, "you there?", "content preserved");
        assert!(messages.items[0].read, "read flag flipped by the receipt");
    }
}

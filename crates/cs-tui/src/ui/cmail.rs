//! C-Mail screen — private 1:1 conversations.
//!
//! Message bodies are drawn by the shared [`super::chat`] primitives, so a
//! C-Mail message renders exactly as its cIRC twin does: wrapped text, decoded
//! art, a text style, the third-person action form and one compact chip per
//! attachment (§ Message fields). C-Mail has neither delete nor flag in v0.8.4,
//! so there is no tombstone and no moderation key here.
//!
//! The screen also carries both halves of the typing indicator (§ Typing
//! Indicator): the inbound one is re-derived from the presence entries on every
//! render, since a flag going stale produces no event, and the outbound one is
//! reported to the shell as [`CmailIntent::TypingActive`] /
//! [`CmailIntent::TypingIdle`] so the shell can throttle the network calls.
use std::collections::HashSet;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{
    CmailConversation, CmailMessage, CmailPresence, CmailPresenceUpdate, CmailTypingResponse,
    CmailTypingStatus, CmailUser,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::chat;
use super::list::{self, TabState};
use super::theme::Theme;

/// Maximum optimistic-outgoing lines shown before collapsing to a "+N more".
const MAX_OUTGOING_ROWS: usize = 4;

/// Collapse a (possibly multi-line) string to a single truncated line for a
/// compact preview.
pub(crate) fn one_line_preview(s: &str, max_chars: usize) -> String {
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
pub(crate) fn avatar_color(username: &str) -> Color {
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
    /// The user typed into the inline composer and it now holds an unsent
    /// draft: publish the typing flag (§ Typing Indicator).
    ///
    /// Emitted on every keystroke, deliberately: the screen has no clock, so
    /// the shell owns the throttle and the heartbeat.
    TypingActive {
        conversation_id: String,
    },
    /// The inline composer went idle (the draft was emptied) or lost focus:
    /// clear the typing flag rather than waiting for it to age out.
    TypingIdle {
        conversation_id: String,
    },
    /// Open the selected message's picture in the user's browser (`o`).
    OpenUrl(String),
    /// Play the selected message's jukebox track (`o`), which outranks a
    /// picture on a message carrying both.
    PlayJukebox(super::audio::JukeboxTrack),
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

/// The inbound typing indicator for the open conversation: whatever the
/// `dm_presence/<conversationId>` RTDB node currently holds, plus the staleness
/// window the server asked for (§ Typing Indicator).
///
/// Both participants appear on that node (this client publishes its own flag
/// there too), so the reader is picked out at render time rather than on
/// arrival, and the flag is re-evaluated against the clock every frame because
/// one going stale produces no event to react to.
#[derive(Debug)]
struct TypingState {
    /// One entry per participant, keyed by user id. Two at most in a 1:1
    /// conversation, so a `Vec` beats a map.
    entries: Vec<CmailPresence>,
    /// How long a flag survives without a refresh.
    stale_after: Duration,
}

impl Default for TypingState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            // The server states the window on every typing call, so this only
            // stands in until one has answered. Taken from cs-api's own
            // fallback rather than written out here, so the figure lives in one
            // place.
            stale_after: CmailTypingResponse::default().stale_after(),
        }
    }
}

impl TypingState {
    /// Merge one decoded RTDB update into the held entries.
    fn apply(&mut self, update: CmailPresenceUpdate) {
        match update {
            CmailPresenceUpdate::Full(entry) => self.upsert(entry),
            CmailPresenceUpdate::Partial { user_id, patch } => {
                // A fragment is not an entry: with nothing held to merge into
                // there is nothing to show, which is the rule cs-api documents.
                if let Some(entry) = self.entries.iter_mut().find(|e| e.user_id == user_id) {
                    patch.apply_to(entry);
                }
            }
            CmailPresenceUpdate::Removed { user_id } => {
                self.entries.retain(|e| e.user_id != user_id);
            }
        }
    }

    /// Replace the entry held for this user id, or add it.
    fn upsert(&mut self, entry: CmailPresence) {
        match self.entries.iter_mut().find(|e| e.user_id == entry.user_id) {
            Some(held) => *held = entry,
            None => self.entries.push(entry),
        }
    }
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
    /// Ids of the messages whose spoiler the reader has revealed with `v`.
    /// Reader state, not message state, so it lives here and not on the wire.
    revealed: HashSet<String>,
    /// The other participant's live typing state.
    typing: TypingState,
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
            revealed: HashSet::new(),
            typing: TypingState::default(),
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
        // Both are per-thread reader state: a spoiler revealed in one
        // conversation stays hidden in the next, and the other participant's
        // typing flag has nothing to say about the thread being opened.
        self.revealed.clear();
        self.typing = TypingState::default();
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

    /// The conversation whose typing flag should be kept alive, i.e. the open
    /// one when its inline composer is focused and holds an unsent draft
    /// (§ Typing Indicator).
    ///
    /// The shell drives the heartbeat off this rather than off the intents
    /// alone, because two paths change the draft without a keystroke reaching
    /// [`Self::handle_key`]: a bracketed paste ([`Self::paste_text`]) and the
    /// full editor handing its text back ([`Self::set_draft_and_focus`]).
    pub fn typing_conversation(&self) -> Option<&str> {
        match &self.mode {
            CmailMode::Conversation { conversation, .. }
                if self.composing && !self.draft.trim().is_empty() =>
            {
                Some(&conversation.conversation_id)
            }
            _ => None,
        }
    }

    /// Whether `conversation_id` is the conversation currently open.
    fn is_open_conversation(&self, conversation_id: &str) -> bool {
        matches!(
            &self.mode,
            CmailMode::Conversation { conversation, .. }
                if conversation.conversation_id == conversation_id
        )
    }

    /// Merge live typing-indicator changes into the open conversation, as
    /// decoded from the `dm_presence/<conversationId>` RTDB node by
    /// [`cs_api::cmail_presence_updates_from_rtdb_event`]
    /// (§ Reading in real time).
    ///
    /// Updates for any other conversation are dropped: only one is open at a
    /// time, and a late event from the previous one must not raise an indicator
    /// over this one.
    pub fn apply_typing_presence(
        &mut self,
        conversation_id: &str,
        updates: Vec<CmailPresenceUpdate>,
    ) {
        if !self.is_open_conversation(conversation_id) {
            return;
        }
        for update in updates {
            self.typing.apply(update);
        }
    }

    /// Record the staleness window the server asked for (§ Typing Indicator),
    /// read off the response to `POST /v1/cmail/:conversationId/typing`.
    ///
    /// The spec is explicit that the figure comes off the response rather than
    /// being assumed, and it is the window this screen ages inbound flags out
    /// with, so the outbound call is what teaches it.
    pub fn set_typing_stale_after(&mut self, conversation_id: &str, stale_after: Duration) {
        if !self.is_open_conversation(conversation_id) {
            return;
        }
        self.typing.stale_after = stale_after;
    }

    /// Apply a polled `GET /v1/cmail/:conversationId/typing` answer
    /// (§ Typing Indicator).
    ///
    /// The status is the answer as of the moment of the call, so the entry is
    /// stamped with the time it arrived rather than with `since` (which is when
    /// they started composing, not when the flag was last refreshed). It then
    /// ages out from there like any other entry, so a poll that is never
    /// followed up cannot leave the indicator stuck on.
    pub fn apply_typing_status(&mut self, conversation_id: &str, status: &CmailTypingStatus) {
        let CmailMode::Conversation { conversation, .. } = &self.mode else {
            return;
        };
        if conversation.conversation_id != conversation_id {
            return;
        }
        let other = &conversation.other_user;
        self.typing.stale_after = status.stale_after();
        if status.typing && !(status.user_id.is_empty() && status.username.is_empty()) {
            self.typing.upsert(CmailPresence {
                user_id: status.user_id.clone(),
                username: status.username.clone(),
                typing: true,
                timestamp: now_epoch_millis(),
            });
        } else {
            // Nobody is typing, so drop what is held for them instead of
            // waiting for it to age out.
            self.typing
                .entries
                .retain(|e| !presence_from_other(e, other));
        }
    }

    /// The message the reader has selected in the open conversation.
    fn selected_message(&self) -> Option<&CmailMessage> {
        match &self.mode {
            CmailMode::Conversation { messages, .. } => messages.items.get(messages.selected),
            _ => None,
        }
    }

    /// The "…is typing" line for the open conversation as of `now_ms`, or
    /// `None` when the other participant is not typing.
    ///
    /// Re-derived every render rather than latched, because § Typing Indicator
    /// makes the flag expire on a clock: it counts only while `typing` is set
    /// *and* the entry is newer than the staleness window, and a flag going
    /// stale produces no event, so a latched indicator would never come down.
    fn typing_label(&self, now_ms: i64) -> Option<String> {
        let CmailMode::Conversation { conversation, .. } = &self.mode else {
            return None;
        };
        let other = &conversation.other_user;
        let stale_after = self.typing.stale_after;
        self.typing
            .entries
            .iter()
            .find(|e| presence_from_other(e, other) && e.is_typing_at(now_ms, stale_after))
            .map(|_| format!("{} is typing…", display_name_of(other)))
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
            CmailMode::Conversation { conversation, .. } if self.composing => {
                // First Esc unfocuses the composer (keeping the draft); a second
                // Esc then leaves the conversation. Unfocusing is the input
                // going idle, so the typing flag comes down now rather than
                // ageing out (§ Typing Indicator).
                let conversation_id = conversation.conversation_id.clone();
                self.composing = false;
                Some(CmailIntent::TypingIdle { conversation_id })
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

    /// Merge messages that arrived over the live RTDB stream into the open
    /// conversation, de-duped by id and kept in timestamp order. If the view was
    /// pinned to the newest message it follows the new tail; otherwise the
    /// caller's scroll position is preserved.
    pub fn apply_live(&mut self, conversation_id: &str, incoming: Vec<CmailMessage>) {
        let messages = match &mut self.mode {
            CmailMode::Conversation {
                conversation,
                messages,
            } if conversation.conversation_id == conversation_id => messages,
            _ => return,
        };
        let fresh: Vec<CmailMessage> = incoming.into_iter().filter(|m| !m.id.is_empty()).collect();
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
        // The indicator only claims a row while it is live, so an idle thread
        // renders exactly as it did before there was one.
        let typing = self.typing_label(now_epoch_millis());
        let typing_rows = u16::from(typing.is_some());
        let footer_rows = if self.composing { 2 } else { 1 };
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(out_rows),
                Constraint::Length(typing_rows),
                Constraint::Length(footer_rows),
            ])
            .split(inner);

        let visible: Vec<usize> = (0..messages.items.len()).collect();
        let unread_from = first_unread_index(&messages.items, other, conversation.unread_count);
        // Wrap each body to the pane's content width: the list reserves a 2-col
        // highlight gutter and every body row carries a 2-space indent, so the
        // text flows within `width - 4`.
        let body_width = (layout[0].width as usize).saturating_sub(4).max(1);
        // No single message may exceed the pane. ratatui's `List` renders an
        // over-tall item as NOTHING, blanking the whole conversation, so a long
        // enough DM would hide the entire thread. One row is left for the
        // message's own header.
        let body_cap = (layout[0].height as usize).saturating_sub(1).max(1);
        let body_layout = chat::BodyLayout::new(body_width).with_max_rows(body_cap);
        let heights = message_row_heights(&messages.items, unread_from, body_layout);
        let content_rows: usize = heights.iter().map(|&h| usize::from(h)).sum();
        let mut messages_area = bottom_aligned_messages_area(layout[0], content_rows);
        // When the thread overflows the pane, ratatui's `List` tiles whole items
        // top-down from the scroll offset and cannot show a partial one at the
        // top, so it leaves the leftover rows blank at the *bottom*. Trim that
        // leftover off the top, sizing the pane to the tallest suffix of whole
        // messages that fits, so the newest message stays flush above the
        // composer. (The same fix cIRC needed once its messages could wrap.)
        if content_rows >= messages_area.height as usize {
            let mut suffix = 0u16;
            for &h in heights.iter().rev() {
                if suffix + h > messages_area.height {
                    break;
                }
                suffix += h;
            }
            // Only trim when at least one whole message fits; a single message
            // taller than the pane is left to ratatui (shows its top, clipped).
            if suffix > 0 {
                let remainder = messages_area.height - suffix;
                messages_area.y += remainder;
                messages_area.height -= remainder;
            }
        }
        // `render_body` calls the item closure for every message in order each
        // frame, so a couple of `Cell`s let us inject day separators and a single
        // "new" divider without an extra pass or breaking selection indices.
        let now_local = time::OffsetDateTime::now_utc().to_offset(crate::config::get().tz_offset);
        let last_day: std::cell::Cell<Option<(i32, u16)>> = std::cell::Cell::new(None);
        let idx = std::cell::Cell::new(0usize);
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
                if unread_from == Some(idx.get()) {
                    lines.push(separator_line("new", theme));
                }
                idx.set(idx.get() + 1);
                // Reveal state is per message and per reader, so it rides on the
                // layout handed to this one body.
                let item_layout = body_layout.with_revealed(self.revealed.contains(&m.id));
                lines.extend(message_lines(m, other, theme, item_layout));
                ListItem::new(lines)
            },
        );
        // Attachment chips become clickable links only after the pane is drawn,
        // and only against the very rect it was drawn into. Gated on the
        // `hyperlinks` config like every other OSC 8 surface in the client, so
        // turning it off really does leave the chips as plain text.
        if crate::config::get().hyperlinks {
            // Only the rows the list actually drew, which start at the offset it
            // just settled on. Handing over chips for scrolled-off messages
            // would slide every link onto the wrong message's attachment.
            let chips = chat::collect_chips(
                messages
                    .items
                    .iter()
                    .skip(messages.list_offset())
                    .map(chat::ChatMessage::from),
                body_layout,
            );
            chat::apply_chip_links(frame.buffer_mut(), messages_area, &chips, theme);
        }

        if out_rows > 0 {
            self.render_outgoing(frame, layout[1], theme);
        }
        if let Some(label) = typing {
            self.render_typing(frame, layout[2], theme, &label);
        }
        let scrolled_up = messages.selected + 1 < messages.items.len();
        self.render_conversation_footer(
            frame,
            layout[3],
            theme,
            messages.next_cursor.is_some(),
            scrolled_up,
        );
    }

    /// Draw the live "…is typing" line, between the pending-send strip and the
    /// composer so it sits where the next message will appear.
    fn render_typing(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, label: &str) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {label}"),
                theme.muted_style().add_modifier(Modifier::ITALIC),
            ))),
            area,
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
            // Offer `o` and `v` only where they do something, so the line stays
            // short and never promises an action the selection can't take.
            if let Some(m) = self.selected_message() {
                if !matches!(chat::open_action(&m.extras), chat::OpenAction::None) {
                    hint.push_str("o open · ");
                }
                if chat::has_spoiler(&m.extras) {
                    hint.push_str(if self.revealed.contains(&m.id) {
                        "v hide · "
                    } else {
                        "v reveal · "
                    });
                }
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
                self.typing_signal(conversation_id)
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
                self.typing_signal(conversation_id)
            }
            _ => CmailIntent::None,
        }
    }

    /// What the draft's new state says about the typing flag: still composing,
    /// or gone idle because the draft is now empty (§ Typing Indicator).
    ///
    /// Deliberately clock-free. The shell throttles the calls and runs the
    /// heartbeat, so all this owes it is the fact that a key landed.
    fn typing_signal(&self, conversation_id: &str) -> CmailIntent {
        let conversation_id = conversation_id.to_string();
        if self.draft.trim().is_empty() {
            CmailIntent::TypingIdle { conversation_id }
        } else {
            CmailIntent::TypingActive { conversation_id }
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent, conversation_id: &str) -> CmailIntent {
        match key.code {
            // `i` is deliberately not bound here: the shell's global inline-image
            // toggle claims it on every screen that isn't capturing text, so an
            // arm for it would never run.
            KeyCode::Char('c') | KeyCode::Enter => {
                self.composing = true;
                CmailIntent::None
            }
            // `o` plays the selected message's track, or opens its picture.
            KeyCode::Char('o') => self.open_selected_attachment(),
            // `v` reveals (or re-hides) the selected message's spoiler.
            KeyCode::Char('v') => self.toggle_selected_spoiler(),
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

    /// `o`: hand the selected message's track to the jukebox, or its picture to
    /// the desktop opener. Nothing to open is a no-op, not an error toast.
    fn open_selected_attachment(&self) -> CmailIntent {
        let Some(m) = self.selected_message() else {
            return CmailIntent::None;
        };
        match chat::open_action(&m.extras) {
            chat::OpenAction::Play(track) => CmailIntent::PlayJukebox(track),
            chat::OpenAction::Open(url) => CmailIntent::OpenUrl(url),
            chat::OpenAction::None => CmailIntent::None,
        }
    }

    /// `v`: reveal the selected message's spoiler, or hide it again.
    ///
    /// Only messages that actually carry the `spoiler` style respond, so the
    /// key cannot silently mark an ordinary message as read-through.
    fn toggle_selected_spoiler(&mut self) -> CmailIntent {
        let Some(id) = self
            .selected_message()
            .filter(|m| chat::has_spoiler(&m.extras))
            .map(|m| m.id.clone())
        else {
            return CmailIntent::None;
        };
        if !self.revealed.remove(&id) {
            self.revealed.insert(id);
        }
        CmailIntent::None
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

/// The item index at which the "── new ──" divider should be drawn, given the
/// conversation's `unread_count`. v0.8.4 has no per-message read flag, so this is
/// derived: the divider sits before the oldest of the last `unread_count`
/// messages the other participant sent. `None` when there's nothing unread.
fn first_unread_index(
    messages: &[CmailMessage],
    other: &CmailUser,
    unread_count: u32,
) -> Option<usize> {
    if unread_count == 0 {
        return None;
    }
    let mut remaining = unread_count;
    for (i, m) in messages.iter().enumerate().rev() {
        if message_from_other(m, other) {
            remaining -= 1;
            if remaining == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The rendered height of every message in the thread, in the order the list
/// draws them: the day separator and the single "new" divider that get folded
/// into the item, its header row, and its body.
///
/// The body is as tall as its wrapped text, decoded art and attachment chips
/// need, which is why this replaced a flat two rows per message: anything
/// taller than one line used to be measured short and clipped. Heights come
/// from [`chat::message_height`], which is derived from the same row list
/// [`chat::body_lines`] draws, so the measurement and the render cannot
/// disagree. Revealing a spoiler does not change a height, so the layout passed
/// here need not carry the reveal state.
fn message_row_heights(
    messages: &[CmailMessage],
    unread_from: Option<usize>,
    layout: chat::BodyLayout<'_>,
) -> Vec<u16> {
    let mut heights = Vec::with_capacity(messages.len());
    let mut last_day: Option<(i32, u16)> = None;
    for (i, m) in messages.iter().enumerate() {
        let mut extra = 0u16;
        if let Some(t) = local_datetime(m.timestamp) {
            let key = day_key(t);
            if last_day != Some(key) {
                last_day = Some(key);
                extra += 1;
            }
        }
        if unread_from == Some(i) {
            extra += 1;
        }
        heights.push(extra.saturating_add(chat::message_height(m.into(), layout, 1)));
    }
    heights
}

pub(crate) fn bottom_aligned_messages_area(area: Rect, content_rows: usize) -> Rect {
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
    // The same content rules the message body applies (§ Message fields): a
    // caption that merely repeats its attachment URL is skipped, and a message
    // that is nothing but an attachment previews as its chip rather than as a
    // blank row.
    let preview = c
        .last_message
        .as_ref()
        .map(|m| chat::summary_text(&m.extras, &m.content))
        .unwrap_or_else(|| "no messages yet".to_string());
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

/// Whether a presence entry belongs to the other participant (vs. the local
/// user, whose own typing flag is published to the same node).
///
/// Either identifier is enough, and neither is guaranteed: the entry's user id
/// is the RTDB key, while a polled status may name only a username.
fn presence_from_other(entry: &CmailPresence, other: &CmailUser) -> bool {
    if !other.user_id.is_empty() && entry.user_id == other.user_id {
        return true;
    }
    !other.username.is_empty() && entry.username == other.username
}

/// One message: a header row naming the sender and its age, then the shared
/// chat body (§ Message fields).
///
/// The header stays even for a `/me` action, whose body row already names the
/// sender, because it is the only thing carrying the timestamp and the `→ you`
/// marker that tells the two sides of a 1:1 thread apart.
fn message_lines(
    m: &CmailMessage,
    other: &CmailUser,
    theme: &Theme,
    layout: chat::BodyLayout<'_>,
) -> Vec<Line<'static>> {
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
    let header = vec![
        Span::styled(prefix, theme.muted_style()),
        Span::styled(who, who_style),
        Span::styled(format!(" · {when}"), theme.muted_style()),
    ];
    let mut lines = vec![Line::from(header)];
    // The body is the primary content, so it keeps the base style (only the
    // metadata line above is muted) and comes pre-wrapped and pre-indented:
    // never print `content` directly, since an empty caption, a caption that
    // repeats the attachment URL, base64 art and a text style all have to be
    // resolved first.
    lines.extend(chat::body_lines(m.into(), layout, theme));
    lines
}

/// A centred-ish separator line like `── Today ──` / `── new ──`, used between
/// day boundaries and before the first unread message.
fn separator_line(label: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("  ── {label} ──"),
        theme.muted_style(),
    ))
}

/// Now, in milliseconds since the Unix epoch: the clock a presence entry's
/// `timestamp` is on, and so the one the staleness rule is applied against.
fn now_epoch_millis() -> i64 {
    let millis = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    i64::try_from(millis).unwrap_or(i64::MAX)
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
pub(crate) fn format_epoch_millis_relative(ms: i64) -> String {
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
            extras: cs_api::MessageExtras::default(),
        }
    }

    /// A message from the other participant carrying v0.8.4 extras.
    fn message_with(
        id: &str,
        content: &str,
        timestamp: i64,
        extras: cs_api::MessageExtras,
    ) -> CmailMessage {
        CmailMessage {
            extras,
            ..message(id, content, timestamp)
        }
    }

    /// A `dm_presence` entry as the RTDB decoder hands it over.
    fn presence(user_id: &str, username: &str, typing: bool, timestamp: i64) -> CmailPresence {
        CmailPresence {
            user_id: user_id.into(),
            username: username.into(),
            typing,
            timestamp,
        }
    }

    /// The named text style, in the single-name wire shape.
    fn styled(name: &str) -> cs_api::MessageExtras {
        cs_api::MessageExtras {
            style: Some(cs_api::MessageStyle::One(name.into())),
            ..cs_api::MessageExtras::default()
        }
    }

    /// Render `s` into a `width` x `height` backend and return every cell
    /// symbol, row by row, as one string (escape sequences included).
    fn screen_text(s: &CmailScreen, width: u16, height: u16) -> String {
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| s.render(f, f.area(), &theme)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
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
        // unread_count on the conversation (v0.8.4 has no per-message read) drives
        // the "new" divider; the first message also gets a day separator.
        let mut c = convo("c1", "alice");
        c.unread_count = 1;
        s.apply_conversations(Ok(vec![c]));
        s.open_conversation("c1");
        // A message from alice (the other participant), so it counts as unread.
        s.apply_messages(
            "c1",
            true,
            Ok((vec![message("m1", "hello there", 1_000)], None)),
        );

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
            vec![message("m1", "hi", 1_000), message("m2", "reply", 2_000)],
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
    fn the_typing_indicator_shows_only_while_the_flag_is_fresh() {
        // § Typing Indicator: typing counts only while the flag is set *and*
        // the entry is newer than staleAfterMs, and a flag going stale produces
        // no event, so the answer has to be re-derived against the clock.
        let mut s = open_with_messages(vec![], None);
        s.set_typing_stale_after("c1", Duration::from_secs(9));
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Full(presence(
                "uid-alice",
                "alice",
                true,
                100_000,
            ))],
        );
        assert_eq!(
            s.typing_label(101_000).as_deref(),
            Some("alice is typing…"),
            "a second-old flag is live"
        );
        assert_eq!(
            s.typing_label(109_000),
            None,
            "the same entry is stale nine seconds on, with no event to say so"
        );
    }

    #[test]
    fn the_typing_indicator_ignores_your_own_presence_entry() {
        // Both participants publish to the same node, so the local user's own
        // flag must never come back as "…is typing".
        let mut s = open_with_messages(vec![], None);
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Full(presence(
                "uid-me", "me", true, 100_000,
            ))],
        );
        assert_eq!(s.typing_label(100_500), None);
    }

    #[test]
    fn a_presence_patch_keeps_the_indicator_up_between_heartbeats() {
        let mut s = open_with_messages(vec![], None);
        s.set_typing_stale_after("c1", Duration::from_secs(9));
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Full(presence(
                "uid-alice",
                "alice",
                true,
                100_000,
            ))],
        );
        // A heartbeat moves only the timestamp: merged in, it must not blank the
        // username or clear the flag beside it.
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Partial {
                user_id: "uid-alice".into(),
                patch: cs_api::CmailPresencePatch {
                    timestamp: Some(108_000),
                    ..cs_api::CmailPresencePatch::default()
                },
            }],
        );
        assert_eq!(
            s.typing_label(109_000).as_deref(),
            Some("alice is typing…"),
            "the refreshed entry is live again"
        );

        // A fragment for someone with no held entry is not an entry.
        let mut fresh = open_with_messages(vec![], None);
        fresh.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Partial {
                user_id: "uid-alice".into(),
                patch: cs_api::CmailPresencePatch {
                    typing: Some(true),
                    timestamp: Some(100_000),
                    ..cs_api::CmailPresencePatch::default()
                },
            }],
        );
        assert_eq!(fresh.typing_label(100_500), None);
    }

    #[test]
    fn clearing_the_flag_takes_the_indicator_down_at_once() {
        let mut s = open_with_messages(vec![], None);
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Full(presence(
                "uid-alice",
                "alice",
                true,
                100_000,
            ))],
        );
        assert!(s.typing_label(100_500).is_some());
        // `DELETE .../typing` removes the node rather than letting it age out.
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Removed {
                user_id: "uid-alice".into(),
            }],
        );
        assert_eq!(s.typing_label(100_500), None);
    }

    #[test]
    fn a_polled_typing_status_raises_and_lowers_the_indicator() {
        let mut s = open_with_messages(vec![], None);
        s.apply_typing_status(
            "c1",
            &CmailTypingStatus {
                conversation_id: "c1".into(),
                user_id: "uid-alice".into(),
                username: "alice".into(),
                typing: true,
                // Started composing long ago: the answer is still "typing now",
                // so `since` must not be mistaken for a refresh time.
                since: Some(1_000),
                stale_after_ms: 9_000,
            },
        );
        assert_eq!(
            s.typing_label(now_epoch_millis()).as_deref(),
            Some("alice is typing…")
        );

        s.apply_typing_status(
            "c1",
            &CmailTypingStatus {
                conversation_id: "c1".into(),
                ..CmailTypingStatus::default()
            },
        );
        assert_eq!(s.typing_label(now_epoch_millis()), None);
    }

    #[test]
    fn typing_updates_for_another_conversation_are_ignored() {
        let mut s = open_with_messages(vec![], None);
        s.apply_typing_presence(
            "c2",
            vec![CmailPresenceUpdate::Full(presence(
                "uid-alice",
                "alice",
                true,
                100_000,
            ))],
        );
        assert_eq!(s.typing_label(100_500), None);
    }

    #[test]
    fn the_typing_indicator_renders_between_the_thread_and_the_composer() {
        let mut s = open_with_messages(vec![message("m1", "hello there", 1_000)], None);
        assert!(
            !screen_text(&s, 60, 12).contains("is typing"),
            "an idle thread claims no row for it"
        );
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Full(presence(
                "uid-alice",
                "alice",
                true,
                now_epoch_millis(),
            ))],
        );
        let text = screen_text(&s, 60, 12);
        assert!(text.contains("alice is typing…"), "{text}");
    }

    #[test]
    fn typing_in_the_composer_signals_the_shell_and_going_idle_clears_it() {
        let mut s = open_with_messages(vec![], None);
        s.handle_key(key(KeyCode::Char('c'))); // focus the composer
        assert_eq!(
            s.handle_key(key(KeyCode::Char('y'))),
            CmailIntent::TypingActive {
                conversation_id: "c1".into()
            }
        );
        assert_eq!(s.typing_conversation(), Some("c1"));
        // Backspacing the draft away is the input going idle.
        assert_eq!(
            s.handle_key(key(KeyCode::Backspace)),
            CmailIntent::TypingIdle {
                conversation_id: "c1".into()
            }
        );
        assert_eq!(s.typing_conversation(), None);
    }

    #[test]
    fn unfocusing_the_composer_reports_the_input_idle_and_keeps_the_draft() {
        let mut s = open_with_messages(vec![], None);
        s.handle_key(key(KeyCode::Char('c')));
        s.handle_key(key(KeyCode::Char('h')));
        assert_eq!(
            s.handle_key(key(KeyCode::Esc)),
            CmailIntent::TypingIdle {
                conversation_id: "c1".into()
            }
        );
        assert!(!s.is_text_input(), "the composer lost focus");
        assert_eq!(s.draft_for_test(), "h", "but the draft survives");
        assert_eq!(s.typing_conversation(), None);
    }

    #[test]
    fn a_pasted_or_edited_draft_still_counts_as_typing() {
        // Neither path goes through `handle_key`, so the shell asks instead of
        // waiting for an intent that will never come.
        let mut s = open_with_messages(vec![], None);
        s.handle_key(key(KeyCode::Char('c')));
        s.paste_text("pasted body");
        assert_eq!(s.typing_conversation(), Some("c1"));

        let mut edited = open_with_messages(vec![], None);
        edited.set_draft_and_focus("from the editor".into());
        assert_eq!(edited.typing_conversation(), Some("c1"));
    }

    #[test]
    fn opening_another_conversation_drops_the_previous_typing_state() {
        let mut s = CmailScreen::new();
        s.apply_conversations(Ok(vec![convo("c1", "alice"), convo("c2", "bob")]));
        s.open_conversation("c1");
        s.apply_typing_presence(
            "c1",
            vec![CmailPresenceUpdate::Full(presence(
                "uid-alice",
                "alice",
                true,
                now_epoch_millis(),
            ))],
        );
        assert!(s.typing_label(now_epoch_millis()).is_some());
        s.open_conversation("c2");
        assert_eq!(s.typing_label(now_epoch_millis()), None);
    }

    #[test]
    fn a_long_message_wraps_instead_of_being_clipped() {
        // Regression: the thread hard-coded two rows per message and never
        // wrapped the body, so everything past the pane width was lost.
        let long = "the quick brown fox jumps over the lazy dog and keeps on running";
        let s = open_with_messages(vec![message("m1", long, 1_000)], None);
        let text = screen_text(&s, 40, 12);
        assert!(text.contains("the quick brown"), "{text}");
        assert!(
            text.contains("keeps on running"),
            "the tail must wrap onto another row: {text}"
        );
    }

    #[test]
    fn message_heights_follow_the_body_that_is_drawn() {
        let plain = message("m1", "hello", 1_000);
        let captioned = message_with(
            "m2",
            "look at this",
            2_000,
            cs_api::MessageExtras {
                image_url: Some("https://cdn.example/pic.png".into()),
                ..cs_api::MessageExtras::default()
            },
        );
        let heights = message_row_heights(&[plain, captioned], Some(1), chat::BodyLayout::new(40));
        assert_eq!(
            heights,
            vec![2 + 1, 3 + 1],
            "day separator + header + body, then the unread divider + header + caption + chip",
        );
    }

    #[test]
    fn an_attachment_renders_as_a_chip_carrying_its_link() {
        let s = open_with_messages(
            vec![message_with(
                "m1",
                "",
                1_000,
                cs_api::MessageExtras {
                    image_url: Some("https://cdn.example/pic.png".into()),
                    ..cs_api::MessageExtras::default()
                },
            )],
            None,
        );
        let text = screen_text(&s, 60, 12);
        assert!(text.contains("[image]"), "{text}");
        assert!(
            text.contains("]8;;https://cdn.example/pic.png"),
            "the chip must carry an OSC 8 link: {text}"
        );
    }

    #[test]
    fn an_action_renders_in_the_third_person() {
        let s = open_with_messages(
            vec![message_with(
                "m1",
                "waves",
                1_000,
                cs_api::MessageExtras {
                    is_action: true,
                    ..cs_api::MessageExtras::default()
                },
            )],
            None,
        );
        let text = screen_text(&s, 60, 12);
        assert!(text.contains("* alice waves"), "{text}");
    }

    #[test]
    fn o_plays_a_track_and_otherwise_opens_the_picture() {
        let track = cs_api::AudioAttachment {
            src: "https://youtu.be/dQw4w9WgXcQ".into(),
            origin: "youtube".into(),
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            genre: None,
        };
        let mut s = open_with_messages(
            vec![message_with(
                "m1",
                "listen to this",
                1_000,
                cs_api::MessageExtras {
                    audio_attachment: Some(track),
                    ..cs_api::MessageExtras::default()
                },
            )],
            None,
        );
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            CmailIntent::PlayJukebox(super::super::audio::JukeboxTrack {
                url: "https://youtu.be/dQw4w9WgXcQ".into(),
                artist: "Art of Noise".into(),
                title: "Paranoimia".into(),
            })
        );

        let mut gif = open_with_messages(
            vec![message_with(
                "m1",
                "",
                1_000,
                cs_api::MessageExtras {
                    gif_url: Some("https://cdn.example/a.gif".into()),
                    ..cs_api::MessageExtras::default()
                },
            )],
            None,
        );
        assert_eq!(
            gif.handle_key(key(KeyCode::Char('o'))),
            CmailIntent::OpenUrl("https://cdn.example/a.gif".into())
        );

        // Nothing attached: the key is simply inert.
        let mut plain = open_with_messages(vec![message("m1", "hi", 1_000)], None);
        assert_eq!(plain.handle_key(key(KeyCode::Char('o'))), CmailIntent::None);
    }

    #[test]
    fn v_reveals_a_spoiler_and_hides_it_again() {
        let mut s = open_with_messages(
            vec![message_with(
                "m1",
                "the butler did it",
                1_000,
                styled("spoiler"),
            )],
            None,
        );
        assert!(
            !screen_text(&s, 60, 12).contains("butler"),
            "a spoiler is masked until the reader asks for it"
        );
        assert_eq!(s.handle_key(key(KeyCode::Char('v'))), CmailIntent::None);
        let revealed = screen_text(&s, 60, 12);
        assert!(revealed.contains("the butler did it"), "{revealed}");
        // The same key puts it back.
        s.handle_key(key(KeyCode::Char('v')));
        assert!(!screen_text(&s, 60, 12).contains("butler"));
    }

    #[test]
    fn v_does_nothing_to_a_message_without_a_spoiler() {
        let mut s = open_with_messages(vec![message("m1", "plain text", 1_000)], None);
        assert_eq!(s.handle_key(key(KeyCode::Char('v'))), CmailIntent::None);
        assert!(screen_text(&s, 60, 12).contains("plain text"));
    }

    #[test]
    fn the_browse_footer_offers_only_the_keys_the_selection_can_use() {
        let plain = open_with_messages(vec![message("m1", "hi", 1_000)], None);
        let text = screen_text(&plain, 70, 12);
        assert!(!text.contains("o open"), "{text}");
        assert!(!text.contains("v reveal"), "{text}");

        let spoiler = open_with_messages(
            vec![message_with("m1", "hidden", 1_000, styled("spoiler"))],
            None,
        );
        assert!(screen_text(&spoiler, 70, 12).contains("v reveal"));

        let gif = open_with_messages(
            vec![message_with(
                "m1",
                "",
                1_000,
                cs_api::MessageExtras {
                    gif_url: Some("https://cdn.example/a.gif".into()),
                    ..cs_api::MessageExtras::default()
                },
            )],
            None,
        );
        assert!(screen_text(&gif, 70, 12).contains("o open"));
    }

    #[test]
    fn the_inline_image_toggle_key_is_not_bound_in_browse_mode() {
        // `i` reaches the shell's global toggle, never this screen, so binding
        // it here would be a promise the screen cannot keep.
        let mut s = open_with_messages(vec![message("m1", "hi", 1_000)], None);
        assert_eq!(s.handle_key(key(KeyCode::Char('i'))), CmailIntent::None);
        assert!(!s.is_text_input(), "it must not focus the composer");
    }
}

//! cIRC screen: multi-user chat rooms (API v0.8.4).
//!
//! Structurally a sibling of [`super::cmail`]: a room list, then a room view with
//! a message list, an inline composer (optimistic send), and live RTDB updates.
//! Shares the small chat-rendering helpers from `cmail` and the message-body
//! renderer from [`super::chat`].
//!
//! v0.8.4 adds four things on top of that:
//!
//! - message bodies carry attachments, text styles and command results, all
//!   rendered by [`super::chat::body_lines`] (§ Message fields),
//! - a deletion arrives as a *partial* RTDB patch on a message you already
//!   hold, so [`CircScreen::apply_live`] merges rather than replaces
//!   (§ Reading a room in real time),
//! - a room has a live user list, shown in the roster pane on `Ctrl+U`
//!   (§ Who's in a room),
//! - deleting, flagging, muting, opening an attachment and revealing a spoiler
//!   all need a bare letter, which the always-on composer owns, so they live in
//!   a message-select sub-mode entered with `Ctrl+B`.
use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{
    CircMessage, CircMessageUpdate, CircPresenceEntry, CircPresenceResponse, CircPresenceUpdate,
    CircRoom, CircRoomUser, MessageExtras,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::audio::JukeboxTrack;
use super::chat::{self, BodyLayout, ChatMessage, OpenAction};
use super::cmail::{
    avatar_color, bottom_aligned_messages_area, format_epoch_millis_relative, one_line_preview,
    Outgoing,
};
use super::flag::FlagPromptKey;
use super::list::{self, TabState};
use super::theme::Theme;

const MAX_OUTGOING_ROWS: usize = 4;

/// Columns the roster pane takes when it is open (`Ctrl+U`). Wide enough for a
/// handle plus the admin star and the idle mark.
const ROSTER_WIDTH: u16 = 20;

/// Narrowest message pane worth keeping. Below this the roster pane stays
/// folded away however the toggle is set, so a small terminal never squeezes
/// the conversation down to a column of single letters.
const MIN_MESSAGES_WIDTH: u16 = 24;

/// What the website puts next to an idle person's name (§ Who's in a room).
const IDLE_MARK: &str = "\u{1f4a4}";

/// The wire `content` of a deleted message (§ Delete Your Message). Never
/// rendered: [`super::chat::body_lines`] draws a tombstone instead.
const DELETED_CONTENT: &str = "[DELETED]";

/// What the cIRC screen asks the shell to do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircIntent {
    /// Re-fetch the room list.
    RefreshRooms,
    /// Open a room and start loading its history.
    OpenRoom {
        /// Room slug (`:roomId`).
        room_id: String,
    },
    /// Page older history in the open room.
    LoadOlder {
        /// Room slug (`:roomId`).
        room_id: String,
        /// Cursor timestamp, the oldest message we hold.
        before: Option<i64>,
    },
    /// Hand the draft to the full editor (`Ctrl+E`), which is the only way to
    /// compose the multi-line body `/art` needs.
    StartCompose {
        /// Room slug (`:roomId`).
        room_id: String,
        /// The composer's current text.
        draft: String,
    },
    /// Send the composed message.
    SendMessage {
        /// Room slug (`:roomId`).
        room_id: String,
        /// The message body, already trimmed (see [`send_content`]).
        content: String,
    },
    /// Re-send everything a failed send left in the outgoing strip.
    RetryFailed {
        /// Room slug (`:roomId`).
        room_id: String,
        /// The bodies to retry, oldest first.
        contents: Vec<String>,
    },
    /// Delete one of your own messages (§ Delete Your Message). Confirmed
    /// already: the screen ran the two-step `d` then `y`.
    DeleteMessage {
        /// Room slug (`:roomId`).
        room_id: String,
        /// Which message to tombstone.
        message_id: String,
    },
    /// Report someone else's message (§ Flag a Message).
    FlagMessage {
        /// Room slug (`:roomId`).
        room_id: String,
        /// Which message to report.
        message_id: String,
        /// The typed reason, or `None` when the reader submitted an empty one.
        /// The reason is optional, so `None` is a valid report.
        reason: Option<String>,
    },
    /// Mute a handle in this room (§ Commands, "Muting"). Muting is a slash
    /// command, not an endpoint, so the shell posts `/mute <username>`.
    MuteUser {
        /// Room slug (`:roomId`).
        room_id: String,
        /// The handle to mute, exactly as the message carried it.
        username: String,
    },
    /// Re-read the room's user list (§ Who's in a room), emitted when the
    /// roster pane is opened.
    LoadRoomUsers {
        /// Room slug (`:roomId`).
        room_id: String,
    },
    /// Open an attachment (an image or a GIF) with the desktop handler.
    OpenUrl(String),
    /// Play the selected message's jukebox track.
    PlayJukebox(JukeboxTrack),
    /// Leave the open room and go back to the room list.
    BackToRooms,
    /// Exit the app.
    Quit,
    /// Nothing to do.
    None,
}

/// Which of the screen's two views is showing.
// One `CircMode` exists at a time; the size gap doesn't warrant boxing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum CircMode {
    /// The room list.
    Rooms,
    /// One open room.
    Room {
        /// The room being read.
        room: CircRoom,
        /// Its history. `selected` indexes the *visible* view, i.e. what is
        /// left after muted authors are filtered out.
        messages: TabState<CircMessage>,
        /// Message-select sub-mode state (`Ctrl+B`).
        select: SelectState,
        /// Who is in the room (§ Who's in a room).
        roster: Roster,
    },
}

/// The message-select sub-mode, entered with `Ctrl+B`.
///
/// Every bare letter in a room goes to the always-on composer, so the per
/// message actions (delete, flag, open, reveal, mute) need a mode of their own
/// where the composer is not focused.
#[derive(Debug, Default)]
pub struct SelectState {
    /// Whether the mode has the keyboard.
    active: bool,
    /// Two-step delete: `d` arms it, `y` confirms (the convention journal,
    /// bookmarks and post detail already use).
    confirming_delete: bool,
    /// The open flag-reason prompt, if any.
    flag: Option<MessageFlagPrompt>,
    /// Ids of the messages whose spoiler the reader has revealed with `v`.
    /// Reader state, not message state, so it lives here and dies with the room.
    revealed: HashSet<String>,
}

/// The optional-reason prompt `F` opens (§ Flag a Message): the shared
/// single-line field over the reported message's id, so reporting types the
/// same in a room as it does on the feeds and on a post's detail view.
type MessageFlagPrompt = super::flag::FlagPrompt<String>;

/// A room's live user list (§ Who's in a room).
///
/// Held as presence entries rather than as the REST shape so a partial patch
/// from the `chat_presence/<roomId>` stream can be merged straight in; the REST
/// snapshot converts into the same shape on arrival.
#[derive(Debug)]
pub struct Roster {
    /// Everyone we have heard about, keyed by `user_id`. Filtered for staleness
    /// at render time, never on arrival, since an entry going stale produces no
    /// event of its own.
    entries: Vec<CircPresenceEntry>,
    /// How long a heartbeat stays good for, read off the presence response.
    stale_after_ms: i64,
    /// How long without activity counts as idle, read off the same response.
    idle_after_ms: i64,
    /// Whether the first user-list fetch is still in flight.
    loading: bool,
    /// The last user-list error, shown only when there is nobody to show.
    error: Option<String>,
}

impl Default for Roster {
    fn default() -> Self {
        // The documented cadence, until a presence response says otherwise.
        let cadence = CircPresenceResponse::default();
        Self {
            entries: Vec::new(),
            stale_after_ms: cadence.stale_after_ms,
            idle_after_ms: cadence.idle_after_ms,
            loading: true,
            error: None,
        }
    }
}

/// The cIRC screen: a room list, and one open room at a time.
#[derive(Debug)]
pub struct CircScreen {
    /// The room list.
    pub rooms: TabState<CircRoom>,
    /// Room list, or one open room.
    pub mode: CircMode,
    /// Always-on inline composer buffer for the open room (it's a chat channel,
    /// so the input is focused the whole time you're in a room).
    draft: String,
    /// Optimistic outgoing messages awaiting their server echo.
    outgoing: Vec<Outgoing>,
    /// Whether the roster pane is open. Kept on the screen rather than on the
    /// room so the preference survives moving between rooms.
    roster_open: bool,
    /// Muted handles, lowercased, per room id (§ Commands, "Muting"). Mutes are
    /// per-room, which is how the server stores them in `mutedUsersByRoom`.
    muted: HashMap<String, HashSet<String>>,
    /// The signed-in account's user id, when the shell has told us. Used only
    /// to keep `d` off other people's messages and `F` off your own; unknown
    /// means both are offered and the server has the final say (403).
    viewer_user_id: Option<String>,
}

impl CircScreen {
    /// A screen showing the room list, with the rooms still loading.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rooms: TabState::loading(),
            mode: CircMode::Rooms,
            draft: String::new(),
            outgoing: Vec::new(),
            roster_open: false,
            muted: HashMap::new(),
            viewer_user_id: None,
        }
    }

    /// A room's composer is always focused (instant messaging), so any open room
    /// captures text.
    ///
    /// The one exception is message-select mode, where the composer has
    /// deliberately given up the keyboard so `j`, `k`, `d`, `y`, `F`, `o`, `v`
    /// and `m` can act on a message. Returning `false` there re-enables the
    /// shell's global single-letter interceptors: `?` help, `i` image toggle,
    /// `S` shuffle, the digit section jumps, the left/right section cycle,
    /// Backspace-as-back and the jukebox transport keys. None of those collide
    /// with the select-mode bindings, which is why the mode is safe to unfocus.
    /// The flag-reason prompt is itself a text field, so it captures again while
    /// it is open.
    pub fn is_text_input(&self) -> bool {
        match &self.mode {
            CircMode::Rooms => false,
            CircMode::Room { select, .. } => !select.active || select.flag.is_some(),
        }
    }

    /// Insert bracketed-paste text into whichever field has the keyboard: the
    /// composer, or the flag-reason prompt. Select mode has no field, so a paste
    /// there is dropped rather than typed into the unfocused composer.
    pub fn paste_text(&mut self, text: &str) {
        let CircMode::Room { select, .. } = &mut self.mode else {
            return;
        };
        if let Some(prompt) = &mut select.flag {
            // A single-line field: the shared prompt collapses a pasted newline
            // so it cannot submit the report.
            prompt.paste(text);
            return;
        }
        if select.active {
            return;
        }
        self.draft.push_str(text);
    }

    /// Set the composer text (used when the full editor hands its content back).
    ///
    /// The editor is the only way to compose the multi-line body `/art` needs,
    /// so returning from it always puts the keyboard back on the composer.
    pub fn set_draft_and_focus(&mut self, content: String) {
        let CircMode::Room { select, .. } = &mut self.mode else {
            return;
        };
        select.active = false;
        select.confirming_delete = false;
        select.flag = None;
        self.draft = content;
    }

    /// The room currently open, for the shell's presence heartbeat and for the
    /// `DELETE /v1/circ/:roomId/presence` it sends on leaving and on quitting.
    #[must_use]
    pub fn open_room_id(&self) -> Option<&str> {
        match &self.mode {
            CircMode::Room { room, .. } => Some(room.room_id()),
            CircMode::Rooms => None,
        }
    }

    /// Tell the screen who is signed in, so `d` is only offered on your own
    /// messages and `F` only on everyone else's (§ Delete Your Message,
    /// § Flag a Message, which answer 403 for the other way round).
    pub fn set_viewer_user_id(&mut self, user_id: String) {
        self.viewer_user_id = Some(user_id);
    }

    fn reset_composer(&mut self) {
        self.draft.clear();
        self.outgoing.clear();
    }

    /// Route one key and say what the shell should do about it.
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

    /// Unwind one layer: the flag prompt, then the armed delete, then select
    /// mode, then the room itself. Returning `Some(CircIntent::None)` means the
    /// key was consumed without leaving the room.
    pub fn handle_escape(&mut self) -> Option<CircIntent> {
        let CircMode::Room { select, .. } = &mut self.mode else {
            return None;
        };
        if select.flag.take().is_some() {
            return Some(CircIntent::None);
        }
        if select.confirming_delete {
            select.confirming_delete = false;
            return Some(CircIntent::None);
        }
        if select.active {
            select.active = false;
            return Some(CircIntent::None);
        }
        self.reset_composer();
        self.mode = CircMode::Rooms;
        Some(CircIntent::BackToRooms)
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

    /// Apply a room-list load (`GET /v1/circ`).
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

    /// Switch to the room view for `room_id`, with an empty composer, no select
    /// mode and an empty roster. A no-op for a room that is not in the list.
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
                select: SelectState::default(),
                roster: Roster::default(),
            };
        }
    }

    /// Apply a history load (`GET /v1/circ/:roomId`). `initial` is the first
    /// screenful or a refresh; otherwise it is an older page to prepend.
    pub fn apply_messages(
        &mut self,
        room_id: &str,
        initial: bool,
        result: Result<(Vec<CircMessage>, Option<String>), String>,
    ) {
        let muted = self.muted.get(room_id);
        let CircMode::Room { room, messages, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        if initial {
            messages.apply_initial(result);
            let view_len = visible_indices(&messages.items, muted).len();
            if view_len > 0 {
                messages.selected = view_len - 1;
            }
        } else {
            apply_older_messages(messages, result, muted);
        }
    }

    /// Merge live updates into the open room (de-duped, timestamp order; follows
    /// the tail when pinned to the bottom).
    ///
    /// § Reading a room in real time: a deletion *changes* a message you
    /// already hold rather than adding one, and arrives as a `patch` carrying
    /// only the changed fields. So a whole message replaces (or appends), while
    /// a patch is merged into the copy we hold with
    /// [`cs_api::CircMessagePatch::apply_to`] and is dropped when we hold no
    /// such message: a fragment is not a message, and inserting one would show a
    /// nameless line stamped 1970 instead of the deletion it really is.
    pub fn apply_live(&mut self, room_id: &str, updates: Vec<CircMessageUpdate>) {
        let muted = self.muted.get(room_id);
        let CircMode::Room { room, messages, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }

        // Anchor the cursor before the merge. `selected` is a view index, so it
        // is resolved against the pre-merge view and restored against the
        // post-merge one.
        let view = visible_indices(&messages.items, muted);
        let was_at_bottom = view.is_empty() || messages.selected + 1 >= view.len();
        let selected_id = view
            .get(messages.selected)
            .map(|&i| messages.items[i].id.clone());

        let mut changed = false;
        for update in updates {
            match update {
                CircMessageUpdate::Full(message) => {
                    if message.id.is_empty() {
                        continue;
                    }
                    match messages.items.iter_mut().find(|m| m.id == message.id) {
                        Some(existing) => *existing = message,
                        None => messages.items.push(message),
                    }
                    changed = true;
                }
                CircMessageUpdate::Partial { id, patch } => {
                    if let Some(existing) = messages.items.iter_mut().find(|m| m.id == id) {
                        patch.apply_to(existing);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return;
        }

        // Sort by (timestamp, id): two messages sent in the same millisecond
        // would otherwise swap places between the REST poll and the live
        // stream, because sorting on the timestamp alone leaves their order to
        // whichever arrived first.
        messages
            .items
            .sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
        messages.loading = false;
        messages.loaded = true;

        let view = visible_indices(&messages.items, muted);
        if was_at_bottom {
            messages.selected = view.len().saturating_sub(1);
        } else if let Some(id) = selected_id {
            if let Some(pos) = view.iter().position(|&i| messages.items[i].id == id) {
                messages.selected = pos;
            } else {
                messages.selected = messages.selected.min(view.len().saturating_sub(1));
            }
        }
    }

    /// Tombstone a message locally after `DELETE /v1/circ/:roomId/messages/:id`
    /// succeeded (§ Delete Your Message).
    ///
    /// The same change also arrives as an RTDB patch, but applying it here means
    /// the message updates even when the live stream is down, and the merge is
    /// idempotent either way.
    pub fn apply_deleted(&mut self, room_id: &str, message_id: &str) {
        let CircMode::Room { room, messages, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        if let Some(message) = messages.items.iter_mut().find(|m| m.id == message_id) {
            message.content = DELETED_CONTENT.to_string();
            // The server strips every attachment, style and command result on
            // delete, so the tombstone can't keep the picture it used to carry.
            message.extras = MessageExtras {
                deleted: true,
                ..MessageExtras::default()
            };
        }
    }

    /// Replace the room's user list with a REST snapshot from
    /// `GET /v1/circ/:roomId/users` (§ Who's in a room).
    ///
    /// Everyone that endpoint returns is in the room by definition, so each
    /// entry is recorded as online; staleness is then re-evaluated on the clock
    /// like any streamed entry.
    pub fn apply_room_users(&mut self, room_id: &str, result: Result<Vec<CircRoomUser>, String>) {
        let CircMode::Room { room, roster, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        roster.loading = false;
        match result {
            Ok(users) => {
                roster.entries = users.iter().map(presence_entry_from_user).collect();
                roster.error = None;
            }
            Err(msg) => roster.error = Some(msg),
        }
    }

    /// Merge live entries from the `chat_presence/<roomId>` stream
    /// (§ Reading a room in real time).
    ///
    /// Same rule as the message stream: a whole entry replaces, a patch merges
    /// into the entry we hold and is dropped when we hold none, and a removal
    /// drops the person from the list.
    pub fn apply_presence_updates(&mut self, room_id: &str, updates: Vec<CircPresenceUpdate>) {
        let CircMode::Room { room, roster, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id || updates.is_empty() {
            return;
        }
        for update in updates {
            match update {
                CircPresenceUpdate::Full(entry) => {
                    if entry.user_id.is_empty() {
                        continue;
                    }
                    match roster
                        .entries
                        .iter_mut()
                        .find(|e| e.user_id == entry.user_id)
                    {
                        Some(existing) => *existing = entry,
                        None => roster.entries.push(entry),
                    }
                }
                CircPresenceUpdate::Partial { user_id, patch } => {
                    if let Some(existing) = roster.entries.iter_mut().find(|e| e.user_id == user_id)
                    {
                        patch.apply_to(existing);
                    }
                }
                CircPresenceUpdate::Removed { user_id } => {
                    roster.entries.retain(|e| e.user_id != user_id);
                }
            }
        }
        roster.loading = false;
        roster.error = None;
    }

    /// Record the room's presence cadence from a heartbeat response
    /// (§ Announce Your Presence).
    ///
    /// The spec is explicit that `staleAfterMs` and `idleAfterMs` are read off
    /// the response rather than hard-coded, and they are the thresholds the
    /// roster is filtered and marked by, so the last response is kept rather
    /// than only the timer it started. A non-positive value is ignored so a
    /// malformed response can't empty the roster.
    pub fn apply_presence_cadence(&mut self, room_id: &str, response: &CircPresenceResponse) {
        let CircMode::Room { room, roster, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        if response.stale_after_ms > 0 {
            roster.stale_after_ms = response.stale_after_ms;
        }
        if response.idle_after_ms > 0 {
            roster.idle_after_ms = response.idle_after_ms;
        }
    }

    /// Replace the handles muted in `room_id` (§ Commands, "Muting").
    ///
    /// Muting is not filtered server-side: the history endpoint still returns a
    /// muted author's messages and the client hides them, "which is also what
    /// lets an unmute reveal history you've already fetched". So this only
    /// changes the *view*: nothing is discarded, and handing over a shorter list
    /// brings the hidden messages straight back.
    pub fn set_muted_users(&mut self, room_id: &str, usernames: &[String]) {
        let set: HashSet<String> = usernames
            .iter()
            .map(|u| u.trim().to_lowercase())
            .filter(|u| !u.is_empty())
            .collect();
        if set.is_empty() {
            self.muted.remove(room_id);
        } else {
            self.muted.insert(room_id.to_string(), set);
        }
        // The view just changed length, so the cursor may be past its end.
        let muted = self.muted.get(room_id);
        let CircMode::Room { room, messages, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        let view_len = visible_indices(&messages.items, muted).len();
        if messages.selected >= view_len {
            messages.selected = view_len.saturating_sub(1);
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

        // The roster toggle works from both sub-modes.
        if ctrl && key.code == KeyCode::Char('u') {
            self.roster_open = !self.roster_open;
            return if self.roster_open {
                CircIntent::LoadRoomUsers { room_id }
            } else {
                CircIntent::None
            };
        }

        // While the reason prompt is up it owns every key but Esc.
        if self.flag_prompt_is_open() {
            return self.handle_flag_prompt_key(key, &room_id);
        }

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

        if ctrl && key.code == KeyCode::Char('b') {
            if let CircMode::Room { select, .. } = &mut self.mode {
                select.active = !select.active;
                select.confirming_delete = false;
            }
            return CircIntent::None;
        }

        if self.select_is_active() {
            return self.handle_select_key(key, &room_id);
        }

        // The composer is focused in a room: typed keys go to the draft,
        // Enter sends, Ctrl+E expands to the editor, and arrows scroll history.
        match key.code {
            KeyCode::Char('e') if ctrl => CircIntent::StartCompose {
                room_id,
                draft: self.draft.clone(),
            },
            KeyCode::Enter => {
                let content = send_content(&self.draft);
                if content.trim().is_empty() {
                    return CircIntent::None;
                }
                self.outgoing.push(Outgoing {
                    content: content.clone(),
                    failed: false,
                });
                self.draft.clear();
                CircIntent::SendMessage { room_id, content }
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
            | KeyCode::End => self.scroll_messages(key.code, &room_id),
            KeyCode::Char(c) if !ctrl => {
                self.draft.push(c);
                CircIntent::None
            }
            _ => CircIntent::None,
        }
    }

    /// Whether message-select mode has the keyboard.
    fn select_is_active(&self) -> bool {
        matches!(&self.mode, CircMode::Room { select, .. } if select.active)
    }

    /// Whether the flag-reason prompt is up.
    fn flag_prompt_is_open(&self) -> bool {
        matches!(&self.mode, CircMode::Room { select, .. } if select.flag.is_some())
    }

    /// The message under the cursor, resolved through the mute filter.
    fn selected_message(&self, room_id: &str) -> Option<&CircMessage> {
        let muted = self.muted.get(room_id);
        let CircMode::Room { room, messages, .. } = &self.mode else {
            return None;
        };
        if room.room_id() != room_id {
            return None;
        }
        let view = visible_indices(&messages.items, muted);
        view.get(messages.selected).map(|&i| &messages.items[i])
    }

    /// Whether the selected message is one of ours, as far as we can tell.
    /// `None` when the shell has not told us who is signed in.
    fn selected_is_mine(&self, room_id: &str) -> Option<bool> {
        let viewer = self.viewer_user_id.as_deref()?;
        let message = self.selected_message(room_id)?;
        Some(!message.user_id.is_empty() && message.user_id == viewer)
    }

    fn handle_select_key(&mut self, key: KeyEvent, room_id: &str) -> CircIntent {
        // Two-step delete: `d` armed it, and only `y` goes through. Anything
        // else cancels, which is the convention journal, bookmarks and post
        // detail already use.
        if matches!(&self.mode, CircMode::Room { select, .. } if select.confirming_delete) {
            if let CircMode::Room { select, .. } = &mut self.mode {
                select.confirming_delete = false;
            }
            if key.code != KeyCode::Char('y') {
                return CircIntent::None;
            }
            return match self.selected_deletable_id(room_id) {
                Some(message_id) => CircIntent::DeleteMessage {
                    room_id: room_id.to_string(),
                    message_id,
                },
                None => CircIntent::None,
            };
        }

        match key.code {
            KeyCode::Char('d') => {
                let Some(message_id) = self.selected_deletable_id(room_id) else {
                    return CircIntent::None;
                };
                if crate::config::get().confirm_deletes {
                    if let CircMode::Room { select, .. } = &mut self.mode {
                        select.confirming_delete = true;
                    }
                    CircIntent::None
                } else {
                    CircIntent::DeleteMessage {
                        room_id: room_id.to_string(),
                        message_id,
                    }
                }
            }
            KeyCode::Char('F') => {
                // You can't report your own message (403), so don't offer it.
                if self.selected_is_mine(room_id) == Some(true) {
                    return CircIntent::None;
                }
                let Some(message_id) = self
                    .selected_message(room_id)
                    .map(|m| m.id.clone())
                    .filter(|id| !id.is_empty())
                else {
                    return CircIntent::None;
                };
                if let CircMode::Room { select, .. } = &mut self.mode {
                    select.flag = Some(MessageFlagPrompt::new(message_id));
                }
                CircIntent::None
            }
            KeyCode::Char('o') => match self.selected_message(room_id).map(|m| &m.extras) {
                Some(extras) => match chat::open_action(extras) {
                    OpenAction::Play(track) => CircIntent::PlayJukebox(track),
                    OpenAction::Open(url) => CircIntent::OpenUrl(url),
                    OpenAction::None => CircIntent::None,
                },
                None => CircIntent::None,
            },
            KeyCode::Char('v') => {
                let Some((id, spoiler)) = self
                    .selected_message(room_id)
                    .map(|m| (m.id.clone(), chat::has_spoiler(&m.extras)))
                else {
                    return CircIntent::None;
                };
                if !spoiler {
                    return CircIntent::None;
                }
                if let CircMode::Room { select, .. } = &mut self.mode {
                    // Toggle, so `v` also hides a spoiler again.
                    if !select.revealed.remove(&id) {
                        select.revealed.insert(id);
                    }
                }
                CircIntent::None
            }
            KeyCode::Char('m') => {
                // § Commands describes muting as hiding someone else's messages.
                // Muting yourself would hide every message you send from your
                // own view, and select mode could no longer reach them to undo
                // it, so guard this the way `F` is guarded.
                if self.selected_is_mine(room_id) == Some(true) {
                    return CircIntent::None;
                }
                match self
                    .selected_message(room_id)
                    .map(|m| m.username.trim().to_string())
                    .filter(|u| !u.is_empty())
                {
                    Some(username) => CircIntent::MuteUser {
                        room_id: room_id.to_string(),
                        username,
                    },
                    None => CircIntent::None,
                }
            }
            code => self.scroll_messages(code, room_id),
        }
    }

    /// The id of the selected message when it is one we may delete: our own, or
    /// any message when the shell has not told us who we are (the server then
    /// answers 403). An already-deleted message is skipped, since deleting twice
    /// returns 409.
    fn selected_deletable_id(&self, room_id: &str) -> Option<String> {
        if self.selected_is_mine(room_id) == Some(false) {
            return None;
        }
        let message = self.selected_message(room_id)?;
        if message.extras.deleted || message.id.is_empty() {
            return None;
        }
        Some(message.id.clone())
    }

    fn handle_flag_prompt_key(&mut self, key: KeyEvent, room_id: &str) -> CircIntent {
        let CircMode::Room { select, .. } = &mut self.mode else {
            return CircIntent::None;
        };
        let Some(outcome) = select.flag.as_mut().map(|p| p.handle_key(key)) else {
            return CircIntent::None;
        };
        match outcome {
            FlagPromptKey::Consumed => CircIntent::None,
            // Esc also reaches this screen through `handle_escape`, which unwinds
            // the prompt the same way.
            FlagPromptKey::Cancelled => {
                select.flag = None;
                CircIntent::None
            }
            FlagPromptKey::Submitted => match select.flag.take() {
                Some(prompt) => CircIntent::FlagMessage {
                    room_id: room_id.to_string(),
                    // The reason is optional, so an empty prompt still reports.
                    reason: prompt.reason_to_send(),
                    message_id: prompt.target,
                },
                None => CircIntent::None,
            },
        }
    }

    fn scroll_messages(&mut self, code: KeyCode, room_id: &str) -> CircIntent {
        let muted = self.muted.get(room_id);
        let CircMode::Room { messages, .. } = &mut self.mode else {
            return CircIntent::None;
        };
        if messages.loading {
            return CircIntent::None;
        }
        let view_len = visible_indices(&messages.items, muted).len();
        match code {
            KeyCode::Home => messages.selected = 0,
            KeyCode::End => messages.selected = view_len.saturating_sub(1),
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
                super::list_nav::navigate(other, &mut messages.selected, view_len, false);
            }
        }
        CircIntent::None
    }

    /// Draw the room list, or the open room.
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
        let CircMode::Room {
            room,
            messages,
            select,
            roster,
        } = &self.mode
        else {
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

        // The roster takes a fixed column on the right, but only when there is
        // enough width left for the conversation to stay readable.
        let show_roster = self.roster_open && inner.width >= ROSTER_WIDTH + MIN_MESSAGES_WIDTH;
        let (chat_area, roster_area) = if show_roster {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(MIN_MESSAGES_WIDTH),
                    Constraint::Length(ROSTER_WIDTH),
                ])
                .split(inner);
            (cols[0], Some(cols[1]))
        } else {
            (inner, None)
        };

        let out_rows = self.outgoing_rows();
        // The composer input is always present (2 rows: input + hint).
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(out_rows),
                Constraint::Length(2),
            ])
            .split(chat_area);

        // Muted authors are filtered out of the *view* only: their messages stay
        // in `items` so an unmute brings back history we already hold
        // (§ Commands, "Muting").
        let visible = visible_indices(&messages.items, self.muted.get(room.room_id()));
        // Wrap each message body to the pane's content width so long lines flow
        // onto extra rows instead of being clipped. The list reserves a 2-col
        // highlight gutter and the body carries a 2-space indent, so the text
        // wraps within `width - 4`.
        let body_width = (layout[0].width as usize).saturating_sub(4).max(1);
        // No single message may exceed the pane. ratatui's `List` renders an
        // over-tall item as NOTHING, blanking the whole conversation, and a
        // decoded `/art` picture reaches that height routinely. One row is left
        // for the message's own header.
        let body_cap = (layout[0].height as usize).saturating_sub(1).max(1);
        let layout_of = |m: &CircMessage| {
            BodyLayout::new(body_width)
                .with_revealed(select.revealed.contains(&m.id))
                .with_max_rows(body_cap)
        };
        let heights: Vec<u16> = visible
            .iter()
            .map(|&i| {
                let m = &messages.items[i];
                chat::message_height(ChatMessage::from(m), layout_of(m), 1)
            })
            .collect();
        let content_rows: usize = heights.iter().map(|&h| h as usize).sum();
        let mut messages_area = bottom_aligned_messages_area(layout[0], content_rows);
        // When the history overflows the pane, ratatui's `List` tiles whole
        // items top-down from the scroll offset and can't show a partial item at
        // the top, so it leaves the leftover rows blank at the *bottom* (e.g. the
        // gap that appears above the composer while a send is pending). Trim that
        // leftover off the top — sizing the pane to the tallest suffix of whole
        // messages that fits — so the newest message stays flush above the
        // composer.
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
        list::render_body(
            frame,
            messages_area,
            theme,
            messages,
            &visible,
            "no messages yet — start typing",
            |m| ListItem::new(circ_message_lines(m, theme, layout_of(m))),
        );
        // Attachment chips become clickable only after the pane has drawn, and
        // only against the same rect and the same message order. Gated on the
        // `hyperlinks` config like every other OSC 8 surface in the client, so
        // turning it off really does leave the chips as plain text.
        if crate::config::get().hyperlinks {
            // Only the rows the list actually drew, which start at the offset it
            // just settled on. Handing over chips for scrolled-off messages
            // would slide every link onto the wrong message's attachment.
            let chips = chat::collect_chips(
                visible
                    .iter()
                    .skip(messages.list_offset())
                    .map(|&i| ChatMessage::from(&messages.items[i])),
                // Same cap the bodies were drawn with, or a chip cut by the cap
                // would still be listed and shift the links.
                BodyLayout::new(body_width).with_max_rows(body_cap),
            );
            chat::apply_chip_links(frame.buffer_mut(), messages_area, &chips, theme);
        }

        if out_rows > 0 {
            self.render_outgoing(frame, layout[1], theme);
        }
        let selected = visible
            .get(messages.selected)
            .map(|&i| &messages.items[i])
            .filter(|_| select.active);
        self.render_footer(
            frame,
            layout[2],
            theme,
            messages.next_cursor.is_some(),
            select,
            selected,
        );
        if let Some(roster_area) = roster_area {
            render_roster(frame, roster_area, theme, roster);
        }
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
        select: &SelectState,
        selected: Option<&CircMessage>,
    ) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);

        let (top, hint) = if let Some(prompt) = &select.flag {
            let label = "flag reason (optional) › ";
            let width = (rows[0].width as usize)
                .saturating_sub(label.chars().count())
                .max(1);
            let mut line = Line::from(Span::styled(label, theme.accent_style()));
            line.spans.extend(
                super::input::windowed_line(&prompt.reason, prompt.cursor, width, theme).spans,
            );
            (
                line,
                "enter report · esc cancel · reason optional, max 500".to_string(),
            )
        } else if select.confirming_delete {
            (
                Line::from(Span::styled(
                    "delete this message? y confirms · any other key cancels",
                    theme.warning_style(),
                )),
                "y confirm · esc cancel".to_string(),
            )
        } else if select.active {
            let mine = selected.and_then(|m| {
                let viewer = self.viewer_user_id.as_deref()?;
                Some(!m.user_id.is_empty() && m.user_id == viewer)
            });
            (
                select_status_line(selected, select, theme),
                select_hint(selected, mine),
            )
        } else {
            // Always-on input line.
            let shown = self.draft.replace('\n', " ⏎ ");
            (
                Line::from(vec![
                    Span::styled("› ", theme.accent_style()),
                    Span::styled(shown, theme.base()),
                    Span::styled("▏", theme.accent_style()),
                ]),
                if has_older {
                    "enter send · ↑ older · ctrl+b select · ctrl+u users · ctrl+e editor · esc back"
                        .to_string()
                } else {
                    "enter send · ctrl+b select · ctrl+u users · ctrl+e editor · esc back"
                        .to_string()
                },
            )
        };
        frame.render_widget(Paragraph::new(top), rows[0]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, theme.muted_style()))),
            rows[1],
        );
    }
}

/// The status line shown above the hints while message-select mode is active.
///
/// The preview is deliberately not the raw text: a spoiler the reader has not
/// revealed must stay hidden here too, or selecting a message would spoil it on
/// the footer while the pane is still masking it.
fn select_status_line(
    selected: Option<&CircMessage>,
    select: &SelectState,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = vec![Span::styled("▌ select ", theme.accent_style())];
    if let Some(m) = selected {
        let name = if m.username.trim().is_empty() {
            "?"
        } else {
            m.username.trim()
        };
        spans.push(Span::styled(
            format!("@{name}"),
            Style::default().fg(avatar_color(&m.username)),
        ));
        let preview = if m.extras.deleted {
            chat::TOMBSTONE.to_string()
        } else if chat::has_spoiler(&m.extras) && !select.revealed.contains(&m.id) {
            "spoiler".to_string()
        } else {
            one_line_preview(
                &chat::preview_text(&m.extras, &m.content).unwrap_or_default(),
                40,
            )
        };
        if !preview.is_empty() {
            spans.push(Span::styled(format!(" · {preview}"), theme.muted_style()));
        }
    }
    Line::from(spans)
}

/// The select-mode key hints, trimmed to what the selected message can actually
/// do so the line never advertises a key that does nothing.
fn select_hint(selected: Option<&CircMessage>, mine: Option<bool>) -> String {
    let Some(m) = selected else {
        return "j/k select · esc exit".to_string();
    };
    // Every key here is gated by its handler, so the hint has to be gated the
    // same way or it advertises a key that silently does nothing. `mine` is
    // `None` when the viewer is unknown, in which case offer everything and let
    // the server decide, which is what the handlers do too.
    let mut parts = vec!["j/k"];
    if mine != Some(false) && !m.extras.deleted && !m.id.is_empty() {
        parts.push("d delete");
    }
    if mine != Some(true) {
        parts.push("F flag");
    }
    if !matches!(chat::open_action(&m.extras), OpenAction::None) {
        parts.push("o open");
    }
    if chat::has_spoiler(&m.extras) {
        parts.push("v reveal");
    }
    if mine != Some(true) {
        parts.push("m mute");
    }
    parts.push("esc exit");
    parts.join(" · ")
}

/// Draw the roster pane (§ Who's in a room): username, a `★` for a chat admin,
/// and the website's idle mark for anyone whose `lastActivity` is older than
/// `idleAfterMs`.
///
/// Staleness and idleness are both evaluated here, on every frame, because the
/// spec asks for them on a timer: an entry going stale or idle produces no event
/// of its own.
fn render_roster(frame: &mut Frame<'_>, area: Rect, theme: &Theme, roster: &Roster) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(theme.border_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let now = now_ms();
    let mut people: Vec<&CircPresenceEntry> = roster
        .entries
        .iter()
        .filter(|e| e.is_visible(now, roster.stale_after_ms))
        .collect();
    // The endpoint sorts by username, so the pane does too.
    people.sort_by_key(|e| e.username.to_lowercase());

    let mut lines = vec![Line::from(Span::styled(
        format!(" in room · {}", people.len()),
        theme.heading_style(),
    ))];
    if people.is_empty() {
        let (text, style) = if roster.loading {
            ("loading…".to_string(), theme.accent_style())
        } else if let Some(msg) = &roster.error {
            (format!("⚠ {msg}"), theme.error_style())
        } else {
            ("nobody here yet".to_string(), theme.muted_style())
        };
        lines.push(Line::from(Span::styled(format!(" {text}"), style)));
    }
    for person in people {
        let name = if person.username.trim().is_empty() {
            "?".to_string()
        } else {
            person.username.clone()
        };
        let mut spans = vec![
            Span::styled(" ", theme.muted_style()),
            Span::styled(name, Style::default().fg(avatar_color(&person.username))),
        ];
        if person.is_chat_admin {
            spans.push(Span::styled(" ★", theme.warning_style()));
        }
        if person.is_idle(now, roster.idle_after_ms) {
            spans.push(Span::styled(format!(" {IDLE_MARK}"), theme.muted_style()));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Wall clock in milliseconds since the Unix epoch.
///
/// Presence staleness and idleness are both measured against your own clock
/// (§ Who's in a room), so the roster reads the time it renders at.
fn now_ms() -> i64 {
    let now = time::OffsetDateTime::now_utc();
    now.unix_timestamp() * 1_000 + i64::from(now.millisecond())
}

/// The same person in the shape the presence stream sends, so a REST snapshot
/// and a streamed entry can live in one list.
///
/// `GET /v1/circ/:roomId/users` only returns people who are in the room, so the
/// entry is recorded as online.
fn presence_entry_from_user(user: &CircRoomUser) -> CircPresenceEntry {
    CircPresenceEntry {
        user_id: user.user_id.clone(),
        username: user.username.clone(),
        is_chat_admin: user.is_chat_admin,
        online: true,
        last_seen: user.last_seen,
        last_activity: user.last_activity,
    }
}

/// Indices of the messages a muted author has not taken out of the view.
///
/// Filtering happens here, at render time, and never by discarding messages:
/// § Commands is explicit that nothing is filtered server-side and that hiding
/// locally "is also what lets an unmute reveal history you've already fetched".
fn visible_indices(items: &[CircMessage], muted: Option<&HashSet<String>>) -> Vec<usize> {
    let Some(muted) = muted.filter(|m| !m.is_empty()) else {
        return (0..items.len()).collect();
    };
    items
        .iter()
        .enumerate()
        .filter(|(_, m)| !muted.contains(&m.username.trim().to_lowercase()))
        .map(|(i, _)| i)
        .collect()
}

/// The body to send for a composed draft.
///
/// An ordinary message is trimmed, as it always was. An `/art` draft is not:
/// § Commands says the picture goes on the lines after the command and is
/// "stored as-is, leading spaces are preserved, because they're the picture", so
/// trimming would eat the indentation of the first row and any trailing row that
/// ends in spaces. Only the trailing newline the full editor leaves behind is
/// removed, since that is an artefact of composing rather than part of the
/// picture. Multi-line content can only arrive through the editor, so this is
/// the one shape the inline composer never produces on its own.
fn send_content(draft: &str) -> String {
    if is_art_draft(draft) {
        return draft.trim_end_matches(['\n', '\r']).to_string();
    }
    draft.trim().to_string()
}

/// Whether a draft is an `/art` post: the command has to start the content, and
/// has to be the whole first word, so `/article` is ordinary text.
fn is_art_draft(draft: &str) -> bool {
    let Some(rest) = draft.strip_prefix("/art") else {
        return false;
    };
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

fn apply_older_messages(
    messages: &mut TabState<CircMessage>,
    result: Result<(Vec<CircMessage>, Option<String>), String>,
    muted: Option<&HashSet<String>>,
) {
    messages.loading = false;
    match result {
        Ok((mut older, cursor)) => {
            // `selected` is a view index, so it shifts by however many of the
            // older messages are actually shown: a muted author's are not.
            let added = visible_indices(&older, muted).len();
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
    // `onlineCount` is how many people are in the room right now (v0.8.4,
    // § List Rooms), i.e. how long the roster pane would be.
    let online = match r.online_count {
        0 => "empty".to_string(),
        n => format!("{n} online"),
    };
    let activity = match r.last_message_at {
        Some(ts) => format!("last activity {}", format_epoch_millis_relative(ts)),
        None => "no activity yet".to_string(),
    };
    ListItem::new(vec![
        header,
        Line::from(Span::styled(
            format!("  {online} · {activity}"),
            theme.muted_style(),
        )),
    ])
}

/// One message: the speaker header, then the shared body (text, decoded art,
/// styles, command results, attachment chips, or a tombstone).
fn circ_message_lines(
    m: &CircMessage,
    theme: &Theme,
    layout: BodyLayout<'_>,
) -> Vec<Line<'static>> {
    let mut lines = vec![circ_message_header(m, theme)];
    lines.extend(chat::body_lines(ChatMessage::from(m), layout, theme));
    lines
}

/// The speaker row: name, a `★` for a chat admin, and the relative timestamp.
///
/// Kept even for an action (`/me`), whose body already reads `* username …`,
/// and even for a deleted message: § Delete Your Message keeps the author's name
/// and the original timestamp, and the header is the only place either appears.
fn circ_message_header(m: &CircMessage, theme: &Theme) -> Line<'static> {
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
    Line::from(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::flag::MAX_FLAG_REASON;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use cs_api::{AudioAttachment, CircMessagePatch, MessageStyle};

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
            online_count: 0,
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
            extras: MessageExtras::default(),
        }
    }

    fn full(m: CircMessage) -> CircMessageUpdate {
        CircMessageUpdate::Full(m)
    }

    fn open(slug: &str) -> CircScreen {
        let mut s = CircScreen::new();
        s.apply_rooms(Ok(vec![room(slug)]));
        s.open_room(slug);
        s.apply_messages(slug, true, Ok((vec![], None)));
        s
    }

    /// A room holding `msgs`, with select mode already entered.
    fn selecting(msgs: Vec<CircMessage>) -> CircScreen {
        let mut s = open("general");
        s.apply_messages("general", true, Ok((msgs, None)));
        s.handle_key(ctrl(KeyCode::Char('b')));
        s
    }

    fn held(s: &CircScreen) -> &[CircMessage] {
        let CircMode::Room { messages, .. } = &s.mode else {
            panic!("room should be open");
        };
        &messages.items
    }

    /// Renders `s` into a fixed backend and returns the inner (border-stripped)
    /// text rows, trailing-trimmed.
    fn render_rows(s: &CircScreen, height: u16) -> Vec<String> {
        render_rows_wide(s, 50, height)
    }

    /// Same, at an explicit width (the roster pane needs the room).
    fn render_rows_wide(s: &CircScreen, width: u16, height: u16) -> Vec<String> {
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
                    // Strip the left/right border cells before trimming.
                    .trim_matches(|c| c == '│' || c == '┌' || c == '┐' || c == '└' || c == '┘')
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn pending_send_keeps_newest_message_flush_above_composer() {
        // Regression: cIRC messages are 2 rows each, so an odd message-pane
        // height (as when a pending send's optimistic strip is showing) used to
        // leave a blank remainder row between the newest message and the
        // composer. The newest message must stay flush against the outgoing
        // strip.
        let mut s = open("general");
        // Enough history to overflow the pane (the active-chat case).
        let msgs: Vec<CircMessage> = (0..6)
            .map(|i| {
                message(
                    &format!("m{i}"),
                    "neo",
                    &format!("line {i}"),
                    1_000 + i64::from(i),
                )
            })
            .collect();
        s.apply_messages("general", true, Ok((msgs, None)));

        // Send optimistically — the "sending…" strip forces an odd pane height.
        for c in "hi".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        s.handle_key(key(KeyCode::Enter));

        let rows = render_rows(&s, 14);
        let strip = rows
            .iter()
            .position(|r| r.contains("sending…"))
            .expect("outgoing strip should be visible");
        assert!(
            rows[strip - 1].contains("line 5"),
            "newest message must sit flush above the composer, not across a blank gap:\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn post_send_reload_keeps_newest_flush_not_blank() {
        // Regression: with a room open long enough that live messages accumulate
        // past one page, the render settles a large persisted scroll offset. The
        // post-send reload replaces the history with a shorter page; the stale
        // offset used to survive, so ratatui clamped it past the new end and drew
        // only the newest (selected) message at the *top* of an otherwise blank
        // pane. After the reload the newest message must stay flush at the bottom.
        let mut s = open("general");
        // A long backlog (as accumulated over time), rendered so the viewport
        // scroll offset settles near the tail.
        let backlog: Vec<CircMessage> = (0..60)
            .map(|i| {
                message(
                    &format!("m{i}"),
                    "neo",
                    &format!("line {i}"),
                    1_000 + i64::from(i),
                )
            })
            .collect();
        s.apply_messages("general", true, Ok((backlog, None)));
        let _ = render_rows(&s, 14); // settle the persisted scroll offset

        // The post-send reload returns only the newest page (fewer items than the
        // backlog on screen), mirroring `read_circ_room`'s default 50-limit.
        let page: Vec<CircMessage> = (10..60)
            .map(|i| {
                message(
                    &format!("m{i}"),
                    "neo",
                    &format!("line {i}"),
                    1_000 + i64::from(i),
                )
            })
            .collect();
        s.apply_messages("general", true, Ok((page, None)));

        let rows = render_rows(&s, 14);
        let input = rows
            .iter()
            .position(|r| r.starts_with("› "))
            .expect("composer input line should be visible");
        assert!(
            rows[input - 1].contains("line 59"),
            "after the post-send reload the newest message must sit flush above \
             the composer, not stranded at the top of a blank pane:\n{}",
            rows.join("\n"),
        );
        // The pane must not be blank: more than just the single newest message
        // should be visible above the composer.
        let visible_bodies = rows.iter().filter(|r| r.contains("line ")).count();
        assert!(
            visible_bodies > 1,
            "the message pane blanked after reload (only one row visible):\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn long_message_wraps_onto_multiple_rows() {
        let mut s = open("general");
        let long = "the quick brown fox jumps over the lazy dog and keeps on running well past the edge of the pane";
        s.apply_messages(
            "general",
            true,
            Ok((vec![message("m0", "neo", long, 1_000)], None)),
        );

        let rows = render_rows(&s, 14);
        // The body must span more than one row (wrapped, not clipped): different
        // words from the same message land on different rendered rows.
        let wrapped_rows = rows
            .iter()
            .filter(|r| r.contains("quick") || r.contains("running") || r.contains("lazy"))
            .count();
        assert!(
            wrapped_rows >= 2,
            "a long message should wrap across rows, got:\n{}",
            rows.join("\n"),
        );
        // No single rendered row carries the whole message (i.e. not clipped).
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("fox") && r.contains("running")),
            "message was not wrapped (whole body on one row):\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn odd_pane_height_has_no_trailing_blank_row() {
        // The parity bug also shows at rest on odd-height terminals (no pending
        // send). The bottom message row must be non-blank right above the footer.
        let mut s = open("general");
        let msgs: Vec<CircMessage> = (0..6)
            .map(|i| {
                message(
                    &format!("m{i}"),
                    "neo",
                    &format!("line {i}"),
                    1_000 + i64::from(i),
                )
            })
            .collect();
        s.apply_messages("general", true, Ok((msgs, None)));

        // Height 13 → odd inner message pane; the row above the composer input
        // (`› `) must be the newest message body, not a blank.
        let rows = render_rows(&s, 13);
        let input = rows
            .iter()
            .position(|r| r.starts_with("› "))
            .expect("composer input line should be visible");
        assert!(
            rows[input - 1].contains("line 5"),
            "no blank row should sit between the newest message and the composer:\n{}",
            rows.join("\n"),
        );
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
        // The composer is always focused in a room — no key to press first.
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
                full(message("m1", "neo", "hi", 1_000)),
                full(message("m2", "trinity", "yo", 2_000)),
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

    #[test]
    fn live_partial_merges_a_deletion_into_the_held_message() {
        // v0.8.4 delivers a soft delete as a patch on an existing message's
        // path, so a replace-only merge would either miss it or blank the row.
        let mut s = open("general");
        s.apply_messages(
            "general",
            true,
            Ok((vec![message("m1", "neo", "secret plans", 1_000)], None)),
        );
        s.apply_live(
            "general",
            vec![CircMessageUpdate::Partial {
                id: "m1".into(),
                patch: CircMessagePatch {
                    content: Some(DELETED_CONTENT.into()),
                    deleted: Some(true),
                    ..CircMessagePatch::default()
                },
            }],
        );
        let items = held(&s);
        assert_eq!(items.len(), 1, "a patch must never insert a new message");
        assert!(items[0].extras.deleted);
        assert_eq!(
            items[0].username, "neo",
            "the patch leaves every field it doesn't mention alone"
        );
        assert_eq!(items[0].timestamp, 1_000);

        // The website keeps deleted messages visible so the conversation still
        // reads, so the row stays and shows the tombstone rather than the text.
        let rows = render_rows(&s, 12);
        assert!(
            rows.iter().any(|r| r.contains(chat::TOMBSTONE)),
            "a deleted message must render as a tombstone:\n{}",
            rows.join("\n"),
        );
        assert!(
            !rows.iter().any(|r| r.contains("secret plans")),
            "the deleted text must not survive on screen:\n{}",
            rows.join("\n"),
        );
        assert!(
            !rows.iter().any(|r| r.contains(DELETED_CONTENT)),
            "the literal wire content must never reach the reader:\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn live_partial_for_an_unheld_message_is_dropped() {
        let mut s = open("general");
        s.apply_messages(
            "general",
            true,
            Ok((vec![message("m1", "neo", "hi", 1_000)], None)),
        );
        s.apply_live(
            "general",
            vec![CircMessageUpdate::Partial {
                id: "nope".into(),
                patch: CircMessagePatch {
                    deleted: Some(true),
                    ..CircMessagePatch::default()
                },
            }],
        );
        let items = held(&s);
        assert_eq!(items.len(), 1, "a fragment is not a message");
        assert!(!items[0].extras.deleted);
    }

    #[test]
    fn live_merge_orders_same_millisecond_messages_by_id() {
        // Two messages stamped the same millisecond must land in one stable
        // order, whichever of the REST poll and the live stream saw them first.
        let mut s = open("general");
        s.apply_messages("general", true, Ok((vec![], None)));
        s.apply_live(
            "general",
            vec![
                full(message("b", "neo", "second", 1_000)),
                full(message("a", "trinity", "first", 1_000)),
            ],
        );
        let ids: Vec<&str> = held(&s).iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);

        // The same pair arriving in the other order settles identically.
        let mut other = open("general");
        other.apply_messages("general", true, Ok((vec![], None)));
        other.apply_live(
            "general",
            vec![
                full(message("a", "trinity", "first", 1_000)),
                full(message("b", "neo", "second", 1_000)),
            ],
        );
        let ids: Vec<&str> = held(&other).iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn attachment_renders_as_a_chip_not_as_a_duplicated_url() {
        let mut s = open("general");
        let mut m = message("m1", "neo", "https://cdn.example/a.gif", 1_000);
        m.extras.gif_url = Some("https://cdn.example/a.gif".into());
        s.apply_messages("general", true, Ok((vec![m], None)));

        let rows = render_rows(&s, 12);
        assert!(
            rows.iter().any(|r| r.contains("[gif]")),
            "an attachment renders as a chip:\n{}",
            rows.join("\n"),
        );
        // The URL appears exactly once, inside the chip's OSC 8 hyperlink: a
        // caption that only repeats the attachment URL is skipped rather than
        // printed under the picture it already is.
        let with_url: Vec<&String> = rows.iter().filter(|r| r.contains("cdn.example")).collect();
        assert_eq!(
            with_url.len(),
            1,
            "the URL must not be printed twice:\n{}",
            rows.join("\n"),
        );
        assert!(with_url[0].contains("[gif]"), "only the chip carries it");
    }

    #[test]
    fn action_message_renders_in_the_third_person() {
        let mut s = open("general");
        let mut m = message("m1", "neo", "waves", 1_000);
        m.extras.is_action = true;
        s.apply_messages("general", true, Ok((vec![m], None)));

        let rows = render_rows(&s, 12);
        assert!(
            rows.iter().any(|r| r.contains("* neo waves")),
            "an action renders as `* username content`:\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn ctrl_b_unfocuses_the_composer_and_esc_returns_it() {
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        assert!(
            !s.is_text_input(),
            "select mode hands the keyboard back to the shell's shortcuts"
        );
        // A bare letter now acts on the message instead of typing into the draft.
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.draft, "", "select mode must not type into the composer");

        // Esc unwinds select mode before it unwinds the room.
        assert_eq!(s.handle_escape(), Some(CircIntent::None));
        assert!(matches!(s.mode, CircMode::Room { .. }));
        assert!(s.is_text_input());
        assert_eq!(s.handle_escape(), Some(CircIntent::BackToRooms));
    }

    #[test]
    fn select_mode_d_then_y_deletes_the_selected_message() {
        let mut s = selecting(vec![message("m1", "neo", "oops", 1_000)]);
        // `d` only arms (the repo's two-step delete convention).
        assert_eq!(s.handle_key(key(KeyCode::Char('d'))), CircIntent::None);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('y'))),
            CircIntent::DeleteMessage {
                room_id: "general".into(),
                message_id: "m1".into(),
            }
        );
    }

    #[test]
    fn select_mode_delete_arming_is_cancelled_by_any_other_key() {
        let mut s = selecting(vec![message("m1", "neo", "oops", 1_000)]);
        s.handle_key(key(KeyCode::Char('d')));
        assert_eq!(s.handle_key(key(KeyCode::Char('n'))), CircIntent::None);
        // The arming is gone, so `y` on its own does nothing.
        assert_eq!(s.handle_key(key(KeyCode::Char('y'))), CircIntent::None);
    }

    #[test]
    fn select_mode_will_not_delete_someone_elses_message() {
        let mut s = selecting(vec![message("m1", "trinity", "hi", 1_000)]);
        s.set_viewer_user_id("uid-neo".into());
        assert_eq!(s.handle_key(key(KeyCode::Char('d'))), CircIntent::None);
        assert_eq!(s.handle_key(key(KeyCode::Char('y'))), CircIntent::None);
    }

    #[test]
    fn select_mode_will_not_flag_your_own_message() {
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        s.set_viewer_user_id("uid-neo".into());
        assert_eq!(s.handle_key(key(KeyCode::Char('F'))), CircIntent::None);
        assert!(!s.flag_prompt_is_open());
    }

    #[test]
    fn select_mode_flag_accepts_an_empty_reason() {
        let mut s = selecting(vec![message("m1", "trinity", "spam", 1_000)]);
        assert_eq!(s.handle_key(key(KeyCode::Char('F'))), CircIntent::None);
        assert!(s.flag_prompt_is_open());
        // The prompt is a text field again, so `?` and the section keys defer.
        assert!(s.is_text_input());
        // The reason is optional: submitting an empty prompt still reports.
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CircIntent::FlagMessage {
                room_id: "general".into(),
                message_id: "m1".into(),
                reason: None,
            }
        );
        assert!(!s.flag_prompt_is_open());
    }

    #[test]
    fn select_mode_flag_carries_a_typed_reason() {
        let mut s = selecting(vec![message("m1", "trinity", "spam", 1_000)]);
        s.handle_key(key(KeyCode::Char('F')));
        for c in "rude".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        s.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CircIntent::FlagMessage {
                room_id: "general".into(),
                message_id: "m1".into(),
                reason: Some("rud".into()),
            }
        );
    }

    #[test]
    fn flag_reason_is_capped_at_the_documented_length() {
        let mut s = selecting(vec![message("m1", "trinity", "spam", 1_000)]);
        s.handle_key(key(KeyCode::Char('F')));
        for _ in 0..(MAX_FLAG_REASON + 20) {
            s.handle_key(key(KeyCode::Char('x')));
        }
        let CircIntent::FlagMessage { reason, .. } = s.handle_key(key(KeyCode::Enter)) else {
            panic!("the prompt should submit a report");
        };
        assert_eq!(reason.expect("a reason was typed").chars().count(), 500);
    }

    #[test]
    fn esc_closes_the_flag_prompt_without_leaving_select_mode() {
        let mut s = selecting(vec![message("m1", "trinity", "spam", 1_000)]);
        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(s.handle_escape(), Some(CircIntent::None));
        assert!(!s.flag_prompt_is_open());
        assert!(s.select_is_active(), "the prompt closes, the mode stays");
    }

    #[test]
    fn select_mode_m_mutes_the_author() {
        let mut s = selecting(vec![message("m1", "trinity", "noise", 1_000)]);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('m'))),
            CircIntent::MuteUser {
                room_id: "general".into(),
                username: "trinity".into(),
            }
        );
    }

    #[test]
    fn select_mode_o_plays_a_track_and_opens_a_picture() {
        let mut track = message("m1", "neo", "listen", 1_000);
        track.extras.audio_attachment = Some(AudioAttachment {
            src: "https://youtu.be/abc".into(),
            origin: "youtube".into(),
            artist: "Boards".into(),
            title: "Roygbiv".into(),
            genre: None,
        });
        let mut s = selecting(vec![track]);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            CircIntent::PlayJukebox(JukeboxTrack {
                url: "https://youtu.be/abc".into(),
                artist: "Boards".into(),
                title: "Roygbiv".into(),
            })
        );

        let mut picture = message("m2", "neo", "look", 2_000);
        picture.extras.image_url = Some("https://cdn.example/pic.png".into());
        let mut s = selecting(vec![picture]);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            CircIntent::OpenUrl("https://cdn.example/pic.png".into())
        );
    }

    #[test]
    fn select_mode_o_does_nothing_without_an_attachment() {
        let mut s = selecting(vec![message("m1", "neo", "just words", 1_000)]);
        assert_eq!(s.handle_key(key(KeyCode::Char('o'))), CircIntent::None);
    }

    #[test]
    fn select_mode_v_reveals_and_rehides_a_spoiler() {
        let mut m = message("m1", "neo", "the butler did it", 1_000);
        m.extras.style = Some(MessageStyle::One("spoiler".into()));
        let mut s = selecting(vec![m]);

        let hidden = render_rows(&s, 12);
        assert!(
            !hidden.iter().any(|r| r.contains("butler")),
            "a spoiler starts masked, in the pane and in the select status line:\n{}",
            hidden.join("\n"),
        );
        assert!(
            hidden
                .iter()
                .any(|r| r.contains("select") && r.contains("spoiler")),
            "the status line names the spoiler instead of quoting it:\n{}",
            hidden.join("\n"),
        );

        s.handle_key(key(KeyCode::Char('v')));
        let shown = render_rows(&s, 12);
        assert!(
            shown.iter().any(|r| r.contains("butler")),
            "v reveals the spoiler:\n{}",
            shown.join("\n"),
        );

        // Toggling back hides it again.
        s.handle_key(key(KeyCode::Char('v')));
        let rehidden = render_rows(&s, 12);
        assert!(!rehidden.iter().any(|r| r.contains("butler")));
    }

    #[test]
    fn select_mode_v_is_inert_without_a_spoiler() {
        let mut s = selecting(vec![message("m1", "neo", "plain", 1_000)]);
        assert_eq!(s.handle_key(key(KeyCode::Char('v'))), CircIntent::None);
        let rows = render_rows(&s, 12);
        assert!(rows.iter().any(|r| r.contains("plain")));
    }

    #[test]
    fn muting_hides_messages_at_render_time_and_unmuting_restores_them() {
        // § Commands: nothing is filtered server-side, and hiding locally is
        // "what lets an unmute reveal history you've already fetched".
        let mut s = open("general");
        s.apply_messages(
            "general",
            true,
            Ok((
                vec![
                    message("m1", "neo", "hello there", 1_000),
                    message("m2", "smith", "noise noise", 2_000),
                ],
                None,
            )),
        );
        s.set_muted_users("general", &["Smith".to_string()]);

        let rows = render_rows(&s, 12);
        assert!(rows.iter().any(|r| r.contains("hello there")));
        assert!(
            !rows.iter().any(|r| r.contains("noise noise")),
            "a muted author's message is hidden:\n{}",
            rows.join("\n"),
        );
        assert_eq!(
            held(&s).len(),
            2,
            "the message itself is kept, only the view drops it"
        );

        s.set_muted_users("general", &[]);
        let rows = render_rows(&s, 12);
        assert!(
            rows.iter().any(|r| r.contains("noise noise")),
            "an unmute reveals history we already hold:\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn select_cursor_skips_muted_messages() {
        let mut s = open("general");
        s.apply_messages(
            "general",
            true,
            Ok((
                vec![
                    message("m1", "neo", "one", 1_000),
                    message("m2", "smith", "two", 2_000),
                    message("m3", "neo", "three", 3_000),
                ],
                None,
            )),
        );
        s.set_muted_users("general", &["smith".to_string()]);
        s.handle_key(ctrl(KeyCode::Char('b')));
        // The cursor sits on the newest visible message, and moving up lands on
        // the other visible one rather than on the muted message between them.
        assert_eq!(
            s.selected_message("general").map(|m| m.id.as_str()),
            Some("m3")
        );
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(
            s.selected_message("general").map(|m| m.id.as_str()),
            Some("m1")
        );
    }

    #[test]
    fn ctrl_u_toggles_the_roster_and_asks_for_the_user_list() {
        let mut s = open("general");
        assert_eq!(
            s.handle_key(ctrl(KeyCode::Char('u'))),
            CircIntent::LoadRoomUsers {
                room_id: "general".into()
            }
        );
        assert!(s.roster_open);
        assert_eq!(s.handle_key(ctrl(KeyCode::Char('u'))), CircIntent::None);
        assert!(!s.roster_open);
    }

    #[test]
    fn roster_pane_marks_admins_and_idlers() {
        let mut s = open("general");
        let now = now_ms();
        s.apply_room_users(
            "general",
            Ok(vec![
                CircRoomUser {
                    user_id: "u1".into(),
                    username: "neo".into(),
                    is_chat_admin: true,
                    last_seen: now,
                    last_activity: Some(now),
                },
                CircRoomUser {
                    user_id: "u2".into(),
                    username: "dozer".into(),
                    is_chat_admin: false,
                    // Long past `idleAfterMs`, so the idle mark shows.
                    last_seen: now,
                    last_activity: Some(now - 3_600_000),
                },
            ]),
        );
        s.handle_key(ctrl(KeyCode::Char('u')));

        let rows = render_rows_wide(&s, 80, 14);
        let joined = rows.join("\n");
        assert!(joined.contains("in room · 2"), "roster header:\n{joined}");
        assert!(
            rows.iter().any(|r| r.contains("neo") && r.contains('★')),
            "a chat admin is starred:\n{joined}",
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("dozer") && r.contains(IDLE_MARK)),
            "someone past idleAfterMs carries the idle mark:\n{joined}",
        );
    }

    #[test]
    fn roster_drops_stale_and_removed_entries() {
        let mut s = open("general");
        let now = now_ms();
        s.apply_room_users(
            "general",
            Ok(vec![CircRoomUser {
                user_id: "u1".into(),
                username: "neo".into(),
                is_chat_admin: false,
                last_seen: now,
                last_activity: Some(now),
            }]),
        );
        // A live entry whose heartbeat is long gone must not be shown.
        s.apply_presence_updates(
            "general",
            vec![CircPresenceUpdate::Full(CircPresenceEntry {
                user_id: "u2".into(),
                username: "ghost".into(),
                is_chat_admin: false,
                online: true,
                last_seen: now - 3_600_000,
                last_activity: None,
            })],
        );
        s.handle_key(ctrl(KeyCode::Char('u')));
        let joined = render_rows_wide(&s, 80, 14).join("\n");
        assert!(joined.contains("neo"));
        assert!(
            !joined.contains("ghost"),
            "a stale entry is hidden:\n{joined}"
        );

        // A removal drops the person entirely.
        s.apply_presence_updates(
            "general",
            vec![CircPresenceUpdate::Removed {
                user_id: "u1".into(),
            }],
        );
        let joined = render_rows_wide(&s, 80, 14).join("\n");
        assert!(joined.contains("in room · 0"));
        assert!(joined.contains("nobody here yet"));
    }

    #[test]
    fn presence_patch_merges_rather_than_replacing() {
        let mut s = open("general");
        let now = now_ms();
        s.apply_room_users(
            "general",
            Ok(vec![CircRoomUser {
                user_id: "u1".into(),
                username: "neo".into(),
                is_chat_admin: true,
                last_seen: now - 1_000,
                last_activity: Some(now - 1_000),
            }]),
        );
        // A heartbeat that only moves `lastSeen` must not blank the handle.
        s.apply_presence_updates(
            "general",
            vec![CircPresenceUpdate::Partial {
                user_id: "u1".into(),
                patch: cs_api::CircPresencePatch {
                    last_seen: Some(now),
                    ..cs_api::CircPresencePatch::default()
                },
            }],
        );
        s.handle_key(ctrl(KeyCode::Char('u')));
        let joined = render_rows_wide(&s, 80, 14).join("\n");
        assert!(
            joined.contains("neo"),
            "the handle survives a patch:\n{joined}"
        );
        assert!(joined.contains('★'), "so does the admin flag:\n{joined}");
    }

    #[test]
    fn rooms_list_shows_the_online_count() {
        let mut s = CircScreen::new();
        s.apply_rooms(Ok(vec![CircRoom {
            online_count: 7,
            ..room("general")
        }]));
        let joined = render_rows(&s, 12).join("\n");
        assert!(joined.contains("7 online"), "rooms list:\n{joined}");
    }

    #[test]
    fn art_draft_keeps_its_leading_spaces_and_newlines() {
        // § Commands: the picture goes on the lines after the command and the
        // leading spaces *are* the picture, so the send path must not trim it.
        let art = "/art\n /\\_/\\\n( o.o )\n > ^ <\n";
        assert_eq!(send_content(art), "/art\n /\\_/\\\n( o.o )\n > ^ <");
        // A trailing row that is part of the picture keeps its spaces.
        assert_eq!(send_content("/art\n  #  \n"), "/art\n  #  ");
        // `/article` is ordinary text, not art.
        assert_eq!(send_content("  /article draft  "), "/article draft");
        // An ordinary message is trimmed exactly as before.
        assert_eq!(send_content("  hello  \n"), "hello");
        assert_eq!(send_content("   \n  "), "");
    }

    #[test]
    fn editor_content_with_art_sends_untrimmed() {
        let mut s = open("general");
        // Multi-line content can only arrive through the Ctrl+E editor.
        s.set_draft_and_focus("/art\n  /\\\n /  \\\n".to_string());
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CircIntent::SendMessage {
                room_id: "general".into(),
                content: "/art\n  /\\\n /  \\".into(),
            }
        );
    }

    #[test]
    fn editor_content_returns_the_keyboard_to_the_composer() {
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        assert!(!s.is_text_input());
        s.set_draft_and_focus("back to typing".to_string());
        assert!(s.is_text_input());
        assert!(!s.select_is_active());
    }

    #[test]
    fn apply_deleted_tombstones_the_message_locally() {
        let mut s = open("general");
        let mut m = message("m1", "neo", "oops", 1_000);
        m.extras.gif_url = Some("https://cdn.example/a.gif".into());
        s.apply_messages("general", true, Ok((vec![m], None)));
        s.apply_deleted("general", "m1");

        let items = held(&s);
        assert!(items[0].extras.deleted);
        assert!(
            items[0].extras.gif_url.is_none(),
            "the server strips attachments on delete, so the tombstone can't keep one"
        );
        let rows = render_rows(&s, 12);
        assert!(rows.iter().any(|r| r.contains(chat::TOMBSTONE)));
        assert!(!rows.iter().any(|r| r.contains("[gif]")));
    }

    #[test]
    fn paste_goes_to_the_flag_prompt_while_it_is_open() {
        let mut s = selecting(vec![message("m1", "trinity", "spam", 1_000)]);
        s.handle_key(key(KeyCode::Char('F')));
        s.paste_text("spam\nharassment");
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            CircIntent::FlagMessage {
                room_id: "general".into(),
                message_id: "m1".into(),
                reason: Some("spam harassment".into()),
            }
        );
        assert_eq!(s.draft, "", "the paste must not leak into the composer");
    }

    #[test]
    fn paste_is_dropped_in_select_mode() {
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        s.paste_text("nope");
        assert_eq!(s.draft, "");
    }

    #[test]
    fn muting_yourself_is_refused_and_never_offered() {
        // Regression: `m` had no ownership guard, so pressing it on your own
        // message posted `/mute <yourself>`, which hid every message you send
        // from your own view. Select mode filters muted authors too, so it could
        // no longer reach them to undo it.
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        s.set_viewer_user_id("uid-neo".into());

        assert_eq!(
            s.handle_key(key(KeyCode::Char('m'))),
            CircIntent::None,
            "your own message must not be mutable",
        );

        let mut other = selecting(vec![message("m1", "trinity", "hi", 1_000)]);
        other.set_viewer_user_id("uid-neo".into());
        assert_eq!(
            other.handle_key(key(KeyCode::Char('m'))),
            CircIntent::MuteUser {
                room_id: "general".into(),
                username: "trinity".into(),
            },
        );
    }

    #[test]
    fn the_select_hint_only_advertises_keys_that_would_do_something() {
        // The hint's own contract is that it never names a key that is a no-op,
        // but `d`, `F` and `m` were hard-coded while their handlers refused in
        // exactly the complementary cases.
        let mine = message("m1", "neo", "hi", 1_000);
        let hint = select_hint(Some(&mine), Some(true));
        assert!(hint.contains("d delete"), "{hint}");
        assert!(!hint.contains("F flag"), "cannot report your own: {hint}");
        assert!(!hint.contains("m mute"), "cannot mute yourself: {hint}");

        let theirs = message("m2", "trinity", "hi", 1_000);
        let hint = select_hint(Some(&theirs), Some(false));
        assert!(!hint.contains("d delete"), "cannot delete theirs: {hint}");
        assert!(hint.contains("F flag"), "{hint}");
        assert!(hint.contains("m mute"), "{hint}");

        let mut gone = message("m3", "neo", "[DELETED]", 1_000);
        gone.extras.deleted = true;
        let hint = select_hint(Some(&gone), Some(true));
        assert!(
            !hint.contains("d delete"),
            "a tombstone cannot be deleted again: {hint}",
        );
    }

    #[test]
    fn the_select_footer_decodes_art_instead_of_previewing_base64() {
        // § Message fields: `style: "art"` means content is base64 and must be
        // decoded before display. The footer is where the reader confirms which
        // message they are about to delete or flag, so previewing the raw
        // payload there made every art message look identical.
        let mut m = message("m1", "neo", "IC9cXy9cCiggby5vICk=", 1_000);
        m.extras.style = Some(cs_api::MessageStyle::One("art".into()));
        let select = SelectState::default();
        let theme = Theme::cyber();

        let rendered: String = select_status_line(Some(&m), &select, &theme)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();

        assert!(
            !rendered.contains("IC9cXy9c"),
            "raw base64 must never reach the footer: {rendered:?}",
        );
        assert!(
            rendered.contains("o.o"),
            "the decoded picture should be what previews: {rendered:?}",
        );
    }
}

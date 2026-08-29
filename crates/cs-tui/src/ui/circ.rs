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
//!   a message-action menu opened with `Ctrl+A`, which is the only key in a
//!   room that is not typed.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{
    CircMessage, CircMessageUpdate, CircPresenceEntry, CircPresenceResponse, CircPresenceUpdate,
    CircRoom, CircRoomUser, MessageExtras,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

use super::app::MAX_RECONNECT_ATTEMPTS;
use super::audio::JukeboxTrack;
use super::chat::{self, BodyLayout, ChatMessage, OpenAction};
use super::cmail::{
    avatar_color, bottom_aligned_messages_area, format_epoch_millis_relative, one_line_preview,
    Outgoing,
};
use super::composer::{self, Composer};
use super::flag::FlagPromptKey;
use super::list::{self, TabState};
use super::mention;
use super::theme::Theme;

const MAX_OUTGOING_ROWS: usize = 4;

/// Columns the roster pane takes, border included.
///
/// Sized for the worst case a row can hold rather than for a typical name: one
/// leading space, a 20-character username, the admin mark and the idle mark,
/// which is 26 columns, plus the pane's left border.
///
/// This was 20, which clipped. `Paragraph` truncates rather than wraps, so a
/// long name silently lost its admin and idle marks off the right edge, which
/// is precisely the information the row exists to carry. The pane is shown at
/// this width or not at all (see `render_room`); an in-between width would
/// bring the clipping straight back.
const ROSTER_WIDTH: u16 = 27;

/// Narrowest message pane worth keeping. Below this the roster pane stays
/// folded away however the toggle is set, so a small terminal never squeezes
/// the conversation down to a column of single letters.
const MIN_MESSAGES_WIDTH: u16 = 24;

/// What the website puts next to an idle person's name (§ Who's in a room).
const IDLE_MARK: &str = "\u{1f4a4}";

/// Fills the header gutter of a message that `@`-mentions the reader. Two cells
/// wide, exactly like the blank gutter it replaces, so marking a message never
/// moves the text beside it.
///
/// Deliberately not the `▌` bar a marked row would otherwise want: the list
/// widget already draws `▌ ` in its own gutter for the selected message
/// ([`super::list::render_body`]), one column to the left of this one, so a bar
/// here would read as a second cursor. An `@` says what the mark means anyway.
const MENTION_MARK: &str = "@ ";

/// Messages a PageUp or PageDown moves the selection by.
///
/// A fixed count rather than a pane-height calculation: messages vary wildly in
/// height (a decoded `/art` picture is dozens of rows, a `/me` is two), so
/// "one screenful" is not a stable number of messages and would make the key
/// feel different from one press to the next.
const PAGE_JUMP: usize = 10;

/// How many messages a room keeps before it starts dropping the oldest.
///
/// A room left open all day would otherwise grow without limit, and each
/// message drags its decoded art and cached image bytes along with it. Ten
/// pages of the API's 50-message maximum is far more than anyone scrolls back
/// through in a session, and what falls off is re-fetched on demand.
const MAX_HELD_MESSAGES: usize = 500;

/// The wire `content` of a deleted message (§ Delete Your Message). Never
/// rendered: [`super::chat::body_lines`] draws a tombstone instead.
const DELETED_CONTENT: &str = "[DELETED]";

/// Prefixes a local-only notice, the way an IRC client marks its own output.
const NOTICE_PREFIX: &str = "*** ";

/// Build a local-only notice to append to the open room's transcript.
///
/// These never went to the server and never come back from it: a `/help` reply
/// arrives only in the send response (§ Commands), and a refused command was
/// never sent at all. They exist for the session and die with the room.
///
/// **The empty `id` is load-bearing, not laziness.** It is what makes the
/// message inert: [`CircScreen::selected_deletable_id`] refuses an empty id, so
/// `d` cannot delete it; the `F` arm filters on the same, so it cannot be
/// reported; the empty `username` makes `m` a no-op, since muting needs a
/// handle; and carrying no extras leaves `o` and `v` nothing to act on. The
/// cursor may still rest on one, which is harmless, and that is the trade for
/// keeping the message list 1:1 with the rendered rows that selection,
/// pagination and scrolling all index against.
///
/// [`CircScreen::apply_live`] skips whole updates with an empty id, so a server
/// message can never overwrite a notice, and a notice can never be mistaken for
/// one.
fn local_notice(text: &str, timestamp: i64) -> CircMessage {
    CircMessage {
        id: String::new(),
        user_id: String::new(),
        username: String::new(),
        is_chat_admin: false,
        content: text.to_string(),
        timestamp,
        extras: MessageExtras::default(),
    }
}

/// Whether this row is a local notice rather than something someone posted.
///
/// Both halves are checked: a real message always carries an id, so the pair is
/// unambiguous even against a malformed payload.
fn is_local_notice(m: &CircMessage) -> bool {
    m.id.is_empty() && m.username.is_empty()
}

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
    /// Put the selected message's text on the clipboard.
    CopyText(String),
    /// Open the selected message author's profile.
    OpenProfile { username: String },
    /// Start or resume a C-Mail conversation with the selected author.
    OpenDm { username: String, user_id: String },
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
        /// Message-action menu state (`Ctrl+A`).
        select: SelectState,
        /// Who is in the room (§ Who's in a room).
        roster: Roster,
    },
}

/// What the room's live message stream is doing, for the room header.
///
/// A dropped stream used to be invisible: the REST poll alongside it kept the
/// room updating, so a reader had no way to tell live updates from a three
/// second lag, and neither did we. Surfacing it is what makes a regression in
/// the stream noticeable instead of silently absorbed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CircStreamState {
    /// Connected, or not yet started.
    #[default]
    Live,
    /// Dropped, with attempt `n` of the ladder in flight.
    Reconnecting(u32),
    /// The ladder was exhausted. Stays until the room is re-entered.
    Lost,
}

/// The message-action menu, opened with `Ctrl+A`.
///
/// Every bare letter in a room goes to the always-on composer, so the per
/// message actions (delete, flag, open, reveal, mute) need a mode of their own
/// where the composer is not focused.
#[derive(Debug, Default)]
pub struct SelectState {
    /// Whether the per-message action menu is open.
    ///
    /// Replaces what used to be a *mode*. The composer in a room is always
    /// live, so a mode in which bare letters meant `d`elete, `m`ute and so on
    /// left the reader with an active editor in which some letters typed, some
    /// navigated and some acted. That is not a rule anyone can hold in their
    /// head, and an adversarial review found it could delete a message from an
    /// ordinary typed word. Now exactly one key is special: it opens this menu,
    /// the menu owns the keyboard while it is up, and every other key types.
    menu_open: bool,
    /// Two-step delete: `d` arms it, `y` confirms (the convention journal,
    /// bookmarks and post detail already use).
    confirming_delete: bool,
    /// The open flag-reason prompt, if any.
    flag: Option<MessageFlagPrompt>,
    /// Ids of the messages whose spoiler or substituted text the reader has
    /// revealed. Reader state, not message state, so it dies with the room.
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

impl Roster {
    /// The handle presence reports for `user_id`, if the roster has heard of
    /// them.
    ///
    /// Staleness is deliberately not applied: a stale entry is someone who has
    /// stopped heartbeating, not someone whose name has changed, and this is
    /// how the screen learns the *reader's own* handle (see
    /// [`CircScreen::viewer_handle`]), which it must keep even while their own
    /// heartbeat is between beats.
    fn handle_of(&self, user_id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.user_id == user_id)
            .map(|e| e.username.trim())
            .filter(|name| !name.is_empty())
    }
}

/// The cIRC screen: a room list, and one open room at a time.
pub struct CircScreen {
    /// The room list.
    pub rooms: TabState<CircRoom>,
    /// Room list, or one open room.
    pub mode: CircMode,
    /// Always-on inline composer for the open room (it's a chat channel, so the
    /// input is focused the whole time you're in a room).
    draft: Composer,
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
    /// What the open room's live stream is doing, for the header.
    stream_state: CircStreamState,
    /// Animation frame for `blink`/`wave`/`glitch`, advanced by the shell only
    /// while `animate_styles` is on and a message on screen actually animates.
    anim_frame: usize,
    /// How many times Tab has been pressed on the current mention token.
    ///
    /// Only a counter, wrapped against the live match count wherever it is
    /// read, never a snapshot of the candidates. That way someone leaving the
    /// room stops being offered mid-cycle, and someone joining becomes
    /// reachable, without the reader having to retype the query. The accepted
    /// cost is that a roster change between two Tab presses can land the same
    /// position on a different person, which is better than confidently
    /// offering someone who has gone.
    mention_cycle: Option<usize>,
    /// Raw bytes of images already fetched, keyed by URL. `RefCell` because
    /// `render` takes `&self` and decoding is lazy: an image is only turned
    /// into a protocol the first time it scrolls into view.
    image_bytes: RefCell<HashMap<String, Vec<u8>>>,
    /// URLs already asked for, so the fetch driver does not re-request one that
    /// is in flight or that failed. Never retried within a session.
    image_requested: RefCell<HashSet<String>>,
    /// URLs whose fetch failed; they fall back to a chip instead of a band.
    image_failed: RefCell<HashSet<String>>,
    /// Pixel dimensions per URL, read from the header once the bytes arrive, so
    /// the reserved band can shrink from the fallback ceiling to what the
    /// picture actually needs. See `chat::fitted_image_rows`.
    image_dims: RefCell<HashMap<String, (u32, u32)>>,
    /// Encoded protocols, keyed by URL, with the cell size they were built for
    /// so a resize rebuilds them instead of drawing a stale picture.
    ///
    /// Skipped by `Debug`: `Protocol` does not implement it, and a screenful of
    /// encoded pixel data is not something a debug dump wants anyway.
    image_protocols: RefCell<HashMap<String, (Protocol, Size)>>,
}

impl std::fmt::Debug for CircScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircScreen")
            .field("rooms", &self.rooms)
            .field("mode", &self.mode)
            .field("draft", &self.draft)
            .field("outgoing", &self.outgoing)
            .field("roster_open", &self.roster_open)
            .field("muted", &self.muted)
            .field("viewer_user_id", &self.viewer_user_id)
            .field("stream_state", &self.stream_state)
            .field("mention_cycle", &self.mention_cycle)
            .field("images_cached", &self.image_bytes.borrow().len())
            .finish_non_exhaustive()
    }
}

impl CircScreen {
    /// A screen showing the room list, with the rooms still loading.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rooms: TabState::loading(),
            mode: CircMode::Rooms,
            draft: Composer::default(),
            outgoing: Vec::new(),
            roster_open: false,
            muted: HashMap::new(),
            viewer_user_id: None,
            stream_state: CircStreamState::default(),
            anim_frame: 0,
            mention_cycle: None,
            image_bytes: RefCell::new(HashMap::new()),
            image_requested: RefCell::new(HashSet::new()),
            image_failed: RefCell::new(HashSet::new()),
            image_dims: RefCell::new(HashMap::new()),
            image_protocols: RefCell::new(HashMap::new()),
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
            CircMode::Room { .. } => true,
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
        if select.menu_open {
            return;
        }
        self.draft.insert_str(text);
    }

    /// Set the composer text (used when the full editor hands its content back).
    ///
    /// The editor is the only way to compose the multi-line body `/art` needs,
    /// so returning from it always puts the keyboard back on the composer.
    pub fn set_draft_and_focus(&mut self, content: String) {
        let CircMode::Room { select, .. } = &mut self.mode else {
            return;
        };
        select.menu_open = false;
        select.confirming_delete = false;
        select.flag = None;
        self.draft.set(content);
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
        if select.menu_open {
            select.menu_open = false;
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
        // A new room means a new stream generation, so last room's verdict has
        // nothing to say about this one. Also what makes the persistent "live
        // updates lost" clear on re-entry, which is the documented recovery.
        self.stream_state = CircStreamState::Live;
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

    /// Append a local-only notice to the open room's transcript.
    ///
    /// Used for a command reply the server answers inline (`/help` and the
    /// `/mute` family return `{ "data": { "reply": "..." } }` and post nothing)
    /// and for a command this client refused to send. Both are the client
    /// talking to one reader, so neither belongs on the wire.
    ///
    /// Timestamped `now` so it sorts to the bottom next to whatever prompted
    /// it. A message arriving later sorts below it, which is correct: the
    /// notice really did happen first.
    ///
    /// No-op unless `room_id` is the room actually open, so a reply that
    /// arrives after the reader has moved on is dropped rather than pasted into
    /// the wrong room.
    pub fn append_notice(&mut self, room_id: &str, text: &str) {
        let muted = self.muted.get(room_id);
        let CircMode::Room { room, messages, .. } = &mut self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        let view = visible_indices(&messages.items, muted);
        let was_at_bottom = view.is_empty() || messages.selected + 1 >= view.len();
        messages.items.push(local_notice(text, now_ms()));
        messages.loaded = true;
        if was_at_bottom {
            let view_len = visible_indices(&messages.items, muted).len();
            messages.selected = view_len.saturating_sub(1);
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
        // Notices all carry an empty id by design, so anchoring on one would
        // match the *first* notice in the room rather than the row the reader
        // was on. `None` falls back to the index-clamp below, which is right.
        let selected_id = view
            .get(messages.selected)
            .map(|&i| messages.items[i].id.clone())
            .filter(|id| !id.is_empty());

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
        // Only while following the tail: see `trim_history`.
        trim_history(messages, muted, was_at_bottom);
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

    /// Names worth offering for an `@mention`, best first.
    ///
    /// Everyone currently in the room, then anyone else who has a message in
    /// the history we hold. The second half matters: someone who spoke and then
    /// went offline is still who you want to reply to, and they are gone from
    /// the roster. Online names win a tie, since they are the ones who can read
    /// it now.
    ///
    /// Local notices are excluded: they have no author, and "*** " is not
    /// somebody.
    fn mention_pool(&self) -> Vec<String> {
        let CircMode::Room {
            messages, roster, ..
        } = &self.mode
        else {
            return Vec::new();
        };
        let now = now_ms();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pool: Vec<String> = Vec::new();
        let push = |name: &str, pool: &mut Vec<String>, seen: &mut HashSet<String>| {
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            if seen.insert(name.to_lowercase()) {
                pool.push(name.to_string());
            }
        };
        let mut online: Vec<&CircPresenceEntry> = roster
            .entries
            .iter()
            .filter(|e| e.is_visible(now, roster.stale_after_ms))
            .collect();
        online.sort_by_key(|e| e.username.to_lowercase());
        for entry in online {
            push(&entry.username, &mut pool, &mut seen);
        }
        for m in &messages.items {
            if is_local_notice(m) {
                continue;
            }
            push(&m.username, &mut pool, &mut seen);
        }
        pool
    }

    /// The mention token under the caret and the candidate currently offered.
    ///
    /// Re-resolved on every call rather than cached, which is what keeps the
    /// offer honest as people come and go. `None` when the caret is not in a
    /// mention, nothing matches, or the whole name is already typed.
    fn mention_offer(&self) -> Option<(mention::Query, String, String)> {
        let query = mention::query_at(&self.draft.text, self.draft.cursor)?;
        let pool = self.mention_pool();
        let matches = mention::matches(&pool, &query.prefix);
        if matches.is_empty() {
            return None;
        }
        // Index 0 is the passive default, shown before any Tab is pressed, so
        // the first Tab has to move to 1 or it would look like a no-op.
        let idx = self.mention_cycle.unwrap_or(0) % matches.len();
        let candidate = matches[idx].to_string();
        let ghost = mention::remainder(&candidate, &query.prefix)?;
        Some((query, candidate, ghost))
    }

    /// Whether the open room is holding any messages.
    ///
    /// For tests that need to prove a background event really landed in a
    /// parked room rather than being dropped on the floor.
    #[must_use]
    #[cfg(test)]
    pub fn render_probe_is_empty(&self) -> bool {
        match &self.mode {
            CircMode::Room { messages, .. } => messages.items.is_empty(),
            CircMode::Rooms => true,
        }
    }

    /// Whether the open room is showing anything that animates.
    ///
    /// The gate on the animation clock. Deliberately asks about the *held*
    /// messages rather than a flag set once: a room becomes animated when such
    /// a message arrives and stops being animated when it scrolls out of the
    /// buffer, and the clock should follow both without anyone remembering to
    /// turn it off.
    #[must_use]
    pub fn wants_animation(&self) -> bool {
        if !crate::config::get().animate_styles {
            return false;
        }
        let CircMode::Room { messages, .. } = &self.mode else {
            return false;
        };
        messages
            .items
            .iter()
            .any(|m| super::styles::TextStyles::from_message(m.extras.style.as_ref()).animates())
    }

    /// Advance the animation clock by one frame.
    pub fn tick_animation(&mut self) {
        self.anim_frame = self.anim_frame.wrapping_add(1);
    }

    /// How many of `updates` are messages this room does not already hold.
    ///
    /// The unread badge counts what the reader has not seen, and the live
    /// stream re-delivers: the REST poll resends a whole page, and a
    /// reconnect replays. Counting raw deliveries would inflate the badge into
    /// a number that means nothing. A patch is never news either, since it
    /// changes a message that is already here.
    ///
    /// Muted authors are skipped: their messages will not be shown, so
    /// advertising them as unread would send the reader looking for something
    /// they cannot see.
    #[must_use]
    pub fn count_unheld(&self, room_id: &str, updates: &[CircMessageUpdate]) -> usize {
        let muted = self.muted.get(room_id);
        let CircMode::Room { room, messages, .. } = &self.mode else {
            return 0;
        };
        if room.room_id() != room_id {
            return 0;
        }
        updates
            .iter()
            .filter_map(|u| match u {
                CircMessageUpdate::Full(m) if !m.id.is_empty() => Some(m),
                _ => None,
            })
            .filter(|m| muted.map_or(true, |set| !set.contains(&m.username.trim().to_lowercase())))
            .filter(|m| !messages.items.iter().any(|held| held.id == m.id))
            .count()
    }

    /// Record that `url` could not be fetched, so no band is held open for it.
    ///
    /// Without this the layout reserves rows for a picture that will never
    /// arrive *and* suppresses the `[image]` chip, because as far as the layout
    /// knows one is being drawn. The reader gets a blank hole with nothing to
    /// explain it. Marking the URL failed puts the chip back.
    pub fn note_image_failed(&self, url: &str) {
        self.image_failed.borrow_mut().insert(url.to_string());
    }

    /// The bytes already fetched for `url`, for the fullscreen modal.
    #[must_use]
    pub fn held_image_bytes(&self, url: &str) -> Option<Vec<u8>> {
        self.image_bytes.borrow().get(url).cloned()
    }

    /// Hand the screen the bytes of an image it asked for.
    ///
    /// Decoding is deliberately *not* done here: it happens lazily at render
    /// time, once, when the picture first scrolls into view, because the target
    /// cell size is only known then.
    /// Trim the image caches to what is worth keeping.
    ///
    /// Called from the render, where the set of URLs actually reachable on
    /// screen is known. Without it the caches were insert-only for the session
    /// and grew past whatever `MAX_HELD_MESSAGES` was holding back.
    fn evict_images(&self, keep: &[String]) {
        super::images::evict_to_cap(&mut self.image_bytes.borrow_mut(), keep);
        super::images::evict_to_cap(&mut self.image_protocols.borrow_mut(), keep);
        super::images::evict_to_cap(&mut self.image_dims.borrow_mut(), keep);
    }

    pub fn cache_image_bytes(&self, url: String, bytes: Vec<u8>) {
        // Read just the header for dimensions. Decoding the whole picture here
        // would do the expensive work for an image that may never scroll into
        // view, and the size is all the layout needs.
        if let Some(dims) = super::images::probe_dimensions(&bytes) {
            self.image_dims.borrow_mut().insert(url.clone(), dims);
        }
        self.image_bytes.borrow_mut().insert(url, bytes);
    }

    /// Image URLs in the open room that are worth fetching now.
    ///
    /// Everything currently held, not merely what is on screen: history is
    /// paged in a screenful at a time, so a message just above the fold is about
    /// to be scrolled to, and fetching on demand would show a blank gap for as
    /// long as the round trip takes. Each URL is handed out once per session,
    /// so a failed fetch is not retried in a loop.
    pub fn image_urls_to_fetch(&self, room_id: &str, limit: usize) -> Vec<String> {
        let CircMode::Room { room, messages, .. } = &self.mode else {
            return Vec::new();
        };
        if room.room_id() != room_id {
            return Vec::new();
        }
        let mut requested = self.image_requested.borrow_mut();
        let bytes = self.image_bytes.borrow();
        messages
            .items
            .iter()
            .filter_map(|m| chat::inline_image_url(&m.extras))
            // Three steps, and the order of all three matters. Skip what is
            // already held or already asked for, *then* cap the batch, and only
            // then mark. Capping before the skip would re-take the same URLs
            // every call and hand back nothing; marking before the cap would
            // spend URLs that were never fetched, and a spent URL is never
            // offered again.
            .filter(|url| !bytes.contains_key(url) && !requested.contains(url))
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .inspect(|url| {
                requested.insert(url.clone());
            })
            .collect()
    }

    /// Record what the open room's live message stream is doing (see Reading a
    /// room in real time).
    ///
    /// Ignored unless `room_id` is the room actually open, so a late report from
    /// a stream the reader has already left cannot label the new room.
    pub fn apply_stream_state(&mut self, room_id: &str, state: CircStreamState) {
        let CircMode::Room { room, .. } = &self.mode else {
            return;
        };
        if room.room_id() != room_id {
            return;
        }
        self.stream_state = state;
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

        // One special key: it opens the action menu for the message under the
        // cursor. Everything else in a room types.
        if ctrl && key.code == KeyCode::Char('a') {
            if self.selected_message(&room_id).is_some() {
                if let CircMode::Room { select, .. } = &mut self.mode {
                    select.menu_open = true;
                    select.confirming_delete = false;
                }
            }
            return CircIntent::None;
        }

        if self.menu_is_open() {
            return self.handle_select_key(key, &room_id);
        }

        // The composer is focused in a room: typed keys go to the draft, Enter
        // sends, Ctrl+E expands to the editor, and the vertical keys scroll the
        // history. The horizontal keys — ←/→, Home and End — belong to the
        // composer's caret, the way they do in every other field in the client;
        // jumping the history to its ends is the action menu's Home/End
        // (Ctrl+A), which has no text to move a caret through.
        // Tab cycles the mention preview and Space commits it; every other key
        // ends the preview, leaving exactly what was typed. Nothing is ever
        // inserted without one of those two, so the ghost can never turn into
        // text the reader did not ask for.
        match key.code {
            KeyCode::Tab if !ctrl => {
                if self.mention_offer().is_some() {
                    self.mention_cycle = Some(self.mention_cycle.map_or(1, |n| n + 1));
                }
                return CircIntent::None;
            }
            KeyCode::Char(' ') if !ctrl => {
                if let Some((query, candidate, _)) = self.mention_offer() {
                    let (text, caret) =
                        mention::splice(&self.draft.text, &query, self.draft.cursor, &candidate);
                    self.draft.set(text);
                    self.draft.cursor = caret;
                }
                self.mention_cycle = None;
                self.draft.insert(' ');
                return CircIntent::None;
            }
            _ => self.mention_cycle = None,
        }

        match key.code {
            KeyCode::Char('e') if ctrl => CircIntent::StartCompose {
                room_id,
                draft: self.draft.text.clone(),
            },
            KeyCode::Enter => {
                let content = send_content(&self.draft.text);
                if content.trim().is_empty() {
                    return CircIntent::None;
                }
                // A mistyped command is not an error to the server: anything it
                // does not recognize is posted verbatim, so `/dcie 2d6` becomes
                // a message reading "/dcie 2d6" in front of the whole room.
                // Refuse it here instead. Names only; a recognized command with
                // bad syntax still goes for its 400 (see the Commands section).
                if !super::commands::is_known(&content, super::commands::Surface::Circ) {
                    let word = super::commands::command_word(&content).to_string();
                    // The draft is deliberately KEPT. The whole point of
                    // refusing locally is to let the reader fix a typo; clearing
                    // it destroys the message they were trying to send, which is
                    // worse than the thing this guard exists to prevent.
                    self.append_notice(&room_id, &format!("unknown command: {word}"));
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
                self.draft.backspace();
                CircIntent::None
            }
            KeyCode::Delete => {
                self.draft.delete();
                CircIntent::None
            }
            KeyCode::Left => {
                self.draft.move_left();
                CircIntent::None
            }
            KeyCode::Right => {
                self.draft.move_right();
                CircIntent::None
            }
            KeyCode::Home => {
                self.draft.move_home();
                CircIntent::None
            }
            KeyCode::End => {
                self.draft.move_end();
                CircIntent::None
            }
            // The vertical keys scroll the history and nothing else. An
            // earlier round had `up` from the composer enter message-select
            // mode, copying the reference client; Task 14.1 removed both that
            // gesture and the mode it entered, because a room whose composer is
            // always live cannot also have letters that mean commands. The
            // actions live behind `Ctrl+A` now, so an arrow key is never a mode
            // change and a reader scrolling back stays a reader scrolling back.
            //
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                self.scroll_messages(key.code, &room_id)
            }
            KeyCode::Char(c) if !ctrl => {
                self.draft.insert(c);
                CircIntent::None
            }
            _ => CircIntent::None,
        }
    }

    /// Move the history cursor one step for a mouse-wheel notch.
    ///
    /// Returns whether it handled the event, which is only when a room is open.
    ///
    /// Kept as its own path rather than routed through the `up`/`down` keys.
    /// It once mattered for correctness, when those keys entered and left
    /// message-select mode and a wheel notch could therefore change mode under
    /// a reader who was only scrolling. Task 14.1 removed that mode, so the two
    /// paths now agree; this stays separate because a notch still has to
    /// resolve to exactly one row (see `coalesce_scroll`).
    pub fn wheel_scroll(&mut self, up: bool) -> bool {
        let room_id = match &self.mode {
            CircMode::Room { room, messages, .. } => {
                if messages.loading {
                    return true;
                }
                room.room_id().to_string()
            }
            CircMode::Rooms => return false,
        };
        // The cursor indexes the *visible* view, so the clamp has to count what
        // a muted author leaves behind, not the raw history.
        let len = match &self.mode {
            CircMode::Room { messages, .. } => {
                visible_indices(&messages.items, self.muted.get(&room_id)).len()
            }
            CircMode::Rooms => return false,
        };
        let CircMode::Room { messages, .. } = &mut self.mode else {
            return false;
        };
        messages.selected = if up {
            messages.selected.saturating_sub(1)
        } else {
            messages
                .selected
                .saturating_add(1)
                .min(len.saturating_sub(1))
        };
        true
    }

    /// Whether the open room has any message the cursor could land on.
    ///
    /// Whether message-select mode has the keyboard.
    fn menu_is_open(&self) -> bool {
        matches!(&self.mode, CircMode::Room { select, .. } if select.menu_open)
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

    /// The reader's own handle, so a message that says `@them` can be marked.
    ///
    /// The client is told *who* it is signed in as (the account id, read off the
    /// id token) but never *what it is called*: login takes an email, and no
    /// response hands an account its own handle back. The room's presence list
    /// closes that gap for free: it is keyed by user id and carries the
    /// username (§ Who's in a room), and we publish our own presence on entering
    /// a room, so our entry is in there alongside everyone else's.
    ///
    /// Failing that, our own messages name us: every message carries both the
    /// author's id and their handle, so anything we have said in this room
    /// answers the same question. That is the path for a reader running with
    /// `circ_presence = false`, who is deliberately absent from the roster and
    /// would otherwise never see a mention marked.
    ///
    /// `None` until one of the two knows, which is the case for the first
    /// moments of a room and for an invisible reader who has never spoken in it.
    fn viewer_handle(&self) -> Option<&str> {
        let viewer = self.viewer_user_id.as_deref()?;
        let CircMode::Room {
            roster, messages, ..
        } = &self.mode
        else {
            return None;
        };
        if let Some(handle) = roster.handle_of(viewer) {
            return Some(handle);
        }
        // Newest first: whatever we said last carries the handle we hold now.
        // A linear scan on every frame, but only for a reader the roster does
        // not name, and only over messages already in memory.
        messages
            .items
            .iter()
            .rev()
            .find(|m| m.user_id == viewer)
            .map(|m| m.username.trim())
            .filter(|name| !name.is_empty())
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
            KeyCode::Char('o') => match self.selected_message(room_id) {
                Some(m) => match chat::open_action(&m.extras, &m.content) {
                    OpenAction::Play(track) => CircIntent::PlayJukebox(track),
                    OpenAction::Open(url) => CircIntent::OpenUrl(url),
                    OpenAction::None => CircIntent::None,
                },
                None => CircIntent::None,
            },
            KeyCode::Char('v') => {
                // Also reveals a substituted body. `l33t`, `flip` and `cursive`
                // rewrite the text (see `ui::styles`), and a flipped sentence is
                // genuinely hard to read, so the same key that unmasks a spoiler
                // shows the original. Both are "show me what this really says".
                let Some((id, revealable)) = self.selected_message(room_id).map(|m| {
                    let styles = super::styles::TextStyles::from_message(m.extras.style.as_ref());
                    (
                        m.id.clone(),
                        chat::has_spoiler(&m.extras) || styles.substitutes(),
                    )
                }) else {
                    return CircIntent::None;
                };
                if !revealable {
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
            KeyCode::Char('p') => match self.selected_author(room_id) {
                Some((username, _)) => CircIntent::OpenProfile { username },
                None => CircIntent::None,
            },
            KeyCode::Char('c') => {
                // Your own profile is reachable from the menu, and a
                // conversation with yourself is not a thing, so neither is
                // offered on your own message.
                if self.selected_is_mine(room_id) == Some(true) {
                    return CircIntent::None;
                }
                match self.selected_author(room_id) {
                    Some((username, user_id)) => CircIntent::OpenDm { username, user_id },
                    None => CircIntent::None,
                }
            }
            KeyCode::Char('y') => {
                let Some(text) = self
                    .selected_message(room_id)
                    .map(|m| chat::summary_text(&m.extras, &m.content))
                    .filter(|t| !t.trim().is_empty())
                else {
                    return CircIntent::None;
                };
                CircIntent::CopyText(text)
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
            // Anything else closes the menu and goes back to the composer,
            // including a plain letter: the menu is a momentary overlay, not a
            // mode to be stuck in.
            // `j`/`k` and friends still move the cursor *within* the menu;
            // anything else closes it and types, because the menu is a
            // momentary overlay rather than a mode to be stuck in.
            KeyCode::Char(c)
                if !c.is_control() && !super::list_nav::is_nav_key(KeyCode::Char(c)) =>
            {
                if let CircMode::Room { select, .. } = &mut self.mode {
                    select.menu_open = false;
                    select.confirming_delete = false;
                }
                self.draft.insert(c);
                CircIntent::None
            }
            code => self.scroll_messages(code, room_id),
        }
    }

    /// The selected message's author, as `(username, user_id)`.
    ///
    /// `None` for a local notice or anything else without a real author, which
    /// is what keeps the per-author keys inert on those rows.
    fn selected_author(&self, room_id: &str) -> Option<(String, String)> {
        let m = self.selected_message(room_id)?;
        if is_local_notice(m) {
            return None;
        }
        let username = m.username.trim();
        (!username.is_empty()).then(|| (username.to_string(), m.user_id.clone()))
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
            // `list_nav` has no PageUp arm and only handles PageDown when more
            // history is loadable, so both would otherwise fall through and do
            // nothing. Handled here rather than by widening `list_nav`, which
            // Feed, Bookmarks and Notifications also use: a paging change there
            // is a wider blast radius than this task wants.
            KeyCode::PageUp => {
                messages.selected = messages.selected.saturating_sub(PAGE_JUMP);
            }
            KeyCode::PageDown => {
                messages.selected = messages
                    .selected
                    .saturating_add(PAGE_JUMP)
                    .min(view_len.saturating_sub(1));
            }
            other => {
                super::list_nav::navigate(other, &mut messages.selected, view_len, false);
            }
        }
        CircIntent::None
    }

    /// Draw the room list, or the open room.
    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        picker: Option<&Picker>,
    ) {
        match &self.mode {
            CircMode::Rooms => self.render_rooms(frame, area, theme),
            CircMode::Room { .. } => self.render_room(frame, area, theme, picker),
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

    fn render_room(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        picker: Option<&Picker>,
    ) {
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
        // Live status on the title: who is here, whether older history is on
        // its way, and what the stream is doing. All three used to be invisible
        // with the roster closed, which is the default.
        let present = match &self.mode {
            CircMode::Room { roster, .. } => {
                let now = now_ms();
                roster
                    .entries
                    .iter()
                    .filter(|e| e.is_visible(now, roster.stale_after_ms))
                    .count()
            }
            CircMode::Rooms => 0,
        };
        let title = if present > 0 {
            format!("{title}· {present} here ")
        } else {
            title
        };
        let title = if messages.loading && messages.loaded {
            // Only for a *later* page: the first load already says "loading" in
            // the pane itself, and saying it twice reads as two things loading.
            format!("{title}(loading history…) ")
        } else {
            title
        };
        let title = match self.stream_state {
            CircStreamState::Live => title,
            CircStreamState::Reconnecting(n) => {
                format!("{title}(live updates lost, reconnecting {n}/{MAX_RECONNECT_ATTEMPTS}) ")
            }
            CircStreamState::Lost => format!("{title}(live updates lost) "),
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
        // The composer input is always present: its rows, then one row of hints.
        // It grows as the draft wraps, but never past the point where the
        // conversation it belongs to would be squeezed out.
        let cap = composer::MAX_ROWS
            .min(
                chat_area
                    .height
                    .saturating_sub(out_rows + composer::MIN_CHAT_ROWS + 1),
            )
            .max(1);
        let composer_rows = self.composer_rows(chat_area.width, cap);
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(out_rows),
                Constraint::Length(composer_rows + 1),
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
        // Who we are, so a message that names us can say so. `None` (the room
        // has only just opened, or we never published presence) simply leaves
        // every message rendered the way it was before.
        let mention = self.viewer_handle();
        // Inline pictures need a graphics-capable terminal, images left on, and
        // a pane tall enough that reserving rows still leaves the conversation
        // readable. Otherwise the `[image]` chip stands in, which is what a
        // terminal without graphics should see anyway.
        let image_rows: Option<u16> = picker.filter(|_| layout[0].height > 6).map(|_| {
            crate::config::get()
                .image_height
                .min(layout[0].height / 2)
                .max(1)
        });
        let animating = self.wants_animation();
        // A picture's band is sized from its own aspect ratio once its header
        // has been read, capped at `image_rows`. Before the bytes arrive the cap
        // stands in, so a picture never appears clipped; the band shrinks when
        // the real size is known, which costs one reflow and saves the rows a
        // wide image would otherwise waste.
        let font = picker.map_or((8, 16), |p| {
            let fs = p.font_size();
            (fs.width, fs.height)
        });
        let rows_for = |m: &CircMessage| -> Option<u16> {
            let cap = image_rows?;
            let url = chat::inline_image_url(&m.extras)?;
            // A fetch that failed reserves nothing: no band, and the `[image]`
            // chip comes back, so the reader sees a link rather than a hole.
            if self.image_failed.borrow().contains(&url) {
                return None;
            }
            Some(match self.image_dims.borrow().get(&url) {
                Some(&dims) => chat::fitted_image_rows(
                    dims,
                    u16::try_from(body_width).unwrap_or(u16::MAX),
                    font,
                    cap,
                ),
                None => cap,
            })
        };
        let layout_of = |m: &CircMessage| {
            let layout = BodyLayout::new(body_width)
                .with_revealed(select.revealed.contains(&m.id))
                .with_max_rows(body_cap);
            let layout = match rows_for(m) {
                Some(rows) => layout.with_image_rows(rows),
                None => layout,
            };
            let layout = if animating {
                layout.with_anim_frame(self.anim_frame)
            } else {
                layout
            };
            match mention {
                Some(handle) => layout.with_mention(handle),
                None => layout,
            }
        };
        let heights: Vec<u16> = visible
            .iter()
            .map(|&i| {
                let m = &messages.items[i];
                circ_message_height(m, theme, layout_of(m))
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
        // Keep only what the room still holds; a session's worth of decoded
        // protocols is far more memory than the messages they belong to.
        {
            let keep: Vec<String> = messages
                .items
                .iter()
                .filter_map(|m| chat::inline_image_url(&m.extras))
                .collect();
            self.evict_images(&keep);
        }

        // Paint each reserved gap, now that the list has settled where every
        // message sits. Positions come from the same `heights` the pane was
        // laid out with and the same `layout_of` the bodies were built from, so
        // this cannot drift from what is on screen the way a re-derived guess
        // would.
        if let (Some(picker), Some(_)) = (picker, image_rows) {
            let cols = u16::try_from(body_width).unwrap_or(u16::MAX);
            let mut protocols = self.image_protocols.borrow_mut();
            let bytes = self.image_bytes.borrow();
            let pane_bottom = messages_area.y.saturating_add(messages_area.height);
            let mut y = messages_area.y;
            for (&i, &h) in visible
                .iter()
                .zip(heights.iter())
                .skip(messages.list_offset())
            {
                if y >= pane_bottom {
                    break;
                }
                let m = &messages.items[i];
                if let Some((url, above, gap_rows)) =
                    chat::image_gap(ChatMessage::from(m), layout_of(m))
                {
                    // Encode at the band this message actually reserved, not at
                    // the ceiling, or a wide picture is letterboxed into rows
                    // the layout never gave it.
                    let target = Size::new(cols, gap_rows);
                    // One row for the speaker header, then the body rows above
                    // the gap.
                    let top = y.saturating_add(1).saturating_add(above);
                    // Clip against the pane rather than resizing, so a picture
                    // scrolling in from the bottom slides rather than squashes.
                    let visible_rows = gap_rows.min(pane_bottom.saturating_sub(top));
                    if top < pane_bottom && visible_rows > 0 {
                        // `is_none_or` would read better but is newer than
                        // this crate's MSRV.
                        let stale = protocols
                            .get(&url)
                            .map_or(true, |(_, built)| *built != target);
                        if stale {
                            if let Some(raw) = bytes.get(&url) {
                                match super::images::decode_bounded(raw).and_then(|img| {
                                    picker
                                        .new_protocol(
                                            img,
                                            target,
                                            Resize::Fit(super::images::filter()),
                                        )
                                        .map_err(|e| e.to_string())
                                }) {
                                    Ok(proto) => {
                                        protocols.insert(url.clone(), (proto, target));
                                    }
                                    Err(e) => {
                                        tracing::debug!(error = %e, url = %url, "circ image encode failed");
                                    }
                                }
                            }
                        }
                        if let Some((proto, _)) = protocols.get(&url) {
                            let img_area = Rect::new(
                                // Past the list's highlight gutter and the
                                // body's own indent, so the picture lines up
                                // with the text above it.
                                messages_area.x.saturating_add(4),
                                top,
                                target.width,
                                visible_rows,
                            );
                            frame.render_widget(Image::new(proto).allow_clipping(true), img_area);
                        }
                    }
                }
                y = y.saturating_add(h);
            }
        }

        // Attachment chips become clickable only after the pane has drawn, and
        // only against the same rect and the same message order. Gated on the
        // `hyperlinks` config like every other OSC 8 surface in the client, so
        // turning it off really does leave the chips as plain text.
        if crate::config::get().hyperlinks {
            // Only the rows the list actually drew, which start at the offset it
            // just settled on. Handing over chips for scrolled-off messages
            // would slide every link onto the wrong message's attachment.
            // Built from `layout_of`, the very layout the bodies were drawn
            // with, not from a fresh one. A bare layout reserves no image band,
            // so it still lists the `[image]` chip that a drawn picture
            // suppresses; `apply_chip_links` walks chips against the runs it
            // finds on screen and stops at the first mismatch, so that one
            // phantom entry silently killed *every* attachment link in a room
            // containing an inline image.
            let chips: Vec<chat::ChipLink> = visible
                .iter()
                .skip(messages.list_offset())
                .flat_map(|&i| {
                    let m = &messages.items[i];
                    chat::message_chips(ChatMessage::from(m), layout_of(m))
                })
                .collect();
            chat::apply_chip_links(frame.buffer_mut(), messages_area, &chips, theme);
        }

        if out_rows > 0 {
            self.render_outgoing(frame, layout[1], theme);
        }
        let selected = visible
            .get(messages.selected)
            .map(|&i| &messages.items[i])
            .filter(|_| select.menu_open);
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

    /// Rows the footer's top half takes: one for the flag prompt, the delete
    /// confirmation or the select status line, and however many the wrapped
    /// draft needs (up to `cap`) when the composer is the one showing.
    ///
    /// [`render_room`](Self::render_room) sizes the layout with this and
    /// [`render_footer`](Self::render_footer) fills it, both from the same
    /// `(draft, width, cap)`, so the two can't disagree about the height.
    fn composer_rows(&self, width: u16, cap: u16) -> u16 {
        let CircMode::Room { select, .. } = &self.mode else {
            return 1;
        };
        if select.flag.is_some() || select.confirming_delete || select.menu_open {
            return 1;
        }
        composer::layout(
            &self.draft.text,
            self.draft.cursor,
            composer::width(width),
            usize::from(cap),
            // The ghost occupies columns, so a preview that pushes the draft
            // onto another row has to be counted here too, or the pane and the
            // composer would disagree about how tall it is.
            &self.mention_offer().map_or_else(String::new, |(_, _, g)| g),
        )
        .rows as u16
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
        // `render_room` sized this area as the composer's rows plus the hint, so
        // everything above the last row belongs to the composer.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
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
                vec![line],
                "enter report · esc cancel · reason optional, max 500".to_string(),
            )
        } else if select.confirming_delete {
            (
                vec![Line::from(Span::styled(
                    "delete this message? y confirms · any other key cancels",
                    theme.warning_style(),
                ))],
                "y confirm · esc cancel".to_string(),
            )
        } else if select.menu_open {
            let mine = selected.and_then(|m| {
                let viewer = self.viewer_user_id.as_deref()?;
                Some(!m.user_id.is_empty() && m.user_id == viewer)
            });
            (
                vec![select_status_line(selected, select, theme)],
                select_hint(selected, mine),
            )
        } else {
            // Always-on input line, soft-wrapped over the rows `render_room`
            // reserved for it.
            let view = composer::layout(
                &self.draft.text,
                self.draft.cursor,
                composer::width(rows[0].width),
                usize::from(rows[0].height),
                &self.mention_offer().map_or_else(String::new, |(_, _, g)| g),
            );
            (
                composer::lines(&view, theme),
                // Only the keys that change with state are spelled out; the
                // hint has to fit on one row and `?` covers the rest.
                if has_older {
                    "enter send · ↑ older · ctrl+a actions · ctrl+u users · esc back".to_string()
                } else {
                    "enter send · ctrl+a actions · ctrl+u users · ctrl+e editor · esc back"
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
    // A local notice has no author, so every per-author key is inert on it.
    if is_local_notice(m) {
        return "esc close".to_string();
    }
    // Every key here is gated by its handler, so the hint has to be gated the
    // same way or it advertises a key that silently does nothing. `mine` is
    // `None` when the viewer is unknown, in which case offer everything and let
    // the server decide, which is what the handlers do too.
    let mut parts = vec!["j/k move"];
    if mine != Some(false) && !m.extras.deleted && !m.id.is_empty() {
        parts.push("d delete");
    }
    if mine != Some(true) {
        parts.push("F flag");
    }
    if !matches!(chat::open_action(&m.extras, &m.content), OpenAction::None) {
        parts.push("o open");
    }
    if chat::has_spoiler(&m.extras)
        || super::styles::TextStyles::from_message(m.extras.style.as_ref()).substitutes()
    {
        parts.push("v reveal");
    }
    if mine != Some(true) {
        parts.push("m mute");
    }
    parts.push("y copy");
    parts.push("p profile");
    if mine != Some(true) {
        parts.push("c dm");
    }
    parts.push("esc close");
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
    // Admins first, then everyone else, each block alphabetical. Who can act on
    // a report is worth finding at a glance, and the block is short.
    people.sort_by_key(|e| (!e.is_chat_admin, e.username.to_lowercase()));

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

/// One message's timestamp, per the reader's `time_format`.
fn format_chat_timestamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => crate::config::format_chat_timestamp(t),
        Err(_) => String::new(),
    }
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

/// Drop the oldest messages once a room has held more than [`MAX_HELD_MESSAGES`].
///
/// A room left open all day otherwise grows without limit, and every message
/// carries its decoded art and its cached image bytes with it. Trimming from the
/// front is safe because the cursor is anchored by id across the merge and
/// re-resolved after it; scrolling back up simply re-pages the history from the
/// server, which is what it does at the end of the buffer anyway.
///
/// Only ever trims when doing so leaves a full pane's worth, so this cannot
/// empty a quiet room.
fn trim_history(
    messages: &mut TabState<CircMessage>,
    muted: Option<&HashSet<String>>,
    at_bottom: bool,
) {
    if messages.items.len() <= MAX_HELD_MESSAGES {
        return;
    }
    // Normally never while the reader is scrolled back: trimming takes from the
    // front, which is exactly the history someone paging backwards has just
    // loaded and is reading.
    //
    // But "scrolled back" is a frozen cursor for a *parked* room, whose reader
    // is not looking at it at all, so deferring there switched the cap off for
    // as long as the room stayed backgrounded. Past a hard ceiling the cap wins
    // regardless: an unbounded buffer is worse than a scroll position, and the
    // trimmed history re-pages from the server on demand.
    if !at_bottom && messages.items.len() <= MAX_HELD_MESSAGES.saturating_mul(2) {
        return;
    }
    let drop = messages.items.len() - MAX_HELD_MESSAGES;
    // How many of the dropped rows were actually on screen, so the view index
    // shifts by what the reader would have seen leave.
    let shown_before = visible_indices(&messages.items[..drop], muted).len();
    messages.items.drain(..drop);
    messages.selected = messages.selected.saturating_sub(shown_before);
    messages.shift_offset_back(shown_before);
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
    if is_local_notice(m) {
        return notice_lines(&m.content, theme, layout);
    }
    let for_me = layout
        .mention
        .is_some_and(|handle| chat::mentions(ChatMessage::from(m), handle));
    let mut lines = vec![circ_message_header(m, theme, for_me)];
    lines.extend(chat::body_lines(ChatMessage::from(m), layout, theme));
    lines
}

/// The speaker row: name, a `★` for a chat admin, and the relative timestamp.
///
/// `for_me` fills the row's two-column gutter with [`MENTION_MARK`], so a
/// message that names the reader is findable while scrolling past rather than
/// only once they stop and read it. The gutter is the same width either way, so
/// marking one never shifts the pane.
///
/// Kept even for an action (`/me`), whose body already reads `* username …`,
/// and even for a deleted message: § Delete Your Message keeps the author's name
/// and the original timestamp, and the header is the only place either appears.
/// Rows one message occupies in the pane, notices included.
///
/// **The single place message height is decided.** `heights` used to call
/// `chat::message_height(.., 1)` for every row, which adds a speaker-header row,
/// while `circ_message_lines` short-circuits a local notice to `notice_lines`
/// and draws no header at all. Every row and every inline image below a notice
/// was therefore off by one, and the pane's bottom-anchoring was computed from a
/// total that was too large. Same agreement bug as `body_height` had for image
/// gaps, in a path added after that lesson, which is why it now goes through one
/// function instead of two that merely ought to match.
fn circ_message_height(m: &CircMessage, theme: &Theme, layout: BodyLayout<'_>) -> u16 {
    u16::try_from(circ_message_lines(m, theme, layout).len()).unwrap_or(u16::MAX)
}

/// A local notice: `*** text`, no speaker row, no timestamp.
///
/// Deliberately shaped unlike a message. It has no author and no time, because
/// nobody said it and it did not happen at a moment in the conversation; it is
/// the client talking. Continuation rows align under the text rather than under
/// the prefix, so a wrapped `/help` reply reads as one block.
fn notice_lines(text: &str, theme: &Theme, layout: BodyLayout<'_>) -> Vec<Line<'static>> {
    let style = theme.muted_style().add_modifier(Modifier::ITALIC);
    let pad = " ".repeat(NOTICE_PREFIX.len());
    let width = layout.width.saturating_sub(NOTICE_PREFIX.len()).max(1);
    let mut rows: Vec<Line<'static>> = Vec::new();
    // Each source line wraps on its own, so a multi-line reply (which `/help`
    // is) keeps the breaks the server put in it instead of reflowing into one
    // paragraph.
    for source in text.lines() {
        for (i, row) in chat::word_wrap(source, width).into_iter().enumerate() {
            let lead = if rows.is_empty() && i == 0 {
                NOTICE_PREFIX
            } else {
                pad.as_str()
            };
            rows.push(Line::from(Span::styled(format!("{lead}{row}"), style)));
        }
    }
    if rows.is_empty() {
        rows.push(Line::from(Span::styled(NOTICE_PREFIX.to_string(), style)));
    }
    if let Some(max) = layout.max_rows {
        rows.truncate(max.max(1));
    }
    rows
}

fn circ_message_header(m: &CircMessage, theme: &Theme, for_me: bool) -> Line<'static> {
    // A chat log wants a clock, not "3h": see `format_chat_timestamp`.
    let when = format_chat_timestamp(m.timestamp);
    let name = if m.username.is_empty() {
        "?".to_string()
    } else {
        m.username.clone()
    };
    let gutter = if for_me {
        Span::styled(MENTION_MARK, theme.accent_style())
    } else {
        Span::styled("  ", theme.muted_style())
    };
    // Each speaker's name gets their stable per-user colour so a busy room is
    // easy to scan; a ★ marks chat admins.
    let mut header = vec![
        gutter,
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
    /// A room with the action menu open on the newest message.
    fn selecting(msgs: Vec<CircMessage>) -> CircScreen {
        let mut s = open("general");
        s.apply_messages("general", true, Ok((msgs, None)));
        s.handle_key(ctrl(KeyCode::Char('a')));
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
        // No picker in tests: the pane renders chips, which is also what a
        // terminal without graphics support sees.
        terminal
            .draw(|f| s.render(f, f.area(), &theme, None))
            .unwrap();
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
            // The caret is a reverse-video cell, so the empty composer's row is
            // the prompt plus a blank the row trim takes off.
            .position(|r| r.starts_with('›'))
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
            // The caret is a reverse-video cell, so the empty composer's row is
            // the prompt plus a blank the row trim takes off.
            .position(|r| r.starts_with('›'))
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
    fn a_mistyped_command_is_refused_locally_and_never_sent() {
        // The whole point: the server posts anything it does not recognize as
        // literal text, so without this the room sees "/dcie 2d6".
        let mut s = open("general");
        typed(&mut s, "/dcie 2d6");
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, CircIntent::None, "nothing is sent");
        assert!(s.outgoing.is_empty(), "and nothing is queued as outgoing");
        let joined = render_rows(&s, 14).join("\n");
        assert!(
            joined.contains("unknown command: /dcie"),
            "the reader is told which word was refused:\n{joined}",
        );
    }

    #[test]
    fn refusing_a_command_keeps_what_was_typed() {
        // Clearing the draft here destroyed the message the reader was trying to
        // fix, which is worse than the mistyped command this guard prevents.
        let mut s = open("general");
        typed(&mut s, "/dcie 2d6");
        s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            s.draft.text, "/dcie 2d6",
            "the typo is still there to be corrected",
        );
        assert_eq!(s.draft.cursor, s.draft.len(), "with the caret where it was");
    }

    #[test]
    fn a_real_command_still_sends() {
        let mut s = open("general");
        typed(&mut s, "/dice:6");
        let intent = s.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(intent, CircIntent::SendMessage { .. }),
            "a documented /dice form is not refused: {intent:?}",
        );
    }

    #[test]
    fn a_notice_renders_without_an_author_or_a_timestamp() {
        let mut s = open("general");
        s.apply_messages("general", true, Ok((vec![], None)));
        s.append_notice("general", "commands: /me /dice /help");
        let rows = render_rows(&s, 14);
        let joined = rows.join("\n");
        assert!(
            rows.iter().any(|r| r.contains("*** commands:")),
            "the notice is prefixed like IRC client output:\n{joined}",
        );
        let notice_row = rows
            .iter()
            .find(|r| r.contains("***"))
            .expect("the notice is on screen");
        assert!(
            !notice_row.contains('\u{b7}'),
            "the notice row carries no ' \u{b7} timestamp' speaker separator, since \
             nobody said it and it happened at no point in the conversation: \
             {notice_row:?}",
        );
    }

    #[test]
    fn a_multi_line_reply_keeps_every_line() {
        // This is the regression the toast caused: /help is a command list, and
        // `first_line` showed one line of it, capped, then dropped it.
        let mut s = open("general");
        s.apply_messages("general", true, Ok((vec![], None)));
        s.append_notice("general", "line one\nline two\nline three");
        let joined = render_rows(&s, 14).join("\n");
        for line in ["line one", "line two", "line three"] {
            assert!(joined.contains(line), "{line} survived:\n{joined}");
        }
    }

    #[test]
    fn a_notice_cannot_be_deleted_flagged_or_muted() {
        // The empty id and username are what make this true; see `local_notice`.
        let mut s = open("general");
        s.apply_messages("general", true, Ok((vec![], None)));
        s.append_notice("general", "unknown command: /nope");
        s.handle_key(ctrl(KeyCode::Char('a')));
        assert!(s.menu_is_open(), "select mode is on the notice");
        for k in ['d', 'F', 'm'] {
            assert_eq!(
                s.handle_key(key(KeyCode::Char(k))),
                CircIntent::None,
                "{k} must do nothing to a notice: it is not a real message",
            );
        }
    }

    #[test]
    fn a_notice_is_dropped_for_a_room_that_is_not_open() {
        let mut s = open("general");
        s.apply_messages("general", true, Ok((vec![], None)));
        s.append_notice("other-room", "should not appear");
        let joined = render_rows(&s, 14).join("\n");
        assert!(
            !joined.contains("should not appear"),
            "a late reply for another room is not pasted into this one:\n{joined}",
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

    /// Type `text` into whatever field currently has the keyboard.
    fn typed(s: &mut CircScreen, text: &str) {
        for c in text.chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
    }

    /// The composer's rendered rows with the prompt gutter stripped, joined back
    /// into the draft text they show. The caret is a reverse-video cell, so it
    /// contributes the character underneath it (or a blank at end of text).
    fn composer_text(s: &CircScreen, width: u16, height: u16) -> String {
        let rows = render_rows_wide(s, width, height);
        let first = rows
            .iter()
            .position(|r| r.starts_with('›') || r.starts_with('…'))
            .expect("composer should be visible");
        let hint = rows
            .iter()
            .position(|r| r.contains("enter send"))
            .expect("composer hint row should be visible");
        rows[first..hint]
            .iter()
            .map(|r| r.chars().skip(composer::PROMPT.chars().count()).collect())
            .collect::<Vec<String>>()
            .join("")
    }

    #[test]
    fn home_and_end_move_the_composer_caret_instead_of_jumping_the_history() {
        // Regression: Home/End used to scroll the room's scrollback, which left
        // the composer with no way to reach the start of a line at all.
        let mut s = open("general");
        s.apply_messages(
            "general",
            true,
            Ok((
                (0..5)
                    .map(|i| message(&format!("m{i}"), "neo", "hi", 1_000 + i64::from(i)))
                    .collect(),
                None,
            )),
        );
        let CircMode::Room { messages, .. } = &s.mode else {
            panic!("room should be open");
        };
        let selected = messages.selected;

        typed(&mut s, "hey");
        s.handle_key(key(KeyCode::Home));
        assert_eq!(s.draft.cursor, 0);
        typed(&mut s, ">");
        assert_eq!(s.draft.text, ">hey");
        s.handle_key(key(KeyCode::End));
        typed(&mut s, "!");
        assert_eq!(s.draft.text, ">hey!");

        let CircMode::Room { messages, .. } = &s.mode else {
            panic!("room should be open");
        };
        assert_eq!(
            messages.selected, selected,
            "the caret keys must leave the scrollback where it was"
        );
    }

    #[test]
    fn the_caret_keys_edit_in_the_middle_of_the_draft() {
        let mut s = open("general");
        typed(&mut s, "helo");
        s.handle_key(key(KeyCode::Left));
        typed(&mut s, "l");
        assert_eq!(s.draft.text, "hello");
        s.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            s.draft.text, "helo",
            "backspace takes the char before the caret"
        );
        s.handle_key(key(KeyCode::Delete));
        assert_eq!(s.draft.text, "hel", "delete takes the char under the caret");
        s.handle_key(key(KeyCode::Right));
        typed(&mut s, "p");
        assert_eq!(s.draft.text, "help");
    }

    #[test]
    fn a_paste_lands_at_the_caret() {
        let mut s = open("general");
        typed(&mut s, "ab");
        s.handle_key(key(KeyCode::Left));
        s.paste_text("xy");
        assert_eq!(s.draft.text, "axyb");
        assert_eq!(s.draft.cursor, 3, "the caret follows the pasted text");
    }

    #[test]
    fn multibyte_text_is_edited_by_character_not_by_byte() {
        let mut s = open("general");
        typed(&mut s, "héllo");
        s.handle_key(key(KeyCode::Home));
        s.handle_key(key(KeyCode::Right));
        s.handle_key(key(KeyCode::Delete));
        assert_eq!(s.draft.text, "hllo");
    }

    #[test]
    fn sending_and_leaving_both_reset_the_caret() {
        let mut s = open("general");
        typed(&mut s, "hey");
        s.handle_key(key(KeyCode::Enter));
        assert_eq!(s.draft.cursor, 0, "a sent draft leaves an empty composer");
        typed(&mut s, "more");
        s.handle_escape();
        assert_eq!(s.draft.text, "");
        assert_eq!(s.draft.cursor, 0);
    }

    #[test]
    fn a_prefill_from_the_editor_leaves_the_caret_at_the_end() {
        let mut s = open("general");
        s.set_draft_and_focus("from the editor".to_string());
        assert_eq!(s.draft.cursor, s.draft.len());
        typed(&mut s, "!");
        assert_eq!(s.draft.text, "from the editor!");
    }

    #[test]
    fn a_long_draft_wraps_onto_more_composer_rows() {
        // Regression: the composer drew one row and let anything past the right
        // edge fall off the screen, so a long message became invisible as it was
        // typed.
        let mut s = open("general");
        let draft: String = (0..120)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        typed(&mut s, &draft);

        let rows = render_rows(&s, 16);
        let first = rows
            .iter()
            .position(|r| r.starts_with('›'))
            .expect("composer should be visible");
        let hint = rows
            .iter()
            .position(|r| r.contains("enter send"))
            .expect("composer hint row should be visible");
        assert!(
            hint - first > 1,
            "a draft wider than the pane must wrap onto further rows:\n{}",
            rows.join("\n"),
        );
        assert_eq!(
            composer_text(&s, 50, 16),
            draft,
            "every character typed stays on screen"
        );
    }

    #[test]
    fn an_overlong_draft_scrolls_the_composer_and_keeps_the_caret_in_view() {
        let mut s = open("general");
        let draft: String = (0..400)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        typed(&mut s, &draft);

        let rows = render_rows(&s, 20);
        let first = rows
            .iter()
            .position(|r| r.starts_with('…'))
            .expect("a scrolled composer marks the rows above it");
        let hint = rows
            .iter()
            .position(|r| r.contains("enter send"))
            .expect("composer hint row should be visible");
        assert_eq!(
            (hint - first) as u16,
            composer::MAX_ROWS,
            "the composer stops growing and scrolls instead:\n{}",
            rows.join("\n"),
        );
        let shown = composer_text(&s, 50, 20);
        assert!(
            draft.ends_with(shown.trim_end()),
            "typing shows the tail of the draft, where the caret is:\n{shown}"
        );

        // Home takes the window back to the start along with the caret.
        s.handle_key(key(KeyCode::Home));
        let shown = composer_text(&s, 50, 20);
        assert!(
            draft.starts_with(&shown[..shown.len() - 1]),
            "Home scrolls the composer back to the head of the draft:\n{shown}"
        );
    }

    #[test]
    fn the_composer_never_crowds_the_conversation_out() {
        let mut s = open("general");
        s.apply_messages(
            "general",
            true,
            Ok((vec![message("m1", "neo", "hi", 1_000)], None)),
        );
        let draft: String = (0..400)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        typed(&mut s, &draft);

        // A short terminal: the draft would happily take every row it can get.
        let rows = render_rows(&s, 10);
        let first = rows
            .iter()
            .position(|r| r.starts_with('›') || r.starts_with('…'))
            .expect("composer should be visible");
        assert!(
            first >= usize::from(composer::MIN_CHAT_ROWS),
            "the conversation keeps its rows however long the draft is:\n{}",
            rows.join("\n"),
        );
        assert!(
            rows.iter().any(|r| r.contains("hi")),
            "the message pane is still drawn:\n{}",
            rows.join("\n"),
        );
    }

    #[test]
    fn select_mode_still_jumps_the_history_with_home_and_end() {
        let msgs: Vec<CircMessage> = (0..5)
            .map(|i| {
                message(
                    &format!("m{i}"),
                    "neo",
                    &format!("line {i}"),
                    1_000 + i64::from(i),
                )
            })
            .collect();
        let mut s = selecting(msgs);
        s.handle_key(key(KeyCode::Home));
        let CircMode::Room { messages, .. } = &s.mode else {
            panic!("room should be open");
        };
        assert_eq!(messages.selected, 0, "Home reaches the oldest message");
        s.handle_key(key(KeyCode::End));
        let CircMode::Room { messages, .. } = &s.mode else {
            panic!("room should be open");
        };
        assert_eq!(messages.selected, 4, "End reaches the newest");
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
    fn a_room_left_open_all_day_stops_growing() {
        let mut s = room_knowing_us(vec![]);
        // Well past the cap, arriving the way live messages do.
        let updates: Vec<CircMessageUpdate> = (0..MAX_HELD_MESSAGES + 120)
            .map(|i| full(message(&format!("m{i}"), "trinity", "hi", i as i64)))
            .collect();
        s.apply_live("general", updates);
        let held = held(&s);
        assert_eq!(held.len(), MAX_HELD_MESSAGES, "the buffer is bounded");
        assert_eq!(
            held.last().map(|m| m.id.as_str()),
            Some(format!("m{}", MAX_HELD_MESSAGES + 119).as_str()),
            "and it keeps the newest, not the first it happened to see",
        );
    }

    #[test]
    fn a_frozen_cursor_cannot_switch_the_history_cap_off_forever() {
        // A parked room keeps merging messages but its selection is frozen
        // wherever the reader left it, so "scrolled back" stayed true forever
        // and the cap never applied. Past a hard ceiling the cap wins.
        let seed: Vec<CircMessage> = (0..MAX_HELD_MESSAGES)
            .map(|i| message(&format!("m{i}"), "trinity", "hi", i as i64))
            .collect();
        let mut s = room_knowing_us(seed);
        s.handle_key(ctrl(KeyCode::Char('a')));
        s.handle_key(key(KeyCode::Home));

        let more: Vec<CircMessageUpdate> = (0..MAX_HELD_MESSAGES + 100)
            .map(|i| {
                full(message(
                    &format!("later{i}"),
                    "neo",
                    "hi",
                    (MAX_HELD_MESSAGES + i) as i64,
                ))
            })
            .collect();
        s.apply_live("general", more);

        assert!(
            held(&s).len() <= MAX_HELD_MESSAGES.saturating_mul(2),
            "the ceiling holds even with the cursor off the tail: {} held",
            held(&s).len(),
        );
    }

    #[test]
    fn a_notice_under_the_cursor_does_not_teleport_it_on_a_merge() {
        // Every notice has an empty id, so anchoring the cursor on one matched
        // the first notice in the room rather than the row the reader was on.
        let mut s = room_knowing_us(vec![
            message("m1", "trinity", "one", 1),
            message("m2", "neo", "two", 2),
        ]);
        s.append_notice("general", "first notice");
        s.append_notice("general", "second notice");
        // Sit on the last notice, then scroll back one so the merge takes the
        // anchored path rather than the follow-the-tail path.
        s.handle_key(ctrl(KeyCode::Char('a')));
        s.handle_key(key(KeyCode::End));
        s.handle_key(key(KeyCode::Up));
        let before = match &s.mode {
            CircMode::Room { messages, .. } => messages.selected,
            CircMode::Rooms => unreachable!(),
        };
        s.apply_live("general", vec![full(message("m3", "trinity", "three", 3))]);
        let after = match &s.mode {
            CircMode::Room { messages, .. } => messages.selected,
            CircMode::Rooms => unreachable!(),
        };
        assert_eq!(
            before, after,
            "the cursor stayed where it was instead of jumping to the first notice",
        );
    }

    #[test]
    fn history_is_not_trimmed_out_from_under_someone_reading_it() {
        // Found in review: trimming takes from the front, which is exactly what
        // a reader paging backwards has just loaded. Doing it while they are
        // scrolled up would delete what is on their screen and move the cursor.
        let seed: Vec<CircMessage> = (0..MAX_HELD_MESSAGES)
            .map(|i| message(&format!("m{i}"), "trinity", "hi", i as i64))
            .collect();
        let mut s = room_knowing_us(seed);
        // Scroll back, so the next merge is not following the tail.
        s.handle_key(ctrl(KeyCode::Char('a')));
        s.handle_key(key(KeyCode::Home));

        let more: Vec<CircMessageUpdate> = (0..50)
            .map(|i| {
                full(message(
                    &format!("later{i}"),
                    "neo",
                    "hi",
                    (MAX_HELD_MESSAGES + i) as i64,
                ))
            })
            .collect();
        s.apply_live("general", more);

        assert!(
            held(&s).len() > MAX_HELD_MESSAGES,
            "the cap waits until the reader is following the tail again: {} held",
            held(&s).len(),
        );
        assert_eq!(
            held(&s).first().map(|m| m.id.as_str()),
            Some("m0"),
            "and the oldest message they were reading is still there",
        );
    }

    #[test]
    fn trimming_keeps_the_cursor_on_the_newest_message() {
        let mut s = room_knowing_us(vec![]);
        let updates: Vec<CircMessageUpdate> = (0..MAX_HELD_MESSAGES + 10)
            .map(|i| full(message(&format!("m{i}"), "trinity", "hi", i as i64)))
            .collect();
        s.apply_live("general", updates);
        let CircMode::Room { messages, .. } = &s.mode else {
            unreachable!()
        };
        assert_eq!(
            messages.selected,
            MAX_HELD_MESSAGES - 1,
            "a trim must not leave the cursor pointing past the end, or into \
             the wrong message",
        );
    }

    #[test]
    fn the_header_says_who_is_here_without_opening_the_roster() {
        // The count used to live only inside the Ctrl+U pane, so a reader with
        // it closed, which is the default, had no idea anyone was there.
        let mut s = open("general");
        let now = now_ms();
        s.apply_room_users(
            "general",
            Ok(vec![
                CircRoomUser {
                    user_id: "u1".into(),
                    username: "trinity".into(),
                    is_chat_admin: false,
                    last_seen: now,
                    last_activity: Some(now),
                },
                CircRoomUser {
                    user_id: "u2".into(),
                    username: "neo".into(),
                    is_chat_admin: false,
                    last_seen: now,
                    last_activity: Some(now),
                },
            ]),
        );
        assert!(
            !s.roster_open,
            "precondition: the pane is closed by default"
        );
        let joined = render_rows_wide(&s, 100, 14).join("\n");
        assert!(
            joined.contains("2 here"),
            "the header carries the count:\n{joined}",
        );
    }

    #[test]
    fn unread_counts_only_messages_the_room_does_not_already_hold() {
        // The stream re-delivers: the REST poll resends a whole page and a
        // reconnect replays. Counting raw deliveries would inflate the badge
        // into a number that means nothing.
        let s = room_knowing_us(vec![message("m1", "trinity", "old", 1)]);
        let updates = vec![
            full(message("m1", "trinity", "old", 1)),
            full(message("m2", "neo", "new", 2)),
        ];
        assert_eq!(s.count_unheld("general", &updates), 1);
    }

    #[test]
    fn unread_ignores_muted_authors_and_patches() {
        let mut s = room_knowing_us(vec![message("m1", "trinity", "here", 1)]);
        s.set_muted_users("general", &["spam".to_string()]);
        let updates = vec![
            full(message("m2", "spam", "buy things", 2)),
            CircMessageUpdate::Partial {
                id: "m1".into(),
                patch: CircMessagePatch {
                    deleted: Some(true),
                    ..Default::default()
                },
            },
        ];
        assert_eq!(
            s.count_unheld("general", &updates),
            0,
            "a muted author's message will not be shown, and a patch changes a \
             message already here; neither is news",
        );
    }

    #[test]
    fn unread_is_zero_for_a_room_that_is_not_the_open_one() {
        let s = room_knowing_us(vec![]);
        let updates = vec![full(message("m1", "neo", "hi", 1))];
        assert_eq!(s.count_unheld("other-room", &updates), 0);
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
    fn the_header_says_when_live_updates_are_dropping_and_when_they_are_gone() {
        // A dropped stream used to be invisible, because the REST poll kept the
        // room updating and nothing said the difference. This is the signal.
        let mut s = open("general");
        let clean = render_rows_wide(&s, 100, 14).join("\n");
        assert!(
            !clean.contains("live updates"),
            "a healthy room says nothing:\n{clean}",
        );

        s.apply_stream_state("general", CircStreamState::Reconnecting(3));
        let retrying = render_rows_wide(&s, 100, 14).join("\n");
        assert!(
            retrying.contains(&format!("reconnecting 3/{MAX_RECONNECT_ATTEMPTS}")),
            "the attempt count is shown so the reader can see progress:\n{retrying}",
        );

        s.apply_stream_state("general", CircStreamState::Lost);
        let lost = render_rows_wide(&s, 100, 14).join("\n");
        assert!(
            lost.contains("(live updates lost)") && !lost.contains("reconnecting"),
            "and the final state is stated plainly:\n{lost}",
        );
    }

    #[test]
    fn a_stream_report_for_another_room_is_ignored() {
        let mut s = open("general");
        s.apply_stream_state("other-room", CircStreamState::Lost);
        let joined = render_rows_wide(&s, 100, 14).join("\n");
        assert!(
            !joined.contains("live updates lost"),
            "a late report from a room we already left cannot label this one:\n{joined}",
        );
    }

    #[test]
    fn re_entering_a_room_clears_a_lost_stream_indicator() {
        // The documented recovery from an exhausted ladder is to leave and come
        // back, so re-entry has to actually reset it.
        let mut s = open("general");
        s.apply_stream_state("general", CircStreamState::Lost);
        s.open_room("general");
        let joined = render_rows_wide(&s, 100, 14).join("\n");
        assert!(
            !joined.contains("live updates lost"),
            "re-entry starts a fresh stream generation and a fresh verdict:\n{joined}",
        );
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
    fn a_notice_is_measured_at_exactly_the_height_it_draws() {
        // A notice draws no speaker header, but `heights` used to add one for
        // every row, so the pane and every inline image below a notice sat a row
        // off. Both now go through `circ_message_height`.
        let theme = Theme::cyber();
        let layout = BodyLayout::new(40);
        let notice = local_notice("*** something happened", 1);
        assert_eq!(
            circ_message_height(&notice, &theme, layout) as usize,
            circ_message_lines(&notice, &theme, layout).len(),
        );
        let normal = message("m1", "trinity", "an ordinary line", 1);
        assert_eq!(
            circ_message_height(&normal, &theme, layout) as usize,
            circ_message_lines(&normal, &theme, layout).len(),
        );
        assert!(
            circ_message_height(&notice, &theme, layout)
                < circ_message_height(&normal, &theme, layout),
            "a notice really is shorter, which is what the old measurement missed",
        );
    }

    #[test]
    fn a_failed_image_fetch_gives_the_chip_back_instead_of_a_blank_band() {
        let mut m = message("m1", "trinity", "look", 1);
        m.extras.image_url = Some("https://cdn.example/pic.png".into());
        let s = room_knowing_us(vec![m]);
        s.note_image_failed("https://cdn.example/pic.png");
        let joined = render_rows(&s, 14).join("\n");
        assert!(
            joined.contains("[image]"),
            "a picture that will never arrive is a link, not a hole:\n{joined}",
        );
    }

    #[test]
    fn without_graphics_an_image_message_still_shows_its_chip() {
        // The fallback path, and the one every test renders on: no picker means
        // no gap is reserved, so the pane looks exactly as it did before inline
        // images existed.
        let mut m = message("m1", "trinity", "look at this", 1);
        m.extras.image_url = Some("https://cdn.example/pic.png".into());
        let s = room_knowing_us(vec![m]);
        let joined = render_rows(&s, 14).join("\n");
        assert!(
            joined.contains("[image]"),
            "a terminal without graphics gets the chip it always had:\n{joined}",
        );
    }

    #[test]
    fn an_image_is_only_fetched_once_and_only_for_the_open_room() {
        let mut m = message("m1", "trinity", "pic", 1);
        m.extras.image_url = Some("https://cdn.example/pic.png".into());
        let s = room_knowing_us(vec![m]);

        let first = s.image_urls_to_fetch("general", 8);
        assert_eq!(first, vec!["https://cdn.example/pic.png".to_string()]);
        assert!(
            s.image_urls_to_fetch("general", 8).is_empty(),
            "handing the same URL out twice would re-fetch it on every message \
             that arrives",
        );
        assert!(
            s.image_urls_to_fetch("other-room", 8).is_empty(),
            "a room that is not open has nothing to fetch",
        );
    }

    #[test]
    fn capping_the_fetch_batch_does_not_spend_the_urls_it_skips() {
        // `image_urls_to_fetch` marks a URL requested as it hands it out, and a
        // URL marked but never fetched is never offered again. Capping the batch
        // outside the function would therefore have stranded every picture past
        // the first few, permanently.
        let msgs: Vec<CircMessage> = (0..10)
            .map(|i| {
                let mut m = message(&format!("m{i}"), "trinity", "pic", i as i64);
                m.extras.image_url = Some(format!("https://cdn.example/{i}.png"));
                m
            })
            .collect();
        let s = room_knowing_us(msgs);

        let first = s.image_urls_to_fetch("general", 3);
        assert_eq!(first.len(), 3, "the batch is capped");
        let second = s.image_urls_to_fetch("general", 3);
        assert_eq!(second.len(), 3, "the next call picks up where it left off");
        assert!(
            first.iter().all(|u| !second.contains(u)),
            "and does not re-offer what was already handed out",
        );
        let rest = s.image_urls_to_fetch("general", 99);
        assert_eq!(rest.len(), 4, "every remaining picture is still reachable");
    }

    #[test]
    fn a_gif_or_song_is_never_offered_for_inline_fetching() {
        // Only still images are painted. A GIF would be shown as one frozen
        // frame, and a song has no picture at all, so both keep their chips and
        // must not cost a download.
        let mut m = message("m1", "trinity", "", 1);
        m.extras.gif_url = Some("https://cdn.example/a.gif".into());
        m.extras.audio_attachment = Some(AudioAttachment {
            src: "https://youtu.be/x".into(),
            ..Default::default()
        });
        let s = room_knowing_us(vec![m]);
        assert!(s.image_urls_to_fetch("general", 8).is_empty());
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
    fn typing_a_word_never_deletes_a_message() {
        // The bug an adversarial review found, and the reason `up` entering
        // select mode needed more than the gesture: select mode reads letters as
        // commands, so after a stray `up` the word "already" arms delete on its
        // `d` and confirms on its `y`. Typing must fall back to typing.
        let mut s = selecting(vec![message("m1", "neo", "mine", 1)]);
        s.set_viewer_user_id("uid-neo".into());
        if let CircMode::Room { messages, .. } = &mut s.mode {
            messages.items[0].user_id = "uid-neo".into();
        }
        for c in "already".chars() {
            let intent = s.handle_key(key(KeyCode::Char(c)));
            assert!(
                !matches!(intent, CircIntent::DeleteMessage { .. }),
                "typing {c:?} deleted a message",
            );
        }
        assert!(!s.menu_is_open(), "the first letter dropped back to typing");
        assert_eq!(s.draft.text, "already", "and every letter was typed");
    }

    #[test]
    fn the_action_menu_still_navigates_with_j_and_k() {
        // The other side of that fix: `j`/`k` are navigation here, and must not
        // be mistaken for the reader starting to type.
        let mut s = selecting(vec![
            message("m1", "trinity", "one", 1),
            message("m2", "neo", "two", 2),
        ]);
        s.handle_key(key(KeyCode::Char('k')));
        assert!(s.menu_is_open(), "k navigates rather than exiting");
        assert!(s.draft.text.is_empty(), "and types nothing");
    }

    #[test]
    fn a_mouse_wheel_notch_never_changes_mode() {
        // `wheel_scroll` exists because a synthesised `Up` would put a reader
        // who was only scrolling into a mode that reads their next word as
        // commands.
        let mut s = room_knowing_us(vec![
            message("m1", "trinity", "one", 1),
            message("m2", "neo", "two", 2),
        ]);
        assert!(s.wheel_scroll(true), "a room handles the notch");
        assert!(!s.menu_is_open(), "scrolling is not entering a mode");
        assert!(s.wheel_scroll(false));
        assert!(!s.menu_is_open());
    }

    #[test]
    fn ctrl_a_opens_the_action_menu_and_esc_closes_it() {
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        assert!(s.menu_is_open(), "ctrl+a opens the menu");
        assert!(
            s.is_text_input(),
            "a room always captures text: the composer is never handed away",
        );
        // Navigation keys move the cursor inside the menu without typing.
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.draft.text, "", "j moves, it does not type");

        // Esc closes the menu before it leaves the room.
        assert_eq!(s.handle_escape(), Some(CircIntent::None));
        assert!(!s.menu_is_open());
        assert!(matches!(s.mode, CircMode::Room { .. }));
        assert_eq!(s.handle_escape(), Some(CircIntent::BackToRooms));
    }

    #[test]
    fn any_ordinary_letter_closes_the_menu_and_types() {
        // The whole point of dropping the mode: an active composer in which
        // some letters typed, some navigated and some acted was a rule nobody
        // could hold. Exactly one key is special now.
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        for c in "hello".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert!(!s.menu_is_open());
        assert_eq!(s.draft.text, "hello");
    }

    #[test]
    fn select_mode_y_copies_the_message_text() {
        let mut s = selecting(vec![message("m1", "trinity", "worth keeping", 1)]);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('y'))),
            CircIntent::CopyText("worth keeping".into()),
        );
    }

    #[test]
    fn copying_an_attachment_only_message_copies_its_url() {
        // A `/gif` posted with no caption has no text, and copying an empty
        // string would look like the key did nothing.
        let mut m = message("m1", "trinity", "", 1);
        m.extras.gif_url = Some("https://cdn.example/a.gif".into());
        let mut s = selecting(vec![m]);
        assert!(matches!(
            s.handle_key(key(KeyCode::Char('y'))),
            CircIntent::CopyText(t) if t.contains("gif")
        ));
    }

    #[test]
    fn an_armed_delete_takes_y_before_copy_does() {
        // `y` means both "confirm" and "copy". The confirm branch is checked
        // first, so an armed delete is never turned into a copy, which would
        // leave the reader thinking they had deleted something.
        let mut s = selecting(vec![message("m1", "neo", "oops", 1_000)]);
        s.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(
            s.handle_key(key(KeyCode::Char('y'))),
            CircIntent::DeleteMessage { .. }
        ));
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
        // The arming is gone, so a bare `y` must not delete. It copies instead,
        // which is why this checks the variant rather than `None`.
        assert!(!matches!(
            s.handle_key(key(KeyCode::Char('y'))),
            CircIntent::DeleteMessage { .. }
        ));
    }

    #[test]
    fn select_mode_will_not_delete_someone_elses_message() {
        let mut s = selecting(vec![message("m1", "trinity", "hi", 1_000)]);
        s.set_viewer_user_id("uid-neo".into());
        assert_eq!(s.handle_key(key(KeyCode::Char('d'))), CircIntent::None);
        // `y` is also the copy key, so the property under test is not "does
        // nothing" but "does not delete": arming never happened, so a stray
        // confirm must not reach the endpoint.
        assert!(!matches!(
            s.handle_key(key(KeyCode::Char('y'))),
            CircIntent::DeleteMessage { .. }
        ));
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
        assert!(s.menu_is_open(), "the prompt closes, the mode stays");
    }

    #[test]
    fn select_mode_p_opens_the_author_profile_and_c_opens_a_dm() {
        let mut s = selecting(vec![message("m1", "trinity", "hi", 1)]);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('p'))),
            CircIntent::OpenProfile {
                username: "trinity".into()
            },
        );
        assert!(matches!(
            s.handle_key(key(KeyCode::Char('c'))),
            CircIntent::OpenDm { username, .. } if username == "trinity"
        ));
    }

    #[test]
    fn select_mode_offers_your_own_profile_but_not_a_dm_to_yourself() {
        let mut s = selecting(vec![message("m1", "neo", "mine", 1)]);
        s.set_viewer_user_id("uid-neo".into());
        if let CircMode::Room { messages, .. } = &mut s.mode {
            messages.items[0].user_id = "uid-neo".into();
        }
        assert!(matches!(
            s.handle_key(key(KeyCode::Char('p'))),
            CircIntent::OpenProfile { .. }
        ));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('c'))),
            CircIntent::None,
            "a conversation with yourself is not a thing",
        );
    }

    #[test]
    fn select_mode_pages_with_pgup_and_pgdown() {
        let msgs: Vec<CircMessage> = (0..30)
            .map(|i| message(&format!("m{i}"), "trinity", "hi", i))
            .collect();
        let mut s = selecting(msgs);
        // `selecting` leaves the cursor on the newest message.
        let CircMode::Room { messages, .. } = &s.mode else {
            unreachable!()
        };
        let start = messages.selected;
        s.handle_key(key(KeyCode::PageUp));
        let CircMode::Room { messages, .. } = &s.mode else {
            unreachable!()
        };
        let after_up = messages.selected;
        assert!(
            after_up < start,
            "PageUp moves the selection; it used to fall through to a nav \
             helper with no PageUp arm and do nothing",
        );
        s.handle_key(key(KeyCode::PageDown));
        let CircMode::Room { messages, .. } = &s.mode else {
            unreachable!()
        };
        assert!(messages.selected > after_up, "and PageDown comes back");
    }

    #[test]
    fn a_notice_offers_none_of_the_per_author_keys() {
        let mut s = open("general");
        s.apply_messages("general", true, Ok((vec![], None)));
        s.append_notice("general", "unknown command: /nope");
        s.handle_key(ctrl(KeyCode::Char('a')));
        for k in ['p', 'c'] {
            assert_eq!(
                s.handle_key(key(KeyCode::Char(k))),
                CircIntent::None,
                "{k} needs an author, and a notice has none",
            );
        }
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
    fn a_url_typed_in_a_message_can_finally_be_opened() {
        // Before this it was reachable by no path at all: `open_action` read
        // attachments only, and OSC 8 linkifying is applied to chips rather
        // than to body text.
        let mut s = selecting(vec![message(
            "m1",
            "trinity",
            "have a look at https://example.com/x please",
            1,
        )]);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            CircIntent::OpenUrl("https://example.com/x".into()),
        );
    }

    #[test]
    fn an_attachment_still_wins_over_a_url_in_the_text() {
        let mut m = message("m1", "trinity", "see https://example.com/x", 1);
        m.extras.image_url = Some("https://cdn.example/pic.png".into());
        let mut s = selecting(vec![m]);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            CircIntent::OpenUrl("https://cdn.example/pic.png".into()),
            "the attachment is what the message is for; a link in the prose is \
             an aside",
        );
    }

    #[test]
    fn select_mode_o_does_nothing_without_an_attachment() {
        let mut s = selecting(vec![message("m1", "neo", "just words", 1_000)]);
        assert_eq!(s.handle_key(key(KeyCode::Char('o'))), CircIntent::None);
    }

    #[test]
    fn the_animation_clock_is_off_unless_it_is_earning_its_keep() {
        // The battery guard, and the reason the default is off. `wants_animation`
        // is what gates the clock, so it has to be false in every case where
        // running one would be redrawing for nothing.
        let mut plain = message("m1", "trinity", "hi", 1);
        plain.extras.style = Some(MessageStyle::One("rainbow".into()));
        let s = room_knowing_us(vec![plain]);
        assert!(
            !s.wants_animation(),
            "a style with nothing to animate does not start a clock",
        );

        let s = room_knowing_us(vec![]);
        assert!(!s.wants_animation(), "nor does an empty room");
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
    fn v_reveals_a_substituted_message_too() {
        // A flipped sentence is genuinely hard to read, and `v` already means
        // "show me what this really says". Asserted on the flipped glyphs
        // rather than on the plain text, because the select footer deliberately
        // shows the original either way: a substitution is decoration, not a
        // secret, so unlike a spoiler it need not be hidden there.
        let mut m = message("m1", "trinity", "hello there", 1);
        m.extras.style = Some(MessageStyle::One("flip".into()));
        let mut s = selecting(vec![m]);
        let flipped = "\u{1dd}\u{279}\u{1dd}\u{265}\u{287}";
        assert!(
            render_rows(&s, 14).join("\n").contains(flipped),
            "precondition: the body starts flipped",
        );
        s.handle_key(key(KeyCode::Char('v')));
        let after = render_rows(&s, 14).join("\n");
        assert!(!after.contains(flipped), "v shows the original:\n{after}");
        assert!(after.contains("hello there"));
        s.handle_key(key(KeyCode::Char('v')));
        assert!(
            render_rows(&s, 14).join("\n").contains(flipped),
            "and toggles back",
        );
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
        s.handle_key(ctrl(KeyCode::Char('a')));
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

    /// A room where presence has told us our own handle is `neo`.
    fn room_knowing_us(msgs: Vec<CircMessage>) -> CircScreen {
        let mut s = open("general");
        s.set_viewer_user_id("uid-neo".into());
        let now = now_ms();
        s.apply_room_users(
            "general",
            Ok(vec![CircRoomUser {
                user_id: "uid-neo".into(),
                username: "neo".into(),
                is_chat_admin: false,
                last_seen: now,
                last_activity: Some(now),
            }]),
        );
        s.apply_messages("general", true, Ok((msgs, None)));
        s
    }

    /// A room with three people present and one who has spoken and left.
    fn room_with_people() -> CircScreen {
        let mut s = open("general");
        let now = now_ms();
        let user = |id: &str, name: &str| CircRoomUser {
            user_id: id.into(),
            username: name.into(),
            is_chat_admin: false,
            last_seen: now,
            last_activity: Some(now),
        };
        s.apply_room_users(
            "general",
            Ok(vec![
                user("u1", "trinity"),
                user("u2", "trace"),
                user("u3", "neo"),
            ]),
        );
        s.apply_messages(
            "general",
            true,
            Ok((vec![message("m1", "morpheus", "wake up", 1)], None)),
        );
        s
    }

    #[test]
    fn tab_previews_a_mention_without_touching_the_draft() {
        let mut s = room_with_people();
        typed(&mut s, "@tr");
        let before = s.draft.text.clone();
        s.handle_key(key(KeyCode::Tab));
        assert_eq!(
            s.draft.text, before,
            "nothing is inserted without Space: the preview is a preview",
        );
        let shown = composer_text(&s, 60, 14);
        assert!(
            shown.contains("@trace") || shown.contains("@trinity"),
            "the completion is previewed on screen:\n{shown}",
        );
    }

    #[test]
    fn tab_cycles_and_wraps_through_the_matches() {
        let mut s = room_with_people();
        typed(&mut s, "@tr");
        // Two matches, trace and trinity, in roster order.
        let first = s.mention_offer().expect("a match").1;
        s.handle_key(key(KeyCode::Tab));
        let second = s.mention_offer().expect("a match").1;
        assert_ne!(
            first, second,
            "the first Tab must visibly move: index 0 is already showing",
        );
        s.handle_key(key(KeyCode::Tab));
        assert_eq!(
            s.mention_offer().expect("a match").1,
            first,
            "cycling wraps rather than running out",
        );
    }

    #[test]
    fn space_commits_exactly_what_was_previewed() {
        let mut s = room_with_people();
        typed(&mut s, "@tr");
        let previewed = s.mention_offer().expect("a match").1;
        s.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(s.draft.text, format!("@{previewed} "));
        assert_eq!(
            s.draft.cursor,
            s.draft.len(),
            "the caret follows the inserted name and its space",
        );
    }

    #[test]
    fn any_other_key_clears_the_preview_and_leaves_the_draft_alone() {
        let mut s = room_with_people();
        typed(&mut s, "@tr");
        s.handle_key(key(KeyCode::Tab));
        assert!(s.mention_cycle.is_some());
        s.handle_key(key(KeyCode::Char('a')));
        assert!(s.mention_cycle.is_none(), "the cycle ends");
        assert_eq!(
            s.draft.text, "@tra",
            "and exactly what was typed survives, with nothing spliced in",
        );
    }

    #[test]
    fn someone_who_spoke_and_left_is_still_completable() {
        // The reason the pool is not just the roster: they are who you want to
        // reply to, and they are gone from the online list.
        let mut s = room_with_people();
        typed(&mut s, "@mor");
        assert_eq!(
            s.mention_offer().map(|(_, c, _)| c),
            Some("morpheus".to_string()),
        );
    }

    #[test]
    fn an_email_address_never_starts_a_completion() {
        let mut s = room_with_people();
        typed(&mut s, "mail me at bob@tr");
        assert!(
            s.mention_offer().is_none(),
            "`bob@tr` is an address, not a mention of trace",
        );
        s.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(
            s.draft.text, "mail me at bob@tr ",
            "and Space just types a space",
        );
    }

    #[test]
    fn a_bare_at_offers_the_room() {
        let mut s = room_with_people();
        typed(&mut s, "@");
        assert!(
            s.mention_offer().is_some(),
            "@ plus Tab is how you browse who is here",
        );
    }

    #[test]
    fn a_fully_typed_name_draws_no_ghost() {
        let mut s = room_with_people();
        typed(&mut s, "@neo");
        assert!(
            s.mention_offer().is_none(),
            "there is nothing left to preview, so an empty ghost would just \
             park the caret oddly",
        );
    }

    #[test]
    fn a_message_that_names_you_is_marked_in_the_gutter() {
        let s = room_knowing_us(vec![
            message("m1", "trinity", "hey @neo, you around?", 1),
            message("m2", "dozer", "nothing to see here", 2),
        ]);
        let rows = render_rows(&s, 14);
        let joined = rows.join("\n");
        assert!(
            joined.contains(&format!("{MENTION_MARK}trinity")),
            "the message naming us carries the marker:\n{joined}",
        );
        assert!(
            !joined.contains(&format!("{MENTION_MARK}dozer")),
            "a message that names nobody is left alone:\n{joined}",
        );
        assert!(
            rows.iter().any(|r| r.contains("hey @neo, you around?")),
            "the text itself is untouched:\n{joined}",
        );
    }

    #[test]
    fn a_substituted_style_rewrites_the_body_but_keeps_the_gutter_mark() {
        // The documented trade in `ui::styles`: substituting the text moves the
        // bytes `mention_ranges` works over, so the in-body highlight is lost,
        // but `chat::mentions` reads the raw `content` and so the row-level
        // marker survives. This test is what makes that trade visible, and is
        // the one to revisit if the mention-offset question is ever reopened.
        let mut m = message("m1", "trinity", "hey @neo, you around?", 1);
        m.extras.style = Some(MessageStyle::One("l33t".into()));
        let s = room_knowing_us(vec![m]);
        let rows = render_rows(&s, 14);
        let joined = rows.join("\n");
        assert!(
            rows.iter().any(|r| r.contains("h3y @n30")),
            "the body is leeted, not passed through untouched:\n{joined}",
        );
        assert!(
            !joined.contains("hey @neo"),
            "the raw text is not also drawn:\n{joined}",
        );
        assert!(
            joined.contains(&format!("{MENTION_MARK}trinity")),
            "the gutter still marks a message that names us, since it reads \
             the raw content:\n{joined}",
        );
    }

    #[test]
    fn a_flipped_message_reverses_the_whole_body_before_it_wraps() {
        // Reversing after the wrap would reverse each row on its own, which
        // scrambles the reading order between rows. Checked here rather than in
        // `ui::styles` because only the render path exercises the ordering.
        let mut m = message("m1", "trinity", "sox", 1);
        m.extras.style = Some(MessageStyle::One("flip".into()));
        let s = room_knowing_us(vec![m]);
        let rows = render_rows(&s, 14);
        let joined = rows.join("\n");
        assert!(
            rows.iter().any(|r| r.contains("xos")),
            "the body is reversed:\n{joined}",
        );
    }

    #[test]
    fn nothing_is_marked_until_presence_tells_us_our_own_handle() {
        // The id token gives the account id, never the handle, so until the
        // roster names us there is nothing to match messages against.
        let mut s = open("general");
        s.set_viewer_user_id("uid-neo".into());
        s.apply_messages(
            "general",
            true,
            Ok((vec![message("m1", "trinity", "hey @neo", 1)], None)),
        );
        let joined = render_rows(&s, 10).join("\n");
        assert!(
            !joined.contains(&format!("{MENTION_MARK}trinity")),
            "no handle, no marker:\n{joined}",
        );
    }

    #[test]
    fn an_invisible_reader_learns_their_handle_from_their_own_messages() {
        // With circ_presence off we are in nobody's user list, so the roster
        // can't name us. Anything we have said in the room can.
        let mut s = open("general");
        s.set_viewer_user_id("uid-neo".into());
        s.apply_messages(
            "general",
            true,
            Ok((
                vec![
                    message("m1", "neo", "anyone about?", 1),
                    message("m2", "trinity", "hey @neo", 2),
                ],
                None,
            )),
        );
        let joined = render_rows(&s, 12).join("\n");
        assert!(
            joined.contains(&format!("{MENTION_MARK}trinity")),
            "our own message named us:\n{joined}",
        );
    }

    #[test]
    fn the_roster_fits_a_max_length_name_with_both_marks() {
        // The bug this replaced: at 20 columns `Paragraph` truncated the row,
        // so a long name lost the admin and idle marks off the right edge, and
        // those marks are the whole reason the row is not just a name.
        let mut s = open("general");
        let now = now_ms();
        s.roster_open = true;
        s.apply_room_users(
            "general",
            Ok(vec![CircRoomUser {
                user_id: "u1".into(),
                // 20 characters, the API's documented maximum.
                username: "abcdefghijklmnopqrst".into(),
                is_chat_admin: true,
                last_seen: now,
                // Long enough ago to read as idle.
                last_activity: Some(now - 3_600_000),
            }]),
        );
        let joined = render_rows_wide(&s, 100, 14).join("\n");
        assert!(
            joined.contains("abcdefghijklmnopqrst"),
            "the whole name fits:\n{joined}",
        );
        let row = joined
            .lines()
            .find(|l| l.contains("abcdefghijklmnopqrst"))
            .expect("the row is on screen");
        assert!(
            row.contains('\u{2605}') && row.contains(IDLE_MARK),
            "and so do both marks, which is what 20 columns clipped: {row:?}",
        );
    }

    #[test]
    fn the_roster_lists_admins_before_everyone_else() {
        let mut s = open("general");
        let now = now_ms();
        s.roster_open = true;
        let user = |id: &str, name: &str, admin: bool| CircRoomUser {
            user_id: id.into(),
            username: name.into(),
            is_chat_admin: admin,
            last_seen: now,
            last_activity: Some(now),
        };
        s.apply_room_users(
            "general",
            Ok(vec![
                user("u1", "alice", false),
                user("u2", "zara", true),
                user("u3", "bob", false),
                user("u4", "adam", true),
            ]),
        );
        let rows = render_rows_wide(&s, 100, 14);
        let order: Vec<&str> = ["adam", "zara", "alice", "bob"]
            .into_iter()
            .filter(|n| rows.iter().any(|r| r.contains(n)))
            .collect();
        let pos = |name: &str| {
            rows.iter()
                .position(|r| r.contains(name))
                .unwrap_or(usize::MAX)
        };
        assert_eq!(order.len(), 4, "everyone is listed");
        assert!(
            pos("adam") < pos("zara") && pos("zara") < pos("alice"),
            "admins first (alphabetical), then everyone else (alphabetical): {rows:?}",
        );
        assert!(pos("alice") < pos("bob"));
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
    fn editor_content_closes_the_action_menu() {
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        assert!(s.menu_is_open());
        s.set_draft_and_focus("back to typing".to_string());
        assert!(s.is_text_input());
        assert!(!s.menu_is_open());
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
        assert_eq!(
            s.draft.text, "",
            "the paste must not leak into the composer"
        );
    }

    #[test]
    fn paste_is_dropped_in_select_mode() {
        let mut s = selecting(vec![message("m1", "neo", "hi", 1_000)]);
        s.paste_text("nope");
        assert_eq!(s.draft.text, "");
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

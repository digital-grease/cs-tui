//! cIRC REST endpoints (`/v1/circ`, API v0.8.4): multi-user chat rooms.
//!
//! Structurally the same as [C-Mail](crate::cmail): sending goes through REST
//! (sanitised, rate-limited, identity set server-side); live messages come from
//! subscribing to `chat_messages/<roomId>` in Realtime Database with the
//! `idToken`. A room is addressed by its `roomId` (its slug, e.g. `general`).
//!
//! v0.8.4 adds four things on top of that:
//!
//! - messages carry the optional attachment/style/command extras shared with
//!   C-Mail ([`MessageExtras`], § Message fields),
//! - you can delete and flag messages (§ Delete Your Message, § Flag a Message),
//! - a room has a live user list, read over REST from
//!   `GET /v1/circ/:roomId/users` and published with
//!   `POST /v1/circ/:roomId/presence` (§ Who's in a room, § Announce Your
//!   Presence),
//! - the same user list can be streamed from a second RTDB subscription on
//!   `chat_presence/<roomId>` (§ Reading a room in real time).
//!
//! Deleting also makes RTDB `patch` events load-bearing. A deletion *changes* a
//! message you already hold rather than adding one, and arrives as a partial
//! object (`{ "content": "[DELETED]", "deleted": true }`) on that message's
//! path. A partial object is not a message, so the stream decodes to
//! [`CircMessageUpdate`] (a whole message *or* a targeted patch) rather than
//! straight to [`CircMessage`].
use std::time::Duration;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::message::{AudioAttachment, MessageExtras, MessageStyle};
use crate::rtdb::SseEventKind;
use crate::types::{null_as_default, validate_flag_reason, FlagBody, FlagResponse};

const DEFAULT_MESSAGE_LIMIT: u32 = 50;
const MAX_MESSAGE_LIMIT: u32 = 100;
const MAX_MESSAGE_LEN: usize = 2_048;

/// Documented presence cadence (§ Announce Your Presence), used only as a
/// fallback when a response omits the value or sends a non-positive one. Read
/// the real cadence off [`CircPresenceResponse`]; never hard-code these.
const DEFAULT_HEARTBEAT_MS: i64 = 30_000;
const DEFAULT_STALE_AFTER_MS: i64 = 180_000;
const DEFAULT_IDLE_AFTER_MS: i64 = 600_000;

/// A cIRC chat room summary (`GET /v1/circ`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircRoom {
    #[serde(alias = "roomId", default)]
    pub id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    /// Milliseconds since Unix epoch.
    #[serde(default)]
    pub last_message_at: Option<i64>,
    #[serde(default)]
    pub sort_order: i64,
    /// How many people are in the room right now (v0.8.4, § List Rooms), i.e.
    /// how many entries [`Client::list_circ_room_users`] would return. Defaults
    /// to 0 when the server omits it.
    #[serde(default)]
    pub online_count: u32,
}

impl CircRoom {
    /// The identifier used to address the room in URLs (`:roomId`), which the
    /// spec defines as the slug. Falls back to `id` if the slug is absent.
    #[must_use]
    pub fn room_id(&self) -> &str {
        if self.slug.is_empty() {
            &self.id
        } else {
            &self.slug
        }
    }
}

/// A cIRC message as returned by history responses and RTDB events.
///
/// `content` may be empty: an attached image, GIF or song can be the whole
/// message. Use [`MessageExtras::display_content`] rather than printing
/// `content` blindly.
///
/// Every field tolerates an explicit JSON `null`, which decodes to the same
/// value an absent key does. The API does send nulls (§ Who's in a room
/// documents `lastActivity` as "ms epoch, or `null`"), and a page of history
/// decodes as one `Vec`, so without this a single null would sink every message
/// on the page rather than the one that carried it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircMessage {
    #[serde(alias = "messageId", default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(
        alias = "senderId",
        alias = "senderUid",
        default,
        deserialize_with = "null_as_default"
    )]
    pub user_id: String,
    #[serde(
        alias = "senderUsername",
        default,
        deserialize_with = "null_as_default"
    )]
    pub username: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_chat_admin: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    pub content: String,
    /// Milliseconds since Unix epoch.
    #[serde(default, deserialize_with = "null_as_default")]
    pub timestamp: i64,
    /// The optional attachment, style and command fields (v0.8.4, § Message
    /// fields), including the `deleted` tombstone flag. Flattened, so they sit
    /// at the top level of the wire object exactly as the server sends them.
    #[serde(flatten)]
    pub extras: MessageExtras,
}

/// A *partial* cIRC message: the payload of an RTDB `patch` event.
///
/// Every field is optional, because a patch carries only what changed and
/// anything absent has to be left alone. Deleting a message arrives this way,
/// as `{ "content": "[DELETED]", "deleted": true }` on the message's own path
/// (§ Reading a room in real time).
///
/// A JSON `null` decodes the same as an absent key, i.e. as "no change". RTDB
/// spells a removed child as `null` inside a patch, but the one documented
/// patch that removes fields is the deletion, which
/// [`apply_to`](CircMessagePatch::apply_to) handles through the `deleted` flag
/// instead.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircMessagePatch {
    /// Only ever informational: which message a patch belongs to comes from the
    /// event path, and travels on [`CircMessageUpdate::Partial`].
    #[serde(alias = "messageId", default)]
    pub id: Option<String>,
    /// New sender id, when the server restated it.
    #[serde(alias = "senderId", alias = "senderUid", default)]
    pub user_id: Option<String>,
    /// New sender handle, when the server restated it.
    #[serde(alias = "senderUsername", default)]
    pub username: Option<String>,
    /// New room-admin flag for the sender.
    #[serde(default)]
    pub is_chat_admin: Option<bool>,
    /// New body text, which an edit or a soft delete rewrites.
    #[serde(default)]
    pub content: Option<String>,
    /// Milliseconds since Unix epoch.
    #[serde(default)]
    pub timestamp: Option<i64>,

    // The § Message fields extras, one Option per field of [`MessageExtras`].
    // They are spelled out rather than reused wholesale because that type
    // cannot say "this key was absent" for its boolean flags, and a patch must
    // never turn an absent flag into `false`.
    /// New still-image URL.
    #[serde(default)]
    pub image_url: Option<String>,
    /// New animated-image URL.
    #[serde(default)]
    pub gif_url: Option<String>,
    /// New jukebox track.
    #[serde(default)]
    pub audio_attachment: Option<AudioAttachment>,
    /// New display styles.
    #[serde(default)]
    pub style: Option<MessageStyle>,
    /// New "/me" emote flag.
    #[serde(default)]
    pub is_action: Option<bool>,
    /// New dice-roll flag.
    #[serde(default)]
    pub is_dice: Option<bool>,
    /// New magic-eightball flag.
    #[serde(default)]
    pub is_eightball: Option<bool>,
    /// New eightball answer text.
    #[serde(default)]
    pub eightball_answer: Option<String>,
    /// New fortune-cookie flag.
    #[serde(default)]
    pub is_fortune: Option<bool>,
    /// New fortune text.
    #[serde(default)]
    pub fortune_text: Option<String>,
    /// New soft-delete flag. Set by a delete, which arrives as a patch.
    #[serde(default)]
    pub deleted: Option<bool>,
}

impl CircMessagePatch {
    /// Whether the patch changes nothing, which is what a payload of `null`s or
    /// of fields we don't model decodes to. Empty patches are dropped by the
    /// stream decoder rather than handed to the UI.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Merge this patch into `message`, leaving every field the patch does not
    /// mention exactly as it was.
    ///
    /// The id is deliberately not merged: it says *which* message the patch
    /// belongs to and travels alongside it on [`CircMessageUpdate::Partial`].
    ///
    /// A patch that sets `deleted` clears the extras instead of merging them,
    /// because the server strips every attachment, style and command result
    /// when it deletes (§ Delete Your Message). That stops a tombstone
    /// rendering with the picture the message used to carry, whether or not the
    /// server spells the removals out as nulls.
    pub fn apply_to(&self, message: &mut CircMessage) {
        if let Some(user_id) = &self.user_id {
            message.user_id.clone_from(user_id);
        }
        if let Some(username) = &self.username {
            message.username.clone_from(username);
        }
        if let Some(is_chat_admin) = self.is_chat_admin {
            message.is_chat_admin = is_chat_admin;
        }
        if let Some(content) = &self.content {
            message.content.clone_from(content);
        }
        if let Some(timestamp) = self.timestamp {
            message.timestamp = timestamp;
        }

        if self.deleted == Some(true) {
            message.extras = MessageExtras {
                deleted: true,
                ..MessageExtras::default()
            };
            return;
        }

        let extras = &mut message.extras;
        if let Some(image_url) = &self.image_url {
            extras.image_url = Some(image_url.clone());
        }
        if let Some(gif_url) = &self.gif_url {
            extras.gif_url = Some(gif_url.clone());
        }
        if let Some(audio_attachment) = &self.audio_attachment {
            extras.audio_attachment = Some(audio_attachment.clone());
        }
        if let Some(style) = &self.style {
            extras.style = Some(style.clone());
        }
        if let Some(is_action) = self.is_action {
            extras.is_action = is_action;
        }
        if let Some(is_dice) = self.is_dice {
            extras.is_dice = is_dice;
        }
        if let Some(is_eightball) = self.is_eightball {
            extras.is_eightball = is_eightball;
        }
        if let Some(eightball_answer) = &self.eightball_answer {
            extras.eightball_answer = Some(eightball_answer.clone());
        }
        if let Some(is_fortune) = self.is_fortune {
            extras.is_fortune = is_fortune;
        }
        if let Some(fortune_text) = &self.fortune_text {
            extras.fortune_text = Some(fortune_text.clone());
        }
        if let Some(deleted) = self.deleted {
            extras.deleted = deleted;
        }
    }
}

/// One live change to a room's messages, decoded from a single RTDB event.
///
/// The two shapes are not interchangeable, which is the whole point: a `patch`
/// payload is a fragment, and inserting it as if it were a message would show a
/// nameless line stamped 1970 instead of the deletion it actually is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircMessageUpdate {
    /// A whole message. Append it, or replace the one you already hold with the
    /// same id.
    Full(CircMessage),
    /// A change to the message with this id. Merge it into the copy you hold
    /// with [`CircMessagePatch::apply_to`]. If you don't hold that message,
    /// ignore it (or reload the room); never insert it as a new one.
    Partial {
        /// Id of the message the patch targets.
        id: String,
        /// The changed fields, to merge into the message you hold.
        patch: CircMessagePatch,
    },
}

impl CircMessageUpdate {
    /// The id of the message this update is about.
    #[must_use]
    pub fn message_id(&self) -> &str {
        match self {
            Self::Full(message) => &message.id,
            Self::Partial { id, .. } => id,
        }
    }

    /// The whole message, when this update carries one.
    #[must_use]
    pub fn as_full(&self) -> Option<&CircMessage> {
        match self {
            Self::Full(message) => Some(message),
            Self::Partial { .. } => None,
        }
    }
}

/// Response from `POST /v1/circ/:roomId`.
///
/// A normal send returns `{ roomId, messageId }`; a command that the server
/// answers inline (e.g. `/help`) returns `{ reply }` and posts nothing, so all
/// fields are optional and either shape decodes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircSendResponse {
    #[serde(default)]
    pub room_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    /// Inline command reply (e.g. from `/help`); the message was not posted.
    #[serde(default)]
    pub reply: Option<String>,
}

/// Response from `DELETE /v1/circ/:roomId/messages/:messageId` (v0.8.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircDeleteResponse {
    /// Room the deleted message was in.
    #[serde(default)]
    pub room_id: String,
    /// Id of the message that was tombstoned.
    #[serde(default)]
    pub message_id: String,
    /// Always `true` on success: the message is now a tombstone.
    #[serde(default)]
    pub deleted: bool,
}

/// One person in a room, from `GET /v1/circ/:roomId/users` (v0.8.4, § Who's in
/// a room).
///
/// Presence is heartbeat-based, so this list is whoever is announcing
/// themselves right now; someone who crashes or force-quits drops off on their
/// own.
///
/// As on [`CircMessage`], an explicit JSON `null` decodes to the same value an
/// absent key does, so one null entry cannot take the whole user list down with
/// it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircRoomUser {
    /// Their user id.
    #[serde(alias = "id", default, deserialize_with = "null_as_default")]
    pub user_id: String,
    /// Their handle.
    #[serde(default, deserialize_with = "null_as_default")]
    pub username: String,
    /// Whether they can moderate this room.
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_chat_admin: bool,
    /// Milliseconds since Unix epoch: the last heartbeat we know about.
    #[serde(default, deserialize_with = "null_as_default")]
    pub last_seen: i64,
    /// Milliseconds since Unix epoch, or `None` when their client doesn't
    /// report activity at all. `None` is not "idle forever", it means "treat
    /// them as active".
    #[serde(default)]
    pub last_activity: Option<i64>,
}

impl CircRoomUser {
    /// The spec's idle rule (§ Who's in a room): idle once `lastActivity` is
    /// older than `idleAfterMs`, and never idle when the client doesn't report
    /// activity at all.
    ///
    /// Both arguments are milliseconds: `now_ms` on your own clock, and
    /// `idle_after_ms` read off [`CircPresenceResponse`]. Re-evaluate on a
    /// timer, since going idle produces no update of its own.
    #[must_use]
    pub fn is_idle(&self, now_ms: i64, idle_after_ms: i64) -> bool {
        is_idle_at(self.last_activity, now_ms, idle_after_ms)
    }
}

/// Response from `POST /v1/circ/:roomId/presence` (v0.8.4, § Announce Your
/// Presence).
///
/// The three `*_ms` fields are the room's presence cadence, and the spec is
/// explicit that they must be read off this response rather than hard-coded:
/// heartbeat every `heartbeat_ms`, you drop out of the room once
/// `stale_after_ms` passes with no heartbeat, and you show as idle once
/// `idle_after_ms` passes with no `lastActivity` update. They are the same
/// thresholds the user list and the presence stream are filtered by, so keep
/// the last response around rather than only the timer it started.
///
/// A field the server omits decodes to the documented value rather than to 0,
/// so a heartbeat loop can never end up spinning at zero delay.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircPresenceResponse {
    /// Room the announcement was for.
    #[serde(default)]
    pub room_id: String,
    /// Whether the server recorded the heartbeat.
    #[serde(default)]
    pub ok: bool,
    /// How often to re-announce, in milliseconds.
    #[serde(default = "default_heartbeat_ms")]
    pub heartbeat_ms: i64,
    /// How long a heartbeat stays good for, in milliseconds.
    #[serde(default = "default_stale_after_ms")]
    pub stale_after_ms: i64,
    /// How long without activity counts as idle, in milliseconds.
    #[serde(default = "default_idle_after_ms")]
    pub idle_after_ms: i64,
}

impl Default for CircPresenceResponse {
    fn default() -> Self {
        Self {
            room_id: String::new(),
            ok: false,
            heartbeat_ms: DEFAULT_HEARTBEAT_MS,
            stale_after_ms: DEFAULT_STALE_AFTER_MS,
            idle_after_ms: DEFAULT_IDLE_AFTER_MS,
        }
    }
}

impl CircPresenceResponse {
    /// [`heartbeat_ms`](Self::heartbeat_ms) as a [`Duration`], ready to sleep
    /// on. A non-positive value falls back to the documented cadence so the
    /// heartbeat loop can't turn into a hot loop.
    #[must_use]
    pub fn heartbeat_interval(&self) -> Duration {
        duration_from_ms(self.heartbeat_ms, DEFAULT_HEARTBEAT_MS)
    }

    /// [`stale_after_ms`](Self::stale_after_ms) as a [`Duration`], with the same
    /// fallback.
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        duration_from_ms(self.stale_after_ms, DEFAULT_STALE_AFTER_MS)
    }

    /// [`idle_after_ms`](Self::idle_after_ms) as a [`Duration`], with the same
    /// fallback.
    #[must_use]
    pub fn idle_after(&self) -> Duration {
        duration_from_ms(self.idle_after_ms, DEFAULT_IDLE_AFTER_MS)
    }
}

/// One entry from the `chat_presence/<roomId>` RTDB stream (v0.8.4, § Reading a
/// room in real time).
///
/// Entries are keyed by user id and the value carries no id of its own, so
/// [`circ_presence_updates_from_rtdb_event`] injects the key into `user_id`.
///
/// As on [`CircMessage`], an explicit JSON `null` decodes to the same value an
/// absent key does. That matters most here: the stream hands a whole root map
/// to one decode, so a null on one person would otherwise drop everyone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircPresenceEntry {
    /// Their user id, injected from the RTDB key.
    #[serde(alias = "id", default, deserialize_with = "null_as_default")]
    pub user_id: String,
    /// Their handle.
    #[serde(default, deserialize_with = "null_as_default")]
    pub username: String,
    /// Whether they can moderate this room.
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_chat_admin: bool,
    /// Whether the entry says they're in the room. An entry can linger with
    /// `online: false`, which is why [`is_visible`](Self::is_visible) checks it.
    #[serde(default, deserialize_with = "null_as_default")]
    pub online: bool,
    /// Milliseconds since Unix epoch: their last heartbeat.
    #[serde(default, deserialize_with = "null_as_default")]
    pub last_seen: i64,
    /// Milliseconds since Unix epoch, or `None` when their client doesn't
    /// report activity. Absent means active, not idle.
    #[serde(default)]
    pub last_activity: Option<i64>,
}

impl CircPresenceEntry {
    /// The spec's visibility rule (§ Reading a room in real time): show the
    /// entry only if `online` is true **and** `lastSeen` is newer than
    /// `staleAfterMs`.
    ///
    /// Both arguments are milliseconds: `now_ms` on your own clock, and
    /// `stale_after_ms` read off [`CircPresenceResponse`]. Re-evaluate on a
    /// timer, not just on events, since an entry going stale produces no event.
    /// A `lastSeen` ahead of your clock (skew) counts as fresh.
    ///
    /// The comparison is strict, so an entry whose heartbeat is exactly
    /// `stale_after_ms` old is already gone: "newer than" is the spec's wording,
    /// and it is the same boundary [`crate::cmail::CmailPresence::is_typing_at`]
    /// applies to its own staleness window.
    #[must_use]
    pub fn is_visible(&self, now_ms: i64, stale_after_ms: i64) -> bool {
        self.online && now_ms.saturating_sub(self.last_seen) < stale_after_ms
    }

    /// The spec's idle rule, identical to [`CircRoomUser::is_idle`]: idle once
    /// `lastActivity` is older than `idleAfterMs`, never idle when it's absent.
    #[must_use]
    pub fn is_idle(&self, now_ms: i64, idle_after_ms: i64) -> bool {
        is_idle_at(self.last_activity, now_ms, idle_after_ms)
    }

    /// The same person in the shape `GET /v1/circ/:roomId/users` returns, so a
    /// live entry can be merged into the list fetched over REST. `online` has
    /// no counterpart there (that list only contains people who are), so check
    /// [`is_visible`](Self::is_visible) before converting.
    #[must_use]
    pub fn to_room_user(&self) -> CircRoomUser {
        CircRoomUser {
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            is_chat_admin: self.is_chat_admin,
            last_seen: self.last_seen,
            last_activity: self.last_activity,
        }
    }
}

/// A *partial* presence entry: the payload of an RTDB `patch` event on
/// `chat_presence/<roomId>`.
///
/// Same rule as [`CircMessagePatch`]: every field is optional and anything
/// absent must be left alone, so a heartbeat that only moves `lastSeen` can't
/// blank out the username next to it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircPresencePatch {
    /// New handle for the entry.
    #[serde(default)]
    pub username: Option<String>,
    /// New room-admin flag.
    #[serde(default)]
    pub is_chat_admin: Option<bool>,
    /// New in-the-room flag.
    #[serde(default)]
    pub online: Option<bool>,
    /// Milliseconds since Unix epoch.
    #[serde(default)]
    pub last_seen: Option<i64>,
    /// Milliseconds since Unix epoch. `None` here means "not in this patch",
    /// not "cleared".
    #[serde(default)]
    pub last_activity: Option<i64>,
}

impl CircPresencePatch {
    /// Whether the patch changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Merge this patch into `entry`, leaving every field it doesn't mention
    /// alone. The user id is not merged: it identifies the entry and travels on
    /// [`CircPresenceUpdate::Partial`].
    pub fn apply_to(&self, entry: &mut CircPresenceEntry) {
        if let Some(username) = &self.username {
            entry.username.clone_from(username);
        }
        if let Some(is_chat_admin) = self.is_chat_admin {
            entry.is_chat_admin = is_chat_admin;
        }
        if let Some(online) = self.online {
            entry.online = online;
        }
        if let Some(last_seen) = self.last_seen {
            entry.last_seen = last_seen;
        }
        if let Some(last_activity) = self.last_activity {
            entry.last_activity = Some(last_activity);
        }
    }
}

/// One live change to a room's user list, decoded from a single RTDB event on
/// `chat_presence/<roomId>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircPresenceUpdate {
    /// A whole entry. Insert it, or replace the one held for the same user id.
    Full(CircPresenceEntry),
    /// A change to the entry for this user id. Merge it with
    /// [`CircPresencePatch::apply_to`]; if you hold no entry for them, ignore
    /// it, since a fragment is not an entry.
    Partial {
        /// Who the patch is about.
        user_id: String,
        /// The changed fields, to merge into the entry you hold.
        patch: CircPresencePatch,
    },
    /// The entry for this user id is gone: they left the room
    /// (`DELETE /v1/circ/:roomId/presence`) or the server expired them. Drop
    /// them from the list.
    Removed {
        /// Who left.
        user_id: String,
    },
}

impl CircPresenceUpdate {
    /// The user id this update is about.
    #[must_use]
    pub fn user_id(&self) -> &str {
        match self {
            Self::Full(entry) => &entry.user_id,
            Self::Partial { user_id, .. } | Self::Removed { user_id } => user_id,
        }
    }

    /// The whole entry, when this update carries one.
    #[must_use]
    pub fn as_full(&self) -> Option<&CircPresenceEntry> {
        match self {
            Self::Full(entry) => Some(entry),
            Self::Partial { .. } | Self::Removed { .. } => None,
        }
    }
}

#[derive(Debug, Serialize)]
struct SendMessageBody<'a> {
    content: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PresenceBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    last_activity: Option<i64>,
}

impl Client {
    /// `GET /v1/circ`: list the rooms available to you.
    pub async fn list_circ_rooms(&self) -> Result<Vec<CircRoom>> {
        let value: Value = self
            .request(
                EndpointKey::CircList,
                Method::GET,
                "/v1/circ",
                &[],
                None::<&()>,
            )
            .await?;
        decode_room_list(value)
    }

    /// `GET /v1/circ/:roomId`: load message history, oldest first. Pass `before`
    /// (the previous cursor timestamp) to page older messages.
    pub async fn read_circ_room(
        &self,
        room_id: &str,
        before: Option<i64>,
        limit: Option<u32>,
    ) -> Result<(Vec<CircMessage>, Option<String>)> {
        let limit = limit
            .unwrap_or(DEFAULT_MESSAGE_LIMIT)
            .clamp(1, MAX_MESSAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        let path = format!("/v1/circ/{room_id}");
        let (messages, cursor): (Vec<CircMessage>, Option<String>) = self
            .request_page(EndpointKey::CircRead, Method::GET, &path, &query)
            .await?;
        let cursor = cursor.or_else(|| next_cursor_fallback(&messages, limit));
        Ok((messages, cursor))
    }

    /// `POST /v1/circ/:roomId`: send a message (or run a slash command).
    pub async fn send_circ_message(
        &self,
        room_id: &str,
        content: &str,
    ) -> Result<CircSendResponse> {
        validate_circ_content(content)?;
        let body = SendMessageBody { content };
        let path = format!("/v1/circ/{room_id}");
        self.request(EndpointKey::CircSend, Method::POST, &path, &[], Some(&body))
            .await
    }

    /// `DELETE /v1/circ/:roomId/messages/:messageId`: delete one of your own
    /// messages (v0.8.4, § Delete Your Message).
    ///
    /// A soft delete: the message stays in the room so the conversation around
    /// it still reads, but its `content` becomes `[DELETED]`, it comes back with
    /// `deleted: true`, and any image, GIF, song, style or command result is
    /// stripped. The author's name and the original timestamp stay.
    ///
    /// **It cannot be undone**, and there is no un-delete endpoint. The failure
    /// modes all arrive as [`ApiError::Api`]: `409 CONFLICT` when the message
    /// was already deleted, `403 FORBIDDEN` for someone else's message, and
    /// `404 NOT_FOUND` for an unknown `message_id`. Rate limit: 5/min, 30/day.
    ///
    /// Everyone reading the room live learns about this as a `patch` on that
    /// message's path rather than as a new message, which is why the stream is
    /// decoded with [`circ_message_updates_from_rtdb_event`].
    pub async fn delete_circ_message(
        &self,
        room_id: &str,
        message_id: &str,
    ) -> Result<CircDeleteResponse> {
        let path = format!("/v1/circ/{room_id}/messages/{message_id}");
        self.request::<CircDeleteResponse, ()>(
            EndpointKey::CircDeleteMessage,
            Method::DELETE,
            &path,
            &[],
            None,
        )
        .await
    }

    /// `POST /v1/circ/:roomId/messages/:messageId/flag`: report someone else's
    /// message for review (v0.8.4, § Flag a Message).
    ///
    /// `reason` is optional and capped at 500 characters; an over-long one is
    /// rejected locally as [`ApiError::Config`] rather than spending a request.
    /// The message's text and any attachment are recorded with the report, so it
    /// survives the message being deleted afterwards.
    ///
    /// Reporting is idempotent: a repeat succeeds with
    /// [`already_flagged`](FlagResponse::already_flagged) set and files nothing
    /// new, so branch on [`FlagResponse::is_new`] rather than on the status
    /// code. Reports from the website count too. You can't report your own
    /// message (`403`), an already-deleted message can still be reported, and
    /// there's no way to withdraw a report.
    ///
    /// The budget (5/min, 20/hour, 50/day) is shared with the entry and reply
    /// flag endpoints, hence the shared [`EndpointKey::Flag`].
    pub async fn flag_circ_message(
        &self,
        room_id: &str,
        message_id: &str,
        reason: Option<&str>,
    ) -> Result<FlagResponse> {
        validate_flag_reason(reason)?;
        let body = FlagBody { reason };
        let path = format!("/v1/circ/{room_id}/messages/{message_id}/flag");
        self.request(EndpointKey::Flag, Method::POST, &path, &[], Some(&body))
            .await
    }

    /// `POST /v1/circ/:roomId/read`: mark the room as viewed.
    pub async fn mark_circ_read(&self, room_id: &str) -> Result<()> {
        let path = format!("/v1/circ/{room_id}/read");
        self.request_unit(EndpointKey::CircMarkRead, Method::POST, &path, &[])
            .await
    }

    /// `GET /v1/circ/:roomId/users`: who's in the room right now, sorted by
    /// username (v0.8.4, § Who's in a room).
    ///
    /// The list is presence-derived, so it only holds people who are
    /// heartbeating. Filter it for idleness with [`CircRoomUser::is_idle`], and
    /// prefer the `chat_presence/<roomId>` stream
    /// ([`circ_presence_updates_from_rtdb_event`]) to polling this. Returns
    /// `403` if the room isn't available to you.
    ///
    /// The endpoint isn't paginated; the page envelope is decoded anyway
    /// because that's how the API wraps every list, and the cursor is dropped.
    pub async fn list_circ_room_users(&self, room_id: &str) -> Result<Vec<CircRoomUser>> {
        let path = format!("/v1/circ/{room_id}/users");
        let (users, _cursor) = self
            .request_page::<CircRoomUser>(EndpointKey::CircUsers, Method::GET, &path, &[])
            .await?;
        Ok(users)
    }

    /// `POST /v1/circ/:roomId/presence`: announce that you're in the room
    /// (v0.8.4, § Announce Your Presence).
    ///
    /// Call it when you enter a room and then every
    /// [`heartbeat_ms`](CircPresenceResponse::heartbeat_ms) for as long as you
    /// stay: this is what puts you in the room's user list, for people on the
    /// website as well. Skip it and you can still read and send, you're just
    /// invisible.
    ///
    /// `last_activity_ms` is when your user last did something (a keystroke, a
    /// command, the window regaining focus) as a ms epoch on your own clock.
    /// Send it with every heartbeat, plus one extra the moment they wake up or
    /// go quiet. Leave it out and you always read as active.
    ///
    /// **Read the cadence off the response**, don't hard-code it: the returned
    /// `heartbeat_ms`, `stale_after_ms` and `idle_after_ms` are the real
    /// thresholds, and they're also what the user list and the presence stream
    /// have to be filtered by. Keep heartbeating while your user is idle, or you
    /// drop out of the room instead of just showing as idle. Returns `403` if
    /// the room isn't available to you.
    ///
    /// Rate-limited per room (15/min) as well as overall (90/min), so the
    /// request carries `room_id` as its limiter scope.
    pub async fn announce_circ_presence(
        &self,
        room_id: &str,
        last_activity_ms: Option<i64>,
    ) -> Result<CircPresenceResponse> {
        let body = PresenceBody {
            last_activity: last_activity_ms,
        };
        let path = format!("/v1/circ/{room_id}/presence");
        self.request_scoped(
            EndpointKey::CircPresence,
            Some(room_id),
            Method::POST,
            &path,
            &[],
            Some(&body),
        )
        .await
    }

    /// `DELETE /v1/circ/:roomId/presence`: leave the room's user list
    /// immediately (v0.8.4, § Leave a Room).
    ///
    /// Optional but polite: call it when the user leaves the room or quits.
    /// Without it you stay listed until
    /// [`stale_after_ms`](CircPresenceResponse::stale_after_ms) elapses. Draws
    /// on the same per-room budget as the heartbeat, so it carries the same
    /// scope. The `{ roomId, ok }` body carries nothing worth returning.
    pub async fn leave_circ_room(&self, room_id: &str) -> Result<()> {
        let path = format!("/v1/circ/{room_id}/presence");
        self.request_unit_scoped(
            EndpointKey::CircPresence,
            Some(room_id),
            Method::DELETE,
            &path,
            &[],
        )
        .await
    }
}

/// The RTDB path carrying a room's live messages. Subscribe with
/// `orderBy="timestamp"` and a `limitToLast` of 100 or fewer, as the spec
/// requires (§ Reading a room in real time), and decode each event with
/// [`circ_message_updates_from_rtdb_event`].
#[must_use]
pub fn circ_messages_path(room_id: &str) -> String {
    format!("/chat_messages/{room_id}")
}

/// The RTDB path carrying a room's live user list (v0.8.4). Subscribe to it the
/// same way as [`circ_messages_path`], as a second stream, and decode each event
/// with [`circ_presence_updates_from_rtdb_event`]. No query parameters: the node
/// holds one small entry per person in the room.
///
/// This stream is read-only. Publishing your own presence goes through
/// [`Client::announce_circ_presence`], never through an RTDB write.
#[must_use]
pub fn circ_presence_path(room_id: &str) -> String {
    format!("/chat_presence/{room_id}")
}

/// Decode one RTDB `put`/`patch` event on a `chat_messages/<roomId>`
/// subscription into the message changes it carries.
///
/// `kind` is what decides how the payload is read, and it matters: a `put`
/// replaces a path (so its object is a whole message) while a `patch` merges
/// into one (so its object is a fragment). Deleting a message is delivered as a
/// `patch` of `{ "content": "[DELETED]", "deleted": true }`, which is why a
/// stream that only ever appends whole messages never shows deletions.
///
/// Firebase keys each message by id, so the id is injected from the map key (a
/// root-path event) or from the final path segment (a single-message event).
/// All three shapes are handled:
///
/// - `/` with `{ "<messageId>": {...}, … }`, one update per entry,
/// - `/<messageId>` with the message or a fragment of it,
/// - `/<messageId>/<field>` with a single value, which Firebase sends for a
///   leaf write. It's rebuilt into a one-field patch, since one field is never
///   a whole message.
///
/// A payload that isn't an object yields nothing. That includes `null`, which
/// means the path went away: v0.8.4 deletions are soft and arrive as a patch,
/// whereas a `null` on a `limitToLast` stream usually just means the message
/// scrolled out of the window, and dropping it would eat your scrollback.
///
/// An object that fails to decode is dropped too, since half a message is worse
/// than none, but it is logged at `debug` first so the drop can be explained
/// rather than looking like the server never sent it.
#[must_use]
pub fn circ_message_updates_from_rtdb_event(
    kind: SseEventKind,
    path: &str,
    data: &Value,
) -> Vec<CircMessageUpdate> {
    let partial = matches!(kind, SseEventKind::Patch);
    match rtdb_path_segments(path).as_slice() {
        [] => match data {
            Value::Object(map) => map
                .iter()
                .filter_map(|(id, value)| circ_message_update(partial, id, value))
                .collect(),
            _ => Vec::new(),
        },
        [id] => circ_message_update(partial, id, data).into_iter().collect(),
        [id, field] => circ_message_update(true, id, &one_field_object(field, data))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// The whole messages carried by an RTDB event, ignoring partial updates.
///
/// Superseded by [`circ_message_updates_from_rtdb_event`], which also reports
/// the patches this drops, and a client that only calls this one cannot show
/// deletions. Kept for callers that only care about new messages.
#[must_use]
pub fn circ_messages_from_rtdb_event(path: &str, data: &Value) -> Vec<CircMessage> {
    circ_message_updates_from_rtdb_event(SseEventKind::Put, path, data)
        .into_iter()
        .filter_map(|update| match update {
            CircMessageUpdate::Full(message) => Some(message),
            CircMessageUpdate::Partial { .. } => None,
        })
        .collect()
}

/// Decode one RTDB `put`/`patch` event on a `chat_presence/<roomId>`
/// subscription into the user-list changes it carries (v0.8.4).
///
/// The same three path shapes as
/// [`circ_message_updates_from_rtdb_event`], keyed by user id instead of
/// message id, and with one addition: a payload of `null` is a removal, not a
/// no-op. This node is unfiltered, so `null` really does mean the entry is gone
/// (someone called `DELETE /v1/circ/:roomId/presence`, or the server expired
/// them).
///
/// A `null` on the root path yields nothing, deliberately: entries also expire
/// through [`CircPresenceEntry::is_visible`], which the spec requires you to
/// re-evaluate on a timer anyway.
///
/// An object that fails to decode is dropped, since half an entry is worse than
/// none, but it is logged at `debug` first so the drop can be explained rather
/// than looking like the server never sent it.
#[must_use]
pub fn circ_presence_updates_from_rtdb_event(
    kind: SseEventKind,
    path: &str,
    data: &Value,
) -> Vec<CircPresenceUpdate> {
    let partial = matches!(kind, SseEventKind::Patch);
    match rtdb_path_segments(path).as_slice() {
        [] => match data {
            Value::Object(map) => map
                .iter()
                .filter_map(|(user_id, value)| circ_presence_update(partial, user_id, value))
                .collect(),
            _ => Vec::new(),
        },
        [user_id] => circ_presence_update(partial, user_id, data)
            .into_iter()
            .collect(),
        [user_id, field] => circ_presence_update(true, user_id, &one_field_object(field, data))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn circ_message_update(partial: bool, id: &str, value: &Value) -> Option<CircMessageUpdate> {
    let Value::Object(object) = value else {
        return None;
    };

    if !partial && looks_like_whole_message(object) {
        let mut message: CircMessage = match serde_json::from_value(value.clone()) {
            Ok(message) => message,
            Err(error) => {
                tracing::debug!(
                    message_id = %id,
                    error = %error,
                    "dropping an undecodable cIRC message event"
                );
                return None;
            }
        };
        if message.id.is_empty() {
            message.id = id.to_string();
        }
        return Some(CircMessageUpdate::Full(message));
    }

    let patch: CircMessagePatch = match serde_json::from_value(value.clone()) {
        Ok(patch) => patch,
        Err(error) => {
            tracing::debug!(
                message_id = %id,
                error = %error,
                "dropping an undecodable cIRC message patch event"
            );
            return None;
        }
    };
    let id = if id.is_empty() {
        patch.id.clone().unwrap_or_default()
    } else {
        id.to_string()
    };
    if id.is_empty() || patch.is_empty() {
        return None;
    }
    Some(CircMessageUpdate::Partial { id, patch })
}

/// Whether a `put` payload can be a whole message. A stored message always has
/// a sender **and** a timestamp, so a payload missing either is a fragment
/// however it was delivered, and treating it as whole would replace the message
/// a caller holds with a nameless, empty line stamped 1970 (which is exactly
/// what [`CircMessageUpdate::Full`] tells callers to do).
///
/// Requiring both matches the C-Mail guard, `is_whole_message` in
/// [`crate::cmail`]. A one-key fragment such as `{ "timestamp": 999 }` falls
/// through to the patch branch instead, where it merges into the message it
/// targets and leaves every other field alone.
fn looks_like_whole_message(object: &serde_json::Map<String, Value>) -> bool {
    let has_sender = ["userId", "senderId", "senderUid"]
        .iter()
        .any(|key| object.contains_key(*key));
    has_sender && object.contains_key("timestamp")
}

fn circ_presence_update(partial: bool, user_id: &str, value: &Value) -> Option<CircPresenceUpdate> {
    if user_id.is_empty() {
        return None;
    }
    match value {
        Value::Null => Some(CircPresenceUpdate::Removed {
            user_id: user_id.to_string(),
        }),
        Value::Object(_) if !partial => {
            let mut entry: CircPresenceEntry = match serde_json::from_value(value.clone()) {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::debug!(
                        user_id = %user_id,
                        error = %error,
                        "dropping an undecodable cIRC presence event"
                    );
                    return None;
                }
            };
            if entry.user_id.is_empty() {
                entry.user_id = user_id.to_string();
            }
            Some(CircPresenceUpdate::Full(entry))
        }
        Value::Object(_) => {
            let patch: CircPresencePatch = match serde_json::from_value(value.clone()) {
                Ok(patch) => patch,
                Err(error) => {
                    tracing::debug!(
                        user_id = %user_id,
                        error = %error,
                        "dropping an undecodable cIRC presence patch"
                    );
                    return None;
                }
            };
            if patch.is_empty() {
                return None;
            }
            Some(CircPresenceUpdate::Partial {
                user_id: user_id.to_string(),
                patch,
            })
        }
        _ => None,
    }
}

/// The non-empty segments of an RTDB event path (`/` is zero segments).
fn rtdb_path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Rebuild a leaf write (`path: "/<key>/<field>"`, `data: <value>`) into the
/// one-field object the patch types decode from.
fn one_field_object(field: &str, value: &Value) -> Value {
    let mut object = serde_json::Map::with_capacity(1);
    object.insert(field.to_string(), value.clone());
    Value::Object(object)
}

/// The spec's idle rule, shared by the REST user list and the presence stream:
/// older than `idle_after_ms` is idle, and an absent `lastActivity` is active.
fn is_idle_at(last_activity: Option<i64>, now_ms: i64, idle_after_ms: i64) -> bool {
    let Some(last_activity) = last_activity else {
        return false;
    };
    now_ms.saturating_sub(last_activity) > idle_after_ms
}

fn duration_from_ms(value: i64, fallback: i64) -> Duration {
    let ms = if value > 0 { value } else { fallback };
    Duration::from_millis(u64::try_from(ms).unwrap_or(0))
}

fn default_heartbeat_ms() -> i64 {
    DEFAULT_HEARTBEAT_MS
}

fn default_stale_after_ms() -> i64 {
    DEFAULT_STALE_AFTER_MS
}

fn default_idle_after_ms() -> i64 {
    DEFAULT_IDLE_AFTER_MS
}

/// Derive a next-page cursor when the server didn't send one (see
/// [`crate::cmail`], same convention): the oldest message's timestamp, and only
/// when the page was full.
fn next_cursor_fallback(messages: &[CircMessage], limit: u32) -> Option<String> {
    if messages.len() < limit as usize {
        return None;
    }
    messages.first().map(|m| m.timestamp.to_string())
}

fn validate_circ_content(content: &str) -> Result<()> {
    if content.trim().is_empty() {
        return Err(ApiError::Config("cIRC message cannot be empty".into()));
    }
    if content.chars().count() > MAX_MESSAGE_LEN {
        return Err(ApiError::Config(format!(
            "cIRC message exceeds {MAX_MESSAGE_LEN} characters"
        )));
    }
    Ok(())
}

fn decode_room_list(value: Value) -> Result<Vec<CircRoom>> {
    match value {
        Value::Array(_) => serde_json::from_value(value).map_err(ApiError::from),
        Value::Object(mut obj) => {
            for key in ["rooms", "items", "results"] {
                if let Some(v) = obj.remove(key) {
                    return decode_room_list(v);
                }
            }
            // An RTDB-style `{ <roomId>: {...} }` map: inject the key as the id.
            let mut out = Vec::with_capacity(obj.len());
            for (id, mut v) in obj {
                if let Value::Object(ref mut m) = v {
                    m.entry("id".to_string()).or_insert(Value::String(id));
                }
                out.push(serde_json::from_value(v)?);
            }
            Ok(out)
        }
        Value::Null => Ok(Vec::new()),
        other => serde_json::from_value(other).map_err(ApiError::from),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    /// A minimal [`tracing::Subscriber`] that counts the events emitted while it
    /// is the thread's default, so a test can assert that a dropped realtime
    /// event leaves a trace instead of vanishing.
    struct EventCounter(Arc<AtomicUsize>);

    impl tracing::Subscriber for EventCounter {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    /// Run `f` with an [`EventCounter`] installed, returning its result and how
    /// many tracing events it emitted.
    fn counting_events<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let counter = Arc::new(AtomicUsize::new(0));
        let out = tracing::subscriber::with_default(EventCounter(Arc::clone(&counter)), f);
        (out, counter.load(Ordering::SeqCst))
    }

    fn message(id: &str, content: &str) -> CircMessage {
        CircMessage {
            id: id.to_string(),
            user_id: "u1".to_string(),
            username: "neo".to_string(),
            content: content.to_string(),
            timestamp: 1_719_700_000_000,
            ..CircMessage::default()
        }
    }

    #[test]
    fn room_decodes_and_addresses_by_slug() {
        let room: CircRoom = serde_json::from_str(
            r#"{"id":"r1","slug":"general","name":"General","lastMessageAt":1719700000000,"sortOrder":0,"onlineCount":7}"#,
        )
        .unwrap();
        assert_eq!(room.slug, "general");
        assert_eq!(room.name, "General");
        assert_eq!(room.room_id(), "general");
        assert_eq!(room.online_count, 7);
    }

    #[test]
    fn room_falls_back_to_id_when_slug_missing() {
        let room: CircRoom = serde_json::from_str(r#"{"id":"r1","name":"General"}"#).unwrap();
        assert_eq!(room.room_id(), "r1");
        assert_eq!(room.online_count, 0, "absent onlineCount reads as empty");
    }

    #[test]
    fn message_decodes_full_shape() {
        let msg: CircMessage = serde_json::from_str(
            r#"{"id":"m1","userId":"u1","username":"neo","isChatAdmin":true,"content":"hi","timestamp":1719700000000}"#,
        )
        .unwrap();
        assert_eq!(msg.user_id, "u1");
        assert_eq!(msg.username, "neo");
        assert!(msg.is_chat_admin);
        assert_eq!(msg.timestamp, 1_719_700_000_000);
        assert_eq!(msg.extras, MessageExtras::default(), "no extras sent");
    }

    #[test]
    fn message_decodes_flattened_extras() {
        let msg: CircMessage = serde_json::from_str(
            r#"{"id":"m1","userId":"u1","username":"neo","content":"","timestamp":1,
                "gifUrl":"https://cdn.example/a.gif","style":["comic","rainbow"],"isAction":true}"#,
        )
        .unwrap();
        assert_eq!(
            msg.extras.gif_url.as_deref(),
            Some("https://cdn.example/a.gif")
        );
        assert!(msg.extras.is_action);
        assert!(msg.extras.has_attachment());
        assert!(msg
            .extras
            .style
            .as_ref()
            .expect("style")
            .contains("rainbow"));
        // An attachment can be the whole message.
        assert_eq!(msg.extras.display_content(&msg.content), None);
    }

    #[test]
    fn message_decodes_deleted_tombstone() {
        let msg: CircMessage = serde_json::from_str(
            r#"{"id":"m1","userId":"u1","username":"neo","content":"[DELETED]","timestamp":1,"deleted":true}"#,
        )
        .unwrap();
        assert!(msg.extras.deleted);
        assert_eq!(msg.content, "[DELETED]");
    }

    #[test]
    fn message_tolerates_an_explicit_null_on_every_field() {
        let msg: CircMessage = serde_json::from_str(
            r#"{"id":null,"userId":null,"username":null,"isChatAdmin":null,"content":null,"timestamp":null}"#,
        )
        .expect("an explicit null must decode like an absent key, not fail the message");
        assert_eq!(msg, CircMessage::default());
    }

    #[test]
    fn one_null_field_no_longer_sinks_the_rest_of_the_history_page() {
        let page: Vec<CircMessage> = serde_json::from_str(
            r#"[{"id":"m1","userId":"u1","username":"neo","content":"hi","timestamp":1},
                {"id":"m2","userId":"u2","username":"trinity","content":null,"timestamp":2}]"#,
        )
        .expect("a page decodes as one Vec, so a null must cost one field, not every message");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].content, "hi");
        assert_eq!(page[1].content, "");
        assert_eq!(
            page[1].username, "trinity",
            "the rest of the record survives its own null"
        );
    }

    #[test]
    fn a_null_field_in_a_stream_event_costs_only_that_field() {
        let root = serde_json::json!({
            "m1": {"userId":"u1","username":"neo","content":"hi","timestamp":1_000},
            "m2": {"userId":"u2","username":null,"content":"yo","timestamp":2_000}
        });
        let mut updates = circ_message_updates_from_rtdb_event(SseEventKind::Put, "/", &root);
        updates.sort_by(|a, b| a.message_id().cmp(b.message_id()));
        assert_eq!(updates.len(), 2, "the good message survives the null one");
        let second = updates[1].as_full().expect("full");
        assert_eq!(second.username, "");
        assert_eq!(second.content, "yo");
    }

    #[test]
    fn send_response_decodes_posted_and_command_reply() {
        let posted: CircSendResponse =
            serde_json::from_str(r#"{"roomId":"general","messageId":"m2"}"#).unwrap();
        assert_eq!(posted.message_id.as_deref(), Some("m2"));
        assert!(posted.reply.is_none());

        let help: CircSendResponse = serde_json::from_str(r#"{"reply":"commands: …"}"#).unwrap();
        assert_eq!(help.reply.as_deref(), Some("commands: …"));
        assert!(help.message_id.is_none());
    }

    #[test]
    fn delete_response_decodes() {
        let deleted: CircDeleteResponse =
            serde_json::from_str(r#"{"roomId":"general","messageId":"m1","deleted":true}"#)
                .unwrap();
        assert_eq!(deleted.room_id, "general");
        assert_eq!(deleted.message_id, "m1");
        assert!(deleted.deleted);

        let sparse: CircDeleteResponse = serde_json::from_str("{}").unwrap();
        assert!(
            !sparse.deleted,
            "absent fields decode to the empty response"
        );
    }

    #[test]
    fn flag_response_decodes_first_report_and_repeat() {
        let first: FlagResponse = serde_json::from_str(
            r#"{"roomId":"general","messageId":"m1","flagId":"f1","flagged":true}"#,
        )
        .unwrap();
        assert!(first.flagged);
        assert!(first.is_new());
        assert_eq!(first.flag_id.as_deref(), Some("f1"));

        let repeat: FlagResponse =
            serde_json::from_str(r#"{"flagged":true,"alreadyFlagged":true}"#).unwrap();
        assert!(!repeat.is_new());
        assert!(repeat.flag_id.is_none());
    }

    #[test]
    fn flag_reason_is_length_checked_before_sending() {
        assert!(validate_flag_reason(None).is_ok());
        assert!(validate_flag_reason(Some("spam")).is_ok());
        let long = "x".repeat(501);
        assert!(matches!(
            validate_flag_reason(Some(&long)),
            Err(ApiError::Config(_))
        ));
    }

    #[test]
    fn flag_body_omits_an_absent_reason() {
        let with_reason = serde_json::to_value(FlagBody {
            reason: Some("spam"),
        })
        .unwrap();
        assert_eq!(with_reason["reason"], "spam");

        let without = serde_json::to_value(FlagBody { reason: None }).unwrap();
        assert!(without.get("reason").is_none());
    }

    #[test]
    fn presence_body_omits_an_absent_last_activity() {
        let with_activity = serde_json::to_value(PresenceBody {
            last_activity: Some(1_719_700_000_000),
        })
        .unwrap();
        assert_eq!(with_activity["lastActivity"], 1_719_700_000_000_i64);

        let without = serde_json::to_value(PresenceBody {
            last_activity: None,
        })
        .unwrap();
        assert!(without.get("lastActivity").is_none());
    }

    #[test]
    fn room_list_decodes_array_and_wrapper() {
        let arr = serde_json::json!([{"id":"r1","slug":"general","name":"General"}]);
        assert_eq!(decode_room_list(arr).unwrap().len(), 1);

        let wrapped = serde_json::json!({"rooms":[{"id":"r1","slug":"general"}]});
        let list = decode_room_list(wrapped).unwrap();
        assert_eq!(list[0].room_id(), "general");
    }

    #[test]
    fn rtdb_event_parses_messages_with_ids_from_keys() {
        let data = serde_json::json!({
            "m1": {"userId":"u1","username":"neo","content":"hi","timestamp":1_000}
        });
        let msgs = circ_messages_from_rtdb_event("/", &data);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "m1");
        assert_eq!(msgs[0].username, "neo");

        let single = serde_json::json!({"userId":"u2","username":"trinity","content":"yo","timestamp":2_000});
        let msgs = circ_messages_from_rtdb_event("/m2", &single);
        assert_eq!(msgs[0].id, "m2");
    }

    #[test]
    fn put_event_yields_whole_messages_in_both_shapes() {
        let root = serde_json::json!({
            "m1": {"userId":"u1","username":"neo","content":"hi","timestamp":1_000}
        });
        let updates = circ_message_updates_from_rtdb_event(SseEventKind::Put, "/", &root);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].message_id(), "m1");
        assert_eq!(updates[0].as_full().expect("full").username, "neo");

        let single = serde_json::json!({"userId":"u2","username":"trinity","content":"yo","timestamp":2_000});
        let updates = circ_message_updates_from_rtdb_event(SseEventKind::Put, "/m2", &single);
        assert_eq!(
            updates,
            vec![CircMessageUpdate::Full(CircMessage {
                id: "m2".to_string(),
                user_id: "u2".to_string(),
                username: "trinity".to_string(),
                content: "yo".to_string(),
                timestamp: 2_000,
                ..CircMessage::default()
            })]
        );
    }

    #[test]
    fn delete_patch_is_a_targeted_partial_not_a_message() {
        let data = serde_json::json!({"content":"[DELETED]","deleted":true});
        let updates = circ_message_updates_from_rtdb_event(SseEventKind::Patch, "/m1", &data);
        assert_eq!(updates.len(), 1);
        let CircMessageUpdate::Partial { id, patch } = &updates[0] else {
            panic!("a patch payload must not decode as a whole message: {updates:?}");
        };
        assert_eq!(id, "m1");
        assert_eq!(patch.content.as_deref(), Some("[DELETED]"));
        assert_eq!(patch.deleted, Some(true));
        assert!(patch.username.is_none(), "untouched fields stay absent");
    }

    #[test]
    fn root_patch_maps_message_ids_to_partials() {
        let data = serde_json::json!({"m1":{"content":"[DELETED]","deleted":true}});
        let updates = circ_message_updates_from_rtdb_event(SseEventKind::Patch, "/", &data);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].message_id(), "m1");
        assert!(updates[0].as_full().is_none());
    }

    #[test]
    fn leaf_write_becomes_a_one_field_patch() {
        let updates = circ_message_updates_from_rtdb_event(
            SseEventKind::Put,
            "/m1/deleted",
            &Value::Bool(true),
        );
        let CircMessageUpdate::Partial { id, patch } = &updates[0] else {
            panic!("a single field is never a whole message: {updates:?}");
        };
        assert_eq!(id, "m1");
        assert_eq!(patch.deleted, Some(true));
        assert!(patch.content.is_none());
    }

    #[test]
    fn partial_put_payload_falls_back_to_a_patch() {
        // A `put` carrying neither a sender nor a timestamp can't be a whole
        // message; inserting it would add a nameless line stamped 1970.
        let data = serde_json::json!({"content":"[DELETED]","deleted":true});
        let updates = circ_message_updates_from_rtdb_event(SseEventKind::Put, "/m1", &data);
        assert!(matches!(
            updates.as_slice(),
            [CircMessageUpdate::Partial { .. }]
        ));
        assert!(circ_messages_from_rtdb_event("/m1", &data).is_empty());
    }

    #[test]
    fn a_put_needs_both_a_sender_and_a_timestamp_to_be_a_whole_message() {
        // A lone timestamp is a fragment. Taken as whole, a caller following
        // `CircMessageUpdate::Full` would replace the message it holds with a
        // nameless, contentless one.
        let lone_timestamp = serde_json::json!({"timestamp": 999});
        let updates =
            circ_message_updates_from_rtdb_event(SseEventKind::Put, "/m1", &lone_timestamp);
        let CircMessageUpdate::Partial { id, patch } = &updates[0] else {
            panic!("a lone timestamp is not a whole message: {updates:?}");
        };
        assert_eq!(id, "m1");
        assert_eq!(patch.timestamp, Some(999));
        assert!(patch.username.is_none());
        assert!(circ_messages_from_rtdb_event("/m1", &lone_timestamp).is_empty());

        // A lone sender is a fragment for the same reason.
        let lone_sender = serde_json::json!({"userId": "u1"});
        let updates = circ_message_updates_from_rtdb_event(SseEventKind::Put, "/m1", &lone_sender);
        assert!(matches!(
            updates.as_slice(),
            [CircMessageUpdate::Partial { .. }]
        ));

        // Both keys present is still a whole message, so the guard has not been
        // tightened into rejecting the real thing.
        let whole =
            serde_json::json!({"userId":"u1","username":"neo","content":"hi","timestamp":1_000});
        let updates = circ_message_updates_from_rtdb_event(SseEventKind::Put, "/m1", &whole);
        assert_eq!(updates[0].as_full().expect("full").username, "neo");
    }

    #[test]
    fn an_undecodable_message_event_is_logged_before_it_is_dropped() {
        // Passes the whole-message guard (a sender and a timestamp) but the
        // timestamp is not a number, so decoding it as a message fails.
        let broken = serde_json::json!({"userId":"u1","timestamp":"much later"});
        let (updates, events) = counting_events(|| {
            circ_message_updates_from_rtdb_event(SseEventKind::Put, "/m1", &broken)
        });
        assert!(
            updates.is_empty(),
            "half a message is still worse than none"
        );
        assert!(events >= 1, "the drop must leave a trace behind");

        // The patch branch is the second silent path.
        let broken_patch = serde_json::json!({"content": 7});
        let (updates, events) = counting_events(|| {
            circ_message_updates_from_rtdb_event(SseEventKind::Patch, "/m1", &broken_patch)
        });
        assert!(updates.is_empty());
        assert!(events >= 1, "a dropped patch must be observable too");

        // A decode that works says nothing, so the trace means something.
        let whole =
            serde_json::json!({"userId":"u1","username":"neo","content":"hi","timestamp":1_000});
        let (updates, events) = counting_events(|| {
            circ_message_updates_from_rtdb_event(SseEventKind::Put, "/m1", &whole)
        });
        assert_eq!(updates.len(), 1);
        assert_eq!(events, 0, "a clean decode is quiet");
    }

    #[test]
    fn an_undecodable_presence_event_is_logged_before_it_is_dropped() {
        // The presence decoder had the same two silent drops the message
        // decoder did: a room whose user list quietly stops updating looks
        // exactly like a room nobody is in.
        let broken = serde_json::json!({"userId":"u1","online":"yes"});
        let (updates, events) = counting_events(|| {
            circ_presence_updates_from_rtdb_event(SseEventKind::Put, "/u1", &broken)
        });
        assert!(updates.is_empty(), "half an entry is worse than none");
        assert!(events >= 1, "the drop must leave a trace behind");

        // The patch branch is the second silent path.
        let broken_patch = serde_json::json!({"online": "yes"});
        let (updates, events) = counting_events(|| {
            circ_presence_updates_from_rtdb_event(SseEventKind::Patch, "/u1", &broken_patch)
        });
        assert!(updates.is_empty());
        assert!(events >= 1, "a dropped patch must be observable too");

        // A decode that works says nothing, so the trace means something.
        let whole = serde_json::json!({"userId":"u1","username":"neo","online":true});
        let (updates, events) = counting_events(|| {
            circ_presence_updates_from_rtdb_event(SseEventKind::Put, "/u1", &whole)
        });
        assert_eq!(updates.len(), 1);
        assert_eq!(events, 0, "a clean decode is quiet");
    }

    #[test]
    fn non_object_and_empty_payloads_yield_no_message_updates() {
        for (path, data) in [
            ("/m1", Value::Null),
            ("/", Value::Null),
            ("/m1", Value::String("nope".into())),
            ("/m1/extra/deep", Value::Bool(true)),
        ] {
            assert!(
                circ_message_updates_from_rtdb_event(SseEventKind::Patch, path, &data).is_empty(),
                "{path} {data:?}"
            );
        }
        // A patch of fields we don't model changes nothing, so it's dropped.
        let unknown = serde_json::json!({"somethingNew": 1});
        assert!(
            circ_message_updates_from_rtdb_event(SseEventKind::Patch, "/m1", &unknown).is_empty()
        );
    }

    #[test]
    fn patch_merges_into_the_message_it_targets() {
        let mut msg = message("m1", "hello there");
        msg.extras.image_url = Some("https://cdn.example/pic.png".into());
        msg.extras.is_action = true;

        let patch: CircMessagePatch = serde_json::from_str(r#"{"content":"edited"}"#).unwrap();
        patch.apply_to(&mut msg);
        assert_eq!(msg.content, "edited");
        assert_eq!(msg.username, "neo", "untouched fields survive the merge");
        assert_eq!(msg.timestamp, 1_719_700_000_000);
        assert!(msg.extras.is_action, "an absent flag is not a false flag");
        assert!(msg.extras.image_url.is_some());
    }

    #[test]
    fn delete_patch_strips_the_extras_with_the_content() {
        let mut msg = message("m1", "look at this");
        msg.extras.image_url = Some("https://cdn.example/pic.png".into());
        msg.extras.style = Some(MessageStyle::One("rainbow".into()));
        msg.extras.is_action = true;

        let patch: CircMessagePatch =
            serde_json::from_str(r#"{"content":"[DELETED]","deleted":true}"#).unwrap();
        patch.apply_to(&mut msg);

        assert_eq!(msg.content, "[DELETED]");
        assert!(msg.extras.deleted);
        assert!(!msg.extras.has_attachment(), "the picture goes with it");
        assert!(msg.extras.style.is_none());
        assert!(!msg.extras.is_action);
        assert_eq!(msg.username, "neo", "the author and timestamp stay");
        assert_eq!(msg.timestamp, 1_719_700_000_000);
    }

    #[test]
    fn patch_never_rewrites_the_message_id() {
        let mut msg = message("m1", "hi");
        let patch: CircMessagePatch =
            serde_json::from_str(r#"{"id":"m9","content":"hi again"}"#).unwrap();
        patch.apply_to(&mut msg);
        assert_eq!(msg.id, "m1");
        assert_eq!(msg.content, "hi again");
    }

    #[test]
    fn patch_decodes_every_extra_field() {
        let patch: CircMessagePatch = serde_json::from_str(
            r#"{"imageUrl":"https://cdn.example/p.png","gifUrl":"https://cdn.example/a.gif",
                "audioAttachment":{"src":"https://youtu.be/x","origin":"youtube","artist":"A","title":"T"},
                "style":"l33t","isAction":true,"isDice":true,"isEightball":true,
                "eightballAnswer":"Ask again later","isFortune":true,"fortuneText":"You will ship it",
                "deleted":false,"isChatAdmin":true,"senderId":"u9","senderUsername":"trinity","timestamp":42}"#,
        )
        .unwrap();
        assert_eq!(patch.user_id.as_deref(), Some("u9"));
        assert_eq!(patch.username.as_deref(), Some("trinity"));
        assert_eq!(patch.is_chat_admin, Some(true));
        assert_eq!(patch.timestamp, Some(42));
        assert_eq!(
            patch.image_url.as_deref(),
            Some("https://cdn.example/p.png")
        );
        assert_eq!(patch.gif_url.as_deref(), Some("https://cdn.example/a.gif"));
        assert_eq!(
            patch.audio_attachment.as_ref().map(|a| a.title.as_str()),
            Some("T")
        );
        assert!(patch.style.as_ref().expect("style").contains("l33t"));
        assert_eq!(patch.is_dice, Some(true));
        assert_eq!(patch.eightball_answer.as_deref(), Some("Ask again later"));
        assert_eq!(patch.fortune_text.as_deref(), Some("You will ship it"));
        assert_eq!(patch.deleted, Some(false));

        let mut msg = message("m1", "hi");
        patch.apply_to(&mut msg);
        assert_eq!(msg.username, "trinity");
        assert!(msg.extras.is_eightball);
        assert!(!msg.extras.deleted);
    }

    #[test]
    fn patch_treats_null_as_no_change() {
        let patch: CircMessagePatch =
            serde_json::from_str(r#"{"imageUrl":null,"content":"still here"}"#).unwrap();
        assert!(patch.image_url.is_none());
        assert!(!patch.is_empty());

        let mut msg = message("m1", "hi");
        msg.extras.image_url = Some("https://cdn.example/pic.png".into());
        patch.apply_to(&mut msg);
        assert_eq!(
            msg.extras.image_url.as_deref(),
            Some("https://cdn.example/pic.png")
        );
    }

    #[test]
    fn room_user_decodes_with_and_without_last_activity() {
        let active: CircRoomUser = serde_json::from_str(
            r#"{"userId":"u1","username":"neo","isChatAdmin":true,"lastSeen":1719700000000,"lastActivity":1719699000000}"#,
        )
        .unwrap();
        assert_eq!(active.user_id, "u1");
        assert!(active.is_chat_admin);
        assert_eq!(active.last_seen, 1_719_700_000_000);
        assert_eq!(active.last_activity, Some(1_719_699_000_000));

        let null_activity: CircRoomUser = serde_json::from_str(
            r#"{"userId":"u2","username":"trinity","isChatAdmin":false,"lastSeen":1719700000000,"lastActivity":null}"#,
        )
        .unwrap();
        assert!(null_activity.last_activity.is_none());

        let sparse: CircRoomUser = serde_json::from_str(r#"{"userId":"u3"}"#).unwrap();
        assert_eq!(sparse.username, "");
        assert!(sparse.last_activity.is_none());
    }

    #[test]
    fn room_user_tolerates_an_explicit_null_on_every_field() {
        let user: CircRoomUser = serde_json::from_str(
            r#"{"userId":null,"username":null,"isChatAdmin":null,"lastSeen":null,"lastActivity":null}"#,
        )
        .expect("an explicit null must decode like an absent key");
        assert_eq!(user, CircRoomUser::default());

        // The user list decodes as one page, so a null on one person must not
        // empty the room.
        let page: Vec<CircRoomUser> = serde_json::from_str(
            r#"[{"userId":"u1","username":"neo","isChatAdmin":false,"lastSeen":1},
                {"userId":"u2","username":"trinity","isChatAdmin":null,"lastSeen":2}]"#,
        )
        .expect("one null must cost one field, not the whole list");
        assert_eq!(page.len(), 2);
        assert!(!page[1].is_chat_admin);
        assert_eq!(page[1].username, "trinity");
    }

    #[test]
    fn room_user_idle_rule_matches_the_spec() {
        let now = 1_719_700_000_000;
        let idle_after = 600_000;

        let recent = CircRoomUser {
            last_activity: Some(now - 1_000),
            ..CircRoomUser::default()
        };
        assert!(!recent.is_idle(now, idle_after));

        let stale = CircRoomUser {
            last_activity: Some(now - idle_after - 1),
            ..CircRoomUser::default()
        };
        assert!(stale.is_idle(now, idle_after));

        // Exactly at the threshold is not yet idle.
        let borderline = CircRoomUser {
            last_activity: Some(now - idle_after),
            ..CircRoomUser::default()
        };
        assert!(!borderline.is_idle(now, idle_after));

        // No reported activity means active, not idle forever.
        let quiet = CircRoomUser {
            last_activity: None,
            ..CircRoomUser::default()
        };
        assert!(!quiet.is_idle(now, idle_after));
    }

    #[test]
    fn presence_response_decodes_the_cadence() {
        let resp: CircPresenceResponse = serde_json::from_str(
            r#"{"roomId":"general","ok":true,"heartbeatMs":30000,"staleAfterMs":180000,"idleAfterMs":600000}"#,
        )
        .unwrap();
        assert_eq!(resp.room_id, "general");
        assert!(resp.ok);
        assert_eq!(resp.heartbeat_ms, 30_000);
        assert_eq!(resp.stale_after_ms, 180_000);
        assert_eq!(resp.idle_after_ms, 600_000);
        assert_eq!(resp.heartbeat_interval(), Duration::from_secs(30));
        assert_eq!(resp.stale_after(), Duration::from_secs(180));
        assert_eq!(resp.idle_after(), Duration::from_secs(600));
    }

    #[test]
    fn presence_response_falls_back_when_the_cadence_is_missing_or_zero() {
        let sparse: CircPresenceResponse = serde_json::from_str(r#"{"roomId":"general"}"#).unwrap();
        assert_eq!(sparse.heartbeat_ms, DEFAULT_HEARTBEAT_MS);
        assert_eq!(sparse.stale_after_ms, DEFAULT_STALE_AFTER_MS);
        assert_eq!(sparse.idle_after_ms, DEFAULT_IDLE_AFTER_MS);

        let zeroed: CircPresenceResponse =
            serde_json::from_str(r#"{"heartbeatMs":0,"staleAfterMs":-1,"idleAfterMs":0}"#).unwrap();
        assert_eq!(
            zeroed.heartbeat_interval(),
            Duration::from_millis(u64::try_from(DEFAULT_HEARTBEAT_MS).unwrap()),
            "a zero cadence must never become a hot heartbeat loop"
        );
        assert_eq!(
            zeroed.stale_after(),
            Duration::from_millis(u64::try_from(DEFAULT_STALE_AFTER_MS).unwrap())
        );
        assert_eq!(
            zeroed.idle_after(),
            Duration::from_millis(u64::try_from(DEFAULT_IDLE_AFTER_MS).unwrap())
        );
    }

    #[test]
    fn presence_entry_decodes_from_the_stream_shape() {
        let entry: CircPresenceEntry = serde_json::from_str(
            r#"{"username":"neo","isChatAdmin":false,"online":true,"lastSeen":1719700000000,"lastActivity":1719699000000}"#,
        )
        .unwrap();
        assert_eq!(entry.user_id, "", "the id is the map key, not a field");
        assert_eq!(entry.username, "neo");
        assert!(entry.online);
        assert_eq!(entry.last_activity, Some(1_719_699_000_000));

        let sparse: CircPresenceEntry = serde_json::from_str(r#"{"username":"trinity"}"#).unwrap();
        assert!(!sparse.online);
        assert_eq!(sparse.last_seen, 0);
        assert!(sparse.last_activity.is_none());
    }

    #[test]
    fn presence_entry_tolerates_an_explicit_null_on_every_field() {
        let entry: CircPresenceEntry = serde_json::from_str(
            r#"{"username":null,"isChatAdmin":null,"online":null,"lastSeen":null,"lastActivity":null}"#,
        )
        .expect("an explicit null must decode like an absent key");
        assert_eq!(entry, CircPresenceEntry::default());

        // The stream hands the whole root map to one decode per entry, so a
        // null on one person must not take everyone else out of the room.
        let root = serde_json::json!({
            "u1": {"username":"neo","online":true,"lastSeen":1_000},
            "u2": {"username":"trinity","online":null,"lastSeen":2_000}
        });
        let mut updates = circ_presence_updates_from_rtdb_event(SseEventKind::Put, "/", &root);
        updates.sort_by(|a, b| a.user_id().cmp(b.user_id()));
        assert_eq!(updates.len(), 2, "the good entry survives the null one");
        let second = updates[1].as_full().expect("full");
        assert!(!second.online);
        assert_eq!(second.username, "trinity");
    }

    #[test]
    fn presence_visibility_rule_matches_the_spec() {
        let now = 1_719_700_000_000;
        let stale_after = 180_000;

        let here = CircPresenceEntry {
            online: true,
            last_seen: now - 1_000,
            ..CircPresenceEntry::default()
        };
        assert!(here.is_visible(now, stale_after));

        let gone_quiet = CircPresenceEntry {
            online: true,
            last_seen: now - stale_after - 1,
            ..CircPresenceEntry::default()
        };
        assert!(!gone_quiet.is_visible(now, stale_after));

        let flagged_offline = CircPresenceEntry {
            online: false,
            last_seen: now,
            ..CircPresenceEntry::default()
        };
        assert!(!flagged_offline.is_visible(now, stale_after));

        // Clock skew: a heartbeat stamped ahead of us is fresh, not stale.
        let ahead = CircPresenceEntry {
            online: true,
            last_seen: now + 5_000,
            ..CircPresenceEntry::default()
        };
        assert!(ahead.is_visible(now, stale_after));
    }

    #[test]
    fn presence_visibility_is_strict_at_the_staleness_boundary() {
        let now = 1_719_700_000_000;
        let stale_after = 180_000;

        // § Reading a room in real time says `lastSeen` must be *newer than*
        // `staleAfterMs`, so a heartbeat exactly that old is already gone. The
        // C-Mail typing indicator draws its window at the same boundary.
        let on_the_boundary = CircPresenceEntry {
            online: true,
            last_seen: now - stale_after,
            ..CircPresenceEntry::default()
        };
        assert!(!on_the_boundary.is_visible(now, stale_after));

        let one_millisecond_inside = CircPresenceEntry {
            online: true,
            last_seen: now - stale_after + 1,
            ..CircPresenceEntry::default()
        };
        assert!(one_millisecond_inside.is_visible(now, stale_after));
    }

    #[test]
    fn presence_entry_idle_rule_and_room_user_conversion() {
        let now = 1_719_700_000_000;
        let entry = CircPresenceEntry {
            user_id: "u1".to_string(),
            username: "neo".to_string(),
            is_chat_admin: true,
            online: true,
            last_seen: now,
            last_activity: Some(now - 900_000),
        };
        assert!(entry.is_idle(now, 600_000));
        assert!(!entry.is_idle(now, 1_200_000));

        let user = entry.to_room_user();
        assert_eq!(user.user_id, "u1");
        assert_eq!(user.username, "neo");
        assert!(user.is_chat_admin);
        assert_eq!(user.last_seen, now);
        assert_eq!(user.last_activity, Some(now - 900_000));
        assert!(user.is_idle(now, 600_000));
    }

    #[test]
    fn presence_put_yields_entries_with_ids_from_keys() {
        let root = serde_json::json!({
            "u1": {"username":"neo","isChatAdmin":false,"online":true,"lastSeen":1_000,"lastActivity":900},
            "u2": {"username":"trinity","isChatAdmin":true,"online":true,"lastSeen":2_000,"lastActivity":null}
        });
        let mut updates = circ_presence_updates_from_rtdb_event(SseEventKind::Put, "/", &root);
        updates.sort_by(|a, b| a.user_id().cmp(b.user_id()));
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].as_full().expect("full").username, "neo");
        assert_eq!(updates[1].user_id(), "u2");
        assert!(updates[1].as_full().expect("full").last_activity.is_none());

        let single = serde_json::json!({"username":"neo","online":true,"lastSeen":3_000});
        let updates = circ_presence_updates_from_rtdb_event(SseEventKind::Put, "/u1", &single);
        assert_eq!(updates[0].as_full().expect("full").user_id, "u1");
    }

    #[test]
    fn presence_patch_merges_without_blanking_the_entry() {
        let data = serde_json::json!({"lastSeen":9_000});
        let updates = circ_presence_updates_from_rtdb_event(SseEventKind::Patch, "/u1", &data);
        let CircPresenceUpdate::Partial { user_id, patch } = &updates[0] else {
            panic!("a patch payload must not decode as a whole entry: {updates:?}");
        };
        assert_eq!(user_id, "u1");

        let mut entry = CircPresenceEntry {
            user_id: "u1".to_string(),
            username: "neo".to_string(),
            is_chat_admin: true,
            online: true,
            last_seen: 1_000,
            last_activity: Some(500),
        };
        patch.apply_to(&mut entry);
        assert_eq!(entry.last_seen, 9_000);
        assert_eq!(entry.username, "neo");
        assert!(entry.online, "an absent flag is not a false flag");
        assert!(entry.is_chat_admin);
        assert_eq!(entry.last_activity, Some(500));
    }

    #[test]
    fn presence_null_payload_is_a_removal() {
        let updates = circ_presence_updates_from_rtdb_event(SseEventKind::Put, "/u1", &Value::Null);
        assert_eq!(
            updates,
            vec![CircPresenceUpdate::Removed {
                user_id: "u1".to_string()
            }]
        );

        let root = serde_json::json!({"u1": null});
        let updates = circ_presence_updates_from_rtdb_event(SseEventKind::Patch, "/", &root);
        assert_eq!(updates[0].user_id(), "u1");
        assert!(updates[0].as_full().is_none());

        // A cleared root leaves the list to the staleness rule instead.
        assert!(
            circ_presence_updates_from_rtdb_event(SseEventKind::Put, "/", &Value::Null).is_empty()
        );
    }

    #[test]
    fn presence_leaf_write_becomes_a_one_field_patch() {
        let updates = circ_presence_updates_from_rtdb_event(
            SseEventKind::Put,
            "/u1/online",
            &Value::Bool(false),
        );
        let CircPresenceUpdate::Partial { user_id, patch } = &updates[0] else {
            panic!("a single field is never a whole entry: {updates:?}");
        };
        assert_eq!(user_id, "u1");
        assert_eq!(patch.online, Some(false));
        assert!(patch.username.is_none());

        // A leaf cleared to null changes nothing we model, so nothing is emitted.
        assert!(circ_presence_updates_from_rtdb_event(
            SseEventKind::Put,
            "/u1/lastActivity",
            &Value::Null
        )
        .is_empty());
    }

    #[test]
    fn rtdb_paths_are_built_from_the_room_id() {
        assert_eq!(circ_messages_path("general"), "/chat_messages/general");
        assert_eq!(circ_presence_path("general"), "/chat_presence/general");
    }

    #[test]
    fn validate_rejects_empty_and_too_long() {
        assert!(matches!(
            validate_circ_content("  "),
            Err(ApiError::Config(_))
        ));
        let long = "x".repeat(MAX_MESSAGE_LEN + 1);
        assert!(matches!(
            validate_circ_content(&long),
            Err(ApiError::Config(_))
        ));
    }

    #[test]
    fn next_cursor_fallback_only_on_full_page() {
        let full = vec![
            CircMessage {
                timestamp: 1_000,
                ..CircMessage::default()
            },
            CircMessage {
                timestamp: 2_000,
                ..CircMessage::default()
            },
        ];
        assert_eq!(next_cursor_fallback(&full, 2).as_deref(), Some("1000"));
        assert_eq!(next_cursor_fallback(&full, 50), None);
    }
}

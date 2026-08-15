//! C-Mail REST endpoints (`/v1/cmail`, API v0.8.4).
//!
//! C-Mail is Cyberspace's private 1:1 messaging. REST covers starting/loading
//! conversations, sending/marking messages and publishing the typing indicator
//! (§ Typing Indicator); live updates come from RTDB using the `idToken` +
//! `rtdbUrl` returned by auth.
//!
//! Two RTDB nodes back a conversation (§ Reading in real time):
//! `dm_messages/<conversationId>` for the messages themselves, decoded by
//! [`messages_from_rtdb_event`], and `dm_presence/<conversationId>` for the live
//! typing indicator, decoded by [`cmail_presence_updates_from_rtdb_event`].
use std::time::Duration;

use reqwest::Method;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::message::MessageExtras;
use crate::rtdb::SseEventKind;
use crate::types::null_as_default;

const DEFAULT_MESSAGE_LIMIT: u32 = 50;
const MAX_MESSAGE_LIMIT: u32 = 100;
const MAX_MESSAGE_LEN: usize = 2_048;

/// Refresh interval used only when the server omits `heartbeatMs`. The spec's
/// documented value, not a hard-coded cadence: § Typing Indicator wants the
/// figure read off the response, and this is the last resort when it is absent.
const FALLBACK_TYPING_HEARTBEAT_MS: u64 = 3_000;

/// Staleness window used only when the server omits `staleAfterMs`, on the same
/// terms as [`FALLBACK_TYPING_HEARTBEAT_MS`].
const FALLBACK_TYPING_STALE_AFTER_MS: u64 = 9_000;

/// Body for `POST /v1/cmail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailStartRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
}

impl CmailStartRequest {
    #[must_use]
    pub fn by_username(username: impl Into<String>) -> Self {
        Self {
            recipient_username: Some(username.into()),
            recipient_id: None,
        }
    }

    #[must_use]
    pub fn by_user_id(user_id: impl Into<String>) -> Self {
        Self {
            recipient_username: None,
            recipient_id: Some(user_id.into()),
        }
    }
}

/// User summary nested in C-Mail conversation responses.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailUser {
    #[serde(alias = "id", default, deserialize_with = "null_as_default")]
    pub user_id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub profile_picture_url: Option<String>,
}

/// A C-Mail message as returned by history/list responses and RTDB events.
///
/// Note: v0.7 dropped the per-message `read` flag (unread is tracked per
/// conversation via `unreadCount`); the field is gone here too.
///
/// `content` may be empty: v0.8.4 lets an attachment be the whole message, so
/// render through [`MessageExtras::display_content`] rather than printing
/// `content` directly.
///
/// Every scalar field tolerates an explicit `null`: a page of history decodes
/// as one `Vec`, so a single null would otherwise sink the whole page, and over
/// RTDB it would drop the event without a trace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailMessage {
    #[serde(alias = "messageId", default, deserialize_with = "null_as_default")]
    pub id: String,
    #[serde(alias = "senderUid", default, deserialize_with = "null_as_default")]
    pub sender_id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub sender_username: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub content: String,
    /// Milliseconds since Unix epoch.
    #[serde(default, deserialize_with = "null_as_default")]
    pub timestamp: i64,
    /// The optional attachment and formatting fields shared with cIRC
    /// (§ Message fields): `imageUrl`, `gifUrl`, `audioAttachment`, `style` and
    /// the `/me` + `/dice` + `/8ball` + `/fortune` flags.
    ///
    /// [`MessageExtras::deleted`] is cIRC-only, so it is never set here: C-Mail
    /// has no delete endpoint in v0.8.4.
    #[serde(flatten)]
    pub extras: MessageExtras,
}

/// A C-Mail conversation summary.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailConversation {
    #[serde(alias = "id", default, deserialize_with = "null_as_default")]
    pub conversation_id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub other_user: CmailUser,
    #[serde(default, deserialize_with = "deserialize_last_message")]
    pub last_message: Option<CmailMessage>,
    /// Milliseconds since Unix epoch.
    #[serde(default)]
    pub last_message_at: Option<i64>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub unread_count: u32,
}

/// Response from `POST /v1/cmail/:conversationId`.
///
/// A normal send returns `{ conversationId, messageId }`; a slash command the
/// server answers inline (e.g. `/help`, v0.8.4) returns `{ reply }` and posts
/// nothing — so all fields are optional and either shape decodes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailSendResponse {
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    /// Inline command reply (e.g. from `/help`); the message was not posted.
    #[serde(default)]
    pub reply: Option<String>,
}

/// Response from `POST /v1/cmail/:conversationId/typing` (§ Typing Indicator).
///
/// `heartbeatMs` and `staleAfterMs` are the cadence the server is asking for:
/// refresh the flag every `heartbeatMs` while the user is still typing, and it
/// clears itself `staleAfterMs` after the last refresh, which is what stops a
/// client that quits mid-sentence from leaving "…is typing" stuck on the other
/// screen. The spec is explicit that both are read off this response rather than
/// hard-coded, so both are modelled as real fields; use [`heartbeat`] and
/// [`stale_after`] to get them as durations with a fallback for the case where
/// the server omitted them.
///
/// [`heartbeat`]: CmailTypingResponse::heartbeat
/// [`stale_after`]: CmailTypingResponse::stale_after
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailTypingResponse {
    /// Conversation the flag was set on.
    #[serde(default, deserialize_with = "null_as_default")]
    pub conversation_id: String,
    /// Whether the server recorded the flag.
    #[serde(default, deserialize_with = "null_as_default")]
    pub ok: bool,
    /// Milliseconds between refreshes; `0` when the server did not say.
    #[serde(default, deserialize_with = "null_as_default")]
    pub heartbeat_ms: u64,
    /// Milliseconds after the last refresh at which the flag goes stale; `0`
    /// when the server did not say.
    #[serde(default, deserialize_with = "null_as_default")]
    pub stale_after_ms: u64,
}

impl CmailTypingResponse {
    /// How often to re-post the flag while the user keeps typing.
    #[must_use]
    pub fn heartbeat(&self) -> Duration {
        Duration::from_millis(non_zero_or(self.heartbeat_ms, FALLBACK_TYPING_HEARTBEAT_MS))
    }

    /// How long the flag survives without a refresh. Pass this to
    /// [`CmailPresence::is_typing_at`] when reading the RTDB presence node.
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        Duration::from_millis(non_zero_or(
            self.stale_after_ms,
            FALLBACK_TYPING_STALE_AFTER_MS,
        ))
    }
}

/// Response from `GET /v1/cmail/:conversationId/typing` (§ Typing Indicator):
/// whether the *other* participant is typing right now.
///
/// The server has already applied the staleness rule, so `typing` is the answer
/// as of the moment of the call, not something to re-evaluate against `since`
/// (someone who has been composing for a minute has an old `since` and is still
/// typing). Re-evaluating on a timer is for the RTDB node, see
/// [`CmailPresence::is_typing_at`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailTypingStatus {
    /// Conversation this status is about.
    #[serde(default, deserialize_with = "null_as_default")]
    pub conversation_id: String,
    /// The other participant; empty when nobody is typing.
    #[serde(alias = "id", default, deserialize_with = "null_as_default")]
    pub user_id: String,
    /// The other participant's name; empty when nobody is typing.
    #[serde(default, deserialize_with = "null_as_default")]
    pub username: String,
    /// Whether the other participant is typing right now.
    #[serde(default, deserialize_with = "null_as_default")]
    pub typing: bool,
    /// Milliseconds since Unix epoch: when they started typing. Absent when
    /// nobody is.
    #[serde(default)]
    pub since: Option<i64>,
    /// Milliseconds after the last refresh at which a flag goes stale; `0` when
    /// the server did not say.
    #[serde(default, deserialize_with = "null_as_default")]
    pub stale_after_ms: u64,
}

impl CmailTypingStatus {
    /// The staleness window to apply to the RTDB presence node, with the same
    /// fallback as [`CmailTypingResponse::stale_after`].
    #[must_use]
    pub fn stale_after(&self) -> Duration {
        Duration::from_millis(non_zero_or(
            self.stale_after_ms,
            FALLBACK_TYPING_STALE_AFTER_MS,
        ))
    }
}

/// One entry of the `dm_presence/<conversationId>` RTDB node, which drives the
/// live typing indicator (§ Reading in real time).
///
/// The node is keyed by user id (`{ "<userId>": { username, typing, timestamp } }`),
/// so `user_id` is the map key rather than a field in the payload;
/// [`cmail_presence_updates_from_rtdb_event`] injects it.
///
/// This is the *whole* entry, which only a `put` carries. A `patch` carries
/// [`CmailPresencePatch`], which merges into the copy you already hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailPresence {
    /// Their user id, injected from the RTDB key.
    #[serde(alias = "id", default, deserialize_with = "null_as_default")]
    pub user_id: String,
    /// Their handle.
    #[serde(default, deserialize_with = "null_as_default")]
    pub username: String,
    /// The raw flag. Do not show it on its own: it stays `true` in the database
    /// until it ages out, so it has to be paired with `timestamp`, which is what
    /// [`is_typing_at`](CmailPresence::is_typing_at) does.
    #[serde(default, deserialize_with = "null_as_default")]
    pub typing: bool,
    /// Milliseconds since Unix epoch: when the flag was last refreshed.
    #[serde(default, deserialize_with = "null_as_default")]
    pub timestamp: i64,
}

impl CmailPresence {
    /// The spec's staleness rule: this participant counts as typing only if
    /// `typing` is set *and* `timestamp` is newer than the staleness window.
    ///
    /// `now_ms` is the current time in milliseconds since the Unix epoch, and
    /// `stale_after` comes from [`CmailTypingResponse::stale_after`]. A flag
    /// going stale produces no RTDB event, so callers re-evaluate this on a
    /// timer as well as on each event.
    ///
    /// An entry with no `timestamp` reads as stale, which is the fail-safe
    /// answer: at worst the indicator is not shown.
    #[must_use]
    pub fn is_typing_at(&self, now_ms: i64, stale_after: Duration) -> bool {
        if !self.typing {
            return false;
        }
        // An absent `timestamp` decodes as 0, which is not a refresh time any
        // server would send, so a partial patch carrying only `typing: true`
        // can never raise the indicator on its own.
        if self.timestamp <= 0 {
            return false;
        }
        let stale_after_ms = i64::try_from(stale_after.as_millis()).unwrap_or(i64::MAX);
        // A timestamp in the future (clock skew) yields a negative age, which is
        // "newer than the window" and so still typing.
        now_ms.saturating_sub(self.timestamp) < stale_after_ms
    }
}

/// A *partial* presence entry: the payload of an RTDB `patch` event on
/// `dm_presence/<conversationId>` (§ Reading in real time).
///
/// Every field is optional, and one that is absent must be left alone. A
/// heartbeat that only moves `timestamp` says nothing about the username beside
/// it or about the flag itself, so decoding it as a whole [`CmailPresence`]
/// would invent an empty username and an unset `typing` and take a live
/// "…is typing" indicator down.
///
/// Mirrors `CircPresencePatch`, which cIRC uses for the same node shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailPresencePatch {
    /// The participant's name, when the patch carries it.
    #[serde(default)]
    pub username: Option<String>,
    /// The raw flag, when the patch carries it. Still not something to show on
    /// its own, see [`CmailPresence::is_typing_at`].
    #[serde(default)]
    pub typing: Option<bool>,
    /// Milliseconds since Unix epoch: when the flag was last refreshed. `None`
    /// means "not in this patch", not "cleared".
    #[serde(default)]
    pub timestamp: Option<i64>,
}

impl CmailPresencePatch {
    /// Whether the patch changes nothing, in which case there is no update to
    /// report.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Merge this patch into `entry`, leaving every field it does not mention
    /// alone. The user id is not merged: it identifies the entry and travels on
    /// [`CmailPresenceUpdate::Partial`].
    pub fn apply_to(&self, entry: &mut CmailPresence) {
        if let Some(username) = &self.username {
            entry.username.clone_from(username);
        }
        if let Some(typing) = self.typing {
            entry.typing = typing;
        }
        if let Some(timestamp) = self.timestamp {
            entry.timestamp = timestamp;
        }
    }
}

/// One live change to a conversation's typing indicator, decoded from a single
/// RTDB event on `dm_presence/<conversationId>` (§ Reading in real time).
///
/// Mirrors `CircPresenceUpdate`: the two presence paths differ only in which
/// node they read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmailPresenceUpdate {
    /// A whole entry, from a `put`. Insert it, or replace the one held for the
    /// same user id.
    Full(CmailPresence),
    /// A change to the entry for this user id, from a `patch`. Merge it with
    /// [`CmailPresencePatch::apply_to`]; if you hold no entry for them, ignore
    /// it, since a fragment is not an entry.
    Partial {
        /// Who the patch is about.
        user_id: String,
        /// The changed fields, to merge into the entry you hold.
        patch: CmailPresencePatch,
    },
    /// The entry for this user id is gone: they cleared the flag with
    /// `DELETE /v1/cmail/:conversationId/typing`, or the server expired it.
    /// Drop what you hold, which takes the indicator down at once rather than
    /// waiting for it to age out.
    Removed {
        /// Whose indicator came down.
        user_id: String,
    },
}

impl CmailPresenceUpdate {
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
    pub fn as_full(&self) -> Option<&CmailPresence> {
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

impl Client {
    /// `POST /v1/cmail` — start or get a 1:1 conversation by username or user id.
    pub async fn start_cmail_conversation(
        &self,
        request: &CmailStartRequest,
    ) -> Result<CmailConversation> {
        if request.recipient_username.is_none() && request.recipient_id.is_none() {
            return Err(ApiError::Config(
                "recipientUsername or recipientId is required".into(),
            ));
        }
        self.request(
            EndpointKey::CmailStart,
            Method::POST,
            "/v1/cmail",
            &[],
            Some(request),
        )
        .await
    }

    /// Convenience wrapper for `POST /v1/cmail` with `recipientUsername`.
    pub async fn start_cmail_conversation_by_username(
        &self,
        username: &str,
    ) -> Result<CmailConversation> {
        self.start_cmail_conversation(&CmailStartRequest::by_username(username))
            .await
    }

    /// Convenience wrapper for `POST /v1/cmail` with `recipientId`.
    pub async fn start_cmail_conversation_by_user_id(
        &self,
        user_id: &str,
    ) -> Result<CmailConversation> {
        self.start_cmail_conversation(&CmailStartRequest::by_user_id(user_id))
            .await
    }

    /// `GET /v1/cmail` — list the caller's conversations.
    pub async fn list_cmail_conversations(&self) -> Result<Vec<CmailConversation>> {
        let value: Value = self
            .request(
                EndpointKey::CmailList,
                Method::GET,
                "/v1/cmail",
                &[],
                None::<&()>,
            )
            .await?;
        decode_conversation_list(value)
    }

    /// `GET /v1/cmail/:conversationId` — load message history, oldest first.
    /// Pass `before` as the previous cursor timestamp to page older messages.
    ///
    /// The spec derives the next-page cursor from "the oldest message's
    /// timestamp". If the server omits an explicit `cursor` but returned a full
    /// page, we synthesise one from the oldest message so scroll-back still works;
    /// a short page means history is exhausted and the cursor stays `None`.
    pub async fn read_cmail_conversation(
        &self,
        conversation_id: &str,
        before: Option<i64>,
        limit: Option<u32>,
    ) -> Result<(Vec<CmailMessage>, Option<String>)> {
        let limit = limit
            .unwrap_or(DEFAULT_MESSAGE_LIMIT)
            .clamp(1, MAX_MESSAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        let path = format!("/v1/cmail/{conversation_id}");
        let (messages, cursor): (Vec<CmailMessage>, Option<String>) = self
            .request_page(EndpointKey::CmailRead, Method::GET, &path, &query)
            .await?;
        let cursor = cursor.or_else(|| next_cursor_fallback(&messages, limit));
        Ok((messages, cursor))
    }

    /// `POST /v1/cmail/:conversationId` — send a message.
    ///
    /// Slash commands are expanded server-side (§ Commands), so `content` is
    /// sent as typed and nothing here second-guesses it. C-Mail understands
    /// `/me`, the emotes, `/dice`, `/8ball`, `/fortune`, `/gif`, `/song`, the
    /// text styles and `/help`; `/art` and the `/mute` family are cIRC-only and
    /// come back as a `400` from the server.
    pub async fn send_cmail_message(
        &self,
        conversation_id: &str,
        content: &str,
    ) -> Result<CmailSendResponse> {
        validate_cmail_content(content)?;
        let body = SendMessageBody { content };
        let path = format!("/v1/cmail/{conversation_id}");
        self.request(
            EndpointKey::CmailSend,
            Method::POST,
            &path,
            &[],
            Some(&body),
        )
        .await
    }

    /// `POST /v1/cmail/:conversationId/read` — reset your unread count.
    pub async fn mark_cmail_read(&self, conversation_id: &str) -> Result<()> {
        let path = format!("/v1/cmail/{conversation_id}/read");
        self.request_unit(EndpointKey::CmailMarkRead, Method::POST, &path, &[])
            .await
    }

    /// `POST /v1/cmail/:conversationId/typing`: tell the other participant you
    /// are composing a message (§ Typing Indicator). No body; the username is
    /// set from the authenticated account.
    ///
    /// The flag is short-lived on purpose. Re-post it every
    /// [`CmailTypingResponse::heartbeat`] while the user is still typing, and it
    /// clears itself after [`CmailTypingResponse::stale_after`] once you stop.
    /// Read both off the response rather than assuming a cadence.
    ///
    /// Sending a message clears the flag server-side, so there is no need to
    /// call [`clear_cmail_typing`](Client::clear_cmail_typing) first.
    ///
    /// Rate-limited per conversation as well as overall, so the
    /// `conversationId` is passed as the limiter scope.
    pub async fn set_cmail_typing(&self, conversation_id: &str) -> Result<CmailTypingResponse> {
        let path = format!("/v1/cmail/{conversation_id}/typing");
        self.request_scoped(
            EndpointKey::CmailTyping,
            Some(conversation_id),
            Method::POST,
            &path,
            &[],
            None::<&()>,
        )
        .await
    }

    /// `DELETE /v1/cmail/:conversationId/typing`: clear the flag immediately
    /// (§ Typing Indicator).
    ///
    /// Call it when the input goes idle or the conversation is closed, rather
    /// than waiting for the flag to age out. Shares the one typing budget with
    /// [`set_cmail_typing`](Client::set_cmail_typing), scoped by conversation.
    /// The response body is only `{ conversationId, ok }`, which tells the
    /// caller nothing it does not already know, so it is discarded.
    pub async fn clear_cmail_typing(&self, conversation_id: &str) -> Result<()> {
        let path = format!("/v1/cmail/{conversation_id}/typing");
        self.request_unit_scoped(
            EndpointKey::CmailTyping,
            Some(conversation_id),
            Method::DELETE,
            &path,
            &[],
        )
        .await
    }

    /// `GET /v1/cmail/:conversationId/typing`: whether the other participant is
    /// typing right now (§ Typing Indicator).
    ///
    /// A polling convenience. For a live indicator, subscribe to the
    /// `dm_presence/<conversationId>` RTDB node instead of calling this on a
    /// timer, and decode the events with
    /// [`cmail_presence_updates_from_rtdb_event`]. This endpoint is unscoped: unlike the
    /// two writes, its budget has no per-conversation dimension.
    pub async fn read_cmail_typing(&self, conversation_id: &str) -> Result<CmailTypingStatus> {
        let path = format!("/v1/cmail/{conversation_id}/typing");
        self.request(
            EndpointKey::CmailTypingRead,
            Method::GET,
            &path,
            &[],
            None::<&()>,
        )
        .await
    }
}

/// Derive a next-page cursor when the server didn't send one. Messages come
/// oldest-first, so the oldest (first) message's timestamp is the `before` value
/// for the next page. Only a full page implies more history; a short page is the
/// end of the conversation.
fn next_cursor_fallback(messages: &[CmailMessage], limit: u32) -> Option<String> {
    if messages.len() < limit as usize {
        return None;
    }
    messages.first().map(|m| m.timestamp.to_string())
}

/// Parse the messages carried by an RTDB `put`/`patch` event on a
/// `dm_messages/<conversationId>` subscription into [`CmailMessage`]s.
///
/// Firebase keys each message by id, so the id is the map key (root events) or
/// the final path segment (single-message events), not a field in the payload —
/// this injects it. Deletions (`data: null`), deeper field patches and partial
/// payloads carry no whole message and yield nothing.
#[must_use]
pub fn messages_from_rtdb_event(path: &str, data: &Value) -> Vec<CmailMessage> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        // Root event: `data` is a map of `{ <msgId>: <message>, ... }`.
        [] => match data {
            Value::Object(map) => map
                .iter()
                .filter_map(|(id, value)| message_from_rtdb_value(id, value))
                .collect(),
            _ => Vec::new(),
        },
        // Single-message event: the id is the (only) path segment.
        [id] => message_from_rtdb_value(id, data).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn message_from_rtdb_value(id: &str, value: &Value) -> Option<CmailMessage> {
    let Value::Object(map) = value else {
        return None;
    };
    if !is_whole_message(map) {
        return None;
    }
    let mut message: CmailMessage = serde_json::from_value(value.clone()).ok()?;
    if message.id.is_empty() {
        message.id = id.to_string();
    }
    Some(message)
}

/// Whether an RTDB payload is a whole message rather than a patch of one or two
/// fields of one.
///
/// Every stored message carries a sender and a `timestamp`, so a payload missing
/// either is a partial patch. C-Mail has no edits and no deletions in v0.8.4, so
/// there is nothing to merge and the patch is dropped: a caller that replaces by
/// id keeps the copy it already holds instead of overwriting it with a
/// mostly-default message. (cIRC needs the same guard for real, since its delete
/// tombstone arrives exactly this way.)
fn is_whole_message(map: &Map<String, Value>) -> bool {
    let has_sender = ["senderId", "senderUid", "senderUsername"]
        .iter()
        .any(|key| map.contains_key(*key));
    has_sender && map.contains_key("timestamp")
}

/// Decode one RTDB `put`/`patch` event on a `dm_presence/<conversationId>`
/// subscription into the typing-indicator changes it carries
/// (§ Reading in real time).
///
/// `kind` is what decides how the payload is read, and it matters: a `put`
/// replaces the value at its path, so its object is a whole entry, while a
/// `patch` merges only the keys it carries, so its object is a fragment.
/// Decoding a patch as if it were a whole entry invents empty fields, and a
/// heartbeat that only moves `timestamp` would then blank the participant's
/// username and take a live "…is typing" indicator down.
///
/// The node is keyed by user id and the value carries no id of its own, so the
/// id is injected from the map key (a root-path event) or from a path segment.
/// All three shapes are handled, exactly as in
/// [`circ_presence_updates_from_rtdb_event`](crate::circ_presence_updates_from_rtdb_event):
///
/// - `/` with `{ "<userId>": {...}, … }`, one update per entry,
/// - `/<userId>` with the entry or a fragment of it,
/// - `/<userId>/<field>` with a single value, which Firebase sends for a leaf
///   write. It is rebuilt into a one-field patch, since one field is never a
///   whole entry.
///
/// A `null` at a user's path is a [`Removed`](CmailPresenceUpdate::Removed):
/// that participant cleared their flag with `DELETE .../typing`, so the
/// indicator comes down at once rather than waiting for it to age out. A `null`
/// at the root names nobody and yields nothing, deliberately: entries also
/// expire through [`CmailPresence::is_typing_at`], which the spec requires you
/// to re-evaluate on a timer anyway.
#[must_use]
pub fn cmail_presence_updates_from_rtdb_event(
    kind: SseEventKind,
    path: &str,
    data: &Value,
) -> Vec<CmailPresenceUpdate> {
    let partial = matches!(kind, SseEventKind::Patch);
    match rtdb_path_segments(path).as_slice() {
        // Root event: `data` is a map of `{ <userId>: <entry>, ... }`.
        [] => match data {
            Value::Object(map) => map
                .iter()
                .filter_map(|(user_id, value)| presence_update(partial, user_id, value))
                .collect(),
            _ => Vec::new(),
        },
        // Single-participant event: the user id is the (only) path segment.
        [user_id] => presence_update(partial, user_id, data)
            .into_iter()
            .collect(),
        // Leaf write: one field of one participant's entry, never an entry.
        [user_id, field] => presence_update(true, user_id, &one_field_object(field, data))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn presence_update(partial: bool, user_id: &str, value: &Value) -> Option<CmailPresenceUpdate> {
    if user_id.is_empty() {
        return None;
    }
    match value {
        Value::Null => Some(CmailPresenceUpdate::Removed {
            user_id: user_id.to_string(),
        }),
        Value::Object(_) if !partial => {
            let mut entry: CmailPresence = serde_json::from_value(value.clone()).ok()?;
            if entry.user_id.is_empty() {
                entry.user_id = user_id.to_string();
            }
            Some(CmailPresenceUpdate::Full(entry))
        }
        Value::Object(_) => {
            let patch: CmailPresencePatch = serde_json::from_value(value.clone()).ok()?;
            if patch.is_empty() {
                return None;
            }
            Some(CmailPresenceUpdate::Partial {
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

/// Rebuild a leaf write (`path: "/<userId>/<field>"`, `data: <value>`) into the
/// one-field object [`CmailPresencePatch`] decodes from.
fn one_field_object(field: &str, value: &Value) -> Value {
    let mut object = Map::with_capacity(1);
    object.insert(field.to_string(), value.clone());
    Value::Object(object)
}

/// `value` unless it is zero, in which case `fallback`. Used for the typing
/// cadence, where zero means the server left the field out.
fn non_zero_or(value: u64, fallback: u64) -> u64 {
    if value == 0 {
        fallback
    } else {
        value
    }
}

fn validate_cmail_content(content: &str) -> Result<()> {
    if content.trim().is_empty() {
        return Err(ApiError::Config("C-Mail message cannot be empty".into()));
    }
    if content.chars().count() > MAX_MESSAGE_LEN {
        return Err(ApiError::Config(format!(
            "C-Mail message exceeds {MAX_MESSAGE_LEN} characters"
        )));
    }
    Ok(())
}

fn decode_conversation_list(value: Value) -> Result<Vec<CmailConversation>> {
    match value {
        Value::Array(_) => serde_json::from_value(value).map_err(ApiError::from),
        Value::Object(mut obj) => {
            for key in ["conversations", "items", "results"] {
                if let Some(v) = obj.remove(key) {
                    return decode_conversation_list(v);
                }
            }
            let mut out = Vec::with_capacity(obj.len());
            for (id, mut v) in obj {
                if let Value::Object(ref mut m) = v {
                    m.entry("conversationId".to_string())
                        .or_insert(Value::String(id));
                }
                out.push(serde_json::from_value(v)?);
            }
            Ok(out)
        }
        Value::Null => Ok(Vec::new()),
        other => serde_json::from_value(other).map_err(ApiError::from),
    }
}

fn deserialize_last_message<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<CmailMessage>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(content)) if content.is_empty() => Ok(None),
        Some(Value::String(content)) => Ok(Some(CmailMessage {
            content,
            ..CmailMessage::default()
        })),
        Some(value @ Value::Object(_)) => serde_json::from_value(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_request_by_user_id_serializes() {
        let start = CmailStartRequest::by_user_id("uid");
        let v = serde_json::to_value(&start).unwrap();
        assert_eq!(v["recipientId"], "uid");
        assert!(v.get("recipientUsername").is_none());
    }

    #[test]
    fn message_decodes_timestamp_millis() {
        // A stray `read` (from a pre-v0.7 server) is simply ignored.
        let msg: CmailMessage = serde_json::from_str(
            r#"{"id":"m1","senderId":"u1","senderUsername":"me","content":"hi","timestamp":1719700000000,"read":false}"#,
        )
        .unwrap();
        assert_eq!(msg.timestamp, 1_719_700_000_000);
        assert_eq!(msg.content, "hi");
    }

    #[test]
    fn send_response_decodes_posted_and_command_reply() {
        let sent: CmailSendResponse =
            serde_json::from_str(r#"{"conversationId":"c1","messageId":"m1"}"#).unwrap();
        assert_eq!(sent.conversation_id.as_deref(), Some("c1"));
        assert_eq!(sent.message_id.as_deref(), Some("m1"));
        assert!(sent.reply.is_none());

        // A `/help` command answered inline (posts nothing).
        let help: CmailSendResponse = serde_json::from_str(r#"{"reply":"commands: …"}"#).unwrap();
        assert_eq!(help.reply.as_deref(), Some("commands: …"));
        assert!(help.message_id.is_none());
    }

    #[test]
    fn conversation_decodes_minimal_shape() {
        let c: CmailConversation = serde_json::from_str(
            r#"{"conversationId":"c1","otherUser":{"id":"u2","username":"alice"},"unreadCount":2}"#,
        )
        .unwrap();
        assert_eq!(c.conversation_id, "c1");
        assert_eq!(c.other_user.user_id, "u2");
        assert_eq!(c.unread_count, 2);
    }

    #[test]
    fn conversation_decodes_string_last_message_preview() {
        let c: CmailConversation = serde_json::from_str(
            r#"{"conversationId":"c1","otherUser":{"userId":"u2","username":"alice"},"lastMessage":"hello","lastMessageAt":1781530308271,"unreadCount":0}"#,
        )
        .unwrap();
        assert_eq!(c.last_message.unwrap().content, "hello");
    }

    #[test]
    fn conversation_decodes_empty_last_message_preview_as_none() {
        let c: CmailConversation = serde_json::from_str(
            r#"{"conversationId":"c1","otherUser":{"userId":"u2","username":"alice"},"lastMessage":"","lastMessageAt":1781530308271,"unreadCount":0}"#,
        )
        .unwrap();
        assert!(c.last_message.is_none());
    }

    #[test]
    fn conversation_list_decodes_array_wrapper() {
        let v = serde_json::json!({
            "conversations": [
                {"conversationId":"c1","otherUser":{"userId":"u2","username":"alice"}}
            ]
        });
        let list = decode_conversation_list(v).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].conversation_id, "c1");
    }

    #[test]
    fn conversation_list_decodes_rtdb_style_map() {
        let v = serde_json::json!({
            "c1": {"otherUser":{"id":"u2","username":"alice"},"unreadCount":1}
        });
        let list = decode_conversation_list(v).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].conversation_id, "c1");
        assert_eq!(list[0].unread_count, 1);
    }

    #[test]
    fn next_cursor_fallback_uses_oldest_timestamp_only_on_a_full_page() {
        let full = vec![
            CmailMessage {
                timestamp: 1_000,
                ..CmailMessage::default()
            },
            CmailMessage {
                timestamp: 2_000,
                ..CmailMessage::default()
            },
        ];
        // Full page (len == limit) → cursor is the oldest (first) timestamp.
        assert_eq!(next_cursor_fallback(&full, 2).as_deref(), Some("1000"));
        // Short page → history exhausted, no cursor.
        assert_eq!(next_cursor_fallback(&full, 50), None);
        assert_eq!(next_cursor_fallback(&[], 50), None);
    }

    #[test]
    fn rtdb_root_event_parses_message_map_with_ids_from_keys() {
        let data = serde_json::json!({
            "m1": {"senderId":"u2","senderUsername":"alice","content":"hi","timestamp":1_000},
            "m2": {"senderId":"u1","senderUsername":"me","content":"yo","timestamp":2_000}
        });
        let mut msgs = messages_from_rtdb_event("/", &data);
        msgs.sort_by_key(|m| m.timestamp);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, "m1");
        assert_eq!(msgs[0].content, "hi");
        assert_eq!(msgs[1].id, "m2");
    }

    #[test]
    fn rtdb_single_message_event_takes_id_from_path() {
        let data = serde_json::json!(
            {"senderId":"u2","senderUsername":"alice","content":"you there?","timestamp":3_000}
        );
        assert_eq!(
            messages_from_rtdb_event("/m3", &data),
            vec![CmailMessage {
                id: "m3".into(),
                sender_id: "u2".into(),
                sender_username: "alice".into(),
                content: "you there?".into(),
                timestamp: 3_000,
                ..CmailMessage::default()
            }]
        );
    }

    #[test]
    fn rtdb_deletions_yield_nothing() {
        // `data: null` (a deletion) carries no whole message.
        assert!(messages_from_rtdb_event("/m3", &Value::Null).is_empty());
    }

    #[test]
    fn rtdb_partial_patch_yields_nothing() {
        // A patch of one field is not a message: emitting it would replace the
        // held copy with a mostly-default one.
        let content_only = serde_json::json!({"content": "edited"});
        assert!(messages_from_rtdb_event("/m3", &content_only).is_empty());

        // Even a sender without a timestamp is too little to stand alone.
        let no_timestamp = serde_json::json!({"senderId": "u2", "content": "hi"});
        assert!(messages_from_rtdb_event("/m3", &no_timestamp).is_empty());

        // A partial entry inside a root map is skipped, the whole ones survive.
        let mixed = serde_json::json!({
            "m1": {"senderId":"u2","senderUsername":"alice","content":"hi","timestamp":1_000},
            "m2": {"content":"[DELETED]","deleted":true}
        });
        let msgs = messages_from_rtdb_event("/", &mixed);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "m1");

        // Deeper field paths were already ignored and still are.
        assert!(messages_from_rtdb_event("/m1/content", &serde_json::json!("edited")).is_empty());
    }

    #[test]
    fn message_decodes_attachment_and_command_extras() {
        let msg: CmailMessage = serde_json::from_str(
            r#"{"id":"m1","senderId":"u1","senderUsername":"alice","content":"","timestamp":1719700000000,
                "gifUrl":"https://cdn.example/a.gif","style":["comic","rainbow"],"isAction":true,
                "isDice":false,"isEightball":true,"eightballAnswer":"Ask again later",
                "isFortune":false}"#,
        )
        .unwrap();
        assert_eq!(msg.id, "m1");
        assert!(msg.extras.has_attachment());
        assert_eq!(
            msg.extras.gif_url.as_deref(),
            Some("https://cdn.example/a.gif")
        );
        assert!(msg.extras.style.as_ref().unwrap().contains("rainbow"));
        assert!(msg.extras.is_action);
        assert!(msg.extras.is_eightball);
        assert_eq!(
            msg.extras.eightball_answer.as_deref(),
            Some("Ask again later")
        );
        // An attachment can be the whole message, so there is no text to print.
        assert_eq!(msg.extras.display_content(&msg.content), None);
    }

    #[test]
    fn message_decodes_song_attachment_and_image() {
        let msg: CmailMessage = serde_json::from_str(
            r#"{"id":"m2","senderId":"u1","content":"listen to this","timestamp":1,
                "imageUrl":"https://cdn.example/pic.png",
                "audioAttachment":{"src":"https://youtu.be/x","origin":"youtube","artist":"A","title":"T"}}"#,
        )
        .unwrap();
        assert_eq!(
            msg.extras.image_url.as_deref(),
            Some("https://cdn.example/pic.png")
        );
        let audio = msg
            .extras
            .audio_attachment
            .as_ref()
            .expect("audioAttachment");
        assert_eq!(audio.artist, "A");
        assert!(audio.genre.is_none());
        assert_eq!(
            msg.extras.display_content(&msg.content),
            Some("listen to this")
        );
    }

    #[test]
    fn plain_message_carries_no_extras() {
        let msg: CmailMessage = serde_json::from_str(
            r#"{"id":"m1","senderId":"u1","senderUsername":"me","content":"hi","timestamp":1}"#,
        )
        .unwrap();
        assert_eq!(msg.extras, MessageExtras::default());
        assert!(!msg.extras.has_attachment());
        assert!(!msg.extras.is_art());
        // `deleted` is cIRC-only; a C-Mail message never sets it.
        assert!(!msg.extras.deleted);
    }

    #[test]
    fn typing_response_decodes_cadence() {
        let resp: CmailTypingResponse = serde_json::from_str(
            r#"{"conversationId":"c1","ok":true,"heartbeatMs":3000,"staleAfterMs":9000}"#,
        )
        .unwrap();
        assert_eq!(resp.conversation_id, "c1");
        assert!(resp.ok);
        assert_eq!(resp.heartbeat_ms, 3_000);
        assert_eq!(resp.heartbeat(), Duration::from_secs(3));
        assert_eq!(resp.stale_after(), Duration::from_secs(9));
    }

    #[test]
    fn typing_response_falls_back_when_cadence_absent() {
        let resp: CmailTypingResponse =
            serde_json::from_str(r#"{"conversationId":"c1","ok":true}"#).unwrap();
        assert_eq!(resp.heartbeat_ms, 0);
        assert_eq!(
            resp.heartbeat(),
            Duration::from_millis(FALLBACK_TYPING_HEARTBEAT_MS)
        );
        assert_eq!(
            resp.stale_after(),
            Duration::from_millis(FALLBACK_TYPING_STALE_AFTER_MS)
        );
    }

    #[test]
    fn typing_status_decodes_both_answers() {
        let typing: CmailTypingStatus = serde_json::from_str(
            r#"{"conversationId":"c1","userId":"u2","typing":true,"username":"alice","since":1719700000000,"staleAfterMs":9000}"#,
        )
        .unwrap();
        assert!(typing.typing);
        assert_eq!(typing.user_id, "u2");
        assert_eq!(typing.username, "alice");
        assert_eq!(typing.since, Some(1_719_700_000_000));
        assert_eq!(typing.stale_after(), Duration::from_secs(9));

        // Nobody typing: the server may send only the flag.
        let idle: CmailTypingStatus =
            serde_json::from_str(r#"{"conversationId":"c1","typing":false}"#).unwrap();
        assert!(!idle.typing);
        assert!(idle.username.is_empty());
        assert!(idle.since.is_none());
        assert_eq!(
            idle.stale_after(),
            Duration::from_millis(FALLBACK_TYPING_STALE_AFTER_MS)
        );
    }

    #[test]
    fn presence_root_event_keys_entries_by_user_id() {
        let data = serde_json::json!({
            "u2": {"username":"alice","typing":true,"timestamp":1_000},
            "u1": {"username":"me","typing":false,"timestamp":900}
        });
        let mut updates = cmail_presence_updates_from_rtdb_event(SseEventKind::Put, "/", &data);
        updates.sort_by(|a, b| a.user_id().cmp(b.user_id()));
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].user_id(), "u1");
        let alice = updates[1].as_full().expect("whole entry");
        assert_eq!(alice.user_id, "u2");
        assert_eq!(alice.username, "alice");
        assert!(alice.typing);
        assert_eq!(alice.timestamp, 1_000);
    }

    #[test]
    fn presence_single_entry_event_takes_user_id_from_path() {
        let data = serde_json::json!({"username":"alice","typing":true,"timestamp":2_000});
        assert_eq!(
            cmail_presence_updates_from_rtdb_event(SseEventKind::Put, "/u2", &data),
            vec![CmailPresenceUpdate::Full(CmailPresence {
                user_id: "u2".into(),
                username: "alice".into(),
                typing: true,
                timestamp: 2_000,
            })]
        );
    }

    #[test]
    fn presence_null_clears_that_participant() {
        // `DELETE .../typing` removes the node, which must take the indicator
        // down rather than leave it up until it ages out.
        assert_eq!(
            cmail_presence_updates_from_rtdb_event(SseEventKind::Put, "/u2", &Value::Null),
            vec![CmailPresenceUpdate::Removed {
                user_id: "u2".into()
            }]
        );

        // A null at the root names nobody.
        assert!(
            cmail_presence_updates_from_rtdb_event(SseEventKind::Put, "/", &Value::Null).is_empty()
        );
        // Nothing below a participant's own fields is addressable.
        assert!(cmail_presence_updates_from_rtdb_event(
            SseEventKind::Patch,
            "/u2/typing/nested",
            &Value::Bool(true)
        )
        .is_empty());
    }

    #[test]
    fn presence_patch_merges_into_the_held_entry_rather_than_replacing_it() {
        // A heartbeat that only refreshes the timestamp. Decoding it as a whole
        // entry blanked the username and cleared `typing`, which took a live
        // "…is typing" indicator down between heartbeats.
        let updates = cmail_presence_updates_from_rtdb_event(
            SseEventKind::Patch,
            "/u2",
            &serde_json::json!({"timestamp": 5_000}),
        );
        let [CmailPresenceUpdate::Partial { user_id, patch }] = updates.as_slice() else {
            panic!("expected one partial update, got {updates:?}");
        };
        assert_eq!(user_id, "u2");
        assert_eq!(patch.timestamp, Some(5_000));
        assert_eq!(patch.username, None);
        assert_eq!(patch.typing, None);

        let mut held = CmailPresence {
            user_id: "u2".into(),
            username: "alice".into(),
            typing: true,
            timestamp: 1_000,
        };
        patch.apply_to(&mut held);
        assert_eq!(held.username, "alice");
        assert!(held.typing);
        assert_eq!(held.timestamp, 5_000);
        // And the indicator survives the heartbeat.
        assert!(held.is_typing_at(6_000, Duration::from_secs(9)));
    }

    #[test]
    fn presence_put_still_replaces_the_entry_wholesale() {
        // A `put` really does replace the value at its path, so an entry that
        // arrives without a username has none.
        let updates = cmail_presence_updates_from_rtdb_event(
            SseEventKind::Put,
            "/u2",
            &serde_json::json!({"timestamp": 5_000}),
        );
        assert_eq!(
            updates,
            vec![CmailPresenceUpdate::Full(CmailPresence {
                user_id: "u2".into(),
                username: String::new(),
                typing: false,
                timestamp: 5_000,
            })]
        );
    }

    #[test]
    fn presence_patch_cannot_invent_a_typing_indicator() {
        // A patch that raises the flag without a timestamp merges into an entry
        // whose timestamp is still 0, which the staleness rule reads as stale.
        let updates = cmail_presence_updates_from_rtdb_event(
            SseEventKind::Patch,
            "/u2",
            &serde_json::json!({"typing": true}),
        );
        let [CmailPresenceUpdate::Partial { user_id, patch }] = updates.as_slice() else {
            panic!("expected one partial update, got {updates:?}");
        };
        assert_eq!(user_id, "u2");
        let mut held = CmailPresence {
            user_id: "u2".into(),
            ..CmailPresence::default()
        };
        patch.apply_to(&mut held);
        assert!(held.typing);
        assert!(!held.is_typing_at(5_000, Duration::from_secs(9)));
    }

    #[test]
    fn presence_leaf_write_decodes_as_a_one_field_patch() {
        // Firebase delivers a single-field write on the deep path. One field is
        // never a whole entry, whichever kind carries it.
        for kind in [SseEventKind::Put, SseEventKind::Patch] {
            let updates =
                cmail_presence_updates_from_rtdb_event(kind, "/u2/typing", &Value::Bool(false));
            assert_eq!(
                updates,
                vec![CmailPresenceUpdate::Partial {
                    user_id: "u2".into(),
                    patch: CmailPresencePatch {
                        typing: Some(false),
                        ..CmailPresencePatch::default()
                    },
                }]
            );
        }
    }

    #[test]
    fn presence_patch_that_changes_nothing_yields_nothing() {
        // An empty object, or one carrying only keys this node does not use.
        assert!(cmail_presence_updates_from_rtdb_event(
            SseEventKind::Patch,
            "/u2",
            &serde_json::json!({})
        )
        .is_empty());
        assert!(cmail_presence_updates_from_rtdb_event(
            SseEventKind::Patch,
            "/u2",
            &serde_json::json!({"somethingElse": 1})
        )
        .is_empty());
        // A payload that is not an object carries no entry either.
        assert!(cmail_presence_updates_from_rtdb_event(
            SseEventKind::Put,
            "/u2",
            &serde_json::json!("typing")
        )
        .is_empty());
    }

    #[test]
    fn presence_root_patch_merges_each_participant() {
        // A root `patch` names several participants at once, and each value is
        // still only the keys that changed.
        let data = serde_json::json!({
            "u1": {"timestamp": 2_000},
            "u2": null
        });
        let mut updates = cmail_presence_updates_from_rtdb_event(SseEventKind::Patch, "/", &data);
        updates.sort_by(|a, b| a.user_id().cmp(b.user_id()));
        assert!(matches!(
            updates.as_slice(),
            [
                CmailPresenceUpdate::Partial { .. },
                CmailPresenceUpdate::Removed { .. }
            ]
        ));
        assert!(updates[0].as_full().is_none());
    }

    #[test]
    fn presence_entry_tolerates_explicit_nulls() {
        // The server sends explicit nulls (§ Who's in a room documents one for
        // cIRC), and over RTDB a decode failure drops the event silently.
        let updates = cmail_presence_updates_from_rtdb_event(
            SseEventKind::Put,
            "/u2",
            &serde_json::json!({"username": null, "typing": null, "timestamp": 5_000}),
        );
        assert_eq!(
            updates,
            vec![CmailPresenceUpdate::Full(CmailPresence {
                user_id: "u2".into(),
                username: String::new(),
                typing: false,
                timestamp: 5_000,
            })]
        );

        // And a null inside a root map does not sink the participant beside it.
        let root = serde_json::json!({
            "u1": {"username": "me", "typing": true, "timestamp": 1_000},
            "u2": {"username": null, "typing": true, "timestamp": null}
        });
        assert_eq!(
            cmail_presence_updates_from_rtdb_event(SseEventKind::Put, "/", &root).len(),
            2
        );
    }

    #[test]
    fn message_tolerates_explicit_nulls() {
        let msg: CmailMessage = serde_json::from_str(
            r#"{"id":null,"senderId":null,"senderUsername":null,"content":null,"timestamp":null}"#,
        )
        .unwrap();
        assert_eq!(msg, CmailMessage::default());
    }

    #[test]
    fn one_null_no_longer_sinks_a_page_of_messages() {
        // History decodes as a single `Vec`, so a null on one message used to
        // cost the caller every message on the page.
        let page: Vec<CmailMessage> = serde_json::from_str(
            r#"[{"id":"m1","senderId":"u1","senderUsername":"me","content":"hi","timestamp":1000},
                {"id":"m2","senderId":"u2","senderUsername":"alice","content":null,"timestamp":2000}]"#,
        )
        .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].content, "hi");
        assert!(page[1].content.is_empty());
        assert_eq!(page[1].timestamp, 2_000);
    }

    #[test]
    fn conversation_list_tolerates_explicit_nulls() {
        let v = serde_json::json!({
            "conversations": [
                {"conversationId":"c1","otherUser":{"userId":"u2","username":"alice"},"unreadCount":3},
                {"conversationId":"c2","otherUser":null,"unreadCount":null}
            ]
        });
        let list = decode_conversation_list(v).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].unread_count, 3);
        assert_eq!(list[1].conversation_id, "c2");
        assert_eq!(list[1].unread_count, 0);
        assert!(list[1].other_user.username.is_empty());

        let nulled: CmailUser = serde_json::from_str(r#"{"userId":null,"username":null}"#).unwrap();
        assert_eq!(nulled.user_id, String::new());
    }

    #[test]
    fn typing_endpoints_tolerate_explicit_nulls() {
        let resp: CmailTypingResponse = serde_json::from_str(
            r#"{"conversationId":"c1","ok":null,"heartbeatMs":null,"staleAfterMs":null}"#,
        )
        .unwrap();
        assert!(!resp.ok);
        assert_eq!(
            resp.heartbeat(),
            Duration::from_millis(FALLBACK_TYPING_HEARTBEAT_MS)
        );

        let status: CmailTypingStatus = serde_json::from_str(
            r#"{"conversationId":"c1","userId":null,"username":null,"typing":null,"since":null,"staleAfterMs":null}"#,
        )
        .unwrap();
        assert_eq!(
            status,
            CmailTypingStatus {
                conversation_id: "c1".into(),
                ..CmailTypingStatus::default()
            }
        );
    }

    #[test]
    fn presence_staleness_rule() {
        let stale_after = Duration::from_secs(9);
        let entry = CmailPresence {
            user_id: "u2".into(),
            username: "alice".into(),
            typing: true,
            timestamp: 100_000,
        };
        // Refreshed a second ago: typing.
        assert!(entry.is_typing_at(101_000, stale_after));
        // Just inside the window.
        assert!(entry.is_typing_at(108_999, stale_after));
        // On the boundary and past it: stale, even though the flag is still set.
        assert!(!entry.is_typing_at(109_000, stale_after));
        assert!(!entry.is_typing_at(200_000, stale_after));
        // A timestamp in the future (clock skew) still counts as typing.
        assert!(entry.is_typing_at(99_000, stale_after));

        // The flag itself must be set.
        let not_typing = CmailPresence {
            typing: false,
            ..entry
        };
        assert!(!not_typing.is_typing_at(101_000, stale_after));
    }

    #[test]
    fn validate_message_rejects_empty_and_too_long() {
        assert!(matches!(
            validate_cmail_content(" "),
            Err(ApiError::Config(_))
        ));
        let long = "x".repeat(MAX_MESSAGE_LEN + 1);
        assert!(matches!(
            validate_cmail_content(&long),
            Err(ApiError::Config(_))
        ));
    }
}

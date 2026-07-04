//! cIRC REST endpoints (`/v1/circ`, API v0.7) — multi-user chat rooms.
//!
//! Structurally the same as [C-Mail](crate::cmail): sending goes through REST
//! (sanitised, rate-limited, identity set server-side); live messages come from
//! subscribing to `chat_messages/<roomId>` in Realtime Database with the
//! `idToken`. A room is addressed by its `roomId` (its slug, e.g. `general`).
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};

const DEFAULT_MESSAGE_LIMIT: u32 = 50;
const MAX_MESSAGE_LIMIT: u32 = 100;
const MAX_MESSAGE_LEN: usize = 2_048;

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircMessage {
    #[serde(alias = "messageId", default)]
    pub id: String,
    #[serde(alias = "senderId", alias = "senderUid", default)]
    pub user_id: String,
    #[serde(alias = "senderUsername", default)]
    pub username: String,
    #[serde(default)]
    pub is_chat_admin: bool,
    #[serde(default)]
    pub content: String,
    /// Milliseconds since Unix epoch.
    #[serde(default)]
    pub timestamp: i64,
}

/// Response from `POST /v1/circ/:roomId`.
///
/// A normal send returns `{ roomId, messageId }`; a command that the server
/// answers inline (e.g. `/help`) returns `{ reply }` and posts nothing — so all
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

#[derive(Debug, Serialize)]
struct SendMessageBody<'a> {
    content: &'a str,
}

impl Client {
    /// `GET /v1/circ` — list the rooms available to you.
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

    /// `GET /v1/circ/:roomId` — load message history, oldest first. Pass `before`
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

    /// `POST /v1/circ/:roomId` — send a message (or run a slash command).
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

    /// `POST /v1/circ/:roomId/read` — mark the room as viewed.
    pub async fn mark_circ_read(&self, room_id: &str) -> Result<()> {
        let path = format!("/v1/circ/{room_id}/read");
        self.request_unit(EndpointKey::CircMarkRead, Method::POST, &path, &[])
            .await
    }
}

/// Parse the messages carried by an RTDB `put`/`patch` event on a
/// `chat_messages/<roomId>` subscription into [`CircMessage`]s. Firebase keys
/// each message by id (map key at the root, or the final path segment for a
/// single-message event), so this injects it.
#[must_use]
pub fn circ_messages_from_rtdb_event(path: &str, data: &Value) -> Vec<CircMessage> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [] => match data {
            Value::Object(map) => map
                .iter()
                .filter_map(|(id, value)| circ_message_from_rtdb_value(id, value))
                .collect(),
            _ => Vec::new(),
        },
        [id] => circ_message_from_rtdb_value(id, data).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn circ_message_from_rtdb_value(id: &str, value: &Value) -> Option<CircMessage> {
    if !value.is_object() {
        return None;
    }
    let mut message: CircMessage = serde_json::from_value(value.clone()).ok()?;
    if message.id.is_empty() {
        message.id = id.to_string();
    }
    Some(message)
}

/// Derive a next-page cursor when the server didn't send one (see
/// [`crate::cmail`] — same convention): the oldest message's timestamp, and only
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
    use super::*;

    #[test]
    fn room_decodes_and_addresses_by_slug() {
        let room: CircRoom = serde_json::from_str(
            r#"{"id":"r1","slug":"general","name":"General","lastMessageAt":1719700000000,"sortOrder":0}"#,
        )
        .unwrap();
        assert_eq!(room.slug, "general");
        assert_eq!(room.name, "General");
        assert_eq!(room.room_id(), "general");
    }

    #[test]
    fn room_falls_back_to_id_when_slug_missing() {
        let room: CircRoom = serde_json::from_str(r#"{"id":"r1","name":"General"}"#).unwrap();
        assert_eq!(room.room_id(), "r1");
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

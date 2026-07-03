//! C-Mail REST endpoints (`/v1/cmail`, API v0.6.0).
//!
//! C-Mail is Cyberspace's private 1:1 messaging. REST covers starting/loading
//! conversations and sending/marking messages; live updates come from RTDB using
//! the `idToken` + `rtdbUrl` returned by auth.
use reqwest::Method;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};

const DEFAULT_MESSAGE_LIMIT: u32 = 50;
const MAX_MESSAGE_LIMIT: u32 = 100;
const MAX_MESSAGE_LEN: usize = 2_048;

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
    #[serde(alias = "id", default)]
    pub user_id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub profile_picture_url: Option<String>,
}

/// A C-Mail message as returned by history/list responses and RTDB events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailMessage {
    #[serde(alias = "messageId", default)]
    pub id: String,
    #[serde(alias = "senderUid", default)]
    pub sender_id: String,
    #[serde(default)]
    pub sender_username: String,
    #[serde(default)]
    pub content: String,
    /// Milliseconds since Unix epoch.
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub read: bool,
}

/// A C-Mail conversation summary.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailConversation {
    #[serde(alias = "id", default)]
    pub conversation_id: String,
    #[serde(default)]
    pub other_user: CmailUser,
    #[serde(default, deserialize_with = "deserialize_last_message")]
    pub last_message: Option<CmailMessage>,
    /// Milliseconds since Unix epoch.
    #[serde(default)]
    pub last_message_at: Option<i64>,
    #[serde(default)]
    pub unread_count: u32,
}

/// Response from `POST /v1/cmail/:conversationId`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CmailSendResponse {
    pub conversation_id: String,
    pub message_id: String,
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

/// A single change delivered over the live RTDB stream for a conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum CmailLiveUpdate {
    /// A whole message arrived or was replaced (a new message, or a full-object
    /// push of an existing one).
    Message(CmailMessage),
    /// A field-level flip of an existing message's `read` flag — e.g. the other
    /// participant marked the thread read (a read receipt). Carries only the id
    /// and new value so applying it never clobbers the message's content.
    Read { id: String, read: bool },
}

/// Parse the changes carried by an RTDB `put`/`patch` event on a
/// `dm_messages/<conversationId>` subscription into [`CmailLiveUpdate`]s.
///
/// Firebase keys each message by id, so the id is the map key (root events) or a
/// path segment (single-message events), not a field in the payload — this
/// injects it. A whole-message payload (it carries `content`/`timestamp`) yields
/// [`CmailLiveUpdate::Message`]; a partial `{ "read": true }` patch, or a
/// `.../read` scalar patch, yields [`CmailLiveUpdate::Read`]. Deletions
/// (`data: null`) and other field patches yield nothing.
#[must_use]
pub fn updates_from_rtdb_event(path: &str, data: &Value) -> Vec<CmailLiveUpdate> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        // Root event: `data` is a map of `{ <msgId>: <message-or-patch>, ... }`.
        [] => match data {
            Value::Object(map) => map
                .iter()
                .filter_map(|(id, value)| update_from_rtdb_value(id, value))
                .collect(),
            _ => Vec::new(),
        },
        // Single-object event: the id is the (only) path segment.
        [id] => update_from_rtdb_value(id, data).into_iter().collect(),
        // A scalar patch on a single field, e.g. `dm_messages/<cid>/<msgId>/read`.
        [id, "read"] => data
            .as_bool()
            .map(|read| CmailLiveUpdate::Read {
                id: (*id).to_string(),
                read,
            })
            .into_iter()
            .collect(),
        // Other deep field patches carry no update we model.
        _ => Vec::new(),
    }
}

fn update_from_rtdb_value(id: &str, value: &Value) -> Option<CmailLiveUpdate> {
    let obj = value.as_object()?;
    // A whole message always carries content/timestamp; a partial read-receipt
    // patch carries only `read`.
    if obj.contains_key("content") || obj.contains_key("timestamp") {
        let mut message: CmailMessage = serde_json::from_value(value.clone()).ok()?;
        if message.id.is_empty() {
            message.id = id.to_string();
        }
        Some(CmailLiveUpdate::Message(message))
    } else {
        obj.get("read")
            .and_then(Value::as_bool)
            .map(|read| CmailLiveUpdate::Read {
                id: id.to_string(),
                read,
            })
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
        let msg: CmailMessage = serde_json::from_str(
            r#"{"id":"m1","senderId":"u1","senderUsername":"me","content":"hi","timestamp":1719700000000,"read":false}"#,
        )
        .unwrap();
        assert_eq!(msg.timestamp, 1_719_700_000_000);
        assert!(!msg.read);
    }

    #[test]
    fn send_response_decodes() {
        let sent: CmailSendResponse =
            serde_json::from_str(r#"{"conversationId":"c1","messageId":"m1"}"#).unwrap();
        assert_eq!(sent.conversation_id, "c1");
        assert_eq!(sent.message_id, "m1");
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

    fn only_messages(updates: Vec<CmailLiveUpdate>) -> Vec<CmailMessage> {
        updates
            .into_iter()
            .filter_map(|u| match u {
                CmailLiveUpdate::Message(m) => Some(m),
                CmailLiveUpdate::Read { .. } => None,
            })
            .collect()
    }

    #[test]
    fn rtdb_root_event_parses_message_map_with_ids_from_keys() {
        let data = serde_json::json!({
            "m1": {"senderId":"u2","senderUsername":"alice","content":"hi","timestamp":1_000,"read":false},
            "m2": {"senderId":"u1","senderUsername":"me","content":"yo","timestamp":2_000,"read":true}
        });
        let mut msgs = only_messages(updates_from_rtdb_event("/", &data));
        msgs.sort_by_key(|m| m.timestamp);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, "m1");
        assert_eq!(msgs[0].content, "hi");
        assert_eq!(msgs[1].id, "m2");
    }

    #[test]
    fn rtdb_single_message_event_takes_id_from_path() {
        let data = serde_json::json!(
            {"senderId":"u2","senderUsername":"alice","content":"you there?","timestamp":3_000,"read":false}
        );
        let updates = updates_from_rtdb_event("/m3", &data);
        assert_eq!(
            updates,
            vec![CmailLiveUpdate::Message(CmailMessage {
                id: "m3".into(),
                sender_id: "u2".into(),
                sender_username: "alice".into(),
                content: "you there?".into(),
                timestamp: 3_000,
                read: false,
            })]
        );
    }

    #[test]
    fn rtdb_read_receipt_patches_parse_as_read_updates() {
        // A partial object patch: `{ "read": true }` at the message path.
        assert_eq!(
            updates_from_rtdb_event("/m3", &serde_json::json!({"read": true})),
            vec![CmailLiveUpdate::Read {
                id: "m3".into(),
                read: true
            }]
        );
        // A scalar patch straight on the `read` field.
        assert_eq!(
            updates_from_rtdb_event("/m3/read", &serde_json::json!(true)),
            vec![CmailLiveUpdate::Read {
                id: "m3".into(),
                read: true
            }]
        );
    }

    #[test]
    fn rtdb_deletions_and_unknown_field_patches_yield_nothing() {
        // `data: null` (a deletion) and unmodelled field patches yield no update.
        assert!(updates_from_rtdb_event("/m3", &Value::Null).is_empty());
        assert!(updates_from_rtdb_event("/m3/content", &serde_json::json!("edited")).is_empty());
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

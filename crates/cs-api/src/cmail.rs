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
#[derive(Debug, Clone, Default, Deserialize)]
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
        self.request_page(EndpointKey::CmailRead, Method::GET, &path, &query)
            .await
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

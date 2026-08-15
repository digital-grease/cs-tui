//! Reply read and write endpoints (`/v1/replies`, API v0.8.4).
//!
//! Replies hang off an entry and can be threaded under another reply. Editing
//! (§ Edit Reply) is limited to supporters, within 5 minutes of posting, on
//! their own replies, and never bumps the thread.
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::types::{validate_flag_reason, FlagBody, FlagResponse, Reply};

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;
const MAX_CONTENT_LEN: usize = 32_768;

impl Client {
    /// `GET /v1/posts/:post_id/replies` — replies on an entry, oldest first.
    pub async fn list_replies(
        &self,
        post_id: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Reply>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let path = format!("/v1/posts/{post_id}/replies");
        self.request_page(EndpointKey::RepliesList, Method::GET, &path, &query)
            .await
    }

    /// `GET /v1/replies/:id` — fetch a single reply.
    pub async fn get_reply(&self, reply_id: &str) -> Result<Reply> {
        let path = format!("/v1/replies/{reply_id}");
        self.request::<Reply, ()>(EndpointKey::RepliesGet, Method::GET, &path, &[], None)
            .await
    }

    /// `POST /v1/replies` — create a new reply on `post_id`. Pass
    /// `parent_reply_id = Some(...)` for nested replies, `None` for top-level.
    /// Returns the new `replyId`. Rate limit: 3/min, 10/day.
    pub async fn create_reply(
        &self,
        post_id: &str,
        content: &str,
        parent_reply_id: Option<&str>,
    ) -> Result<String> {
        validate_reply_content(content)?;
        let body = CreateReplyBody {
            post_id,
            content,
            parent_reply_id,
        };
        let r: CreateReplyResponse = self
            .request(
                EndpointKey::RepliesCreate,
                Method::POST,
                "/v1/replies",
                &[],
                Some(&body),
            )
            .await?;
        Ok(r.reply_id)
    }

    /// `PATCH /v1/replies/:id`, edit a reply you posted (v0.8.4 § Edit Reply).
    /// `content` is the only editable field and is required, max 32,768
    /// characters.
    ///
    /// Returns the echoed `replyId`. The server does not return the updated
    /// reply, so re-fetch with [`get_reply`](Client::get_reply) when the UI
    /// needs the new `editedAt`. Editing does not bump the thread: the entry's
    /// reply count and last-activity time are untouched.
    ///
    /// Same permission and window as [`edit_entry`](Client::edit_entry):
    /// supporters, within 5 minutes, on their own replies. Outside that the
    /// server answers `403`.
    ///
    /// Rate limit: 5/min, 30/day.
    pub async fn edit_reply(&self, reply_id: &str, content: &str) -> Result<String> {
        validate_reply_content(content)?;
        let body = EditReplyBody { content };
        let path = format!("/v1/replies/{reply_id}");
        let r: EditReplyResponse = self
            .request(
                EndpointKey::RepliesEdit,
                Method::PATCH,
                &path,
                &[],
                Some(&body),
            )
            .await?;
        Ok(r.reply_id)
    }

    /// `DELETE /v1/replies/:id` — soft-delete a reply. Only the author can.
    pub async fn delete_reply(&self, reply_id: &str) -> Result<()> {
        let path = format!("/v1/replies/{reply_id}");
        self.request_unit(EndpointKey::RepliesDelete, Method::DELETE, &path, &[])
            .await
    }

    /// `POST /v1/replies/:id/flag`, report a reply for review (v0.8.4 § Flag a
    /// Reply). `reason` is optional, max 500 characters.
    ///
    /// Behaves exactly like [`flag_entry`](Client::flag_entry): reporting is
    /// idempotent, so a repeat report files nothing new and answers `200` with
    /// `alreadyFlagged`, which is a success and not an error. Branch on
    /// [`FlagResponse::is_new`] rather than on the status code. Reports cannot
    /// be withdrawn, and reporting your own reply is a `403`.
    ///
    /// Rate limit: 5/min, 20/hour, 50/day, one budget shared with
    /// [`flag_entry`](Client::flag_entry) and the cIRC message flag endpoint.
    pub async fn flag_reply(&self, reply_id: &str, reason: Option<&str>) -> Result<FlagResponse> {
        validate_flag_reason(reason)?;
        let body = FlagBody { reason };
        let path = format!("/v1/replies/{reply_id}/flag");
        self.request(EndpointKey::Flag, Method::POST, &path, &[], Some(&body))
            .await
    }
}

/// Content check shared by [`Client::create_reply`] and
/// [`Client::edit_reply`], which take the same field under the same limit.
fn validate_reply_content(content: &str) -> Result<()> {
    if content.trim().is_empty() {
        return Err(ApiError::Config("reply content cannot be empty".into()));
    }
    if content.chars().count() > MAX_CONTENT_LEN {
        return Err(ApiError::Config(format!(
            "reply content exceeds {MAX_CONTENT_LEN} characters"
        )));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateReplyBody<'a> {
    post_id: &'a str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_reply_id: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateReplyResponse {
    reply_id: String,
}

/// Body for `PATCH /v1/replies/:id`. `content` is the only editable field and
/// the server requires it, so there is nothing optional to skip here.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EditReplyBody<'a> {
    content: &'a str,
}

/// `PATCH /v1/replies/:id` answers with the id alone, not the updated reply.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditReplyResponse {
    reply_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_serializes_top_level() {
        let body = CreateReplyBody {
            post_id: "p1",
            content: "hi",
            parent_reply_id: None,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""postId":"p1""#));
        assert!(s.contains(r#""content":"hi""#));
        assert!(!s.contains("parentReplyId"));
    }

    #[test]
    fn create_body_serializes_nested() {
        let body = CreateReplyBody {
            post_id: "p1",
            content: "hi",
            parent_reply_id: Some("r0"),
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""parentReplyId":"r0""#));
    }

    #[test]
    fn create_response_decodes() {
        let r: CreateReplyResponse = serde_json::from_str(r#"{"replyId":"r1"}"#).unwrap();
        assert_eq!(r.reply_id, "r1");
    }

    #[test]
    fn edit_body_serializes_content_only() {
        let s = serde_json::to_string(&EditReplyBody { content: "fixed" }).unwrap();
        assert_eq!(s, r#"{"content":"fixed"}"#);
    }

    #[test]
    fn edit_response_decodes() {
        let r: EditReplyResponse = serde_json::from_str(r#"{"replyId":"r1"}"#).unwrap();
        assert_eq!(r.reply_id, "r1");
    }

    #[test]
    fn validate_reply_content_rejects_empty_and_overlong() {
        assert!(validate_reply_content("hi").is_ok());
        assert!(matches!(
            validate_reply_content("   "),
            Err(ApiError::Config(_))
        ));
        let big = "x".repeat(MAX_CONTENT_LEN + 1);
        assert!(matches!(
            validate_reply_content(&big),
            Err(ApiError::Config(_))
        ));
        let exact = "x".repeat(MAX_CONTENT_LEN);
        assert!(validate_reply_content(&exact).is_ok());
    }

    #[test]
    fn flag_body_omits_an_absent_reason() {
        let s = serde_json::to_string(&FlagBody { reason: None }).unwrap();
        assert_eq!(s, "{}");
    }

    #[test]
    fn flag_body_sends_a_reason_when_given() {
        let s = serde_json::to_string(&FlagBody {
            reason: Some("harassment"),
        })
        .unwrap();
        assert_eq!(s, r#"{"reason":"harassment"}"#);
    }

    #[test]
    fn reply_flag_response_decodes_both_outcomes() {
        let fresh: FlagResponse =
            serde_json::from_str(r#"{"replyId":"r1","flagId":"f1","flagged":true}"#).unwrap();
        assert!(fresh.flagged);
        assert!(fresh.is_new());
        assert_eq!(fresh.flag_id.as_deref(), Some("f1"));

        // The repeat report is a 200 with alreadyFlagged and no flagId.
        let repeat: FlagResponse =
            serde_json::from_str(r#"{"replyId":"r1","flagged":true,"alreadyFlagged":true}"#)
                .unwrap();
        assert!(repeat.flagged);
        assert!(!repeat.is_new());
        assert!(repeat.flag_id.is_none());
    }

    #[test]
    fn flag_reason_length_is_capped_before_sending() {
        assert!(validate_flag_reason(Some("off-topic")).is_ok());
        let long = "x".repeat(501);
        assert!(matches!(
            validate_flag_reason(Some(&long)),
            Err(ApiError::Config(_))
        ));
    }
}

//! Reply read and write endpoints.
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::types::Reply;

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
        if content.trim().is_empty() {
            return Err(ApiError::Config("reply content cannot be empty".into()));
        }
        if content.chars().count() > MAX_CONTENT_LEN {
            return Err(ApiError::Config(format!(
                "reply content exceeds {MAX_CONTENT_LEN} characters"
            )));
        }
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

    /// `DELETE /v1/replies/:id` — soft-delete a reply. Only the author can.
    pub async fn delete_reply(&self, reply_id: &str) -> Result<()> {
        let path = format!("/v1/replies/{reply_id}");
        self.request_unit(EndpointKey::RepliesDelete, Method::DELETE, &path, &[])
            .await
    }
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
}

//! Bookmark types and endpoints (`/v1/bookmarks/*`).
use reqwest::Method;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;
use crate::types::{Entry, Reply};

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

/// Whether a bookmark refers to a post or a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BookmarkKind {
    Post,
    Reply,
}

/// A bookmark record returned by `GET /v1/bookmarks`. Exactly one of
/// `post` / `reply` is populated, matching `kind`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    #[serde(alias = "id")]
    pub bookmark_id: String,

    #[serde(rename = "type")]
    pub kind: BookmarkKind,

    #[serde(default)]
    pub post_id: Option<String>,

    #[serde(default)]
    pub reply_id: Option<String>,

    /// Embedded entry when `kind == Post` (the list endpoint inlines the
    /// referenced content).
    #[serde(default)]
    pub post: Option<Entry>,

    /// Embedded reply when `kind == Reply`.
    #[serde(default)]
    pub reply: Option<Reply>,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateBookmarkPostBody<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    post_id: &'a str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateBookmarkReplyBody<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    reply_id: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBookmarkResponse {
    // The live server returns `bookmarkId`; `id` is accepted as a fallback.
    #[serde(alias = "id")]
    bookmark_id: String,
}

impl Client {
    /// `GET /v1/bookmarks` — your saved posts and replies, newest first.
    pub async fn list_bookmarks(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Bookmark>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.request_page(
            EndpointKey::BookmarksList,
            Method::GET,
            "/v1/bookmarks",
            &query,
        )
        .await
    }

    /// `POST /v1/bookmarks` for a post target. Returns the new bookmark id.
    pub async fn bookmark_post(&self, post_id: &str) -> Result<String> {
        let body = CreateBookmarkPostBody {
            kind: "post",
            post_id,
        };
        let r: CreateBookmarkResponse = self
            .request(
                EndpointKey::BookmarksCreate,
                Method::POST,
                "/v1/bookmarks",
                &[],
                Some(&body),
            )
            .await?;
        Ok(r.bookmark_id)
    }

    /// `POST /v1/bookmarks` for a reply target. Returns the new bookmark id.
    pub async fn bookmark_reply(&self, reply_id: &str) -> Result<String> {
        let body = CreateBookmarkReplyBody {
            kind: "reply",
            reply_id,
        };
        let r: CreateBookmarkResponse = self
            .request(
                EndpointKey::BookmarksCreate,
                Method::POST,
                "/v1/bookmarks",
                &[],
                Some(&body),
            )
            .await?;
        Ok(r.bookmark_id)
    }

    /// `DELETE /v1/bookmarks/:id` — remove a bookmark by its document id.
    pub async fn delete_bookmark(&self, bookmark_id: &str) -> Result<()> {
        let path = format!("/v1/bookmarks/{bookmark_id}");
        self.request_unit(EndpointKey::BookmarksDelete, Method::DELETE, &path, &[])
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_kind_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&BookmarkKind::Post).unwrap(),
            "\"post\""
        );
        assert_eq!(
            serde_json::to_string(&BookmarkKind::Reply).unwrap(),
            "\"reply\""
        );
    }

    #[test]
    fn bookmark_for_post_decodes() {
        let raw = r#"{
            "bookmarkId": "b1",
            "type": "post",
            "postId": "p1",
            "post": {
                "postId": "p1",
                "authorId": "a",
                "authorUsername": "u",
                "content": "hi"
            },
            "createdAt": "2026-03-27T10:12:01Z"
        }"#;
        let b: Bookmark = serde_json::from_str(raw).unwrap();
        assert_eq!(b.bookmark_id, "b1");
        assert_eq!(b.kind, BookmarkKind::Post);
        assert_eq!(b.post_id.as_deref(), Some("p1"));
        assert!(b.post.is_some());
        assert!(b.reply.is_none());
    }

    #[test]
    fn bookmark_for_reply_decodes() {
        let raw = r#"{
            "bookmarkId": "b2",
            "type": "reply",
            "replyId": "r1",
            "reply": {
                "replyId": "r1",
                "postId": "p1",
                "authorId": "a",
                "authorUsername": "u",
                "content": "yo"
            }
        }"#;
        let b: Bookmark = serde_json::from_str(raw).unwrap();
        assert_eq!(b.bookmark_id, "b2");
        assert_eq!(b.kind, BookmarkKind::Reply);
        assert_eq!(b.reply_id.as_deref(), Some("r1"));
        assert!(b.reply.is_some());
        assert!(b.post.is_none());
    }

    #[test]
    fn bookmark_accepts_id_alias() {
        let raw = r#"{"id":"b3","type":"post"}"#;
        let b: Bookmark = serde_json::from_str(raw).unwrap();
        assert_eq!(b.bookmark_id, "b3");
    }

    #[test]
    fn create_bookmark_response_decodes_camelcase_id() {
        // The server returns {"data":{"bookmarkId":"..."}}; after the `data`
        // envelope is unwrapped this is what CreateBookmarkResponse must decode.
        // Regression: the missing camelCase rename made every bookmark report a
        // false "server sent something unexpected" failure.
        let raw = r#"{"bookmarkId":"JxPvqF5_post_OtwiYmmW0"}"#;
        let r: CreateBookmarkResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(r.bookmark_id, "JxPvqF5_post_OtwiYmmW0");
    }

    #[test]
    fn create_bookmark_response_still_accepts_id_alias() {
        let raw = r#"{"id":"b9"}"#;
        let r: CreateBookmarkResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(r.bookmark_id, "b9");
    }

    #[test]
    fn create_post_body_uses_documented_fields() {
        let body = CreateBookmarkPostBody {
            kind: "post",
            post_id: "p1",
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""type":"post""#));
        assert!(s.contains(r#""postId":"p1""#));
    }

    #[test]
    fn create_reply_body_uses_documented_fields() {
        let body = CreateBookmarkReplyBody {
            kind: "reply",
            reply_id: "r1",
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""type":"reply""#));
        assert!(s.contains(r#""replyId":"r1""#));
    }
}

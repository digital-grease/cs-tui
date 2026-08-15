//! Full-text search (`GET /v1/search`, API v0.8.4).
//!
//! `type=all` returns a grouped preview (up to 8 hits per group, no pagination);
//! a specific `type` returns a paginated list whose `cursor` is the next page
//! number. Every hit carries a `type` discriminator (`user`/`post`/`reply`).
use reqwest::Method;
use serde::Deserialize;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};

const MAX_QUERY_LEN: usize = 512;

/// Which corpus to search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchType {
    Posts,
    Replies,
    Users,
}

impl SearchType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Posts => "posts",
            Self::Replies => "replies",
            Self::Users => "users",
        }
    }
}

/// A user search hit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserHit {
    #[serde(alias = "id", default)]
    pub user_id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub profile_picture_url: Option<String>,
    #[serde(default)]
    pub supporter_icon: Option<String>,
}

/// A post (entry) search hit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostHit {
    #[serde(alias = "id", default)]
    pub post_id: String,
    #[serde(default)]
    pub author_id: String,
    #[serde(default)]
    pub author_username: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
}

/// A reply search hit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyHit {
    #[serde(alias = "id", default)]
    pub reply_id: String,
    #[serde(default)]
    pub post_id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub parent_post_author: Option<String>,
    #[serde(default)]
    pub parent_post_content: Option<String>,
}

/// A single search result, discriminated by the `type` field. Unknown types
/// (from a future API) decode to [`SearchHit::Unknown`] rather than erroring.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SearchHit {
    User(UserHit),
    Post(PostHit),
    Reply(ReplyHit),
    #[serde(other)]
    Unknown,
}

/// Grouped preview returned by `type=all`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPreview {
    #[serde(default)]
    pub users: Vec<UserHit>,
    #[serde(default)]
    pub posts: Vec<PostHit>,
    #[serde(default)]
    pub replies: Vec<ReplyHit>,
}

impl SearchPreview {
    /// Whether every group is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.posts.is_empty() && self.replies.is_empty()
    }
}

impl Client {
    /// `GET /v1/search?type=all` — the grouped preview across users, posts, and
    /// replies (up to 8 each, no pagination).
    pub async fn search_all(&self, query: &str) -> Result<SearchPreview> {
        validate_query(query)?;
        let params = [("q", query.to_string()), ("type", "all".to_string())];
        self.request(
            EndpointKey::Search,
            Method::GET,
            "/v1/search",
            &params,
            None::<&()>,
        )
        .await
    }

    /// `GET /v1/search?type=<posts|replies|users>` — a paginated list of hits.
    /// Returns the hits and the next `page` cursor (`None` on the last page).
    pub async fn search_typed(
        &self,
        query: &str,
        kind: SearchType,
        page: u32,
    ) -> Result<(Vec<SearchHit>, Option<String>)> {
        validate_query(query)?;
        let params = [
            ("q", query.to_string()),
            ("type", kind.as_str().to_string()),
            ("page", page.to_string()),
        ];
        self.request_page(EndpointKey::Search, Method::GET, "/v1/search", &params)
            .await
    }
}

fn validate_query(query: &str) -> Result<()> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(ApiError::Config("search query cannot be empty".into()));
    }
    if query.chars().count() > MAX_QUERY_LEN {
        return Err(ApiError::Config(format!(
            "search query exceeds {MAX_QUERY_LEN} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_decodes_grouped_shape() {
        let preview: SearchPreview = serde_json::from_str(
            r#"{
                "users":[{"type":"user","userId":"u1","username":"neo"}],
                "posts":[{"type":"post","postId":"p1","authorUsername":"neo","content":"hi","title":"T"}],
                "replies":[{"type":"reply","replyId":"r1","postId":"p1","content":"re"}]
            }"#,
        )
        .unwrap();
        assert_eq!(preview.users[0].username, "neo");
        assert_eq!(preview.posts[0].post_id, "p1");
        assert_eq!(preview.replies[0].reply_id, "r1");
        assert!(!preview.is_empty());
    }

    #[test]
    fn hit_enum_dispatches_on_type() {
        let post: SearchHit =
            serde_json::from_str(r#"{"type":"post","postId":"p1","content":"x"}"#).unwrap();
        assert!(matches!(post, SearchHit::Post(h) if h.post_id == "p1"));

        let user: SearchHit = serde_json::from_str(r#"{"type":"user","username":"neo"}"#).unwrap();
        assert!(matches!(user, SearchHit::User(h) if h.username == "neo"));
    }

    #[test]
    fn unknown_hit_type_falls_through() {
        let hit: SearchHit = serde_json::from_str(r#"{"type":"guild","name":"x"}"#).unwrap();
        assert_eq!(hit, SearchHit::Unknown);
    }

    #[test]
    fn validate_query_bounds() {
        assert!(matches!(validate_query("  "), Err(ApiError::Config(_))));
        let long = "x".repeat(MAX_QUERY_LEN + 1);
        assert!(matches!(validate_query(&long), Err(ApiError::Config(_))));
        assert!(validate_query("neon").is_ok());
    }
}

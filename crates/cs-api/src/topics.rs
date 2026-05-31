//! Topic types and endpoints (`/v1/topics/*`).
use reqwest::Method;
use serde::Deserialize;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;
use crate::types::Entry;

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

/// A topic record returned by `GET /v1/topics`. The list is sorted by post
/// count (most popular first).
///
/// The spec doesn't document the object's field names. The topic identifier
/// (used verbatim as `:slug` in `/v1/topics/:slug/posts`, where the spec says
/// `:slug` is the lowercase topic name) is accepted under several aliases —
/// `name`/`topic`/`tag` — because live responses use `name`, not `slug`. The
/// count is likewise tolerant and defaults to 0 if absent.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Topic {
    #[serde(alias = "name", alias = "topic", alias = "tag")]
    pub slug: String,
    #[serde(
        default,
        alias = "count",
        alias = "entryCount",
        alias = "entriesCount",
        alias = "postsCount",
        alias = "posts"
    )]
    pub post_count: u32,
}

impl Client {
    /// `GET /v1/topics` — topics sorted by post count (most popular first).
    ///
    /// Despite the spec wording ("all topics"), the live endpoint is
    /// cursor-paginated like the other lists (≈20 per page), so this returns the
    /// page plus the next cursor. `Page<T>` tolerates a missing/null cursor, so
    /// a non-paginated server still decodes (with `None`).
    pub async fn list_topics(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Topic>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.request_page(EndpointKey::TopicsList, Method::GET, "/v1/topics", &query)
            .await
    }

    /// `GET /v1/topics/:slug/posts` — entries tagged with this topic, newest first.
    pub async fn list_topic_posts(
        &self,
        slug: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Entry>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let path = format!("/v1/topics/{slug}/posts");
        self.request_page(EndpointKey::TopicsListPosts, Method::GET, &path, &query)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_decodes_with_post_count() {
        let raw = r#"{"slug":"music","postCount":42}"#;
        let t: Topic = serde_json::from_str(raw).unwrap();
        assert_eq!(t.slug, "music");
        assert_eq!(t.post_count, 42);
    }

    #[test]
    fn topic_tolerates_missing_post_count() {
        let raw = r#"{"slug":"linux"}"#;
        let t: Topic = serde_json::from_str(raw).unwrap();
        assert_eq!(t.post_count, 0);
    }

    #[test]
    fn topic_decodes_name_alias_with_entry_count() {
        // The live server names the identifier `name` (not `slug`) — the shape
        // that produced "missing field `slug`" during smoke testing.
        let raw = r#"{"name":"music","entryCount":42}"#;
        let t: Topic = serde_json::from_str(raw).unwrap();
        assert_eq!(t.slug, "music");
        assert_eq!(t.post_count, 42);
    }
}

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
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Topic {
    pub slug: String,
    #[serde(default)]
    pub post_count: u32,
}

impl Client {
    /// `GET /v1/topics` — all topics, sorted by post count (most popular first).
    ///
    /// Spec ambiguity: the spec example doesn't show the response wrapper around
    /// the list. We assume `{ "data": [...] }` (the global envelope). If smoke
    /// testing reveals `{ "data": { "topics": [...] } }`, switch to a wrapped
    /// struct here.
    pub async fn list_topics(&self) -> Result<Vec<Topic>> {
        self.request::<Vec<Topic>, ()>(
            EndpointKey::TopicsList,
            Method::GET,
            "/v1/topics",
            &[],
            None,
        )
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
}

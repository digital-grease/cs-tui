//! Entry (post) read and write endpoints.
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::types::Entry;

const MAX_CONTENT_LEN: usize = 32_768;
const MAX_TOPICS: usize = 3;
const MAX_TITLE_LEN: usize = 100;
const MAX_SLUG_LEN: usize = 60;

/// Slugs the server reserves and will reject if submitted.
const RESERVED_SLUGS: &[&str] = &[
    "blog", "jukebox", "public", "replies", "index", "edit", "new", "admin",
];

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

impl Client {
    /// `GET /v1/posts` — the home feed. Pass `None` for the first page; thread
    /// the returned cursor for subsequent pages. `limit` is clamped to 1–50
    /// (spec ceiling) with a default of 20.
    pub async fn list_entries(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Entry>, Option<String>)> {
        let limit = clamp_limit(limit);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.request_page(EndpointKey::EntriesList, Method::GET, "/v1/posts", &query)
            .await
    }

    /// `GET /v1/posts/:id` — fetch a single entry.
    pub async fn get_entry(&self, post_id: &str) -> Result<Entry> {
        let path = format!("/v1/posts/{post_id}");
        self.request::<Entry, ()>(EndpointKey::EntriesGet, Method::GET, &path, &[], None)
            .await
    }

    /// `POST /v1/posts` — create a new entry. Returns the created entry's id,
    /// final slug (server may suffix on collision), and any echo-back title.
    ///
    /// Rate limit: 2/min, 10/day.
    pub async fn create_entry(
        &self,
        content: &str,
        title: Option<&str>,
        slug: Option<&str>,
        topics: &[String],
        is_public: bool,
        is_nsfw: bool,
    ) -> Result<CreatedEntry> {
        validate_content_topics(content, topics)?;
        if let Some(t) = title {
            if t.chars().count() > MAX_TITLE_LEN {
                return Err(ApiError::Config(format!(
                    "title exceeds {MAX_TITLE_LEN} characters"
                )));
            }
        }
        if let Some(s) = slug {
            validate_slug(s)?;
        }
        let body = CreateEntryBody {
            content,
            title,
            slug,
            topics,
            is_public,
            is_nsfw,
        };
        let r: CreateEntryResponse = self
            .request(
                EndpointKey::EntriesCreate,
                Method::POST,
                "/v1/posts",
                &[],
                Some(&body),
            )
            .await?;
        Ok(CreatedEntry {
            post_id: r.post_id,
            slug: r.slug,
            title: r.title,
        })
    }

    /// `DELETE /v1/posts/:id` — soft-delete an entry. Only the author can.
    pub async fn delete_entry(&self, post_id: &str) -> Result<()> {
        let path = format!("/v1/posts/{post_id}");
        self.request_unit(EndpointKey::EntriesDelete, Method::DELETE, &path, &[])
            .await
    }

    /// `GET /v1/users/:username/posts/:slug` — resolve an entry by its
    /// per-author URL slug (v0.3.7+). Returns the same shape as `get_entry`;
    /// 404 if no entry matches that `(username, slug)` pair.
    pub async fn get_entry_by_slug(&self, username: &str, slug: &str) -> Result<Entry> {
        let path = format!("/v1/users/{username}/posts/{slug}");
        self.request::<Entry, ()>(
            EndpointKey::UsersGetPostBySlug,
            Method::GET,
            &path,
            &[],
            None,
        )
        .await
    }
}

/// Result of [`Client::create_entry`].
#[derive(Debug, Clone)]
pub struct CreatedEntry {
    pub post_id: String,
    /// The final stored slug — may differ from what was submitted (server
    /// appends `-2`, `-3`… on per-author collisions).
    pub slug: Option<String>,
    /// Echoed back only when a title was set.
    pub title: Option<String>,
}

fn validate_slug(s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(ApiError::Config("slug cannot be empty".into()));
    }
    if s.chars().count() > MAX_SLUG_LEN {
        return Err(ApiError::Config(format!(
            "slug exceeds {MAX_SLUG_LEN} characters"
        )));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ApiError::Config(
            "slug must be lowercase a-z, 0-9, or hyphen".into(),
        ));
    }
    if s.starts_with('_') {
        return Err(ApiError::Config(
            "slug cannot start with underscore (server-reserved)".into(),
        ));
    }
    if RESERVED_SLUGS.contains(&s) {
        return Err(ApiError::Config(format!(
            "slug {s:?} is reserved by the server"
        )));
    }
    Ok(())
}

fn validate_content_topics(content: &str, topics: &[String]) -> Result<()> {
    if content.trim().is_empty() {
        return Err(ApiError::Config("content cannot be empty".into()));
    }
    if content.chars().count() > MAX_CONTENT_LEN {
        return Err(ApiError::Config(format!(
            "content exceeds {MAX_CONTENT_LEN} characters"
        )));
    }
    if topics.len() > MAX_TOPICS {
        return Err(ApiError::Config(format!(
            "at most {MAX_TOPICS} topics allowed"
        )));
    }
    for t in topics {
        if t.chars()
            .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_')
        {
            return Err(ApiError::Config(format!(
                "topic {t:?} must be lowercase a-z, 0-9, or underscore"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateEntryBody<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slug: Option<&'a str>,
    topics: &'a [String],
    is_public: bool,
    #[serde(rename = "isNSFW")]
    is_nsfw: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEntryResponse {
    post_id: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_uses_default_when_absent() {
        assert_eq!(clamp_limit(None), DEFAULT_PAGE_LIMIT);
    }

    #[test]
    fn clamp_limit_enforces_ceiling() {
        assert_eq!(clamp_limit(Some(9999)), MAX_PAGE_LIMIT);
    }

    #[test]
    fn clamp_limit_enforces_floor() {
        assert_eq!(clamp_limit(Some(0)), 1);
    }

    #[test]
    fn clamp_limit_passes_valid_through() {
        assert_eq!(clamp_limit(Some(25)), 25);
    }

    #[test]
    fn create_entry_body_uses_spec_field_names() {
        let topics = vec!["music".to_string()];
        let body = CreateEntryBody {
            content: "hi",
            title: None,
            slug: None,
            topics: &topics,
            is_public: true,
            is_nsfw: true,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""content":"hi""#));
        assert!(s.contains(r#""topics":["music"]"#));
        assert!(s.contains(r#""isPublic":true"#));
        assert!(s.contains(r#""isNSFW":true"#));
        // Optional fields omitted when None.
        assert!(!s.contains(r#""title""#));
        assert!(!s.contains(r#""slug""#));
    }

    #[test]
    fn create_entry_body_includes_title_and_slug_when_set() {
        let body = CreateEntryBody {
            content: "hi",
            title: Some("My Title"),
            slug: Some("my-title"),
            topics: &[],
            is_public: false,
            is_nsfw: false,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""title":"My Title""#));
        assert!(s.contains(r#""slug":"my-title""#));
    }

    #[test]
    fn create_entry_response_decodes_with_optional_fields() {
        let r: CreateEntryResponse =
            serde_json::from_str(r#"{"postId":"p1","slug":"hello","title":"Hello"}"#).unwrap();
        assert_eq!(r.post_id, "p1");
        assert_eq!(r.slug.as_deref(), Some("hello"));
        assert_eq!(r.title.as_deref(), Some("Hello"));
    }

    #[test]
    fn create_entry_response_decodes_minimal() {
        let r: CreateEntryResponse = serde_json::from_str(r#"{"postId":"p1"}"#).unwrap();
        assert_eq!(r.post_id, "p1");
        assert!(r.slug.is_none());
        assert!(r.title.is_none());
    }

    #[test]
    fn validate_slug_accepts_lowercase_alnum_hyphen() {
        assert!(validate_slug("hello-world-2026").is_ok());
    }

    #[test]
    fn validate_slug_rejects_uppercase() {
        assert!(validate_slug("Hello").is_err());
    }

    #[test]
    fn validate_slug_rejects_underscore_prefix() {
        assert!(validate_slug("_internal").is_err());
    }

    #[test]
    fn validate_slug_rejects_reserved() {
        assert!(validate_slug("admin").is_err());
        assert!(validate_slug("new").is_err());
    }

    #[test]
    fn validate_slug_rejects_overlong() {
        let big = "x".repeat(MAX_SLUG_LEN + 1);
        assert!(validate_slug(&big).is_err());
    }

    #[test]
    fn entry_decodes_with_title_and_slug() {
        let raw = r#"{
            "postId": "abc",
            "authorId": "u",
            "authorUsername": "a",
            "content": "hi",
            "title": "Hello",
            "slug": "hello"
        }"#;
        let e: crate::Entry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.title.as_deref(), Some("Hello"));
        assert_eq!(e.slug.as_deref(), Some("hello"));
    }

    #[test]
    fn validate_rejects_empty_content() {
        let r = validate_content_topics("   ", &[]);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_rejects_overlong_content() {
        let big = "x".repeat(MAX_CONTENT_LEN + 1);
        let r = validate_content_topics(&big, &[]);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_rejects_too_many_topics() {
        let topics = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let r = validate_content_topics("ok", &topics);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_rejects_uppercase_topic() {
        let topics = vec!["Music".into()];
        let r = validate_content_topics("ok", &topics);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_accepts_lowercase_underscore_topic() {
        let topics = vec!["retro_music".into(), "linux".into(), "2026".into()];
        assert!(validate_content_topics("ok", &topics).is_ok());
    }
}

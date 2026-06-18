//! Thread-watching types and endpoints (`/v1/posts/:id/watch`, `/v1/watches`, v0.5.1).
//!
//! Watching a thread subscribes you to `thread_reply` notifications when anyone
//! replies to it. You're auto-watched when you reply to a thread (unless
//! `autoWatchOnReply` is off in [`crate::Settings`]) or when you're `@mentioned`;
//! these endpoints let you watch/unwatch explicitly and list what you watch.
use reqwest::Method;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

/// Server payload for the watch status/toggle endpoints: whether you currently
/// watch the thread. The methods below unwrap this to a bare `bool`.
#[derive(Debug, Clone, Copy, Deserialize)]
struct WatchState {
    #[serde(default)]
    watching: bool,
}

/// A watched-thread record returned by `GET /v1/watches`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Watch {
    /// Composite document id, `<userId>_<postId>`.
    #[serde(alias = "id")]
    pub watch_id: String,

    /// The watched thread's post id.
    #[serde(default)]
    pub post_id: String,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
}

impl Client {
    /// `GET /v1/posts/:id/watch` — whether you currently watch this thread.
    pub async fn watch_status(&self, post_id: &str) -> Result<bool> {
        let path = format!("/v1/posts/{post_id}/watch");
        let r: WatchState = self
            .request::<WatchState, ()>(EndpointKey::WatchStatus, Method::GET, &path, &[], None)
            .await?;
        Ok(r.watching)
    }

    /// `POST /v1/posts/:id/watch` — start watching a thread. Idempotent; returns
    /// the resulting watch state (`true`).
    pub async fn watch_thread(&self, post_id: &str) -> Result<bool> {
        let path = format!("/v1/posts/{post_id}/watch");
        let r: WatchState = self
            .request::<WatchState, ()>(EndpointKey::WatchCreate, Method::POST, &path, &[], None)
            .await?;
        Ok(r.watching)
    }

    /// `DELETE /v1/posts/:id/watch` — stop watching a thread. Returns the
    /// resulting watch state (`false`).
    pub async fn unwatch_thread(&self, post_id: &str) -> Result<bool> {
        let path = format!("/v1/posts/{post_id}/watch");
        let r: WatchState = self
            .request::<WatchState, ()>(EndpointKey::WatchDelete, Method::DELETE, &path, &[], None)
            .await?;
        Ok(r.watching)
    }

    /// `GET /v1/watches` — your watched threads, newest first.
    pub async fn list_watches(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Watch>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.request_page(EndpointKey::WatchesList, Method::GET, "/v1/watches", &query)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_state_decodes() {
        let on: WatchState = serde_json::from_str(r#"{"watching":true}"#).unwrap();
        assert!(on.watching);
        let off: WatchState = serde_json::from_str(r#"{"watching":false}"#).unwrap();
        assert!(!off.watching);
    }

    #[test]
    fn watch_state_defaults_false_when_missing() {
        let s: WatchState = serde_json::from_str(r#"{}"#).unwrap();
        assert!(!s.watching);
    }

    #[test]
    fn watch_record_decodes_documented_shape() {
        let raw = r#"{
            "id": "user123_abc123",
            "postId": "abc123",
            "createdAt": "2026-03-27T10:12:01Z"
        }"#;
        let w: Watch = serde_json::from_str(raw).unwrap();
        assert_eq!(w.watch_id, "user123_abc123");
        assert_eq!(w.post_id, "abc123");
        assert!(w.created_at.is_some());
    }

    #[test]
    fn watch_record_tolerates_missing_fields() {
        let w: Watch = serde_json::from_str(r#"{"id":"u_p"}"#).unwrap();
        assert_eq!(w.watch_id, "u_p");
        assert!(w.post_id.is_empty());
        assert!(w.created_at.is_none());
    }
}

//! User profile types and endpoints (`/v1/users/*`, API v0.8.4 § Users).
//!
//! `User` models the `/v1/users/me` and `/v1/users/:username` response shapes.
//! Because the spec still only enumerates fields under PATCH input (§ Update
//! Own Profile), several optional response fields (followers/following/posts
//! counts, supporter flags) are inferred and decoded leniently.
use reqwest::Method;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::types::{Entry, Reply};

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(alias = "userId")]
    pub id: String,
    pub username: String,

    #[serde(default)]
    pub display_name: Option<String>,

    /// Present only on `/v1/users/me`.
    #[serde(default)]
    pub email: Option<String>,

    #[serde(default)]
    pub bio: Option<String>,

    #[serde(default)]
    pub pinned_post_id: Option<String>,

    #[serde(default)]
    pub website_url: Option<String>,
    #[serde(default)]
    pub website_name: Option<String>,
    #[serde(default)]
    pub website_image_url: Option<String>,

    #[serde(default)]
    pub location_latitude: Option<f64>,
    #[serde(default)]
    pub location_longitude: Option<f64>,
    #[serde(default)]
    pub location_name: Option<String>,

    #[serde(default)]
    pub followers_count: Option<u32>,
    #[serde(default)]
    pub following_count: Option<u32>,
    #[serde(default)]
    pub posts_count: Option<u32>,

    /// Whether the *viewing* user currently follows this user. May be absent.
    #[serde(default)]
    pub is_following: Option<bool>,

    /// The follow-document id between viewer and this user, when followed.
    /// Used to unfollow without making an extra round-trip.
    #[serde(default)]
    pub follow_id: Option<String>,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
}

/// Result of `POST /v1/users/:username/poke` (API v0.8.4 § Poke a User).
///
/// The server echoes the poked user back, so a client that pokes by handle
/// learns the id without a second lookup.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PokeResponse {
    /// Id of the poked user.
    #[serde(default)]
    pub user_id: String,

    /// Handle of the poked user, echoed back by the server.
    #[serde(default)]
    pub username: String,

    /// True once the nudge was delivered. A refused poke comes back as an
    /// error, not as `poked: false`, so this is true on every success.
    #[serde(default)]
    pub poked: bool,
}

impl Client {
    /// `GET /v1/users/me` — the authenticated user's profile.
    pub async fn get_own_profile(&self) -> Result<User> {
        self.request::<User, ()>(
            EndpointKey::UsersGetMe,
            Method::GET,
            "/v1/users/me",
            &[],
            None,
        )
        .await
    }

    /// `GET /v1/users/:username` — any user's public profile.
    pub async fn get_profile(&self, username: &str) -> Result<User> {
        let path = format!("/v1/users/{username}");
        self.request::<User, ()>(EndpointKey::UsersGet, Method::GET, &path, &[], None)
            .await
    }

    /// `GET /v1/users/:username/posts` — that user's entries, newest first.
    pub async fn list_user_posts(
        &self,
        username: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Entry>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let path = format!("/v1/users/{username}/posts");
        self.request_page(EndpointKey::UsersListPosts, Method::GET, &path, &query)
            .await
    }

    /// `GET /v1/users/:username/replies` — that user's replies, newest first.
    pub async fn list_user_replies(
        &self,
        username: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Reply>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let path = format!("/v1/users/{username}/replies");
        self.request_page(EndpointKey::UsersListReplies, Method::GET, &path, &query)
            .await
    }

    /// `POST /v1/users/:username/poke`, a nudge, the same notification the web
    /// client's `[P] Poke` button sends (API v0.8.4 § Poke a User). No body.
    ///
    /// The target gets a `poke` notification; there is nothing to un-poke.
    /// Errors follow the spec: `400` for poking yourself, `403` when either side
    /// has blocked the other, `404` for an unknown handle.
    ///
    /// The budget (1/hour, 8/day) is global across all users rather than per
    /// user, so one poke spends the whole allowance for the hour. Use
    /// [`time_until_writable`](Client::time_until_writable) with
    /// [`EndpointKey::UsersPoke`] to show the wait instead of blocking on it.
    pub async fn poke_user(&self, username: &str) -> Result<PokeResponse> {
        let path = format!("/v1/users/{username}/poke");
        match self
            .request::<PokeResponse, ()>(EndpointKey::UsersPoke, Method::POST, &path, &[], None)
            .await
        {
            Ok(r) => Ok(r),
            Err(e) => {
                // The spec says a rejected poke (400 self-poke, 403 blocked, 404
                // unknown user) "doesn't count against it", but our limiter
                // spends its token before the request is sent. Without a refund
                // one mistyped handle would lock poking out locally for a full
                // hour while the server would still have allowed it, so hand the
                // token back on exactly those three statuses.
                //
                // Nothing else is refunded. A 429 means the token really was
                // spent, and a transport error may well have reached the server
                // and been counted there, so both keep the deduction rather than
                // risk letting the client spend more than the server allows.
                if matches!(&e, ApiError::Api { status, .. } if matches!(*status, 400 | 403 | 404))
                {
                    self.refund_rate_limit(EndpointKey::UsersPoke, None);
                }
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_decodes_full_shape() {
        let raw = r#"{
            "id": "u1",
            "username": "alice",
            "displayName": "Alice A.",
            "email": "a@b.c",
            "bio": "hi",
            "pinnedPostId": "p1",
            "websiteUrl": "https://example.com",
            "websiteName": "ex",
            "websiteImageUrl": "https://example.com/i.png",
            "locationLatitude": 51.5,
            "locationLongitude": -0.1,
            "locationName": "London",
            "followersCount": 42,
            "followingCount": 17,
            "postsCount": 5,
            "isFollowing": true,
            "followId": "f1",
            "createdAt": "2026-01-01T00:00:00Z"
        }"#;
        let u: User = serde_json::from_str(raw).unwrap();
        assert_eq!(u.id, "u1");
        assert_eq!(u.username, "alice");
        assert_eq!(u.display_name.as_deref(), Some("Alice A."));
        assert_eq!(u.email.as_deref(), Some("a@b.c"));
        assert_eq!(u.followers_count, Some(42));
        assert_eq!(u.is_following, Some(true));
        assert_eq!(u.follow_id.as_deref(), Some("f1"));
    }

    #[test]
    fn user_tolerates_minimal_shape() {
        let raw = r#"{"id":"u1","username":"alice"}"#;
        let u: User = serde_json::from_str(raw).unwrap();
        assert_eq!(u.id, "u1");
        assert!(u.bio.is_none());
        assert!(u.followers_count.is_none());
        assert!(u.is_following.is_none());
    }

    #[test]
    fn user_accepts_user_id_alias() {
        let raw = r#"{"userId":"u1","username":"alice"}"#;
        let u: User = serde_json::from_str(raw).unwrap();
        assert_eq!(u.id, "u1");
    }

    #[test]
    fn poke_response_decodes_documented_shape() {
        let raw = r#"{"userId":"u2","username":"bob","poked":true}"#;
        let p: PokeResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(p.user_id, "u2");
        assert_eq!(p.username, "bob");
        assert!(p.poked);
    }

    #[test]
    fn poke_response_tolerates_missing_fields() {
        let p: PokeResponse = serde_json::from_str("{}").unwrap();
        assert!(p.user_id.is_empty());
        assert!(p.username.is_empty());
        assert!(!p.poked);
    }
}

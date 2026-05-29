//! User profile types and endpoints (`/v1/users/*`).
//!
//! `User` models the `/v1/users/me` and `/v1/users/:username` response shapes.
//! Because the v0.3.6 spec only enumerates fields under PATCH input, several
//! optional response fields (followers/following/posts counts, supporter flags)
//! are inferred and decoded leniently.
use reqwest::Method;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;
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
}

//! Follow types and endpoints (`/v1/follows/*`).
use reqwest::Method;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

/// Which side of the follow relationship to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowsDirection {
    /// People who follow the target user.
    Followers,
    /// People the target user follows.
    Following,
}

impl FollowsDirection {
    fn wire(self) -> &'static str {
        match self {
            Self::Followers => "followers",
            Self::Following => "following",
        }
    }
}

/// A follow edge between two users. The "id" is the follow document id —
/// pass it back to [`Client::unfollow`] to remove the relationship.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    #[serde(alias = "id")]
    pub follow_id: String,

    #[serde(default)]
    pub follower_id: String,
    #[serde(default)]
    pub followed_id: String,

    #[serde(default)]
    pub follower_username: String,
    #[serde(default)]
    pub followed_username: String,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize)]
struct FollowBody<'a> {
    #[serde(rename = "followedId")]
    followed_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct FollowCreated {
    #[serde(alias = "id")]
    follow_id: String,
}

impl Client {
    /// `GET /v1/follows?type=…` — list followers/following of a user (or yourself
    /// when `user_id` is `None`).
    pub async fn list_follows(
        &self,
        direction: FollowsDirection,
        user_id: Option<&str>,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Follow>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![
            ("type", direction.wire().to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(uid) = user_id {
            query.push(("userId", uid.to_string()));
        }
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.request_page(EndpointKey::FollowsList, Method::GET, "/v1/follows", &query)
            .await
    }

    /// `POST /v1/follows` — follow a user. Returns the new follow document id.
    pub async fn follow_user(&self, followed_id: &str) -> Result<String> {
        let body = FollowBody { followed_id };
        let r: FollowCreated = self
            .request(
                EndpointKey::FollowsCreate,
                Method::POST,
                "/v1/follows",
                &[],
                Some(&body),
            )
            .await?;
        Ok(r.follow_id)
    }

    /// `DELETE /v1/follows/:id` — unfollow by the document id returned at
    /// follow-time (or surfaced via `User::follow_id`).
    pub async fn unfollow(&self, follow_id: &str) -> Result<()> {
        let path = format!("/v1/follows/{follow_id}");
        self.request_unit(EndpointKey::FollowsDelete, Method::DELETE, &path, &[])
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_direction_wire() {
        assert_eq!(FollowsDirection::Followers.wire(), "followers");
        assert_eq!(FollowsDirection::Following.wire(), "following");
    }

    #[test]
    fn follow_decodes() {
        let raw = r#"{
            "followId": "f1",
            "followerId": "u1",
            "followedId": "u2",
            "followerUsername": "alice",
            "followedUsername": "bob",
            "createdAt": "2026-03-27T10:12:01Z"
        }"#;
        let f: Follow = serde_json::from_str(raw).unwrap();
        assert_eq!(f.follow_id, "f1");
        assert_eq!(f.follower_id, "u1");
        assert_eq!(f.followed_id, "u2");
        assert_eq!(f.follower_username, "alice");
        assert_eq!(f.followed_username, "bob");
        assert!(f.created_at.is_some());
    }

    #[test]
    fn follow_accepts_id_alias() {
        let raw = r#"{"id":"f1","followerUsername":"a","followedUsername":"b"}"#;
        let f: Follow = serde_json::from_str(raw).unwrap();
        assert_eq!(f.follow_id, "f1");
    }

    #[test]
    fn follow_body_serializes_documented_field_name() {
        let body = FollowBody { followed_id: "u2" };
        let s = serde_json::to_string(&body).unwrap();
        assert_eq!(s, r#"{"followedId":"u2"}"#);
    }
}

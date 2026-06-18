//! Guild types and endpoints (`/v1/guilds/*`, v0.5.1).
//!
//! Guilds are member groups with their own forum of threads; a user belongs to
//! at most one. Founding a guild and editing its profile happen on the web —
//! the API covers discovery, membership, and the forum. A thread is an ordinary
//! [`Entry`] carrying guild context, modeled here as [`GuildThread`].
use reqwest::Method;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::entries::{validate_content_topics, validate_slug, validate_title, CreatedEntry};
use crate::error::Result;
use crate::types::Entry;

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

/// A member's role within a guild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuildRole {
    Founder,
    Member,
    /// Any role the client doesn't model yet (forward compatibility).
    #[serde(other)]
    Unknown,
}

/// A guild (member group). Discovery + membership only.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guild {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub founder_id: String,
    #[serde(default)]
    pub founder_username: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub profile_picture_url: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub link_text: Option<String>,
    #[serde(default)]
    pub member_count: u32,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
    /// Caller's membership — populated by `GET /v1/guilds/:slug`, absent (so
    /// `false`/`None`) in list responses.
    #[serde(default)]
    pub is_member: bool,
    #[serde(default)]
    pub role: Option<GuildRole>,
}

/// One membership row from `GET /v1/guilds/:slug/members`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuildMembership {
    #[serde(alias = "id")]
    pub membership_id: String,
    #[serde(default)]
    pub guild_id: String,
    #[serde(default)]
    pub guild_slug: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub role: Option<GuildRole>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub joined_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub profile_picture_url: Option<String>,
}

/// A guild forum thread: an ordinary [`Entry`] plus its guild context. The
/// server returns entry fields and the guild fields in one flat object.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuildThread {
    #[serde(flatten)]
    pub entry: Entry,
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub guild_slug: Option<String>,
    #[serde(default)]
    pub is_guild_thread: bool,
}

/// Result of [`Client::join_guild`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinedGuild {
    pub guild_id: String,
    #[serde(default)]
    pub role: Option<GuildRole>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateGuildThreadBody<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slug: Option<&'a str>,
    topics: &'a [String],
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedThreadResponse {
    post_id: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeftGuild {
    guild_id: String,
}

impl Client {
    /// `GET /v1/guilds` — guilds with at least one member, most populated first.
    pub async fn list_guilds(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Guild>, Option<String>)> {
        let query = page_query(cursor, limit);
        self.request_page(EndpointKey::GuildsList, Method::GET, "/v1/guilds", &query)
            .await
    }

    /// `GET /v1/guilds/:slug` — a guild plus the caller's membership state
    /// (`is_member`, `role`). 404 if no guild has that slug.
    pub async fn get_guild(&self, slug: &str) -> Result<Guild> {
        let path = format!("/v1/guilds/{slug}");
        self.request::<Guild, ()>(EndpointKey::GuildsGet, Method::GET, &path, &[], None)
            .await
    }

    /// `GET /v1/guilds/:slug/members` — memberships, oldest-joined first.
    pub async fn list_guild_members(
        &self,
        slug: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<GuildMembership>, Option<String>)> {
        let query = page_query(cursor, limit);
        let path = format!("/v1/guilds/{slug}/members");
        self.request_page(EndpointKey::GuildsMembersList, Method::GET, &path, &query)
            .await
    }

    /// `GET /v1/guilds/:slug/posts` — the guild's threads, most recently active
    /// first.
    pub async fn list_guild_threads(
        &self,
        slug: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<GuildThread>, Option<String>)> {
        let query = page_query(cursor, limit);
        let path = format!("/v1/guilds/{slug}/posts");
        self.request_page(EndpointKey::GuildsThreadsList, Method::GET, &path, &query)
            .await
    }

    /// `POST /v1/guilds/:slug/posts` — create a thread. You must be a member of
    /// the guild; non-members get `ApiError::Api { code: Forbidden }` (403).
    /// Validation mirrors [`Client::create_entry`]; guild threads carry no
    /// public/NSFW flags.
    ///
    /// Rate limit: 2/min, 15/day.
    pub async fn create_guild_thread(
        &self,
        guild_slug: &str,
        content: &str,
        title: Option<&str>,
        thread_slug: Option<&str>,
        topics: &[String],
    ) -> Result<CreatedEntry> {
        validate_content_topics(content, topics)?;
        if let Some(t) = title {
            validate_title(t)?;
        }
        if let Some(s) = thread_slug {
            validate_slug(s)?;
        }
        let body = CreateGuildThreadBody {
            content,
            title,
            slug: thread_slug,
            topics,
        };
        let path = format!("/v1/guilds/{guild_slug}/posts");
        let r: CreatedThreadResponse = self
            .request(
                EndpointKey::GuildsThreadsCreate,
                Method::POST,
                &path,
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

    /// `POST /v1/guilds/:slug/join` — join as a member. A user can be in only
    /// one guild; the server returns 409 (`ApiError::Api { code: Conflict }`)
    /// if you're already in another.
    pub async fn join_guild(&self, slug: &str) -> Result<JoinedGuild> {
        let path = format!("/v1/guilds/{slug}/join");
        self.request::<JoinedGuild, ()>(EndpointKey::GuildsJoin, Method::POST, &path, &[], None)
            .await
    }

    /// `POST /v1/guilds/:slug/leave` — leave, returning the guild id. Founders
    /// can't leave via the API (`ApiError::Api { code: Forbidden }`, 403);
    /// `ApiError::Api { code: NotFound }` (404) if you aren't a member.
    pub async fn leave_guild(&self, slug: &str) -> Result<String> {
        let path = format!("/v1/guilds/{slug}/leave");
        let r: LeftGuild = self
            .request::<LeftGuild, ()>(EndpointKey::GuildsLeave, Method::POST, &path, &[], None)
            .await?;
        Ok(r.guild_id)
    }
}

fn page_query(cursor: Option<&str>, limit: Option<u32>) -> Vec<(&'static str, String)> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
    let mut query: Vec<(&'static str, String)> = vec![("limit", limit.to_string())];
    if let Some(c) = cursor {
        query.push(("cursor", c.to_string()));
    }
    query
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guild_role_decodes_known_and_unknown() {
        assert_eq!(
            serde_json::from_str::<GuildRole>(r#""founder""#).unwrap(),
            GuildRole::Founder
        );
        assert_eq!(
            serde_json::from_str::<GuildRole>(r#""member""#).unwrap(),
            GuildRole::Member
        );
        assert_eq!(
            serde_json::from_str::<GuildRole>(r#""moderator""#).unwrap(),
            GuildRole::Unknown
        );
    }

    #[test]
    fn guild_decodes_full_shape() {
        let raw = r#"{
            "id": "g1",
            "name": "Night Owls",
            "slug": "night-owls",
            "founderId": "uid",
            "founderUsername": "someone",
            "icon": "🦉",
            "profilePictureUrl": "https://x/p.png",
            "bio": "We never sleep",
            "link": "https://x",
            "linkText": "our site",
            "memberCount": 42,
            "createdAt": "2026-03-27T10:12:01.516Z"
        }"#;
        let g: Guild = serde_json::from_str(raw).unwrap();
        assert_eq!(g.id, "g1");
        assert_eq!(g.slug, "night-owls");
        assert_eq!(g.founder_username, "someone");
        assert_eq!(g.member_count, 42);
        assert_eq!(g.link_text.as_deref(), Some("our site"));
        assert!(g.created_at.is_some());
        // List shape has no membership info.
        assert!(!g.is_member);
        assert!(g.role.is_none());
    }

    #[test]
    fn guild_decodes_get_shape_with_membership() {
        let raw = r#"{"id":"g1","slug":"s","isMember":true,"role":"founder"}"#;
        let g: Guild = serde_json::from_str(raw).unwrap();
        assert!(g.is_member);
        assert_eq!(g.role, Some(GuildRole::Founder));
    }

    #[test]
    fn guild_membership_decodes() {
        let raw = r#"{
            "membershipId": "g1_uid",
            "guildId": "g1",
            "guildSlug": "night-owls",
            "userId": "uid",
            "username": "someone",
            "role": "member",
            "joinedAt": "2026-03-27T10:12:01.516Z",
            "displayName": "Some One",
            "profilePictureUrl": "https://x/p.png"
        }"#;
        let m: GuildMembership = serde_json::from_str(raw).unwrap();
        assert_eq!(m.membership_id, "g1_uid");
        assert_eq!(m.guild_slug, "night-owls");
        assert_eq!(m.role, Some(GuildRole::Member));
        assert_eq!(m.display_name.as_deref(), Some("Some One"));
        assert!(m.joined_at.is_some());
    }

    #[test]
    fn guild_membership_accepts_id_alias() {
        let raw = r#"{"id":"g1_uid","username":"someone"}"#;
        let m: GuildMembership = serde_json::from_str(raw).unwrap();
        assert_eq!(m.membership_id, "g1_uid");
    }

    #[test]
    fn guild_thread_flattens_entry_plus_guild_fields() {
        let raw = r#"{
            "postId": "p1",
            "authorId": "u",
            "authorUsername": "a",
            "content": "thread body",
            "title": "Hello",
            "guildId": "g1",
            "guildSlug": "night-owls",
            "isGuildThread": true
        }"#;
        let t: GuildThread = serde_json::from_str(raw).unwrap();
        assert_eq!(t.entry.post_id, "p1");
        assert_eq!(t.entry.content, "thread body");
        assert_eq!(t.entry.title.as_deref(), Some("Hello"));
        assert_eq!(t.guild_id.as_deref(), Some("g1"));
        assert_eq!(t.guild_slug.as_deref(), Some("night-owls"));
        assert!(t.is_guild_thread);
    }

    #[test]
    fn joined_guild_decodes() {
        let j: JoinedGuild = serde_json::from_str(r#"{"guildId":"g1","role":"member"}"#).unwrap();
        assert_eq!(j.guild_id, "g1");
        assert_eq!(j.role, Some(GuildRole::Member));
    }

    #[test]
    fn left_guild_decodes() {
        let l: LeftGuild = serde_json::from_str(r#"{"guildId":"g1"}"#).unwrap();
        assert_eq!(l.guild_id, "g1");
    }

    #[test]
    fn create_thread_body_omits_optional_and_has_no_public_nsfw() {
        let topics = vec!["music".to_string()];
        let body = CreateGuildThreadBody {
            content: "hi",
            title: None,
            slug: None,
            topics: &topics,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""content":"hi""#));
        assert!(s.contains(r#""topics":["music"]"#));
        assert!(!s.contains("title"));
        assert!(!s.contains("slug"));
        // Guild threads have no public/NSFW flags.
        assert!(!s.contains("isPublic"));
        assert!(!s.contains("isNSFW"));
    }

    #[test]
    fn create_thread_body_includes_title_and_slug_when_set() {
        let body = CreateGuildThreadBody {
            content: "hi",
            title: Some("T"),
            slug: Some("t"),
            topics: &[],
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""title":"T""#));
        assert!(s.contains(r#""slug":"t""#));
    }

    #[test]
    fn page_query_clamps_and_threads_cursor() {
        let q = page_query(Some("c1"), Some(9999));
        assert!(q.contains(&("limit", "50".to_string())));
        assert!(q.contains(&("cursor", "c1".to_string())));
        let q0 = page_query(None, None);
        assert!(q0.contains(&("limit", "20".to_string())));
        assert!(!q0.iter().any(|(k, _)| *k == "cursor"));
    }
}

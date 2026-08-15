//! User profile types and endpoints (`/v1/users/*`, API v0.8.6 § Users).
//!
//! `User` models the `/v1/users/me` and `/v1/users/:username` response shapes.
//! Because the spec still only enumerates fields under PATCH input (§ Update
//! Own Profile), several optional response fields (followers/following/posts
//! counts, supporter flags) are inferred and decoded leniently.
//!
//! The guild fields on a profile describe one guild, the badge. Since v0.8.6 a
//! user can also hold up to five apprenticeships alongside it, and
//! [`UserGuild`] plus [`Client::list_user_guilds`] are how a client sees those.
use reqwest::Method;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::guilds::GuildRole;
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

    /// The guild badge: the one guild this user holds as founder or member
    /// (§ Get User Profile). Apprenticeships are NOT here, they come from
    /// `GET /v1/users/:username/guilds`, so a profile showing only these fields
    /// is showing the badge and nothing more.
    ///
    /// All four are absent for a user who is in no guild.
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub guild_slug: Option<String>,
    #[serde(default)]
    pub guild_icon: Option<String>,
    #[serde(default)]
    pub guild_name: Option<String>,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
}

/// One guild a user is in, from `GET /v1/users/:username/guilds` (API v0.8.6
/// § List a User's Guilds).
///
/// This is a membership seen from the user's side, so it carries the guild's
/// identity and the role the user holds in it, not the guild's own counts or
/// profile text. Fetch [`Guild`](crate::Guild) by slug for those.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserGuild {
    /// Id of the guild.
    pub guild_id: String,

    /// Guild slug, the identifier every `/v1/guilds/:slug` route takes.
    #[serde(default)]
    pub slug: String,

    /// Guild display name.
    #[serde(default)]
    pub name: String,

    /// Guild icon, typically a single emoji.
    #[serde(default)]
    pub icon: Option<String>,

    /// Guild avatar. Optional in the documented shape: the apprenticeship in
    /// the spec's own example has none.
    #[serde(default)]
    pub profile_picture_url: Option<String>,

    /// The user's role here: founder, member or apprentice. Exactly one entry
    /// in the list is a badge role, see [`UserGuild::is_badge`].
    #[serde(default)]
    pub role: Option<GuildRole>,

    /// When the user joined this guild.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub joined_at: Option<OffsetDateTime>,
}

impl UserGuild {
    /// Whether this is the badge guild, the one whose name and icon appear on
    /// the user's profile, rather than one of their apprenticeships.
    ///
    /// The list is ordered badge-guild first (§ List a User's Guilds), but
    /// reading the role says so outright and keeps working if a user has no
    /// badge guild at all, which leaves the list all apprenticeships.
    #[must_use]
    pub fn is_badge(&self) -> bool {
        matches!(self.role, Some(GuildRole::Founder | GuildRole::Member))
    }
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
    ///
    /// The profile's guild fields describe the badge guild only (API v0.8.6
    /// § Get User Profile). Apprenticeships are not on the profile at all; they
    /// come from [`Client::list_user_guilds`].
    pub async fn get_profile(&self, username: &str) -> Result<User> {
        let path = format!("/v1/users/{username}");
        self.request::<User, ()>(EndpointKey::UsersGet, Method::GET, &path, &[], None)
            .await
    }

    /// `GET /v1/users/me/guilds`, every guild the authenticated user is in.
    ///
    /// The `me` form of [`Client::list_user_guilds`]; the ordering, the size
    /// bound and the reason there is no cursor are all documented there.
    pub async fn list_own_guilds(&self) -> Result<Vec<UserGuild>> {
        self.request::<Vec<UserGuild>, ()>(
            EndpointKey::UsersListGuilds,
            Method::GET,
            "/v1/users/me/guilds",
            &[],
            None,
        )
        .await
    }

    /// `GET /v1/users/:username/guilds`, every guild that user is in (API
    /// v0.8.6 § List a User's Guilds): the badge guild first, then their
    /// apprenticeships oldest first. `ApiError::Api { code: NotFound }` (404)
    /// for an unknown handle.
    ///
    /// Deliberately **not** routed through the crate's paged request helper,
    /// unlike every other `list_*` here. A user holds at most six guilds, one
    /// badge plus five apprenticeships, so the spec states outright that the
    /// endpoint has no pagination and `cursor` is always null. Returning the
    /// `(items, cursor)` pair the paged helper produces would hand callers a
    /// cursor that can never be `Some`, implying a second page to fetch and
    /// inviting a paging loop with no exit condition to get wrong. The body is
    /// still the ordinary list envelope, and decoding `data` as the whole
    /// payload simply ignores the null `cursor` beside it.
    ///
    /// Rate limit: 30/min.
    pub async fn list_user_guilds(&self, username: &str) -> Result<Vec<UserGuild>> {
        let path = format!("/v1/users/{username}/guilds");
        self.request::<Vec<UserGuild>, ()>(
            EndpointKey::UsersListGuilds,
            Method::GET,
            &path,
            &[],
            None,
        )
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

    use crate::envelope::Data;

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
    fn user_guild_decodes_the_documented_shape() {
        let raw = r#"{
            "guildId": "g1",
            "slug": "night-owls",
            "name": "Night Owls",
            "icon": "🦉",
            "profilePictureUrl": "https://x/p.png",
            "role": "member",
            "joinedAt": "2026-03-27T10:12:01.516Z"
        }"#;
        let g: UserGuild = serde_json::from_str(raw).unwrap();
        assert_eq!(g.guild_id, "g1");
        assert_eq!(g.slug, "night-owls");
        assert_eq!(g.name, "Night Owls");
        assert_eq!(g.icon.as_deref(), Some("🦉"));
        assert_eq!(g.profile_picture_url.as_deref(), Some("https://x/p.png"));
        assert_eq!(g.role, Some(GuildRole::Member));
        assert!(g.joined_at.is_some());
        assert!(g.is_badge());
    }

    #[test]
    fn an_apprenticeship_row_needs_no_profile_picture_and_is_not_the_badge() {
        // The apprenticeship in the spec's own example omits
        // `profilePictureUrl`, so requiring it would fail the whole list.
        let raw = r#"{
            "guildId": "g2",
            "slug": "deep-divers",
            "name": "Deep Divers",
            "icon": "🐳",
            "role": "apprentice",
            "joinedAt": "2026-05-02T18:44:12.004Z"
        }"#;
        let g: UserGuild = serde_json::from_str(raw).unwrap();
        assert_eq!(g.role, Some(GuildRole::Apprentice));
        assert!(g.profile_picture_url.is_none());
        assert!(
            !g.is_badge(),
            "an apprenticeship never wears the profile badge"
        );
    }

    #[test]
    fn a_founder_row_is_the_badge_guild_too() {
        let g = UserGuild {
            role: Some(GuildRole::Founder),
            ..UserGuild::default()
        };
        assert!(g.is_badge());

        // A role the client doesn't model yet is not assumed to be a badge:
        // guessing wrong here puts a guild on the profile that isn't there.
        let unknown = UserGuild {
            role: Some(GuildRole::Unknown),
            ..UserGuild::default()
        };
        assert!(!unknown.is_badge());
        assert!(!UserGuild::default().is_badge());
    }

    #[test]
    fn the_user_guilds_body_decodes_whole_ignoring_its_always_null_cursor() {
        // § List a User's Guilds is not paginated, so `list_user_guilds`
        // decodes the body as a plain `{ "data": [...] }` and returns a `Vec`
        // with no cursor. The envelope still carries `cursor: null`, and this
        // pins that the extra key is ignored rather than failing the decode.
        let raw = r#"{
            "data": [
                {"guildId":"g1","slug":"night-owls","name":"Night Owls","role":"member","joinedAt":"2026-03-27T10:12:01.516Z"},
                {"guildId":"g2","slug":"deep-divers","name":"Deep Divers","role":"apprentice","joinedAt":"2026-05-02T18:44:12.004Z"}
            ],
            "cursor": null
        }"#;
        let env: Data<Vec<UserGuild>> = serde_json::from_str(raw).unwrap();
        assert_eq!(env.data.len(), 2);
        // Badge guild first, then apprenticeships oldest first.
        assert!(env.data[0].is_badge());
        assert!(!env.data[1].is_badge());
        assert_eq!(env.data[1].slug, "deep-divers");
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

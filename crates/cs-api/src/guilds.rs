//! Guild types and endpoints (`/v1/guilds/*`, API v0.8.6 § Guilds).
//!
//! Guilds are member groups with their own forum of threads. A user can be in
//! several at once, in two capacities: one *badge* guild, held as `founder` or
//! `member`, which is what the `guildId` / `guildSlug` / `guildIcon` /
//! `guildName` fields on a user object describe, plus up to five
//! *apprenticeships*, held as `apprentice`. An apprentice appears in the
//! guild's member list and receives its new-thread notifications, but the
//! profile badge stays with the badge guild.
//!
//! Founding a guild and editing its profile happen on the web, so the API
//! covers discovery, membership, and the forum. A thread is an ordinary
//! [`Entry`] carrying guild context, modeled here as [`GuildThread`]. Every
//! guild a given user is in, both capacities together, comes from
//! [`Client::list_user_guilds`](crate::Client::list_user_guilds).
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

/// A member's role within a guild (API v0.8.6 § Guilds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuildRole {
    /// Founded the guild. A badge role: it is the guild on the user's profile,
    /// and founders cannot leave through the API.
    Founder,
    /// Joined the guild as its badge holder. A user has one badge guild at a
    /// time, as `Founder` or `Member`.
    Member,
    /// One of the user's up-to-five apprenticeships, added in v0.8.6. An
    /// apprentice is in the member list and gets the guild's new-thread
    /// notifications, but the profile badge stays with the badge guild, so an
    /// apprenticeship never shows there.
    Apprentice,
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
    /// Founders and members only. Apprentices are counted separately, in
    /// `apprentice_count`, and the guild list is ordered on this field alone
    /// (API v0.8.6 § List Guilds).
    #[serde(default)]
    pub member_count: u32,
    /// Apprentices (API v0.8.6 § List Guilds). Guilds that predate
    /// apprenticeships omit the field, which the spec says to read as 0, so a
    /// `#[serde(default)]` `u32` is exactly the documented behaviour. Use
    /// [`Guild::headcount`] for the total rather than either count alone.
    #[serde(default)]
    pub apprentice_count: u32,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
    /// Caller's membership, populated by `GET /v1/guilds/:slug` and absent (so
    /// `false`/`None`) in list responses.
    #[serde(default)]
    pub is_member: bool,
    /// Caller's role in this guild, which since v0.8.6 may be
    /// [`GuildRole::Apprentice`] as well as founder or member (§ Get Guild).
    #[serde(default)]
    pub role: Option<GuildRole>,
}

impl Guild {
    /// Total headcount: `member_count + apprentice_count`, the sum API v0.8.6
    /// § List Guilds defines. Saturating, so a nonsense pair of counts cannot
    /// panic a release build in one place and wrap in another.
    ///
    /// Note that this is *not* what the guild list is ordered by: § List Guilds
    /// orders on `member_count` alone, so apprentices do not move a guild up.
    #[must_use]
    pub fn headcount(&self) -> u32 {
        self.member_count.saturating_add(self.apprentice_count)
    }
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
    /// Id of the guild just joined.
    pub guild_id: String,
    /// The role the server picked (API v0.8.6 § Join a Guild): [`GuildRole::Member`]
    /// for a user with no badge guild yet, [`GuildRole::Apprentice`] otherwise.
    /// The caller cannot ask for one or predict it, so read it rather than
    /// assuming a join made you a member.
    #[serde(default)]
    pub role: Option<GuildRole>,
}

/// Result of [`Client::promote_guild`] (API v0.8.6 § Change Your Guild Badge).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotedGuild {
    /// Id of the guild that now carries the profile badge.
    pub guild_id: String,
    /// What the caller now is in that guild: [`GuildRole::Member`] after a
    /// promotion, or the role they already held when the guild was already
    /// their badge guild and nothing changed ([`GuildRole::Member`] or
    /// [`GuildRole::Founder`]).
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
    /// `GET /v1/guilds`, guilds with at least one member, most populated first.
    ///
    /// Ordering is by `member_count` alone (API v0.8.6 § List Guilds), so
    /// apprentices never move a guild up the list even though they count
    /// towards [`Guild::headcount`].
    pub async fn list_guilds(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Guild>, Option<String>)> {
        let query = page_query(cursor, limit);
        self.request_page(EndpointKey::GuildsList, Method::GET, "/v1/guilds", &query)
            .await
    }

    /// `GET /v1/guilds/:slug`, a guild plus the caller's membership state
    /// (`is_member`, `role`). 404 if no guild has that slug.
    ///
    /// Since v0.8.6 the role may be [`GuildRole::Apprentice`] (§ Get Guild), so
    /// a caller that reads `is_member` as "this is my guild" now has to check
    /// the role too: an apprentice is in the guild without wearing its badge.
    pub async fn get_guild(&self, slug: &str) -> Result<Guild> {
        let path = format!("/v1/guilds/{slug}");
        self.request::<Guild, ()>(EndpointKey::GuildsGet, Method::GET, &path, &[], None)
            .await
    }

    /// `GET /v1/guilds/:slug/members`, memberships, oldest-joined first.
    ///
    /// Members and apprentices come back in one list (API v0.8.6 § List Guild
    /// Members); group the rows by [`GuildMembership::role`] to separate them
    /// the way the website does.
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

    /// `POST /v1/guilds/:slug/posts`, create a thread. Guild forums are open:
    /// any authenticated user can start a thread, membership is not required
    /// (§ Create Guild Thread). Validation mirrors [`Client::create_entry`];
    /// guild threads carry no public/NSFW flags.
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

    /// `POST /v1/guilds/:slug/join`, join a guild. No body.
    ///
    /// The server picks the role and reports it in [`JoinedGuild::role`] (API
    /// v0.8.6 § Join a Guild): [`GuildRole::Member`], badge written to the
    /// profile, when the caller has no badge guild yet, and
    /// [`GuildRole::Apprentice`], badge untouched, otherwise. Being in a guild
    /// already is no longer a reason to refuse the join, so the pre-v0.8.6 rule
    /// that a user must leave their current guild first is gone.
    ///
    /// `ApiError::Api { code: Conflict }` (409) now means one of two things:
    /// the caller is already in this guild, or they already hold the maximum of
    /// five apprenticeships and have to leave one first.
    ///
    /// Rate limit: 3/min, 15/day.
    pub async fn join_guild(&self, slug: &str) -> Result<JoinedGuild> {
        let path = format!("/v1/guilds/{slug}/join");
        self.request::<JoinedGuild, ()>(EndpointKey::GuildsJoin, Method::POST, &path, &[], None)
            .await
    }

    /// `POST /v1/guilds/:slug/promote`, make an apprenticeship the caller's
    /// badge guild (API v0.8.6 § Change Your Guild Badge). No body.
    ///
    /// The guild the caller was a member of becomes an apprenticeship rather
    /// than being left, so a promotion never drops anyone out of a guild. It is
    /// also the only way to hand the badge to an apprenticeship: leaving the
    /// badge guild clears the badge and promotes nothing.
    ///
    /// Outcomes:
    /// - `ApiError::Api { code: NotFound }` (404) if the caller isn't in this
    ///   guild at all.
    /// - `ApiError::Api { code: Forbidden }` (403) if the caller founded the
    ///   guild they are currently a member of. Handing that one over happens on
    ///   the web.
    /// - 200 with nothing changed when this is already the badge guild, with
    ///   [`PromotedGuild::role`] reporting what the caller already is.
    ///
    /// Rate limit: 3/min, 15/day.
    pub async fn promote_guild(&self, slug: &str) -> Result<PromotedGuild> {
        let path = format!("/v1/guilds/{slug}/promote");
        self.request::<PromotedGuild, ()>(
            EndpointKey::GuildsPromote,
            Method::POST,
            &path,
            &[],
            None,
        )
        .await
    }

    /// `POST /v1/guilds/:slug/leave`, leave a guild, returning its id.
    ///
    /// Which membership goes is decided by the slug, not by the badge (API
    /// v0.8.6 § Leave a Guild): leaving an apprenticeship leaves the profile
    /// badge alone, while leaving the badge guild clears the badge and promotes
    /// nothing, so an apprenticeship only takes its place if the caller then
    /// asks for it with [`Client::promote_guild`].
    ///
    /// Founders can't leave via the API (`ApiError::Api { code: Forbidden }`,
    /// 403); `ApiError::Api { code: NotFound }` (404) if the caller isn't in
    /// the guild.
    ///
    /// Rate limit: 3/min, 15/day.
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
    fn apprentice_role_decodes_as_itself_not_as_unknown() {
        // Before v0.8.6 modelled apprenticeships the wire value had no variant
        // and fell into the `#[serde(other)]` catch-all, so every apprentice in
        // a member list read as `Unknown` and could not be told from a role the
        // client has genuinely never heard of.
        assert_eq!(
            serde_json::from_str::<GuildRole>(r#""apprentice""#).unwrap(),
            GuildRole::Apprentice
        );
        assert_ne!(
            serde_json::from_str::<GuildRole>(r#""apprentice""#).unwrap(),
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
            "apprenticeCount": 7,
            "createdAt": "2026-03-27T10:12:01.516Z"
        }"#;
        let g: Guild = serde_json::from_str(raw).unwrap();
        assert_eq!(g.id, "g1");
        assert_eq!(g.slug, "night-owls");
        assert_eq!(g.founder_username, "someone");
        assert_eq!(g.member_count, 42);
        assert_eq!(g.apprentice_count, 7);
        assert_eq!(g.headcount(), 49);
        assert_eq!(g.link_text.as_deref(), Some("our site"));
        assert!(g.created_at.is_some());
        // List shape has no membership info.
        assert!(!g.is_member);
        assert!(g.role.is_none());
    }

    #[test]
    fn a_guild_predating_apprenticeships_reads_as_zero_apprentices() {
        // § List Guilds: the field is missing on older guilds and must read as
        // 0, so their headcount is still just their members rather than a
        // decode failure that would sink the whole page.
        let g: Guild = serde_json::from_str(r#"{"id":"g1","memberCount":42}"#).unwrap();
        assert_eq!(g.apprentice_count, 0);
        assert_eq!(g.headcount(), 42);
    }

    #[test]
    fn headcount_saturates_instead_of_overflowing() {
        let g = Guild {
            member_count: u32::MAX,
            apprentice_count: 5,
            ..Guild::default()
        };
        assert_eq!(g.headcount(), u32::MAX);
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
    fn a_member_list_row_can_be_an_apprentice() {
        // § List Guild Members: members and apprentices arrive in one list,
        // told apart only by `role`.
        let raw = r#"{
            "membershipId": "g1_uid",
            "guildId": "g1",
            "username": "someone",
            "role": "apprentice",
            "joinedAt": "2026-05-02T18:44:12.004Z"
        }"#;
        let m: GuildMembership = serde_json::from_str(raw).unwrap();
        assert_eq!(m.role, Some(GuildRole::Apprentice));
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
    fn a_join_can_come_back_as_an_apprenticeship() {
        // § Join a Guild: the server, not the caller, picks the role, and a
        // user who already has a badge guild joins as an apprentice. A client
        // that assumed "joined" means "member" would show the wrong badge.
        let j: JoinedGuild =
            serde_json::from_str(r#"{"guildId":"g2","role":"apprentice"}"#).unwrap();
        assert_eq!(j.role, Some(GuildRole::Apprentice));
    }

    #[test]
    fn promoted_guild_decodes_both_documented_roles() {
        // § Change Your Guild Badge returns `member` after a promotion, and on
        // the no-op call (this guild is already the badge) reports whatever the
        // caller already is, which for a guild they founded is `founder`.
        let promoted: PromotedGuild =
            serde_json::from_str(r#"{"guildId":"g1","role":"member"}"#).unwrap();
        assert_eq!(promoted.guild_id, "g1");
        assert_eq!(promoted.role, Some(GuildRole::Member));

        let unchanged: PromotedGuild =
            serde_json::from_str(r#"{"guildId":"g1","role":"founder"}"#).unwrap();
        assert_eq!(unchanged.role, Some(GuildRole::Founder));

        let roleless: PromotedGuild = serde_json::from_str(r#"{"guildId":"g1"}"#).unwrap();
        assert!(roleless.role.is_none());
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

//! Notification types and endpoints (`/v1/notifications/*`, API v0.8.6
//! § Notifications).
use reqwest::Method;
use serde::{Deserialize, Deserializer};
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

/// The handle notifications about your own account carry instead of a real
/// actor (API v0.8.6 § Notification object).
///
/// The spec's instruction is blunt: do not try to open a profile for it. Match
/// on it through [`Notification::actor_profile`] rather than comparing strings
/// at each call site.
pub const SYSTEM_ACTOR: &str = "system";

/// How many `read-all` passes [`Client::mark_all_notifications_read`] will make
/// before giving up. The server marks up to 5,000 per call (API v0.8.6 § Mark
/// All as Read), so this covers a quarter of a million unread notifications:
/// far past any real inbox, and a bound means a server that never stops saying
/// `hasMore` cannot spin the loop forever.
const MAX_READ_ALL_PASSES: u32 = 50;

/// The documented notification types (API v0.8.6 § List Notifications), plus
/// `Unknown` for forward compatibility with future types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationType {
    Bookmark,
    Reply,
    ThreadReply,
    NewFollower,
    Unfollowed,
    NewPostFollowing,
    NewPostFriend,
    /// Someone poked you via `POST /v1/users/:username/poke`, which v0.8.4
    /// opened up to API clients (§ How notifications are generated). The poker
    /// is `actor_username`; the type carries no `metadata`.
    Poke,
    ChatMention,
    PostMention,
    ReplyMention,
    /// You were mentioned in graffiti (v0.8.6). The spec names the type in
    /// § List Notifications and nowhere else: graffiti has no API surface of
    /// its own, so there is no documented `metadata` shape and nothing for a
    /// client to open beyond the actor's profile.
    GraffitiMention,
    DmMessage,
    GuildNewThread,
    SupporterGranted,
    SupporterRemoved,
    HackerGranted,
    HackerRemoved,
    /// You were made a moderator (v0.8.6). One of the account notifications
    /// with no sender; `reason` explains what happened.
    ModeratorGranted,
    /// Your moderator role was taken away (v0.8.6). No sender; see `reason`.
    ModeratorRemoved,
    /// Your account was granted API access (v0.8.6). No sender; see `reason`.
    ApiAccessGranted,
    /// Your API access was taken away (v0.8.6). No sender; see `reason`. A
    /// client that gets this is about to start failing on every call, so it is
    /// worth surfacing loudly rather than filing quietly.
    ApiAccessRemoved,
    ImagePermissionGranted,
    ImagePermissionRemoved,
    AttachmentPermissionGranted,
    AttachmentPermissionRemoved,
    SystemBan,
    /// A restriction on your account was lifted (v0.8.6). Carries the literal
    /// [`SYSTEM_ACTOR`] handle rather than omitting the actor the way
    /// `system_ban` does; `reason` explains what happened.
    SystemBanLifted,
    /// An entry you wrote was held back and saved as a private note instead
    /// (v0.8.6). Sent by [`SYSTEM_ACTOR`], with `reason` explaining the hold.
    /// It is about your own posting, so there is no other user to show.
    PostCooldown,
    /// You are approaching a posting limit (v0.8.6). Sent by [`SYSTEM_ACTOR`],
    /// with `reason` naming the limit. This is the server warning that its own
    /// budget is nearly spent, which is not something the client-side limiter
    /// in [`crate::EndpointKey`] can derive on its own.
    RateLimitWarning,
    #[serde(other)]
    Unknown,
}

impl NotificationType {
    /// Stable wire form (matches spec's `type` filter values). Used for the
    /// `type=` query parameter when listing.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Bookmark => "bookmark",
            Self::Reply => "reply",
            Self::ThreadReply => "thread_reply",
            Self::NewFollower => "new_follower",
            Self::Unfollowed => "unfollowed",
            Self::NewPostFollowing => "new_post_following",
            Self::NewPostFriend => "new_post_friend",
            Self::Poke => "poke",
            Self::ChatMention => "chat_mention",
            Self::PostMention => "post_mention",
            Self::ReplyMention => "reply_mention",
            Self::GraffitiMention => "graffiti_mention",
            Self::DmMessage => "dm_message",
            Self::GuildNewThread => "guild_new_thread",
            Self::SupporterGranted => "supporter_granted",
            Self::SupporterRemoved => "supporter_removed",
            Self::HackerGranted => "hacker_granted",
            Self::HackerRemoved => "hacker_removed",
            Self::ModeratorGranted => "moderator_granted",
            Self::ModeratorRemoved => "moderator_removed",
            Self::ApiAccessGranted => "api_access_granted",
            Self::ApiAccessRemoved => "api_access_removed",
            Self::ImagePermissionGranted => "image_permission_granted",
            Self::ImagePermissionRemoved => "image_permission_removed",
            Self::AttachmentPermissionGranted => "attachment_permission_granted",
            Self::AttachmentPermissionRemoved => "attachment_permission_removed",
            Self::SystemBan => "system_ban",
            Self::SystemBanLifted => "system_ban_lifted",
            Self::PostCooldown => "post_cooldown",
            Self::RateLimitWarning => "rate_limit_warning",
            Self::Unknown => "unknown",
        }
    }
}

/// Type-dependent context attached to a notification (API v0.8.6
/// § Notification object). The server treats `metadata` as open-ended, so only
/// the commonly-used keys are modelled here; unknown keys are ignored.
///
/// Several types carry no context at all and send `null` here, `poke` among
/// them: the poker's handle arrives as `actor_username`, so there is nothing
/// type-specific to model for it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationMetadata {
    /// Entry slug — with `author_username`, builds the `/{username}/{slug}` link.
    #[serde(default)]
    pub post_slug: Option<String>,

    /// Entry author handle, for deep links and `thread_reply` summaries.
    #[serde(default)]
    pub author_username: Option<String>,

    /// Reply id to highlight in the linked post detail view.
    #[serde(default)]
    pub reply_id: Option<String>,

    /// Guild display name, for `guild_new_thread` summaries.
    #[serde(default)]
    pub guild_name: Option<String>,

    /// Guild slug, for guild deep links.
    #[serde(default)]
    pub guild_slug: Option<String>,

    /// Guild thread id.
    #[serde(default)]
    pub thread_id: Option<String>,

    /// Set on guild-thread notifications.
    #[serde(default)]
    pub is_guild_thread: Option<bool>,
}

/// A notification record. Shape per API v0.8.6 § Notification object: the actor is
/// denormalized onto `actorId` / `actorUsername`, and type-dependent context
/// (deep-link slug, reply id, guild/thread info) lives under `metadata`.
///
/// Both actor fields are optional, because several types are about the reader's
/// own account and have no sender at all. Use [`Notification::actor_profile`]
/// before navigating anywhere on the strength of an actor.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    #[serde(alias = "id")]
    pub notification_id: String,

    #[serde(rename = "type")]
    pub kind: NotificationType,

    #[serde(default)]
    pub read: bool,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,

    /// Actor id (denormalized so no extra lookup is needed). Optional:
    /// `system_ban` omits it, and the other account notifications carry the
    /// [`SYSTEM_ACTOR`] sentinel instead of a real uid.
    #[serde(default)]
    pub actor_id: Option<String>,

    /// Actor display handle. `None` when the notification omits the actor, and
    /// the literal [`SYSTEM_ACTOR`] on the account notifications that name a
    /// sender without there being a user behind it.
    #[serde(default)]
    pub actor_username: Option<String>,

    /// Target resource id — typically a post id for navigable notifications.
    #[serde(default)]
    pub target_id: Option<String>,

    /// Target kind — `"post"` or `"reply"`; empty for non-navigable types.
    #[serde(default)]
    pub target_type: Option<String>,

    /// Present only on some system notifications (e.g. `system_ban`).
    #[serde(default)]
    pub reason: Option<String>,

    /// Type-dependent context; unknown keys are ignored. `#[serde(default)]`
    /// covers an absent key, and `deserialize_metadata` maps an explicit
    /// `"metadata": null` (which the server sends for context-free types) to the
    /// default rather than failing to decode the whole page.
    #[serde(default, deserialize_with = "deserialize_metadata")]
    pub metadata: NotificationMetadata,
}

/// Decode a notification's `metadata`, tolerating an explicit JSON `null`.
/// Plain `#[serde(default)]` only fills in a *missing* key; a present-but-null
/// value would otherwise error with "invalid type: null, expected struct
/// NotificationMetadata" and sink the entire notifications page.
fn deserialize_metadata<'de, D>(
    deserializer: D,
) -> std::result::Result<NotificationMetadata, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<NotificationMetadata>::deserialize(deserializer)?.unwrap_or_default())
}

impl Notification {
    /// Actor display handle, or [`SYSTEM_ACTOR`] for actor-less notifications.
    ///
    /// A label to print, never a handle to act on: the fallback it returns and
    /// the `"system"` v0.8.6 puts on the wire (§ Notification object) are the
    /// same string, so this cannot tell an absent actor from a named one. Use
    /// [`Notification::actor_profile`] to decide whether there is a profile to
    /// open.
    pub fn actor_name(&self) -> &str {
        self.actor_username.as_deref().unwrap_or(SYSTEM_ACTOR)
    }

    /// The handle a client may open a profile for, or `None` when there is no
    /// profile behind this notification.
    ///
    /// v0.8.6 made both actor fields optional and gave the account
    /// notifications (`post_cooldown`, `rate_limit_warning`,
    /// `system_ban_lifted`) the literal [`SYSTEM_ACTOR`] handle, with the
    /// instruction not to open a profile for it (§ Notification object).
    /// `system_ban` omits the actor entirely. Both cases answer `None` here, so
    /// one check covers the whole family and a caller cannot mistake the
    /// sentinel for a user by reading [`Notification::actor_name`].
    ///
    /// The sentinel is recognised by its value rather than by
    /// [`Notification::kind`] deliberately: the type list is open-ended, so a
    /// system type this build has never heard of decodes as
    /// [`NotificationType::Unknown`] and would otherwise sail straight past a
    /// match on known kinds into a profile fetch for a user who does not exist.
    #[must_use]
    pub fn actor_profile(&self) -> Option<&str> {
        match self.actor_username.as_deref() {
            None | Some(SYSTEM_ACTOR) => None,
            Some(name) => Some(name),
        }
    }

    /// Whether this notification is about the reader's own account rather than
    /// another user's action, so there is nobody to open. The inverse of
    /// [`Notification::actor_profile`] being `Some`, spelled out for the call
    /// sites that only want the question answered.
    #[must_use]
    pub fn is_from_system(&self) -> bool {
        self.actor_profile().is_none()
    }

    /// Reply id to highlight when opening the linked post detail.
    pub fn reply_id(&self) -> Option<&str> {
        self.metadata.reply_id.as_deref()
    }

    /// Original thread author, for `thread_reply` summaries.
    pub fn thread_author(&self) -> Option<&str> {
        self.metadata.author_username.as_deref()
    }

    /// Guild display name, for `guild_new_thread` summaries.
    pub fn guild_display_name(&self) -> Option<&str> {
        self.metadata.guild_name.as_deref()
    }
}

/// Result of [`Client::unread_notification_count`] (API v0.8.6 § Unread Count).
///
/// The count covers the same filtered set `GET /v1/notifications` returns, so a
/// badge built on it matches the list the user then opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCount {
    /// Unread notifications, capped at 100 when `exact` is false.
    #[serde(default)]
    pub count: u32,

    /// Whether `count` is the true total. It is false once more than 100 are
    /// unread, where the server counts only the 100 most recent, and the spec
    /// says to render "99+" rather than the number. See [`UnreadCount::badge`].
    ///
    /// Defaults to `true` when the field is absent: a server that predates
    /// v0.8.6 answers with a plain exact count, and defaulting a bare `bool` to
    /// `false` would show every one of those users "99+" over an inbox of
    /// three.
    #[serde(default = "exact_by_default")]
    pub exact: bool,
}

/// See [`UnreadCount::exact`]: an omitted flag means an exact count.
fn exact_by_default() -> bool {
    true
}

impl UnreadCount {
    /// The badge text § Unread Count prescribes: the number, or `"99+"` once
    /// the count is capped. One implementation so every screen showing the
    /// badge shows the same thing.
    #[must_use]
    pub fn badge(&self) -> String {
        if self.exact {
            self.count.to_string()
        } else {
            "99+".to_string()
        }
    }

    /// Whether there is anything unread at all. Safe on a capped count, since
    /// the cap only ever hides notifications above 100, never below 1.
    #[must_use]
    pub fn any(&self) -> bool {
        self.count > 0
    }
}

impl Default for UnreadCount {
    /// Zero unread, exactly, which is what a client shows before its first
    /// poll rather than a spurious "99+".
    fn default() -> Self {
        Self {
            count: 0,
            exact: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkAllResponse {
    #[serde(default)]
    updated: u32,
    /// True while unread notifications remain beyond the 5,000 this pass
    /// marked (API v0.8.6 § Mark All as Read).
    #[serde(default)]
    has_more: bool,
}

/// Filter for listing notifications.
#[derive(Debug, Clone, Copy, Default)]
pub enum NotificationsFilter {
    #[default]
    All,
    Unread,
    Read,
}

impl Client {
    /// `GET /v1/notifications` with optional `read=` and `type=` filters.
    /// Pass an empty `types` slice to omit the type filter.
    ///
    /// Since v0.8.6 the server drops notifications the user has muted, blocked
    /// or switched off under `notifications` in `GET /v1/settings`, and it
    /// filters *after* taking the page (§ List Notifications). A page can
    /// therefore come back shorter than `limit`, or even empty, while more
    /// notifications wait behind it. Keep paging while the returned cursor is
    /// `Some`; treating a short page as the end silently truncates the user's
    /// notifications at the first muted one.
    pub async fn list_notifications(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
        filter: NotificationsFilter,
        types: &[NotificationType],
    ) -> Result<(Vec<Notification>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        match filter {
            NotificationsFilter::All => {}
            NotificationsFilter::Unread => query.push(("read", "false".to_string())),
            NotificationsFilter::Read => query.push(("read", "true".to_string())),
        }
        if !types.is_empty() {
            let joined: String = types.iter().map(|t| t.wire()).collect::<Vec<_>>().join(",");
            query.push(("type", joined));
        }
        self.request_page(
            EndpointKey::NotificationsList,
            Method::GET,
            "/v1/notifications",
            &query,
        )
        .await
    }

    /// `GET /v1/notifications/unread-count`. Cached server-side ~5 s, and
    /// marking anything read clears that cache, so the count drops on the next
    /// poll rather than lagging five seconds behind the user's own action
    /// (API v0.8.6 § Unread Count).
    ///
    /// Returns the count *and* [`UnreadCount::exact`], because the number alone
    /// is not enough to render the badge: above 100 unread the server counts
    /// only the 100 most recent and the spec asks for "99+" instead of the
    /// figure. [`UnreadCount::badge`] applies that rule.
    pub async fn unread_notification_count(&self) -> Result<UnreadCount> {
        self.request::<UnreadCount, ()>(
            EndpointKey::NotificationsUnreadCount,
            Method::GET,
            "/v1/notifications/unread-count",
            &[],
            None,
        )
        .await
    }

    /// `PATCH /v1/notifications/:id` — mark a single notification as read.
    pub async fn mark_notification_read(&self, notification_id: &str) -> Result<()> {
        let path = format!("/v1/notifications/{notification_id}");
        self.request_unit(
            EndpointKey::NotificationsMarkRead,
            Method::PATCH,
            &path,
            &[],
        )
        .await
    }

    /// `POST /v1/notifications/read-all`, mark every unread notification as
    /// read. Returns how many were marked, added up across every call made.
    ///
    /// One request marks at most 5,000 and answers `hasMore` when unread
    /// notifications remain (API v0.8.6 § Mark All as Read), so this calls the
    /// endpoint again until the server says it is done. Doing that here rather
    /// than in each caller is what makes the name true: a single call leaves an
    /// inbox over 5,000 partly unread, and "mark all as read" that quietly
    /// doesn't is worse than an error. Each pass goes through the ordinary
    /// request path, so it draws its own rate-limiter grant and honours any
    /// `429` penalty exactly like a caller-driven loop would.
    ///
    /// The loop stops early, keeping the total it has, in two cases a runaway
    /// `hasMore` could otherwise turn into an endless one: after the
    /// `MAX_READ_ALL_PASSES` cap of 50 passes, a quarter of a million
    /// notifications, and when a pass reports `hasMore` having marked nothing,
    /// which is no progress by definition. Both are logged at warn level.
    /// Anything left over is picked up by the next call.
    ///
    /// A pass that fails returns its error rather than the partial total. The
    /// passes that had already succeeded still stand on the server, so the
    /// remedy is the same in every case: call again, or just refresh the unread
    /// count, which is what a caller does after this anyway.
    pub async fn mark_all_notifications_read(&self) -> Result<u32> {
        let mut total: u32 = 0;
        for pass in 1..=MAX_READ_ALL_PASSES {
            let r: MarkAllResponse = self
                .request::<MarkAllResponse, ()>(
                    EndpointKey::NotificationsMarkAllRead,
                    Method::POST,
                    "/v1/notifications/read-all",
                    &[],
                    None,
                )
                .await?;
            total = total.saturating_add(r.updated);
            if !r.has_more {
                return Ok(total);
            }
            if r.updated == 0 {
                tracing::warn!(
                    total,
                    pass,
                    "read-all reported more to do having marked nothing, stopping"
                );
                return Ok(total);
            }
        }
        tracing::warn!(
            total,
            passes = MAX_READ_ALL_PASSES,
            "read-all still reported more to do, stopping"
        );
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_type_deserializes_snake_case() {
        let kinds = [
            ("bookmark", NotificationType::Bookmark),
            ("reply", NotificationType::Reply),
            ("thread_reply", NotificationType::ThreadReply),
            ("new_follower", NotificationType::NewFollower),
            ("dm_message", NotificationType::DmMessage),
            ("guild_new_thread", NotificationType::GuildNewThread),
            (
                "attachment_permission_granted",
                NotificationType::AttachmentPermissionGranted,
            ),
        ];
        for (s, expected) in kinds {
            let t: NotificationType =
                serde_json::from_str(&format!("\"{s}\"")).expect("must decode");
            assert_eq!(t, expected, "decoding {s}");
        }
    }

    #[test]
    fn the_v086_notification_types_decode_as_themselves() {
        // All eight are new in § List Notifications. Before they were modelled
        // every one of them decoded as `Unknown`, which is also what a genuinely
        // unrecognised future type decodes as, so a client could not tell "we
        // don't render this yet" from "this build is out of date".
        let kinds = [
            ("graffiti_mention", NotificationType::GraffitiMention),
            ("moderator_granted", NotificationType::ModeratorGranted),
            ("moderator_removed", NotificationType::ModeratorRemoved),
            ("api_access_granted", NotificationType::ApiAccessGranted),
            ("api_access_removed", NotificationType::ApiAccessRemoved),
            ("system_ban_lifted", NotificationType::SystemBanLifted),
            ("post_cooldown", NotificationType::PostCooldown),
            ("rate_limit_warning", NotificationType::RateLimitWarning),
        ];
        for (s, expected) in kinds {
            let t: NotificationType =
                serde_json::from_str(&format!("\"{s}\"")).expect("must decode");
            assert_eq!(t, expected, "decoding {s}");
            assert_eq!(t.wire(), s, "wire form of {s}");
        }
    }

    #[test]
    fn unknown_notification_type_falls_through() {
        let t: NotificationType = serde_json::from_str("\"some_new_type_2027\"").expect("decode");
        assert_eq!(t, NotificationType::Unknown);
    }

    #[test]
    fn notification_type_wire_round_trips() {
        for variant in [
            NotificationType::Bookmark,
            NotificationType::Reply,
            NotificationType::ThreadReply,
            NotificationType::Poke,
            NotificationType::SystemBan,
        ] {
            let s = format!("\"{}\"", variant.wire());
            let decoded: NotificationType = serde_json::from_str(&s).unwrap();
            assert_eq!(decoded, variant);
        }
    }

    #[test]
    fn notification_decodes_minimal_shape() {
        let raw = r#"{
            "notificationId": "n1",
            "type": "reply",
            "read": false,
            "createdAt": "2026-03-27T10:12:01Z",
            "actorId": "u1",
            "actorUsername": "alice",
            "targetId": "p1",
            "targetType": "post",
            "metadata": {"postSlug": "my-entry", "authorUsername": "me", "replyId": "r1"}
        }"#;
        let n: Notification = serde_json::from_str(raw).unwrap();
        assert_eq!(n.notification_id, "n1");
        assert_eq!(n.kind, NotificationType::Reply);
        assert!(!n.read);
        assert!(n.created_at.is_some());
        assert_eq!(n.actor_name(), "alice");
        assert_eq!(n.target_id.as_deref(), Some("p1"));
        assert_eq!(n.target_type.as_deref(), Some("post"));
        assert_eq!(n.reply_id(), Some("r1"));
        assert_eq!(n.metadata.post_slug.as_deref(), Some("my-entry"));
        assert_eq!(n.thread_author(), Some("me"));
    }

    #[test]
    fn notification_accepts_id_alias() {
        let raw = r#"{"id":"n1","type":"poke"}"#;
        let n: Notification = serde_json::from_str(raw).unwrap();
        assert_eq!(n.notification_id, "n1");
        assert_eq!(n.kind, NotificationType::Poke);
    }

    #[test]
    fn notification_tolerates_null_metadata() {
        // The server sends `"metadata": null` for context-free types (e.g.
        // chat mentions). `#[serde(default)]` alone can't absorb an explicit
        // null, so this must not fail the whole decode.
        let raw = r#"{"id":"n1","type":"chat_mention","metadata":null}"#;
        let n: Notification = serde_json::from_str(raw).unwrap();
        assert_eq!(n.notification_id, "n1");
        assert_eq!(n.kind, NotificationType::ChatMention);
        assert!(n.metadata.post_slug.is_none());
        assert!(n.reply_id().is_none());
    }

    #[test]
    fn notification_page_survives_one_null_metadata() {
        // A single null-metadata entry mixed with a fully-populated one must
        // still decode the page (the live-repro shape from cs-tui.log).
        let raw = r#"[
            {"id":"a","type":"chat_mention","metadata":{"roomName":"The Sprawl"}},
            {"id":"b","type":"poke","metadata":null}
        ]"#;
        let ns: Vec<Notification> = serde_json::from_str(raw).unwrap();
        assert_eq!(ns.len(), 2);
        assert_eq!(ns[1].notification_id, "b");
    }

    #[test]
    fn poke_notification_carries_only_its_actor() {
        // v0.8.4 § How notifications are generated added `poke` to the list a
        // client can trigger (`POST /v1/users/:username/poke`). It has no
        // type-dependent context: the poker is the actor, there is no target
        // and no metadata, so nothing extra has to be modelled for it.
        let raw = r#"{
            "id": "n9",
            "type": "poke",
            "actorId": "u2",
            "actorUsername": "bob",
            "read": false,
            "createdAt": "2026-08-01T09:00:00Z",
            "metadata": null
        }"#;
        let n: Notification = serde_json::from_str(raw).unwrap();
        assert_eq!(n.kind, NotificationType::Poke);
        assert_eq!(n.actor_name(), "bob");
        assert_eq!(n.actor_id.as_deref(), Some("u2"));
        assert!(n.target_id.is_none());
        assert!(n.target_type.is_none());
        assert!(n.reply_id().is_none());
        assert!(n.created_at.is_some());
    }

    #[test]
    fn notification_tolerates_missing_fields() {
        let raw = r#"{"notificationId":"n1","type":"poke"}"#;
        let n: Notification = serde_json::from_str(raw).unwrap();
        assert!(!n.read);
        assert!(n.created_at.is_none());
        assert_eq!(n.actor_name(), "system");
        assert!(n.target_id.is_none());
        assert!(n.reply_id().is_none());
    }

    #[test]
    fn an_account_notification_offers_no_profile_to_open() {
        // § Notification object: `post_cooldown`, `rate_limit_warning` and
        // `system_ban_lifted` carry the literal "system", which is also what
        // `actor_name` invents for an absent actor, so the display string
        // cannot be used to decide whether there is a profile behind it.
        let raw = r#"{
            "id": "n1",
            "type": "post_cooldown",
            "actorUsername": "system",
            "reason": "posting too quickly, saved as a note",
            "metadata": null
        }"#;
        let n: Notification = serde_json::from_str(raw).unwrap();
        assert_eq!(n.kind, NotificationType::PostCooldown);
        assert_eq!(n.actor_name(), "system", "still fine as a label");
        assert_eq!(n.actor_profile(), None, "but there is nobody to open");
        assert!(n.is_from_system());
        assert_eq!(
            n.reason.as_deref(),
            Some("posting too quickly, saved as a note")
        );
    }

    #[test]
    fn a_ban_omits_the_actor_entirely_and_reads_the_same_way() {
        // `system_ban` sends no actor at all, the other half of the same case.
        let n: Notification =
            serde_json::from_str(r#"{"id":"n2","type":"system_ban","reason":"spam"}"#).unwrap();
        assert_eq!(n.actor_name(), SYSTEM_ACTOR);
        assert_eq!(n.actor_profile(), None);
        assert!(n.is_from_system());
    }

    #[test]
    fn a_real_actor_still_has_a_profile_to_open() {
        let n: Notification =
            serde_json::from_str(r#"{"id":"n3","type":"new_follower","actorUsername":"alice"}"#)
                .unwrap();
        assert_eq!(n.actor_profile(), Some("alice"));
        assert!(!n.is_from_system());
    }

    #[test]
    fn an_unmodelled_system_type_is_still_recognised_as_actorless() {
        // The sentinel is read off the actor, not off the type, so a system
        // notification invented after this build decodes as `Unknown` and is
        // still not offered as a profile link.
        let raw = r#"{"id":"n4","type":"system_something_2027","actorUsername":"system"}"#;
        let n: Notification = serde_json::from_str(raw).unwrap();
        assert_eq!(n.kind, NotificationType::Unknown);
        assert!(n.is_from_system());
    }

    #[test]
    fn unread_count_decodes_both_documented_shapes() {
        let exact: UnreadCount = serde_json::from_str(r#"{"count":7,"exact":true}"#).unwrap();
        assert_eq!(exact.count, 7);
        assert!(exact.exact);
        assert_eq!(exact.badge(), "7");
        assert!(exact.any());

        // Over 100 unread the server counts only the 100 most recent, and
        // § Unread Count asks for "99+" rather than the capped figure.
        let capped: UnreadCount = serde_json::from_str(r#"{"count":100,"exact":false}"#).unwrap();
        assert_eq!(capped.count, 100);
        assert!(!capped.exact);
        assert_eq!(capped.badge(), "99+");
    }

    #[test]
    fn a_count_with_no_exact_flag_is_treated_as_exact() {
        // Defaulting the bool would make it false and show "99+" over an inbox
        // of three on any server that predates the flag.
        let r: UnreadCount = serde_json::from_str(r#"{"count":3}"#).unwrap();
        assert!(r.exact);
        assert_eq!(r.badge(), "3");

        let empty: UnreadCount = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, UnreadCount::default());
        assert_eq!(empty.badge(), "0");
        assert!(!empty.any());
    }

    #[test]
    fn mark_all_response_decodes() {
        let r: MarkAllResponse = serde_json::from_str(r#"{"updated":12,"hasMore":false}"#).unwrap();
        assert_eq!(r.updated, 12);
        assert!(!r.has_more);

        let more: MarkAllResponse =
            serde_json::from_str(r#"{"updated":5000,"hasMore":true}"#).unwrap();
        assert_eq!(more.updated, 5000);
        assert!(more.has_more, "5,000 is one full pass, so more remain");

        // A server that predates the flag answers without it, which has to read
        // as "nothing left" or one pass would loop until the safety cap.
        let legacy: MarkAllResponse = serde_json::from_str(r#"{"updated":12}"#).unwrap();
        assert!(!legacy.has_more);
    }
}

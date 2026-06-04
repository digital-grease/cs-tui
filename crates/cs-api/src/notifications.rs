//! Notification types and endpoints (`/v1/notifications/*`).
use reqwest::Method;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

/// The 22 notification types documented in v0.3.6, plus `Unknown` for forward
/// compatibility with future types.
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
    Poke,
    ChatMention,
    PostMention,
    ReplyMention,
    DmMessage,
    GuildNewThread,
    SupporterGranted,
    SupporterRemoved,
    HackerGranted,
    HackerRemoved,
    ImagePermissionGranted,
    ImagePermissionRemoved,
    AttachmentPermissionGranted,
    AttachmentPermissionRemoved,
    SystemBan,
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
            Self::DmMessage => "dm_message",
            Self::GuildNewThread => "guild_new_thread",
            Self::SupporterGranted => "supporter_granted",
            Self::SupporterRemoved => "supporter_removed",
            Self::HackerGranted => "hacker_granted",
            Self::HackerRemoved => "hacker_removed",
            Self::ImagePermissionGranted => "image_permission_granted",
            Self::ImagePermissionRemoved => "image_permission_removed",
            Self::AttachmentPermissionGranted => "attachment_permission_granted",
            Self::AttachmentPermissionRemoved => "attachment_permission_removed",
            Self::SystemBan => "system_ban",
            Self::Unknown => "unknown",
        }
    }
}

/// The user who triggered a notification.
/// Type-dependent context attached to a notification (API v0.5.0 § Notifications,
/// "Notification object"). The server treats `metadata` as open-ended, so only
/// the commonly-used keys are modelled here; unknown keys are ignored.
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

/// A notification record. Shape per API v0.5.0 § Notifications: the actor is
/// denormalized onto `actorId` / `actorUsername`, and type-dependent context
/// (deep-link slug, reply id, guild/thread info) lives under `metadata`.
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

    /// Actor id (denormalized so no extra lookup is needed).
    #[serde(default)]
    pub actor_id: Option<String>,

    /// Actor display handle; `None` for actor-less system notifications.
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

    /// Type-dependent context; unknown keys are ignored.
    #[serde(default)]
    pub metadata: NotificationMetadata,
}

impl Notification {
    /// Actor display handle, or `"system"` for actor-less system notifications.
    pub fn actor_name(&self) -> &str {
        self.actor_username.as_deref().unwrap_or("system")
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

#[derive(Debug, Deserialize)]
struct UnreadCountResponse {
    #[serde(default)]
    count: u32,
}

#[derive(Debug, Deserialize)]
struct MarkAllResponse {
    #[serde(default)]
    updated: u32,
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

    /// `GET /v1/notifications/unread-count`. Cached server-side ~5 s.
    pub async fn unread_notification_count(&self) -> Result<u32> {
        let r: UnreadCountResponse = self
            .request::<UnreadCountResponse, ()>(
                EndpointKey::NotificationsUnreadCount,
                Method::GET,
                "/v1/notifications/unread-count",
                &[],
                None,
            )
            .await?;
        Ok(r.count)
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

    /// `POST /v1/notifications/read-all` — mark every unread notification as
    /// read. Returns the count of notifications updated.
    pub async fn mark_all_notifications_read(&self) -> Result<u32> {
        let r: MarkAllResponse = self
            .request::<MarkAllResponse, ()>(
                EndpointKey::NotificationsMarkAllRead,
                Method::POST,
                "/v1/notifications/read-all",
                &[],
                None,
            )
            .await?;
        Ok(r.updated)
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
    fn unread_count_response_decodes() {
        let r: UnreadCountResponse = serde_json::from_str(r#"{"count":7}"#).unwrap();
        assert_eq!(r.count, 7);
    }

    #[test]
    fn mark_all_response_decodes() {
        let r: MarkAllResponse = serde_json::from_str(r#"{"updated":12}"#).unwrap();
        assert_eq!(r.updated, 12);
    }
}

//! Async Rust client for the cyberspace.online API (v0.8.6).
//!
//! Authoritative spec: `docs/api-v0.8.6.md` at the repo root.
#![deny(rust_2018_idioms)]

mod auth;
mod bookmarks;
mod circ;
mod client;
mod cmail;
mod endpoint;
mod entries;
mod envelope;
mod error;
mod follows;
mod guilds;
mod message;
mod notes;
mod notifications;
mod profile_patch;
mod rate_limit;
mod replies;
pub mod rtdb;
mod search;
mod settings;
mod tokens;
mod topics;
mod types;
mod users;
mod watch;

pub use auth::UsernameAvailability;
pub use bookmarks::{Bookmark, BookmarkKind};
pub use circ::{
    circ_message_updates_from_rtdb_event, circ_messages_from_rtdb_event, circ_messages_path,
    circ_presence_path, circ_presence_updates_from_rtdb_event, CircDeleteResponse, CircMessage,
    CircMessagePatch, CircMessageUpdate, CircPresenceEntry, CircPresencePatch,
    CircPresenceResponse, CircPresenceUpdate, CircRoom, CircRoomUser, CircSendResponse,
};
pub use client::{Client, ClientBuilder};
pub use cmail::{
    cmail_presence_updates_from_rtdb_event, messages_from_rtdb_event, CmailConversation,
    CmailMessage, CmailPresence, CmailPresencePatch, CmailPresenceUpdate, CmailSendResponse,
    CmailStartRequest, CmailTypingResponse, CmailTypingStatus, CmailUser,
};
pub use endpoint::EndpointKey;
pub use entries::{CreatedEntry, EntryEdit, TitleEdit};
pub use error::{ApiError, ErrorCode, Result};
pub use follows::{Follow, FollowsDirection};
pub use guilds::{Guild, GuildMembership, GuildRole, GuildThread, JoinedGuild, PromotedGuild};
pub use message::{AudioAttachment, MessageExtras, MessageStyle};
pub use notes::{Note, NoteRevision};
pub use notifications::{
    Notification, NotificationMetadata, NotificationType, NotificationsFilter, UnreadCount,
    SYSTEM_ACTOR,
};
pub use profile_patch::{Patch, ProfileUpdate};
pub use rate_limit::RateLimit;
pub use search::{PostHit, ReplyHit, SearchHit, SearchPreview, SearchType, UserHit};
pub use settings::{NotificationPrefs, Settings, SettingsUpdate};
pub use tokens::Tokens;
pub use topics::Topic;
pub use types::{Attachment, Entry, FlagResponse, Reply};
pub use users::{PokeResponse, User, UserGuild};
pub use watch::Watch;

/// Spec version this client targets.
pub const API_VERSION: &str = "v0.8.6";
/// Production API host, used unless [`ClientBuilder`] is given another.
pub const DEFAULT_BASE_URL: &str = "https://api.cyberspace.online";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_present() {
        assert_eq!(API_VERSION, "v0.8.6");
        assert!(DEFAULT_BASE_URL.starts_with("https://"));
    }

    #[test]
    fn cmail_request_and_response_shapes_match_spec() {
        let start = CmailStartRequest::by_username("alice");
        let v = serde_json::to_value(&start).unwrap();
        assert_eq!(v["recipientUsername"], "alice");
        assert!(v.get("recipientId").is_none());

        let convo: CmailConversation = serde_json::from_str(
            r#"{
                "conversationId":"c1",
                "otherUser":{"userId":"u2","username":"alice","displayName":"Alice","profilePictureUrl":"https://example/avatar.png"},
                "lastMessage":{"id":"m1","senderId":"u2","senderUsername":"alice","content":"hi","timestamp":1719700000000},
                "lastMessageAt":1719700000000,
                "unreadCount":2
            }"#,
        )
        .unwrap();
        assert_eq!(convo.conversation_id, "c1");
        assert_eq!(convo.other_user.username, "alice");
        assert_eq!(convo.last_message.unwrap().content, "hi");

        let sent: CmailSendResponse =
            serde_json::from_str(r#"{"conversationId":"c1","messageId":"m2"}"#).unwrap();
        assert_eq!(sent.message_id.as_deref(), Some("m2"));
    }

    #[test]
    fn the_v086_apprenticeship_and_notification_types_are_re_exported() {
        // Same reasoning as the rtdb re-export test below: a type the TUI has
        // to name is unusable until it is reachable through the crate root, and
        // naming it here turns a dropped `pub use` into a failure in this crate
        // rather than in the consumer.
        let apprenticeship = UserGuild {
            role: Some(GuildRole::Apprentice),
            ..UserGuild::default()
        };
        assert!(!apprenticeship.is_badge());

        let promoted: PromotedGuild =
            serde_json::from_str(r#"{"guildId":"g1","role":"member"}"#).unwrap();
        assert_eq!(promoted.role, Some(GuildRole::Member));

        let capped = UnreadCount {
            count: 100,
            exact: false,
        };
        assert_eq!(capped.badge(), "99+");
        assert_eq!(SYSTEM_ACTOR, "system");
        assert_eq!(NotificationType::PostCooldown.wire(), "post_cooldown");
    }

    #[test]
    fn the_rtdb_update_types_are_re_exported_at_the_crate_root() {
        // A decoder that returns a type callers cannot name is unusable: the
        // TUI has to match on these to merge a presence patch into the entry it
        // holds. Naming them through the crate root is what proves the re-export
        // is there, so dropping one from the `pub use` list fails to compile
        // here rather than in the consumer.
        let patch = CmailPresencePatch {
            typing: Some(true),
            ..CmailPresencePatch::default()
        };
        let mut entry = CmailPresence::default();
        patch.apply_to(&mut entry);
        assert!(entry.typing, "the patch merged through the public surface");

        let update: CmailPresenceUpdate = CmailPresenceUpdate::Removed {
            user_id: "u2".into(),
        };
        assert_eq!(update.user_id(), "u2");
        assert!(update.as_full().is_none());

        // The cIRC halves are already exported; pin them alongside so the two
        // sides of the same feature cannot drift apart.
        let circ: CircPresenceUpdate = CircPresenceUpdate::Removed {
            user_id: "u1".into(),
        };
        assert_eq!(circ.user_id(), "u1");
    }
}

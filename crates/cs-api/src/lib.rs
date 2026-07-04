//! Async Rust client for the cyberspace.online API (v0.7).
//!
//! Authoritative spec: `docs/api-v0.7.md` at the repo root.
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

pub use bookmarks::{Bookmark, BookmarkKind};
pub use circ::{circ_messages_from_rtdb_event, CircMessage, CircRoom, CircSendResponse};
pub use client::{Client, ClientBuilder};
pub use cmail::{
    messages_from_rtdb_event, CmailConversation, CmailMessage, CmailSendResponse,
    CmailStartRequest, CmailUser,
};
pub use endpoint::EndpointKey;
pub use entries::CreatedEntry;
pub use error::{ApiError, ErrorCode, Result};
pub use follows::{Follow, FollowsDirection};
pub use guilds::{Guild, GuildMembership, GuildRole, GuildThread, JoinedGuild};
pub use notes::{Note, NoteRevision};
pub use notifications::{
    Notification, NotificationMetadata, NotificationType, NotificationsFilter,
};
pub use profile_patch::{Patch, ProfileUpdate};
pub use rate_limit::RateLimit;
pub use search::{PostHit, ReplyHit, SearchHit, SearchPreview, SearchType, UserHit};
pub use settings::{NotificationPrefs, Settings, SettingsUpdate};
pub use tokens::Tokens;
pub use topics::Topic;
pub use types::{Attachment, Entry, Reply};
pub use users::User;
pub use watch::Watch;

pub const API_VERSION: &str = "v0.7";
pub const DEFAULT_BASE_URL: &str = "https://api.cyberspace.online";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_present() {
        assert_eq!(API_VERSION, "v0.7");
        assert!(DEFAULT_BASE_URL.starts_with("https://"));
    }

    #[test]
    fn cmail_request_and_response_shapes_match_v07_spec() {
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
}

//! Async Rust client for the cyberspace.online API (v0.4).
//!
//! Authoritative spec: `docs/api-v0.4.md` at the repo root.
#![deny(rust_2018_idioms)]

mod auth;
mod bookmarks;
mod client;
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
mod settings;
mod tokens;
mod topics;
mod types;
mod users;

pub use auth::{CheckUsernameResponse, ResendVerificationResponse};
pub use bookmarks::{Bookmark, BookmarkKind};
pub use client::{Client, ClientBuilder};
pub use endpoint::EndpointKey;
pub use entries::CreatedEntry;
pub use error::{ApiError, ErrorCode, Result};
pub use follows::{Follow, FollowsDirection};
pub use guilds::{Guild, GuildMembership, GuildRole, GuildThread, JoinedGuild};
pub use notes::{Note, NoteRevision};
pub use notifications::{Notification, NotificationActor, NotificationType, NotificationsFilter};
pub use profile_patch::{Patch, ProfileUpdate};
pub use rate_limit::RateLimit;
pub use settings::{NotificationPrefs, Settings, SettingsUpdate};
pub use tokens::Tokens;
pub use topics::Topic;
pub use types::{Attachment, Entry, Reply};
pub use users::User;

pub const API_VERSION: &str = "v0.4";
pub const DEFAULT_BASE_URL: &str = "https://api.cyberspace.online";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_present() {
        assert_eq!(API_VERSION, "v0.4");
        assert!(DEFAULT_BASE_URL.starts_with("https://"));
    }
}

//! Endpoint keys for rate-limiter accounting. One variant per documented endpoint.
//!
//! Rate-limit values come from `docs/api-v0.4.md`. Where the consolidated table
//! and the per-endpoint section disagree, the lower (more restrictive) value is
//! used so the client cannot self-trigger 429s.
use crate::rate_limit::RateLimit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointKey {
    // Auth
    AuthLogin,
    AuthRegister,
    AuthRefresh,
    AuthResendVerification,
    AuthCheckUsername,

    // Entries (posts)
    EntriesList,
    EntriesGet,
    EntriesCreate,
    EntriesDelete,

    // Replies
    RepliesList,
    RepliesGet,
    RepliesCreate,
    RepliesDelete,

    // Users
    UsersGetMe,
    UsersGet,
    UsersListPosts,
    UsersGetPostBySlug,
    UsersListReplies,
    UsersUpdateMe,

    // Bookmarks
    BookmarksList,
    BookmarksCreate,
    BookmarksDelete,

    // Follows
    FollowsList,
    FollowsCreate,
    FollowsDelete,

    // Notifications
    NotificationsList,
    NotificationsUnreadCount,
    NotificationsMarkRead,
    NotificationsMarkAllRead,

    // Notes
    NotesList,
    NotesGet,
    NotesGetRevision,
    NotesListRevisions,
    NotesCreate,
    NotesUpdate,
    NotesDelete,

    // Topics
    TopicsList,
    TopicsListPosts,

    // Settings
    SettingsGet,
    SettingsUpdate,
}

impl EndpointKey {
    /// Returns the documented rate limit for this endpoint. `None` fields mean no
    /// explicit limit was stated in the spec.
    #[must_use]
    pub fn rate_limit(self) -> RateLimit {
        use EndpointKey::{
            AuthCheckUsername, AuthLogin, AuthRefresh, AuthRegister, AuthResendVerification,
            BookmarksCreate, BookmarksDelete, BookmarksList, EntriesCreate, EntriesDelete,
            EntriesGet, EntriesList, FollowsCreate, FollowsDelete, FollowsList, NotesCreate,
            NotesDelete, NotesGet, NotesGetRevision, NotesList, NotesListRevisions, NotesUpdate,
            NotificationsList, NotificationsMarkAllRead, NotificationsMarkRead,
            NotificationsUnreadCount, RepliesCreate, RepliesDelete, RepliesGet, RepliesList,
            SettingsGet, SettingsUpdate, TopicsList, TopicsListPosts, UsersGet, UsersGetMe,
            UsersGetPostBySlug, UsersListPosts, UsersListReplies, UsersUpdateMe,
        };

        match self {
            // Auth — only resend-verification has a documented limit.
            AuthResendVerification => RateLimit::per_minute(1),
            AuthLogin | AuthRegister | AuthRefresh | AuthCheckUsername => RateLimit::none(),

            // Reads — table values from § Anti-Scraping.
            EntriesList
            | RepliesList
            | UsersListPosts
            | UsersGetPostBySlug
            | UsersListReplies
            | TopicsListPosts
            | NotificationsList
            | NotificationsUnreadCount => RateLimit::per_minute(30),
            TopicsList | BookmarksList | NotesList | FollowsList | UsersGet => {
                RateLimit::per_minute(20)
            }

            // Single-resource reads — not documented; no client-side cap.
            EntriesGet | RepliesGet | UsersGetMe | NotesGet | NotesGetRevision
            | NotesListRevisions | SettingsGet => RateLimit::none(),

            // Writes — lower of (table, section) values.
            EntriesCreate | UsersUpdateMe | SettingsUpdate => RateLimit::with_day(2, 10),
            RepliesCreate | FollowsCreate | FollowsDelete => RateLimit::with_day(3, 10),
            NotesCreate => RateLimit::with_day(3, 20),
            BookmarksCreate => RateLimit::with_day(5, 50),

            // Deletes — not documented; no client-side cap.
            EntriesDelete
            | RepliesDelete
            | BookmarksDelete
            | NotesUpdate
            | NotesDelete
            | NotificationsMarkRead
            | NotificationsMarkAllRead => RateLimit::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_endpoints_have_per_minute_caps() {
        let rl = EndpointKey::EntriesList.rate_limit();
        assert_eq!(rl.per_minute, Some(30));
        assert_eq!(rl.per_day, None);
    }

    #[test]
    fn write_endpoints_have_both_caps() {
        let rl = EndpointKey::EntriesCreate.rate_limit();
        assert_eq!(rl.per_minute, Some(2));
        assert_eq!(rl.per_day, Some(10));
    }

    #[test]
    fn auth_login_has_no_caps() {
        let rl = EndpointKey::AuthLogin.rate_limit();
        assert!(rl.per_minute.is_none());
        assert!(rl.per_day.is_none());
    }
}

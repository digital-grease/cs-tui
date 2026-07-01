//! Endpoint keys for rate-limiter accounting. One variant per documented endpoint.
//!
//! Rate-limit values come from `docs/api-v0.6.0.md`. Where the consolidated table
//! and the per-endpoint section disagree, the lower (more restrictive) value is
//! used so the client cannot self-trigger 429s.
use crate::rate_limit::RateLimit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointKey {
    // Auth — login + refresh only (the API exposes no registration endpoint).
    AuthLogin,
    AuthRefresh,

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

    // Thread watching (v0.5.1)
    WatchStatus,
    WatchCreate,
    WatchDelete,
    WatchesList,

    // Guilds (v0.5.1)
    GuildsList,
    GuildsGet,
    GuildsMembersList,
    GuildsThreadsList,
    GuildsThreadsCreate,
    GuildsJoin,
    GuildsLeave,

    // C-Mail (v0.6.0)
    CmailStart,
    CmailList,
    CmailRead,
    CmailSend,
    CmailMarkRead,
}

impl EndpointKey {
    /// Returns the documented rate limit for this endpoint. `None` fields mean no
    /// explicit limit was stated in the spec.
    #[must_use]
    pub fn rate_limit(self) -> RateLimit {
        use EndpointKey::{
            AuthLogin, AuthRefresh, BookmarksCreate, BookmarksDelete, BookmarksList, CmailList,
            CmailMarkRead, CmailRead, CmailSend, CmailStart, EntriesCreate, EntriesDelete,
            EntriesGet, EntriesList, FollowsCreate, FollowsDelete, FollowsList, GuildsGet,
            GuildsJoin, GuildsLeave, GuildsList, GuildsMembersList, GuildsThreadsCreate,
            GuildsThreadsList, NotesCreate, NotesDelete, NotesGet, NotesGetRevision, NotesList,
            NotesListRevisions, NotesUpdate, NotificationsList, NotificationsMarkAllRead,
            NotificationsMarkRead, NotificationsUnreadCount, RepliesCreate, RepliesDelete,
            RepliesGet, RepliesList, SettingsGet, SettingsUpdate, TopicsList, TopicsListPosts,
            UsersGet, UsersGetMe, UsersGetPostBySlug, UsersListPosts, UsersListReplies,
            UsersUpdateMe, WatchCreate, WatchDelete, WatchStatus, WatchesList,
        };

        match self {
            // Auth — login/refresh carry no documented limit.
            AuthLogin | AuthRefresh => RateLimit::none(),

            // Reads — table values from § Anti-Scraping (v0.6.0).
            EntriesList | RepliesList | UsersListPosts | UsersListReplies | TopicsListPosts
            | CmailRead => RateLimit::per_minute(45),
            TopicsList
            | BookmarksList
            | NotesList
            | FollowsList
            | UsersGet
            | NotificationsList
            | NotificationsUnreadCount
            | WatchStatus
            | WatchesList
            | CmailList => RateLimit::per_minute(30),
            // Single-post-by-slug isn't in the table; keep a conservative read cap.
            UsersGetPostBySlug => RateLimit::per_minute(30),

            // Single-resource reads — not documented; no client-side cap.
            EntriesGet | RepliesGet | UsersGetMe | NotesGet | NotesGetRevision
            | NotesListRevisions | SettingsGet => RateLimit::none(),

            // Writes — lower of (table, section) values (v0.6.0).
            EntriesCreate | UsersUpdateMe | SettingsUpdate => RateLimit::with_day(2, 15),
            RepliesCreate | FollowsCreate | FollowsDelete => RateLimit::with_day(3, 15),
            NotesCreate => RateLimit::with_day(3, 30),
            BookmarksCreate => RateLimit::with_day(5, 75),
            // Thread watching (v0.5.1 § Rate Limits — "Watch thread" 10/min, 100/day).
            WatchCreate => RateLimit::with_day(10, 100),
            // C-Mail (v0.6.0). The spec also documents hourly caps for start/send;
            // this limiter currently models minute + day windows only.
            CmailStart => RateLimit::with_day(5, 50),
            CmailSend => RateLimit::with_day(15, 300),
            CmailMarkRead => RateLimit::per_minute(60),

            // Deletes — not documented; no client-side cap.
            EntriesDelete
            | RepliesDelete
            | BookmarksDelete
            | WatchDelete
            | NotesUpdate
            | NotesDelete
            | NotificationsMarkRead
            | NotificationsMarkAllRead => RateLimit::none(),

            // Guilds (v0.5.1) — per-endpoint sections + § Anti-Scraping table.
            GuildsList | GuildsMembersList => RateLimit::per_minute(30),
            GuildsThreadsList => RateLimit::per_minute(45),
            GuildsGet => RateLimit::none(),
            GuildsThreadsCreate => RateLimit::with_day(2, 15),
            GuildsJoin | GuildsLeave => RateLimit::with_day(3, 15),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_endpoints_have_per_minute_caps() {
        let rl = EndpointKey::EntriesList.rate_limit();
        assert_eq!(rl.per_minute, Some(45));
        assert_eq!(rl.per_day, None);
    }

    #[test]
    fn write_endpoints_have_both_caps() {
        let rl = EndpointKey::EntriesCreate.rate_limit();
        assert_eq!(rl.per_minute, Some(2));
        assert_eq!(rl.per_day, Some(15));
    }

    #[test]
    fn auth_login_has_no_caps() {
        let rl = EndpointKey::AuthLogin.rate_limit();
        assert!(rl.per_minute.is_none());
        assert!(rl.per_day.is_none());
    }

    #[test]
    fn cmail_endpoints_use_v06_caps() {
        let start = EndpointKey::CmailStart.rate_limit();
        assert_eq!(start.per_minute, Some(5));
        assert_eq!(start.per_day, Some(50));

        let send = EndpointKey::CmailSend.rate_limit();
        assert_eq!(send.per_minute, Some(15));
        assert_eq!(send.per_day, Some(300));

        let read = EndpointKey::CmailRead.rate_limit();
        assert_eq!(read.per_minute, Some(45));
        assert_eq!(read.per_day, None);

        let mark_read = EndpointKey::CmailMarkRead.rate_limit();
        assert_eq!(mark_read.per_minute, Some(60));
        assert_eq!(mark_read.per_day, None);
    }
}

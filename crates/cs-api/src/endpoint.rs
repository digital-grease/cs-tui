//! Endpoint keys for rate-limiter accounting. One variant per documented endpoint.
//!
//! Rate-limit values come from the v0.8.6 spec (§ Rate Limits, plus each
//! endpoint's own section). Where the consolidated table and the per-endpoint
//! section disagree, the lower (more restrictive) value is used so the client
//! cannot self-trigger 429s.
//!
//! Two limits have a second dimension: cIRC presence is capped per room
//! as well as overall, C-Mail typing per conversation as well as overall. Those
//! endpoints declare the extra budget in [`EndpointKey::scoped_rate_limit`];
//! [`EndpointKey::rate_limit`] always means the overall budget.
use crate::rate_limit::RateLimit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointKey {
    // Auth. There is no registration endpoint: § Access says accounts are made
    // on the website.
    AuthLogin,
    AuthRefresh,
    /// `POST /v1/auth/resend-verification` (v0.8.4). The documented cure for a
    /// `403 EMAIL_NOT_VERIFIED` (§ Access), limited separately from every other
    /// auth route (§ Resend Verification Email).
    AuthResendVerification,
    /// `POST /v1/auth/check-username` (v0.8.4): unauthenticated availability
    /// check (§ Check Username Availability).
    AuthCheckUsername,

    // Entries (posts)
    EntriesList,
    EntriesGet,
    EntriesCreate,
    /// `PATCH /v1/posts/:id` (v0.8.4).
    EntriesEdit,
    EntriesDelete,

    // Replies
    RepliesList,
    RepliesGet,
    RepliesCreate,
    /// `PATCH /v1/replies/:id` (v0.8.4).
    RepliesEdit,
    RepliesDelete,

    /// The single budget shared by all three flag endpoints (v0.8.4):
    /// `POST /v1/posts/:id/flag`, `POST /v1/replies/:id/flag` and
    /// `POST /v1/circ/:roomId/messages/:messageId/flag`.
    ///
    /// One variant, not three, because the server counts them together
    /// (§ Flag an Entry: "shared with the other flag endpoints"). Three
    /// variants would each carry a full 5/min, 20/hour, 50/day budget and let
    /// the client spend 3x what the server allows, so it would sail past its
    /// own limiter straight into a 429.
    Flag,

    // Users
    UsersGetMe,
    UsersGet,
    UsersListPosts,
    UsersGetPostBySlug,
    UsersListReplies,
    /// `GET /v1/users/me/guilds` and `GET /v1/users/:username/guilds` (v0.8.6).
    /// Both forms of § List a User's Guilds, one budget: they are the same read
    /// and the caller picks which handle to name.
    UsersListGuilds,
    UsersUpdateMe,
    /// `POST /v1/users/:username/poke` (v0.8.4). The budget is global across all
    /// users, not per user, so this key is never scoped.
    UsersPoke,

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
    /// `POST /v1/guilds/:slug/promote` (v0.8.6): hand the profile badge to an
    /// apprenticeship (§ Change Your Guild Badge). Its own key, not shared with
    /// join or leave, because § Rate Limits gives it its own row.
    GuildsPromote,
    GuildsLeave,

    // C-Mail (v0.7)
    CmailStart,
    CmailList,
    CmailRead,
    CmailSend,
    CmailMarkRead,
    /// `POST` and `DELETE /v1/cmail/:conversationId/typing` (v0.8.4). Both halves
    /// draw on the one "C-Mail typing on/off" budget. Scoped by `conversationId`.
    CmailTyping,
    /// `GET /v1/cmail/:conversationId/typing` (v0.8.4): the polling read.
    CmailTypingRead,

    // cIRC (v0.7)
    CircList,
    CircRead,
    CircSend,
    CircMarkRead,
    /// `DELETE /v1/circ/:roomId/messages/:messageId` (v0.8.4).
    CircDeleteMessage,
    /// `GET /v1/circ/:roomId/users` (v0.8.4).
    CircUsers,
    /// `POST` and `DELETE /v1/circ/:roomId/presence` (v0.8.4). Both halves draw
    /// on the one "cIRC presence heartbeat / leave" budget. Scoped by `roomId`.
    CircPresence,

    // Search (v0.7)
    Search,
}

impl EndpointKey {
    /// Returns the documented *overall* rate limit for this endpoint. `None`
    /// fields mean no explicit limit was stated in the spec. Endpoints that also
    /// carry a per-room or per-conversation cap declare it separately, in
    /// [`EndpointKey::scoped_rate_limit`].
    #[must_use]
    pub fn rate_limit(self) -> RateLimit {
        use EndpointKey::{
            AuthCheckUsername, AuthLogin, AuthRefresh, AuthResendVerification, BookmarksCreate,
            BookmarksDelete, BookmarksList, CircDeleteMessage, CircList, CircMarkRead,
            CircPresence, CircRead, CircSend, CircUsers, CmailList, CmailMarkRead, CmailRead,
            CmailSend, CmailStart, CmailTyping, CmailTypingRead, EntriesCreate, EntriesDelete,
            EntriesEdit, EntriesGet, EntriesList, Flag, FollowsCreate, FollowsDelete, FollowsList,
            GuildsGet, GuildsJoin, GuildsLeave, GuildsList, GuildsMembersList, GuildsPromote,
            GuildsThreadsCreate, GuildsThreadsList, NotesCreate, NotesDelete, NotesGet,
            NotesGetRevision, NotesList, NotesListRevisions, NotesUpdate, NotificationsList,
            NotificationsMarkAllRead, NotificationsMarkRead, NotificationsUnreadCount,
            RepliesCreate, RepliesDelete, RepliesEdit, RepliesGet, RepliesList, Search,
            SettingsGet, SettingsUpdate, TopicsList, TopicsListPosts, UsersGet, UsersGetMe,
            UsersGetPostBySlug, UsersListGuilds, UsersListPosts, UsersListReplies, UsersPoke,
            UsersUpdateMe, WatchCreate, WatchDelete, WatchStatus, WatchesList,
        };

        match self {
            // Auth: login/refresh carry no documented limit.
            AuthLogin | AuthRefresh => RateLimit::none(),
            // § Resend Verification Email: "Rate limit: 1/min, 5/hour."
            AuthResendVerification => RateLimit::per_minute_with_hour(1, 5),
            // § Check Username Availability: "Rate limit: 10/min, 60/hour (per
            // IP)." Counted per IP rather than per account, but the client only
            // ever has one of each, so one budget models it.
            AuthCheckUsername => RateLimit::per_minute_with_hour(10, 60),

            // Reads: table values from v0.8.6 § Read Actions (Anti-Scraping),
            // plus § Get User's Entry by Slug, which states 45/min in its own
            // section (the anti-scraping table omits the row).
            EntriesList | RepliesList | UsersListPosts | UsersListReplies | TopicsListPosts
            | CmailRead | CircRead | UsersGetPostBySlug => RateLimit::per_minute(45),
            TopicsList
            | BookmarksList
            | NotesList
            | FollowsList
            | UsersGet
            | UsersListGuilds
            | NotificationsList
            | NotificationsUnreadCount
            | WatchStatus
            | WatchesList
            | CmailList
            | CircList
            | Search => RateLimit::per_minute(30),
            // Polling reads added in v0.8.4 (§ Rate Limits, Read Actions):
            // "Check C-Mail typing" and "List who's in a cIRC room". Neither row
            // carries a per-conversation or per-room qualifier, so both are
            // counted overall only.
            CmailTypingRead | CircUsers => RateLimit::per_minute(60),

            // Single-resource reads — not documented; no client-side cap.
            EntriesGet | RepliesGet | UsersGetMe | NotesGet | NotesGetRevision
            | NotesListRevisions | SettingsGet => RateLimit::none(),

            // Writes: lower of the (table, section) values in v0.8.4.
            EntriesCreate | UsersUpdateMe | SettingsUpdate => RateLimit::with_day(2, 15),
            RepliesCreate | FollowsCreate | FollowsDelete => RateLimit::with_day(3, 15),
            NotesCreate => RateLimit::with_day(3, 30),
            BookmarksCreate => RateLimit::with_day(5, 75),
            // Thread watching (v0.8.4 § Rate Limits, "Watch thread" 10/min, 100/day).
            WatchCreate => RateLimit::with_day(10, 100),
            // C-Mail (v0.8.4). Start/send declare all three windows, and the hourly
            // cap (30/hr start, 150/hr send) is stricter than per_minute * 60, so
            // it has to be modelled explicitly or the client could self-inflict a
            // 429. Mark-read has only a per-minute cap.
            CmailStart => RateLimit::full(5, 30, 50),
            CmailSend => RateLimit::full(15, 150, 300),
            CmailMarkRead => RateLimit::per_minute(60),
            // C-Mail typing (v0.8.4 § Rate Limits, "C-Mail typing on/off",
            // 40 per conversation, 120 overall). This is the overall half; the
            // per-conversation half lives in `scoped_rate_limit`. The DELETE
            // (clear) draws on the same budget as the POST, which is both what
            // the single table row says and the conservative reading.
            CmailTyping => RateLimit::per_minute(120),
            // cIRC (v0.8.4): same message caps as C-Mail (15/min, 150/hr, 300/day).
            CircSend => RateLimit::full(15, 150, 300),
            CircMarkRead => RateLimit::per_minute(60),
            // cIRC presence (v0.8.4 § Rate Limits, "cIRC presence heartbeat /
            // leave", 15 per room, 90 overall). Overall half; see
            // `scoped_rate_limit` for the per-room half. Heartbeat (POST) and
            // leave (DELETE) share the budget.
            CircPresence => RateLimit::per_minute(90),
            // Delete a cIRC message (§ Delete Your Message).
            CircDeleteMessage => RateLimit::with_day(5, 30),

            // Edits (v0.8.4 § Edit Entry / § Edit Reply): 5/min, 30/day.
            EntriesEdit | RepliesEdit => RateLimit::with_day(5, 30),

            // Poke (§ Poke a User): 1/hour and 8/day, with no per-minute cap at
            // all (the write table leaves that column blank). The budget is
            // global across all users, so there is nothing to scope it by.
            UsersPoke => RateLimit::per_hour_with_day(1, 8),

            // Flagging (§ Flag an Entry): 5/min, 20/hour, 50/day for entries,
            // replies and cIRC messages together. The write table lists only
            // 5/min and 50/day; the prose adds the 20/hour cap, and the lower
            // value wins.
            Flag => RateLimit::full(5, 20, 50),

            // Deletes — not documented; no client-side cap.
            EntriesDelete
            | RepliesDelete
            | BookmarksDelete
            | WatchDelete
            | NotesUpdate
            | NotesDelete
            | NotificationsMarkRead
            | NotificationsMarkAllRead => RateLimit::none(),

            // Guilds (v0.8.6): per-endpoint sections plus the § Read Actions
            // (Anti-Scraping) table. Its "List guilds / members / a user's
            // guilds" row names three endpoints without saying they share one
            // budget, so each gets 30/min of its own, the same reading applied
            // to the other grouped read rows. Only the flag endpoints get one
            // shared key, and only because § Flag an Entry says outright that
            // they share a budget.
            GuildsList | GuildsMembersList => RateLimit::per_minute(30),
            GuildsThreadsList => RateLimit::per_minute(45),
            GuildsGet => RateLimit::none(),
            GuildsThreadsCreate => RateLimit::with_day(2, 15),
            // § Rate Limits, Write Actions: "Guild promote | 3 | 15", matching
            // join and leave, and § Change Your Guild Badge repeats it.
            GuildsJoin | GuildsPromote | GuildsLeave => RateLimit::with_day(3, 15),
        }
    }

    /// The extra per-scope budget this endpoint declares, keyed on a `roomId` or
    /// a `conversationId`. `None` for every endpoint with no second dimension,
    /// which is all but two of them.
    ///
    /// A scoped call has to satisfy this limit *and* [`EndpointKey::rate_limit`]
    /// at the same time, so 90 presence heartbeats a minute are available across
    /// all rooms but no more than 15 of them may go to any one room.
    #[must_use]
    pub fn scoped_rate_limit(self) -> Option<RateLimit> {
        match self {
            // § Rate Limits, "cIRC presence heartbeat / leave: 15 per room".
            Self::CircPresence => Some(RateLimit::per_minute(15)),
            // § Rate Limits, "C-Mail typing on/off: 40 per conversation".
            Self::CmailTyping => Some(RateLimit::per_minute(40)),
            _ => None,
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
    fn the_two_separately_limited_auth_routes_carry_their_own_caps() {
        // § Resend Verification Email: 1/min, 5/hour.
        let resend = EndpointKey::AuthResendVerification.rate_limit();
        assert_eq!(resend.per_minute, Some(1));
        assert_eq!(resend.per_hour, Some(5));
        assert_eq!(resend.per_day, None);

        // § Check Username Availability: 10/min, 60/hour.
        let check = EndpointKey::AuthCheckUsername.rate_limit();
        assert_eq!(check.per_minute, Some(10));
        assert_eq!(check.per_hour, Some(60));
        assert_eq!(check.per_day, None);
    }

    #[test]
    fn entry_by_slug_uses_the_documented_read_cap() {
        // § Get User's Entry by Slug states 45/min. Anything lower throttles the
        // client below what the server allows.
        let rl = EndpointKey::UsersGetPostBySlug.rate_limit();
        assert_eq!(rl.per_minute, Some(45));
        assert_eq!(rl.per_hour, None);
        assert_eq!(rl.per_day, None);
        assert_eq!(
            rl.per_minute,
            EndpointKey::UsersListPosts.rate_limit().per_minute,
            "the slug read is capped exactly like the list it belongs to"
        );
    }

    #[test]
    fn cmail_endpoints_use_v06_caps() {
        let start = EndpointKey::CmailStart.rate_limit();
        assert_eq!(start.per_minute, Some(5));
        assert_eq!(start.per_hour, Some(30));
        assert_eq!(start.per_day, Some(50));

        let send = EndpointKey::CmailSend.rate_limit();
        assert_eq!(send.per_minute, Some(15));
        assert_eq!(send.per_hour, Some(150));
        assert_eq!(send.per_day, Some(300));

        let read = EndpointKey::CmailRead.rate_limit();
        assert_eq!(read.per_minute, Some(45));
        assert_eq!(read.per_hour, None);
        assert_eq!(read.per_day, None);

        let mark_read = EndpointKey::CmailMarkRead.rate_limit();
        assert_eq!(mark_read.per_minute, Some(60));
        assert_eq!(mark_read.per_hour, None);
        assert_eq!(mark_read.per_day, None);
    }

    #[test]
    fn edit_endpoints_share_the_edit_caps() {
        for key in [EndpointKey::EntriesEdit, EndpointKey::RepliesEdit] {
            let rl = key.rate_limit();
            assert_eq!(rl.per_minute, Some(5), "{key:?}");
            assert_eq!(rl.per_day, Some(30), "{key:?}");
            assert!(key.scoped_rate_limit().is_none(), "{key:?}");
        }
    }

    #[test]
    fn poke_is_hourly_and_daily_with_no_minute_cap() {
        let rl = EndpointKey::UsersPoke.rate_limit();
        assert_eq!(rl.per_minute, None, "the write table leaves this blank");
        assert_eq!(rl.per_hour, Some(1));
        assert_eq!(rl.per_day, Some(8));
        assert!(
            EndpointKey::UsersPoke.scoped_rate_limit().is_none(),
            "the poke budget is global across all users, never per user"
        );
    }

    #[test]
    fn one_flag_key_carries_the_budget_all_three_endpoints_share() {
        let rl = EndpointKey::Flag.rate_limit();
        assert_eq!(rl.per_minute, Some(5));
        assert_eq!(rl.per_hour, Some(20));
        assert_eq!(rl.per_day, Some(50));
    }

    #[test]
    fn presence_and_typing_declare_both_dimensions() {
        let presence = EndpointKey::CircPresence;
        assert_eq!(presence.rate_limit().per_minute, Some(90));
        assert_eq!(
            presence.scoped_rate_limit().and_then(|rl| rl.per_minute),
            Some(15)
        );

        let typing = EndpointKey::CmailTyping;
        assert_eq!(typing.rate_limit().per_minute, Some(120));
        assert_eq!(
            typing.scoped_rate_limit().and_then(|rl| rl.per_minute),
            Some(40)
        );
    }

    #[test]
    fn the_v086_guild_and_user_guild_routes_carry_their_documented_caps() {
        // § Rate Limits, Write Actions: "Guild promote | 3 | 15", the same
        // budget join and leave carry, but counted on its own key.
        let promote = EndpointKey::GuildsPromote.rate_limit();
        assert_eq!(promote.per_minute, Some(3));
        assert_eq!(promote.per_day, Some(15));
        assert!(
            EndpointKey::GuildsPromote.scoped_rate_limit().is_none(),
            "the promote budget is global, there is nothing to scope it by"
        );

        // § Read Actions: "List guilds / members / a user's guilds | 30".
        let user_guilds = EndpointKey::UsersListGuilds.rate_limit();
        assert_eq!(user_guilds.per_minute, Some(30));
        assert_eq!(user_guilds.per_hour, None);
        assert_eq!(user_guilds.per_day, None);
        assert_eq!(
            user_guilds.per_minute,
            EndpointKey::GuildsList.rate_limit().per_minute,
            "the same table row caps both"
        );
    }

    #[test]
    fn promoting_spends_its_own_budget_not_the_one_join_and_leave_share() {
        // One key per row: were promote folded in with join or leave, a client
        // that joined a guild and then promoted an apprenticeship would have
        // spent two of one 3/min budget instead of one of each.
        assert_ne!(EndpointKey::GuildsPromote, EndpointKey::GuildsJoin);
        assert_ne!(EndpointKey::GuildsPromote, EndpointKey::GuildsLeave);
    }

    #[test]
    fn new_reads_and_deletes_use_v084_caps() {
        assert_eq!(
            EndpointKey::CmailTypingRead.rate_limit().per_minute,
            Some(60)
        );
        assert!(
            EndpointKey::CmailTypingRead.scoped_rate_limit().is_none(),
            "the read table gives no per-conversation qualifier"
        );
        assert_eq!(EndpointKey::CircUsers.rate_limit().per_minute, Some(60));

        let delete = EndpointKey::CircDeleteMessage.rate_limit();
        assert_eq!(delete.per_minute, Some(5));
        assert_eq!(delete.per_day, Some(30));
    }
}

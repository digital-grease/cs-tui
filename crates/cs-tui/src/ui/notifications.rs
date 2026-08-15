//! Notifications screen.
//!
//! Paging note (API v0.8.6 § List Notifications): the server drops the types
//! the reader has muted, blocked or switched off *after* it has taken a page,
//! so a page can arrive shorter than the limit, or empty, while more
//! notifications wait behind it. "More to load" is therefore the cursor being
//! non-null and nothing else, and a page that added no rows asks for the next
//! one instead of ending the list.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Notification, NotificationType, NotificationsFilter, UnreadCount};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::list::{self, TabState};
use super::theme::Theme;

/// How many pages the screen pulls on its own after a page that added nothing,
/// before it waits for the user again.
///
/// A page can come back empty because every notification on it was filtered out
/// server-side (API v0.8.6 § List Notifications), and an empty list has no last
/// row to scroll off, so nothing would ever ask for the page behind it. The
/// screen asks instead. The cap keeps that from becoming an unbounded run of
/// requests against a 30/min budget when a reader has muted hundreds of
/// notifications in a row; the next scroll or refresh renews it.
const MAX_AUTO_PAGES: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationsIntent {
    /// Load next cursor page.
    LoadMore,
    /// Re-fetch from scratch.
    Refresh,
    /// Cycle the read filter (all → unread → read → all).
    ToggleFilter,
    /// Mark the selected notification as read.
    MarkSelectedRead {
        notification_id: String,
    },
    /// Mark every unread notification as read.
    MarkAllRead,
    /// Navigate to the post referenced by the selected notification (if any).
    OpenSelected {
        post_id: String,
        highlight_reply_id: Option<String>,
    },
    /// Open the C-Mail conversation with the sender of a `dm_message`
    /// notification.
    OpenCmail {
        username: String,
        user_id: Option<String>,
    },
    /// Open the profile of the user behind the selected notification, for the
    /// types that name somebody but give nothing to read (a new follower, a
    /// poke, a graffiti mention).
    ///
    /// Only ever built from [`Notification::actor_profile`], so the literal
    /// `"system"` sender v0.8.6 puts on the account notifications is never
    /// routed here (API v0.8.6 § Notification object).
    OpenUser {
        username: String,
    },
    Quit,
    None,
}

/// Server-side type buckets, cycled with `t`. Each maps to a set of
/// [`NotificationType`]s passed to the list endpoint's `type` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifTypeFilter {
    All,
    Mentions,
    Replies,
    Social,
    System,
}

impl NotifTypeFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Mentions,
            Self::Mentions => Self::Replies,
            Self::Replies => Self::Social,
            Self::Social => Self::System,
            Self::System => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mentions => "mentions",
            Self::Replies => "replies",
            Self::Social => "social",
            Self::System => "system",
        }
    }

    /// The notification types this bucket selects (empty = no type filter).
    ///
    /// § List Notifications caps the `type` query at 20 values, so a bucket has
    /// to stay inside that; `System` is the one that grew in v0.8.6 and it sits
    /// at 16.
    fn types(self) -> Vec<NotificationType> {
        use NotificationType::*;
        match self {
            Self::All => vec![],
            Self::Mentions => vec![
                PostMention,
                ReplyMention,
                ChatMention,
                GraffitiMention,
                DmMessage,
            ],
            Self::Replies => vec![Reply, ThreadReply],
            Self::Social => vec![
                NewFollower,
                Unfollowed,
                Poke,
                NewPostFollowing,
                NewPostFriend,
                Bookmark,
                GuildNewThread,
            ],
            // Everything the platform says about the reader's own account:
            // roles, permissions, restrictions, and (new in v0.8.6) the two
            // posting-limit notices. None of them has a sender.
            Self::System => vec![
                SupporterGranted,
                SupporterRemoved,
                HackerGranted,
                HackerRemoved,
                ModeratorGranted,
                ModeratorRemoved,
                ApiAccessGranted,
                ApiAccessRemoved,
                ImagePermissionGranted,
                ImagePermissionRemoved,
                AttachmentPermissionGranted,
                AttachmentPermissionRemoved,
                SystemBan,
                SystemBanLifted,
                PostCooldown,
                RateLimitWarning,
            ],
        }
    }
}

#[derive(Debug)]
pub struct NotificationsScreen {
    pub list: TabState<Notification>,
    pub filter: NotificationsFilter,
    pub type_filter: NotifTypeFilter,
    /// The server's unread total as last reported to the shell (API v0.8.6
    /// § Unread Count). Kept here so the status line can say how much is unread
    /// *beyond* the loaded page, and rendered through [`UnreadCount::badge`] so
    /// a capped count reads "99+" rather than "100".
    unread: UnreadCount,
    /// Automatic follow-up pages still allowed; see [`MAX_AUTO_PAGES`].
    auto_pages_left: u8,
}

impl NotificationsScreen {
    /// A screen that starts out loading its first page.
    pub fn new() -> Self {
        Self {
            list: TabState::loading(),
            filter: NotificationsFilter::All,
            type_filter: NotifTypeFilter::All,
            unread: UnreadCount::default(),
            auto_pages_left: MAX_AUTO_PAGES,
        }
    }

    /// The notification types currently selected by the `t` filter.
    #[must_use]
    pub fn selected_types(&self) -> Vec<NotificationType> {
        self.type_filter.types()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> NotificationsIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return NotificationsIntent::Quit;
        }

        // Always-available actions (don't gate on loading): filter, mark, open.
        match key.code {
            KeyCode::Char('f') => {
                self.filter = match self.filter {
                    NotificationsFilter::All => NotificationsFilter::Unread,
                    NotificationsFilter::Unread => NotificationsFilter::Read,
                    NotificationsFilter::Read => NotificationsFilter::All,
                };
                self.list.items.clear();
                self.list.next_cursor = None;
                self.list.selected = 0;
                self.list.loading = true;
                self.list.error = None;
                return NotificationsIntent::ToggleFilter;
            }
            KeyCode::Char('t') => {
                // Cycle the server-side type bucket; reload like the read filter.
                self.type_filter = self.type_filter.next();
                self.list.items.clear();
                self.list.next_cursor = None;
                self.list.selected = 0;
                self.list.loading = true;
                self.list.error = None;
                return NotificationsIntent::ToggleFilter;
            }
            KeyCode::Char('m') => {
                // Only mark when actually unread: marking an already-read item
                // would burn a rate-limited write and wrongly decrement the
                // global unread count.
                if let Some(n) = self.list.items.get(self.list.selected) {
                    if !n.read {
                        return NotificationsIntent::MarkSelectedRead {
                            notification_id: n.notification_id.clone(),
                        };
                    }
                }
            }
            KeyCode::Char('M') => return NotificationsIntent::MarkAllRead,
            KeyCode::Enter => {
                if let Some(n) = self.list.items.get(self.list.selected) {
                    // Every route that needs a user goes through `actor_profile`
                    // rather than `actor_username`: the account notifications
                    // carry the literal "system" handle (or no actor at all) and
                    // § Notification object says not to open a profile for it.
                    // `actor_name`, which the row is labelled with, cannot tell
                    // the two apart.
                    let actor = n.actor_profile().map(String::from);
                    // A DM notification opens the conversation with its sender
                    // rather than trying to resolve a post.
                    if n.kind == NotificationType::DmMessage {
                        if let Some(username) = actor.clone() {
                            return NotificationsIntent::OpenCmail {
                                username,
                                user_id: n.actor_id.clone(),
                            };
                        }
                    }
                    // Only notifications with a non-empty target_type are
                    // navigable to a post (post/reply); non-navigable ones
                    // (followers, pokes, …) carry an empty target_type and would
                    // otherwise try to open an unrelated id as a post.
                    let navigable = n.target_type.as_deref().is_some_and(|t| !t.is_empty());
                    if navigable {
                        if let Some(post_id) = &n.target_id {
                            return NotificationsIntent::OpenSelected {
                                post_id: post_id.clone(),
                                highlight_reply_id: n.reply_id().map(String::from),
                            };
                        }
                    }
                    // Nothing to read, but somebody to look at: a new follower,
                    // a poke, a graffiti mention (which v0.8.6 gives no target of
                    // its own). The account notifications stop here with nothing
                    // to open, which is the point of them.
                    if let Some(username) = actor {
                        return NotificationsIntent::OpenUser { username };
                    }
                }
            }
            _ => {}
        }

        // Movement + load keys are gated on not-currently-loading so a single
        // press doesn't queue duplicate fetches.
        if self.list.loading {
            return NotificationsIntent::None;
        }
        match super::list_nav::navigate(
            key.code,
            &mut self.list.selected,
            self.list.items.len(),
            self.list.next_cursor.is_some(),
        ) {
            super::list_nav::ListNav::LoadMore => {
                self.list.loading = true;
                // The user asked for more, so the screen may again chase pages
                // that come back empty (see [`MAX_AUTO_PAGES`]).
                self.auto_pages_left = MAX_AUTO_PAGES;
                return NotificationsIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return NotificationsIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        if key.code == KeyCode::Char('r') {
            self.list.items.clear();
            self.list.next_cursor = None;
            self.list.selected = 0;
            self.list.loading = true;
            self.list.error = None;
            return NotificationsIntent::Refresh;
        }
        NotificationsIntent::None
    }

    /// Apply an initial load or refresh, returning the cursor of a page that
    /// has to be fetched straight away.
    ///
    /// v0.8.6 filters muted, blocked and switched-off types out of a page after
    /// taking it (§ List Notifications), so the very first page can land empty
    /// with plenty behind it. An empty list has no last row to scroll off, so
    /// nothing would ever ask for the next page: the caller must fetch the
    /// cursor returned here, or the reader is left staring at "no
    /// notifications" over a full inbox. The screen marks itself loading when
    /// it hands one back, so the fetch has to happen.
    #[must_use = "an empty page is not the end of the list: fetch the returned cursor"]
    pub fn apply_initial(
        &mut self,
        result: Result<(Vec<Notification>, Option<String>), String>,
    ) -> Option<String> {
        self.auto_pages_left = MAX_AUTO_PAGES;
        self.list.apply_initial(result);
        self.autoload_cursor(0)
    }

    /// Append a load-more page, returning the cursor of a page that has to be
    /// fetched straight away (see [`NotificationsScreen::apply_initial`]): a
    /// page that added no rows is server-side filtering, not the end of the
    /// list.
    #[must_use = "a page that added nothing is not the end of the list: fetch the returned cursor"]
    pub fn apply_more(
        &mut self,
        result: Result<(Vec<Notification>, Option<String>), String>,
    ) -> Option<String> {
        let before = self.list.items.len();
        self.list.apply_more(result);
        self.autoload_cursor(before)
    }

    /// The next page to fetch without waiting for the user: `Some` when the
    /// page just applied left `items` no longer than `items_before`, a cursor
    /// remains, and the [`MAX_AUTO_PAGES`] budget is not spent.
    ///
    /// Nothing here looks at how full the page was, only at whether it moved
    /// the list on, because the filtered page size carries no information about
    /// what is left (§ List Notifications). A failed page is left alone: the
    /// error belongs on screen with a retry, not under a run of retries the
    /// reader never asked for. Marks the screen loading, which is what stops a
    /// keypress from fetching the same cursor a second time and appending the
    /// page twice.
    fn autoload_cursor(&mut self, items_before: usize) -> Option<String> {
        if self.list.error.is_some() || self.list.items.len() > items_before {
            return None;
        }
        if self.auto_pages_left == 0 {
            return None;
        }
        let cursor = self.list.next_cursor.clone()?;
        self.auto_pages_left -= 1;
        self.list.loading = true;
        Some(cursor)
    }

    /// Record the server's unread total (API v0.8.6 § Unread Count).
    ///
    /// The shell polls this endpoint for its tab badge; handing the result to
    /// the screen as well is what lets it say that notifications are unread
    /// beyond the loaded page. It is the count that settles the question after a
    /// "mark all as read", which marks at most 5,000 per call (§ Mark All as
    /// Read) and can therefore leave some behind.
    pub fn set_unread_count(&mut self, unread: UnreadCount) {
        self.unread = unread;
    }

    /// Optimistically mark a single notification as read in local state.
    pub fn mark_local(&mut self, notification_id: &str) {
        for n in &mut self.list.items {
            if n.notification_id == notification_id {
                if !n.read {
                    // Keep the unread figure in step until the next poll, so the
                    // status line doesn't count a row the reader just cleared.
                    self.unread.count = self.unread.count.saturating_sub(1);
                }
                n.read = true;
                break;
            }
        }
    }

    /// Undo a `mark_local` when the server rejected the mark.
    pub fn unmark_local(&mut self, notification_id: &str) {
        for n in &mut self.list.items {
            if n.notification_id == notification_id {
                if n.read {
                    self.unread.count = self.unread.count.saturating_add(1);
                }
                n.read = false;
                break;
            }
        }
    }

    /// Optimistically mark every notification as read in local state.
    ///
    /// The unread figure goes to zero with them, but only until the next
    /// [`NotificationsScreen::set_unread_count`]: one "mark all" call marks at
    /// most 5,000 (§ Mark All as Read), so the server is the only thing that
    /// knows whether the inbox is really clear. When it comes back non-zero the
    /// status line says so rather than leaving a screen of read rows implying
    /// the job is done.
    pub fn mark_all_local(&mut self) {
        for n in &mut self.list.items {
            n.read = true;
        }
        self.unread = UnreadCount::default();
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let read = match self.filter {
            NotificationsFilter::All => "",
            NotificationsFilter::Unread => " · unread",
            NotificationsFilter::Read => " · read",
        };
        let typ = if self.type_filter == NotifTypeFilter::All {
            String::new()
        } else {
            format!(" · {}", self.type_filter.label())
        };
        let title = format!(" cs-tui • notifications{read}{typ} ");
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let visible: Vec<usize> = (0..self.list.items.len()).collect();
        list::render_body(
            frame,
            layout[0],
            theme,
            &self.list,
            &visible,
            empty_label(self),
            |n| notification_item(n, theme),
        );

        let status = status_line(self, theme);
        frame.render_widget(status, layout[1]);
    }
}

impl Default for NotificationsScreen {
    fn default() -> Self {
        Self::new()
    }
}

/// What the body says when there is no row to draw.
///
/// An empty list with a cursor still pending is not an empty inbox: this page's
/// contents were filtered out server-side (API v0.8.6 § List Notifications) and
/// more sit behind it, so "no notifications" would be the screen inventing an
/// end the server never reported. The screen chases those pages itself (see
/// [`MAX_AUTO_PAGES`]); this is what the reader sees once that budget is spent
/// and it is their turn again.
fn empty_label(s: &NotificationsScreen) -> &'static str {
    if s.list.next_cursor.is_some() {
        "nothing on this page · n for the next"
    } else {
        "no notifications"
    }
}

fn notification_item(n: &Notification, theme: &Theme) -> ListItem<'static> {
    let actor = n.actor_name();
    let when = n
        .created_at
        .map(crate::config::format_list_timestamp)
        .unwrap_or_default();
    let unread_marker = if n.read {
        Span::styled("  ", theme.muted_style())
    } else {
        Span::styled("● ", theme.accent_style())
    };
    let summary = summarize(n, actor);
    let header = Line::from(vec![
        unread_marker,
        Span::styled(summary, summary_style(n, theme)),
        Span::styled(format!(" · {when}"), theme.muted_style()),
    ]);
    let mut lines = vec![header];
    // The reason is the whole content of an account notification: it is what
    // says which limit was hit or where a held-back entry went. It gets its own
    // line, indented under the summary past the unread marker, rather than
    // riding the header where a long one would push the timestamp off the row.
    if let Some(reason) = reason_text(n) {
        lines.push(Line::from(Span::styled(
            format!("  {reason}"),
            theme.muted_style(),
        )));
    }
    if !crate::config::get().compact {
        lines.push(Line::from(""));
    }
    ListItem::new(lines)
}

/// The notification's explanatory text, when it has one (API v0.8.6
/// § Notification object: `reason` is carried by the notifications about the
/// reader's own account and explains what happened).
///
/// Blank reasons are treated as absent so the row doesn't grow an empty line.
fn reason_text(n: &Notification) -> Option<&str> {
    n.reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
}

/// Style for a notification's summary: the few that are the platform acting
/// against the reader's account are worth picking out of a list of pokes and
/// follows, since each one changes what the reader can do next.
///
/// The screen has no icon vocabulary, so this stays a colour rather than
/// introducing a prefix the other rows would not have.
fn summary_style(n: &Notification, theme: &Theme) -> Style {
    use NotificationType::*;
    match n.kind {
        SystemBan | PostCooldown | RateLimitWarning | ApiAccessRemoved => theme.warning_style(),
        _ => theme.base(),
    }
}

/// One line describing a notification.
///
/// The account notifications (API v0.8.6 § How notifications are generated:
/// `post_cooldown`, `rate_limit_warning`, `system_ban` / `system_ban_lifted`,
/// the role and permission changes) have no sender, so they are phrased about
/// the reader and never name an actor. `actor` is only ever interpolated for
/// the types that genuinely have one.
fn summarize(n: &Notification, actor: &str) -> String {
    use NotificationType::*;
    match n.kind {
        Bookmark => format!("@{actor} bookmarked your post"),
        Reply => format!("@{actor} replied to your post"),
        ThreadReply => {
            let thread = n.thread_author().unwrap_or("a thread");
            format!("@{actor} replied in @{thread}'s thread")
        }
        ReplyMention => format!("@{actor} mentioned you in a reply"),
        PostMention => format!("@{actor} mentioned you in a post"),
        ChatMention => format!("@{actor} mentioned you in chat"),
        GraffitiMention => format!("@{actor} mentioned you in graffiti"),
        DmMessage => format!("@{actor} sent you a DM"),
        NewFollower => format!("@{actor} followed you"),
        Unfollowed => format!("@{actor} unfollowed you"),
        NewPostFollowing => format!("@{actor} posted (from following)"),
        NewPostFriend => format!("@{actor} posted (from friends)"),
        Poke => format!("@{actor} poked you"),
        GuildNewThread => {
            let guild = n.guild_display_name().unwrap_or("a guild");
            format!("new thread in {guild} by @{actor}")
        }
        SupporterGranted => "supporter status granted".to_string(),
        SupporterRemoved => "supporter status removed".to_string(),
        HackerGranted => "hacker status granted".to_string(),
        HackerRemoved => "hacker status removed".to_string(),
        ModeratorGranted => "you are now a moderator".to_string(),
        ModeratorRemoved => "your moderator role was removed".to_string(),
        ApiAccessGranted => "API access granted".to_string(),
        ApiAccessRemoved => "API access removed".to_string(),
        ImagePermissionGranted => "image-upload permission granted".to_string(),
        ImagePermissionRemoved => "image-upload permission removed".to_string(),
        AttachmentPermissionGranted => "attachment permission granted".to_string(),
        AttachmentPermissionRemoved => "attachment permission removed".to_string(),
        SystemBan => "your account has been banned".to_string(),
        SystemBanLifted => "the restriction on your account was lifted".to_string(),
        PostCooldown => "your entry was held back and saved as a note".to_string(),
        RateLimitWarning => "you're approaching a posting limit".to_string(),
        // A type this build has never heard of. If it arrives without an actor,
        // or with the "system" sentinel, it is about the reader's own account
        // (§ Notification object) and must not be dressed up as somebody's
        // action, which is exactly what the `@{actor}` phrasing would do with
        // `actor_name`'s fallback.
        Unknown if n.is_from_system() => "notice about your account".to_string(),
        Unknown => format!("notification from @{actor}"),
    }
}

fn status_line<'a>(s: &'a NotificationsScreen, theme: &Theme) -> Paragraph<'a> {
    if let Some(msg) = list::load_more_error(&s.list) {
        return Paragraph::new(Line::from(Span::styled(msg, theme.error_style())));
    }
    Paragraph::new(Line::from(Span::styled(
        status_text(s),
        theme.muted_style(),
    )))
}

/// Status-bar text: what is loaded, what is unread, and the keys.
///
/// The unread figure is the server's (API v0.8.6 § Unread Count) rather than a
/// tally of the rows on screen, and it goes through [`UnreadCount::badge`], so
/// an inbox over 100 reads "99+" instead of the capped "100". When the server
/// still reports unread notifications and none of the loaded rows is unread,
/// the line says where they are: "mark all as read" clears at most 5,000 per
/// call (§ Mark All as Read), and a screen of read rows must not be left
/// implying the rest went with them.
fn status_text(s: &NotificationsScreen) -> String {
    let keys = "enter open · m mark · M mark-all · f read · t type · r refresh · esc menu";
    if s.list.loading {
        return format!("loading… · {keys}");
    }
    let mut head = format!("{} items", s.list.items.len());
    if s.unread.any() {
        let badge = s.unread.badge();
        if s.list.items.iter().any(|n| !n.read) {
            head.push_str(&format!(" · {badge} unread"));
        } else {
            head.push_str(&format!(" · {badge} unread not on this page"));
        }
    }
    // Never "end" while a cursor remains: a short page is filtering, not the
    // bottom of the list (§ List Notifications).
    let progress = if s.list.next_cursor.is_some() {
        "scroll down for more"
    } else {
        "end"
    };
    format!("{head} · {progress} · {keys}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn notif(
        id: &str,
        kind: NotificationType,
        target: Option<&str>,
        reply: Option<&str>,
    ) -> Notification {
        Notification {
            notification_id: id.into(),
            kind,
            read: false,
            created_at: None,
            actor_id: None,
            actor_username: None,
            target_id: target.map(String::from),
            target_type: target.map(|_| "post".to_string()),
            reason: None,
            metadata: cs_api::NotificationMetadata {
                reply_id: reply.map(String::from),
                ..Default::default()
            },
        }
    }

    /// A notification about the reader's own account: no real sender, just the
    /// `"system"` sentinel and a reason (API v0.8.6 § Notification object).
    fn account_notif(id: &str, kind: NotificationType, reason: &str) -> Notification {
        let mut n = notif(id, kind, None, None);
        n.actor_username = Some("system".into());
        n.reason = Some(reason.into());
        n
    }

    /// Load a page into the screen, checking it needs no follow-up fetch (the
    /// tests about automatic paging call `apply_initial` / `apply_more`
    /// themselves).
    fn seed(s: &mut NotificationsScreen, items: Vec<Notification>, cursor: Option<&str>) {
        let next = s.apply_initial(Ok((items, cursor.map(String::from))));
        assert!(
            next.is_none(),
            "seed page unexpectedly asked to keep paging"
        );
    }

    /// Run the automatic chain to exhaustion over pages that are filtered away
    /// to nothing, returning how many pages the screen asked for.
    fn drain_auto_pages(s: &mut NotificationsScreen, first: Option<String>) -> usize {
        let mut next = first;
        let mut pages = 0_usize;
        while next.is_some() {
            pages += 1;
            assert!(pages <= 64, "the automatic chain ran away");
            next = s.apply_more(Ok((vec![], Some(format!("c{pages}")))));
        }
        pages
    }

    #[test]
    fn new_starts_loading() {
        let s = NotificationsScreen::new();
        assert!(s.list.loading);
        assert!(s.list.items.is_empty());
    }

    #[test]
    fn apply_initial_populates_and_clears_loading() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("n1", NotificationType::Reply, Some("p1"), Some("r1"))],
            Some("cur"),
        );
        assert!(!s.list.loading);
        assert_eq!(s.list.items.len(), 1);
        assert_eq!(s.list.next_cursor.as_deref(), Some("cur"));
    }

    #[test]
    fn j_advances_selection_bounded() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![
                notif("a", NotificationType::Poke, None, None),
                notif("b", NotificationType::Poke, None, None),
            ],
            None,
        );
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.list.selected, 1);
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.list.selected, 1);
    }

    #[test]
    fn j_at_bottom_auto_loads() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("a", NotificationType::Poke, None, None)],
            Some("next"),
        );
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, NotificationsIntent::LoadMore);
        assert!(s.list.loading);
    }

    #[test]
    fn an_empty_first_page_with_a_cursor_asks_for_the_next_one() {
        // § List Notifications drops muted and switched-off types after taking
        // the page, so a page can arrive empty with plenty behind it. Nothing
        // else would ever ask: an empty list has no last row to scroll off.
        let mut s = NotificationsScreen::new();
        let next = s.apply_initial(Ok((vec![], Some("cur".into()))));
        assert_eq!(next.as_deref(), Some("cur"));
        assert!(s.list.loading, "the screen owns the follow-up fetch");
    }

    #[test]
    fn an_empty_page_without_a_cursor_is_the_end() {
        let mut s = NotificationsScreen::new();
        let next = s.apply_initial(Ok((vec![], None)));
        assert!(next.is_none());
        assert!(!s.list.loading);
    }

    #[test]
    fn a_page_that_added_nothing_keeps_paging() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("a", NotificationType::Poke, None, None)],
            Some("c1"),
        );
        let next = s.apply_more(Ok((vec![], Some("c2".into()))));
        assert_eq!(next.as_deref(), Some("c2"), "a short page is not the end");
    }

    #[test]
    fn a_page_that_added_rows_hands_the_list_back() {
        // Progress is enough: the cursor is still on the state, so scrolling off
        // the last row pulls the next page the ordinary way.
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("a", NotificationType::Poke, None, None)],
            Some("c1"),
        );
        let next = s.apply_more(Ok((
            vec![notif("b", NotificationType::Poke, None, None)],
            Some("c2".into()),
        )));
        assert!(next.is_none());
        assert!(!s.list.loading);
        assert_eq!(s.list.next_cursor.as_deref(), Some("c2"));
    }

    #[test]
    fn the_automatic_chain_is_capped() {
        let mut s = NotificationsScreen::new();
        let first = s.apply_initial(Ok((vec![], Some("c0".into()))));
        assert_eq!(
            drain_auto_pages(&mut s, first),
            usize::from(MAX_AUTO_PAGES),
            "a run of filtered pages must not turn into unbounded requests"
        );
        assert!(!s.list.loading, "the reader gets the screen back");
        assert!(
            s.list.next_cursor.is_some(),
            "and the list still knows there is more"
        );
    }

    #[test]
    fn asking_for_more_renews_the_automatic_chain() {
        let mut s = NotificationsScreen::new();
        let first = s.apply_initial(Ok((vec![], Some("c0".into()))));
        drain_auto_pages(&mut s, first);
        // `n` is a load-more key while a cursor remains.
        assert_eq!(
            s.handle_key(key(KeyCode::Char('n'))),
            NotificationsIntent::LoadMore
        );
        let next = s.apply_more(Ok((vec![], Some("c9".into()))));
        assert!(next.is_some(), "the renewed budget chases the next page");
    }

    #[test]
    fn a_fresh_load_renews_the_automatic_chain() {
        let mut s = NotificationsScreen::new();
        let first = s.apply_initial(Ok((vec![], Some("c0".into()))));
        drain_auto_pages(&mut s, first);
        let again = s.apply_initial(Ok((vec![], Some("c0".into()))));
        assert!(again.is_some(), "a refresh starts with a full budget");
    }

    #[test]
    fn a_failed_page_is_not_retried_automatically() {
        // The error belongs on the status line with a retry key, not under a run
        // of requests the reader never asked for.
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("a", NotificationType::Poke, None, None)],
            Some("c1"),
        );
        let next = s.apply_more(Err("boom".into()));
        assert!(next.is_none());
        assert!(!s.list.loading);
    }

    #[test]
    fn an_empty_list_with_a_cursor_does_not_claim_an_empty_inbox() {
        let mut s = NotificationsScreen::new();
        let _ = s.apply_initial(Ok((vec![], Some("c".into()))));
        assert_eq!(empty_label(&s), "nothing on this page · n for the next");

        let mut done = NotificationsScreen::new();
        seed(&mut done, vec![], None);
        assert_eq!(empty_label(&done), "no notifications");
    }

    #[test]
    fn enter_opens_when_target_present() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("n1", NotificationType::Reply, Some("p1"), Some("r1"))],
            None,
        );
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            NotificationsIntent::OpenSelected {
                post_id: "p1".into(),
                highlight_reply_id: Some("r1".into()),
            }
        );
    }

    #[test]
    fn enter_with_no_target_yields_none() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("n1", NotificationType::Poke, None, None)],
            None,
        );
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, NotificationsIntent::None);
    }

    #[test]
    fn enter_ignores_non_navigable_notification_with_a_target_id() {
        // A follower-style notification can carry a non-post target_id but an
        // empty target_type, so Enter must not try to open it as a post.
        let mut s = NotificationsScreen::new();
        let mut n = notif("n1", NotificationType::NewFollower, Some("u1"), None);
        n.target_type = None;
        seed(&mut s, vec![n], None);
        assert_eq!(s.handle_key(key(KeyCode::Enter)), NotificationsIntent::None);
    }

    #[test]
    fn enter_on_dm_notification_opens_cmail_with_sender() {
        let mut s = NotificationsScreen::new();
        let mut n = notif("n1", NotificationType::DmMessage, None, None);
        n.actor_username = Some("alice".into());
        n.actor_id = Some("u-alice".into());
        seed(&mut s, vec![n], None);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            NotificationsIntent::OpenCmail {
                username: "alice".into(),
                user_id: Some("u-alice".into()),
            }
        );
    }

    #[test]
    fn enter_falls_back_to_the_actors_profile() {
        // A graffiti mention has no target of its own, so the person who wrote
        // it is the only thing to open.
        let mut s = NotificationsScreen::new();
        let mut n = notif("n1", NotificationType::GraffitiMention, None, None);
        n.actor_username = Some("alice".into());
        seed(&mut s, vec![n], None);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            NotificationsIntent::OpenUser {
                username: "alice".into()
            }
        );
    }

    #[test]
    fn a_readable_target_still_wins_over_the_profile_fallback() {
        let mut s = NotificationsScreen::new();
        let mut n = notif("n1", NotificationType::Reply, Some("p1"), Some("r1"));
        n.actor_username = Some("alice".into());
        seed(&mut s, vec![n], None);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            NotificationsIntent::OpenSelected {
                post_id: "p1".into(),
                highlight_reply_id: Some("r1".into()),
            }
        );
    }

    #[test]
    fn enter_on_an_account_notification_opens_nothing() {
        // § Notification object: post_cooldown carries the literal "system"
        // handle and the instruction not to open a profile for it. Routing off
        // `actor_username` would push a profile screen for a user that is not
        // there.
        let mut s = NotificationsScreen::new();
        let n = account_notif("n1", NotificationType::PostCooldown, "posting too fast");
        seed(&mut s, vec![n], None);
        assert_eq!(s.handle_key(key(KeyCode::Enter)), NotificationsIntent::None);
    }

    #[test]
    fn enter_on_a_dm_from_system_opens_no_conversation() {
        // Same sentinel, the other route out of this screen.
        let mut s = NotificationsScreen::new();
        let mut n = notif("n1", NotificationType::DmMessage, None, None);
        n.actor_username = Some("system".into());
        seed(&mut s, vec![n], None);
        assert_eq!(s.handle_key(key(KeyCode::Enter)), NotificationsIntent::None);
    }

    #[test]
    fn m_marks_selected() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("n1", NotificationType::Reply, None, None)],
            None,
        );
        let intent = s.handle_key(key(KeyCode::Char('m')));
        assert_eq!(
            intent,
            NotificationsIntent::MarkSelectedRead {
                notification_id: "n1".into()
            }
        );
    }

    #[test]
    fn m_on_already_read_is_a_noop() {
        // Marking an already-read item would waste a rate-limited write and
        // wrongly decrement the global unread count.
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("n1", NotificationType::Reply, None, None)],
            None,
        );
        s.mark_local("n1"); // now read
        assert_eq!(
            s.handle_key(key(KeyCode::Char('m'))),
            NotificationsIntent::None
        );
    }

    #[test]
    fn capital_m_marks_all() {
        let mut s = NotificationsScreen::new();
        seed(&mut s, vec![], None);
        let key_m = KeyEvent {
            code: KeyCode::Char('M'),
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(s.handle_key(key_m), NotificationsIntent::MarkAllRead);
    }

    #[test]
    fn f_cycles_filter() {
        let mut s = NotificationsScreen::new();
        seed(&mut s, vec![], None);
        assert!(matches!(s.filter, NotificationsFilter::All));
        s.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(s.filter, NotificationsFilter::Unread));
        s.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(s.filter, NotificationsFilter::Read));
        s.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(s.filter, NotificationsFilter::All));
    }

    #[test]
    fn t_cycles_type_filter_and_reloads() {
        let mut s = NotificationsScreen::new();
        seed(&mut s, vec![], None);
        assert!(matches!(s.type_filter, NotifTypeFilter::All));
        assert!(s.selected_types().is_empty());

        let intent = s.handle_key(key(KeyCode::Char('t')));
        assert_eq!(intent, NotificationsIntent::ToggleFilter);
        assert!(matches!(s.type_filter, NotifTypeFilter::Mentions));
        assert!(s.selected_types().contains(&NotificationType::PostMention));
        assert!(s.list.loading, "changing the type filter triggers a reload");
    }

    #[test]
    fn type_filter_wraps_back_to_all() {
        let mut s = NotificationsScreen::new();
        seed(&mut s, vec![], None);
        for _ in 0..5 {
            s.handle_key(key(KeyCode::Char('t')));
            s.list.loading = false; // let the next press through
        }
        assert!(matches!(s.type_filter, NotifTypeFilter::All));
    }

    #[test]
    fn the_type_buckets_carry_the_v086_types_and_fit_the_query() {
        assert!(
            NotifTypeFilter::Mentions
                .types()
                .contains(&NotificationType::GraffitiMention),
            "a graffiti mention is a mention"
        );
        for kind in [
            NotificationType::ModeratorGranted,
            NotificationType::ModeratorRemoved,
            NotificationType::ApiAccessGranted,
            NotificationType::ApiAccessRemoved,
            NotificationType::SystemBanLifted,
            NotificationType::PostCooldown,
            NotificationType::RateLimitWarning,
        ] {
            assert!(
                NotifTypeFilter::System.types().contains(&kind),
                "{kind:?} is missing from the system bucket, so the filter hides it"
            );
        }
        // § List Notifications caps the `type` query at 20 values.
        for bucket in [
            NotifTypeFilter::All,
            NotifTypeFilter::Mentions,
            NotifTypeFilter::Replies,
            NotifTypeFilter::Social,
            NotifTypeFilter::System,
        ] {
            assert!(
                bucket.types().len() <= 20,
                "the {} bucket is past the query cap",
                bucket.label()
            );
        }
    }

    #[test]
    fn mark_local_flips_read_flag() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![
                notif("a", NotificationType::Poke, None, None),
                notif("b", NotificationType::Poke, None, None),
            ],
            None,
        );
        s.mark_local("a");
        assert!(s.list.items[0].read);
        assert!(!s.list.items[1].read);
    }

    #[test]
    fn mark_all_local_flips_every_record() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![
                notif("a", NotificationType::Poke, None, None),
                notif("b", NotificationType::Poke, None, None),
            ],
            None,
        );
        s.mark_all_local();
        assert!(s.list.items.iter().all(|n| n.read));
    }

    #[test]
    fn marking_read_moves_the_unread_figure_and_back() {
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![
                notif("a", NotificationType::Poke, None, None),
                notif("b", NotificationType::Poke, None, None),
            ],
            None,
        );
        s.set_unread_count(UnreadCount {
            count: 2,
            exact: true,
        });
        s.mark_local("a");
        assert!(status_text(&s).contains("1 unread"));
        s.unmark_local("a"); // the server rejected the mark
        assert!(status_text(&s).contains("2 unread"));
        // A second mark of the same row must not count twice.
        s.mark_local("a");
        s.mark_local("a");
        assert!(status_text(&s).contains("1 unread"));
    }

    #[test]
    fn the_status_line_renders_a_capped_count_as_99_plus() {
        // § Unread Count: over 100 unread the server counts only the 100 most
        // recent, and asks for "99+" rather than the figure it did count.
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("a", NotificationType::Poke, None, None)],
            None,
        );
        s.set_unread_count(UnreadCount {
            count: 100,
            exact: false,
        });
        let text = status_text(&s);
        assert!(text.contains("99+ unread"), "{text}");
        assert!(!text.contains("100"), "{text}");
    }

    #[test]
    fn the_status_line_owns_up_to_unread_left_by_a_partial_mark_all() {
        // "Mark all as read" clears at most 5,000 per call (§ Mark All as Read),
        // so a screen of read rows is not proof of an empty inbox. The server's
        // next count is what settles it.
        let mut s = NotificationsScreen::new();
        seed(
            &mut s,
            vec![notif("a", NotificationType::Poke, None, None)],
            None,
        );
        s.mark_all_local();
        assert!(
            !status_text(&s).contains("unread"),
            "the optimistic state is a clean inbox"
        );
        s.set_unread_count(UnreadCount {
            count: 4,
            exact: true,
        });
        let text = status_text(&s);
        assert!(text.contains("4 unread not on this page"), "{text}");
    }

    #[test]
    fn summary_includes_actor_for_reply() {
        let actor_n = Notification {
            notification_id: "n".into(),
            kind: NotificationType::Reply,
            read: false,
            created_at: None,
            actor_id: Some("u".into()),
            actor_username: Some("alice".into()),
            target_id: None,
            target_type: None,
            reason: None,
            metadata: cs_api::NotificationMetadata::default(),
        };
        let s = summarize(&actor_n, actor_n.actor_name());
        assert!(s.contains("@alice"));
        assert!(s.contains("replied"));
    }

    #[test]
    fn graffiti_mention_names_its_actor() {
        let mut n = notif("n", NotificationType::GraffitiMention, None, None);
        n.actor_username = Some("alice".into());
        let text = summarize(&n, n.actor_name());
        assert!(text.contains("@alice"), "{text}");
        assert!(text.contains("graffiti"), "{text}");
    }

    #[test]
    fn account_notifications_never_read_as_someone_elses_doing() {
        // They arrive with the "system" sentinel (or no actor at all), which
        // `actor_name` renders as "system": phrasing any of them as "@system did
        // X" would invent a user and offer a handle nobody can open.
        for kind in [
            NotificationType::PostCooldown,
            NotificationType::RateLimitWarning,
            NotificationType::SystemBan,
            NotificationType::SystemBanLifted,
            NotificationType::ModeratorGranted,
            NotificationType::ModeratorRemoved,
            NotificationType::ApiAccessGranted,
            NotificationType::ApiAccessRemoved,
        ] {
            let n = account_notif("n", kind, "because");
            let text = summarize(&n, n.actor_name());
            assert!(!text.is_empty(), "{kind:?} has no summary");
            assert!(!text.contains('@'), "{kind:?} named an actor: {text}");
        }
    }

    #[test]
    fn an_unmodelled_system_notification_is_not_pinned_on_a_user() {
        // A type invented after this build decodes as `Unknown`, and the system
        // ones still carry the sentinel handle.
        let mut n = notif("n", NotificationType::Unknown, None, None);
        n.actor_username = Some("system".into());
        assert_eq!(summarize(&n, n.actor_name()), "notice about your account");
        n.actor_username = Some("alice".into());
        assert!(summarize(&n, n.actor_name()).contains("@alice"));
    }

    #[test]
    fn the_reason_is_what_an_account_notification_has_to_say() {
        let mut n = account_notif("n", NotificationType::PostCooldown, "  saved as a note  ");
        assert_eq!(reason_text(&n), Some("saved as a note"));
        n.reason = Some("   ".into());
        assert_eq!(reason_text(&n), None, "a blank reason adds no line");
        n.reason = None;
        assert_eq!(reason_text(&n), None);
    }

    #[test]
    fn the_notifications_that_change_what_you_can_do_stand_out() {
        let theme = Theme::cyber();
        for kind in [
            NotificationType::SystemBan,
            NotificationType::PostCooldown,
            NotificationType::RateLimitWarning,
            NotificationType::ApiAccessRemoved,
        ] {
            let n = account_notif("n", kind, "because");
            assert_eq!(
                summary_style(&n, &theme),
                theme.warning_style(),
                "{kind:?} should be picked out of the list"
            );
        }
        let ordinary = notif("n", NotificationType::Poke, None, None);
        assert_eq!(summary_style(&ordinary, &theme), theme.base());
    }
}

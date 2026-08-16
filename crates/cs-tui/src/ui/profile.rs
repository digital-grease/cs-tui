//! Profile screen, 6 tabs (Info, Posts, Replies, Followers, Following,
//! Guilds).
//!
//! The Guilds tab is every guild the viewed user is in (v0.8.6 § List a User's
//! Guilds): the one guild whose name and icon are their profile badge, held as
//! founder or member, then up to five apprenticeships. That endpoint returns at
//! most six rows and never paginates, so the tab has no load-more.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Entry, Follow, Reply, User, UserGuild};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileTab {
    Info,
    Posts,
    Replies,
    Followers,
    Following,
    /// Every guild this user is in, badge guild first (v0.8.6 § List a User's
    /// Guilds). Added last so the tabs that came before it keep the shifted
    /// number they have always answered to.
    Guilds,
}

impl ProfileTab {
    pub const ALL: [ProfileTab; 6] = [
        Self::Info,
        Self::Posts,
        Self::Replies,
        Self::Followers,
        Self::Following,
        Self::Guilds,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Posts => "posts",
            Self::Replies => "replies",
            Self::Followers => "followers",
            Self::Following => "following",
            Self::Guilds => "guilds",
        }
    }

    /// Map shifted number row to a tab (Shift+1=!, Shift+2=@, ...). Picked
    /// because plain `1` … `5` are reserved for the top-level root nav.
    pub fn from_shifted(c: char) -> Option<Self> {
        match c {
            '!' => Some(Self::Info),
            '@' => Some(Self::Posts),
            '#' => Some(Self::Replies),
            '$' => Some(Self::Followers),
            '%' => Some(Self::Following),
            '^' => Some(Self::Guilds),
            _ => None,
        }
    }
}

pub use super::list::TabState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileIntent {
    Back,
    Quit,
    /// Switch to the given tab. The app fetches data if the tab isn't loaded yet.
    SelectTab(ProfileTab),
    /// Load the next cursor page for the active tab.
    LoadMoreCurrentTab,
    /// Refresh the active tab.
    RefreshCurrentTab,
    /// Toggle follow/unfollow for the viewed user (only meaningful when not self).
    ToggleFollow,
    /// Start/open a C-Mail conversation with the viewed user (not self).
    MessageUser {
        username: String,
        user_id: Option<String>,
    },
    /// Enter edit mode (only meaningful when viewing self).
    EditOwnProfile,
    /// Nudge the viewed user (`P`), the notification v0.8.4 § Poke a User
    /// sends. Emitted on other people's profiles only: poking yourself is a
    /// documented `400`, so the key is not offered on your own.
    ///
    /// The budget is 1/hour and 8/day *across every user*, not per user, so one
    /// poke spends the whole allowance for the hour and the shell has to report
    /// the outcome rather than let it pass silently.
    PokeUser {
        username: String,
    },
    /// Edit the entry under the cursor on the Posts tab (`E`).
    ///
    /// `content` is the entry's current body, for the editor to open on.
    /// Nothing is checked client-side: v0.8.4 § Edit Entry restricts editing to
    /// supporters, to their own entries, and to the first five minutes, and all
    /// three are enforced by the server, so the shell sends the edit and
    /// surfaces whatever comes back.
    EditEntry {
        post_id: String,
        content: String,
    },
    /// Pin (or unpin) one of your own posts to your profile.
    PinPost {
        post_id: String,
        pin: bool,
    },
    /// Open the post under the cursor (Posts / Replies tab).
    OpenPost {
        post_id: String,
    },
    /// Open the reply under the cursor (Replies tab); falls back to OpenPost.
    OpenReply {
        post_id: String,
        reply_id: String,
    },
    /// Push another user's profile (Followers / Following tabs).
    OpenUser {
        username: String,
    },
    /// Open the guild under the cursor (Guilds tab) in the guild detail screen,
    /// the same one the guilds index opens.
    ///
    /// Addressed by slug, which is how every `/v1/guilds/…` route identifies a
    /// guild. Rows carrying no slug never emit this.
    OpenGuild {
        slug: String,
    },
    None,
}

#[derive(Debug)]
pub struct ProfileScreen {
    /// The username being viewed. `None` means "me" — resolved after `user`
    /// loads.
    pub username: Option<String>,
    /// True if this is the user's own profile (the root invocation).
    pub is_self: bool,
    /// True if this profile is the root invocation (so Backspace and menu→Quit
    /// terminate the app) rather than being pushed (where they pop back).
    pub is_root: bool,
    pub tab: ProfileTab,

    pub user: Option<User>,
    pub loading_user: bool,
    pub user_error: Option<String>,

    pub posts: TabState<Entry>,
    pub replies: TabState<Reply>,
    pub followers: TabState<Follow>,
    pub following: TabState<Follow>,

    /// Every guild the viewed user is in: the badge guild first, then their
    /// apprenticeships oldest first (v0.8.6 § List a User's Guilds). Filled by
    /// [`ProfileScreen::apply_guilds`]. `next_cursor` stays `None` for the life
    /// of the tab, because that endpoint returns at most six rows and has no
    /// pagination at all, so the shared list nav never offers a load-more here.
    pub guilds: TabState<UserGuild>,

    pub follow_action_pending: bool,

    /// True while a poke is in flight. The shell sets it when it fires the
    /// poke and clears it when the call settles, so the status line can say the
    /// nudge is on its way. Poke is capped at 1/hour and 8/day across all users
    /// (v0.8.4 § Poke a User), which makes a silently repeated press expensive.
    pub poke_pending: bool,

    /// The signed-in account's user id, when the shell knows it.
    ///
    /// `is_self` is only true for the root profile, so a profile opened BY NAME
    /// (a search hit, a row in someone's followers list) reads as somebody else
    /// even when it is you. Poking yourself is a documented `400`, so the poke
    /// affordance compares ids once the profile loads rather than trusting
    /// `is_self` alone. See [`ProfileScreen::is_viewing_self`].
    pub viewer_user_id: Option<String>,
}

impl ProfileScreen {
    pub fn new_own() -> Self {
        Self::new_inner(None, true, true)
    }

    pub fn new_for(username: String) -> Self {
        Self::new_inner(Some(username), false, false)
    }

    fn new_inner(username: Option<String>, is_self: bool, is_root: bool) -> Self {
        Self {
            username,
            is_self,
            is_root,
            tab: ProfileTab::Info,
            user: None,
            loading_user: true,
            user_error: None,
            posts: TabState::default(),
            replies: TabState::default(),
            followers: TabState::default(),
            following: TabState::default(),
            guilds: TabState::default(),
            follow_action_pending: false,
            poke_pending: false,
            viewer_user_id: None,
        }
    }

    /// Whether this profile is the signed-in user's own, however it was reached.
    ///
    /// Broader than `is_self`, which only covers the root profile. Falls back to
    /// `is_self` until the profile loads, and answers `false` when the shell has
    /// no viewer id, so an unknown viewer keeps every affordance and lets the
    /// server decide.
    #[must_use]
    pub fn is_viewing_self(&self) -> bool {
        if self.is_self {
            return true;
        }
        match (self.user.as_ref(), self.viewer_user_id.as_deref()) {
            (Some(u), Some(viewer)) => !u.id.is_empty() && u.id == viewer,
            _ => false,
        }
    }

    pub fn apply_user(&mut self, result: Result<User, String>) {
        self.loading_user = false;
        match result {
            Ok(u) => {
                if self.username.is_none() {
                    self.username = Some(u.username.clone());
                }
                self.user = Some(u);
                self.user_error = None;
            }
            Err(msg) => self.user_error = Some(msg),
        }
    }

    /// Hand the Guilds tab this user's guilds (v0.8.6 § List a User's Guilds):
    /// the guild whose badge is on their profile first, then the
    /// apprenticeships, oldest first. Used for the first load and for `r`
    /// alike, since both replace the whole list.
    ///
    /// Takes a plain `Vec` rather than the `(items, cursor)` pair every other
    /// tab takes. A user holds at most six guilds, one badge plus five
    /// apprenticeships, and the spec says outright that the endpoint has no
    /// pagination and its `cursor` is always null, so there is no page token to
    /// carry. Leaving `next_cursor` unset is also what keeps the shared list
    /// nav from offering a load-more the server could never answer.
    pub fn apply_guilds(&mut self, result: Result<Vec<UserGuild>, String>) {
        self.guilds.apply_initial(result.map(|items| (items, None)));
    }

    /// Replace one entry on the Posts tab with a freshly fetched copy, after an
    /// edit succeeded.
    ///
    /// The edit endpoint answers with the post id alone (v0.8.4 § Edit Entry:
    /// "Returns `{ "data": { "postId": "..." } }`"), so the shell re-fetches the
    /// entry and hands the new one here. That keeps the `(edited)` marker and
    /// the new body in sync without refreshing the whole tab, which would cost
    /// a page request and throw away the pagination cursor.
    ///
    /// Returns `false` when no row carries that post id, which happens when the
    /// tab has been refreshed or another profile is now on screen. There is
    /// nothing to update in that case and nothing to report.
    pub fn apply_edited_entry(&mut self, entry: Entry) -> bool {
        match self
            .posts
            .items
            .iter_mut()
            .find(|e| e.post_id == entry.post_id)
        {
            Some(slot) => {
                *slot = entry;
                true
            }
            None => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ProfileIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ProfileIntent::Quit;
        }
        // Backspace is the direct back/quit shortcut on Profile. Esc opens the
        // App menu; from there the user picks Back / Logout / Quit explicitly.
        if key.code == KeyCode::Backspace {
            return if self.is_root {
                ProfileIntent::Quit
            } else {
                ProfileIntent::Back
            };
        }

        // Tab switching via shifted number keys.
        if let KeyCode::Char(c) = key.code {
            if let Some(t) = ProfileTab::from_shifted(c) {
                if self.tab != t {
                    self.tab = t;
                    return ProfileIntent::SelectTab(t);
                }
                return ProfileIntent::None;
            }
        }

        // Tab / Shift+Tab cycle the tabs; h/l are vim aliases. (←/→ are global
        // section nav, handled before the screen sees them.)
        match key.code {
            KeyCode::Char('h') | KeyCode::BackTab => {
                let i = ProfileTab::ALL
                    .iter()
                    .position(|t| *t == self.tab)
                    .unwrap_or(0);
                let new = ProfileTab::ALL[(i + ProfileTab::ALL.len() - 1) % ProfileTab::ALL.len()];
                self.tab = new;
                return ProfileIntent::SelectTab(new);
            }
            KeyCode::Char('l') | KeyCode::Tab => {
                let i = ProfileTab::ALL
                    .iter()
                    .position(|t| *t == self.tab)
                    .unwrap_or(0);
                let new = ProfileTab::ALL[(i + 1) % ProfileTab::ALL.len()];
                self.tab = new;
                return ProfileIntent::SelectTab(new);
            }
            _ => {}
        }

        // Always-available actions (regardless of tab/loading).
        match key.code {
            KeyCode::Char('F') if !self.is_viewing_self() && self.user.is_some() => {
                return ProfileIntent::ToggleFollow;
            }
            KeyCode::Char('m') if !self.is_viewing_self() => {
                if let Some(u) = &self.user {
                    return ProfileIntent::MessageUser {
                        username: u.username.clone(),
                        user_id: Some(u.id.clone()),
                    };
                }
            }
            KeyCode::Char('e') if self.is_self => {
                return ProfileIntent::EditOwnProfile;
            }
            // Pin/unpin the selected post on your own Posts tab (server requires
            // it to be your own entry, which it always is here).
            KeyCode::Char('P') if self.is_self && self.tab == ProfileTab::Posts => {
                if let Some(e) = self.posts.items.get(self.posts.selected) {
                    let pinned = self.user.as_ref().and_then(|u| u.pinned_post_id.as_deref())
                        == Some(e.post_id.as_str());
                    return ProfileIntent::PinPost {
                        post_id: e.post_id.clone(),
                        pin: !pinned,
                    };
                }
            }
            // Poke, on other people only (v0.8.4 § Poke a User: poking yourself
            // is a `400`, so your own profile never offers it). Like `m`, it
            // waits for the profile to load so the nudge goes to the handle the
            // server confirmed rather than to whatever was typed to get here.
            KeyCode::Char('P') if !self.is_viewing_self() => {
                if let Some(u) = &self.user {
                    return ProfileIntent::PokeUser {
                        username: u.username.clone(),
                    };
                }
            }
            // Edit the entry under the cursor (v0.8.4 § Edit Entry). Uppercase
            // because lowercase `e` already edits your own profile from every
            // tab.
            //
            // Your own Posts tab only. Editing is never gated on supporter
            // status or on the five-minute window, which the client cannot
            // know, but authorship here is not a guess: every entry on someone
            // else's Posts tab is theirs, so offering `E` there would promise an
            // action that can only ever come back `403`. The feeds and the post
            // detail still offer `e` on everything, because there authorship
            // genuinely is unknown.
            KeyCode::Char('E') if self.is_self && self.tab == ProfileTab::Posts => {
                if let Some(e) = self.posts.items.get(self.posts.selected) {
                    return ProfileIntent::EditEntry {
                        post_id: e.post_id.clone(),
                        content: e.content.clone(),
                    };
                }
            }
            _ => {}
        }

        match self.tab {
            ProfileTab::Info => ProfileIntent::None,
            ProfileTab::Posts => self.handle_list_key(key, ListTarget::Posts),
            ProfileTab::Replies => self.handle_list_key(key, ListTarget::Replies),
            ProfileTab::Followers => self.handle_list_key(key, ListTarget::Followers),
            ProfileTab::Following => self.handle_list_key(key, ListTarget::Following),
            ProfileTab::Guilds => self.handle_list_key(key, ListTarget::Guilds),
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent, target: ListTarget) -> ProfileIntent {
        let (len, loading, cursor_present) = match target {
            ListTarget::Posts => (
                self.posts.items.len(),
                self.posts.loading,
                self.posts.next_cursor.is_some(),
            ),
            ListTarget::Replies => (
                self.replies.items.len(),
                self.replies.loading,
                self.replies.next_cursor.is_some(),
            ),
            ListTarget::Followers => (
                self.followers.items.len(),
                self.followers.loading,
                self.followers.next_cursor.is_some(),
            ),
            ListTarget::Following => (
                self.following.items.len(),
                self.following.loading,
                self.following.next_cursor.is_some(),
            ),
            // No third element to read: § List a User's Guilds is not
            // paginated, so this tab never has another page to pull.
            ListTarget::Guilds => (self.guilds.items.len(), self.guilds.loading, false),
        };

        // Pagination must not fire while a page is already in flight, so the
        // limiter folds `!loading` into "has more" for the shared nav helper.
        match super::list_nav::navigate(
            key.code,
            self.selection_mut(target),
            len,
            cursor_present && !loading,
        ) {
            super::list_nav::ListNav::LoadMore => return ProfileIntent::LoadMoreCurrentTab,
            super::list_nav::ListNav::Moved => return ProfileIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        match key.code {
            KeyCode::Char('r') if !loading => return ProfileIntent::RefreshCurrentTab,
            KeyCode::Enter => return self.enter_on_list(target),
            _ => {}
        }
        ProfileIntent::None
    }

    fn selection_mut(&mut self, target: ListTarget) -> &mut usize {
        match target {
            ListTarget::Posts => &mut self.posts.selected,
            ListTarget::Replies => &mut self.replies.selected,
            ListTarget::Followers => &mut self.followers.selected,
            ListTarget::Following => &mut self.following.selected,
            ListTarget::Guilds => &mut self.guilds.selected,
        }
    }

    fn enter_on_list(&self, target: ListTarget) -> ProfileIntent {
        match target {
            ListTarget::Posts => self
                .posts
                .items
                .get(self.posts.selected)
                .map(|e| ProfileIntent::OpenPost {
                    post_id: e.post_id.clone(),
                })
                .unwrap_or(ProfileIntent::None),
            ListTarget::Replies => self
                .replies
                .items
                .get(self.replies.selected)
                .map(|r| ProfileIntent::OpenReply {
                    post_id: r.post_id.clone(),
                    reply_id: r.reply_id.clone(),
                })
                .unwrap_or(ProfileIntent::None),
            // The follows API returns IDs without usernames, so a row may have
            // no username to open. Skip navigation in that case (opening by an
            // empty username would request `/v1/users/` and fail).
            ListTarget::Followers => self
                .followers
                .items
                .get(self.followers.selected)
                .filter(|f| !f.follower_username.is_empty())
                .map(|f| ProfileIntent::OpenUser {
                    username: f.follower_username.clone(),
                })
                .unwrap_or(ProfileIntent::None),
            ListTarget::Following => self
                .following
                .items
                .get(self.following.selected)
                .filter(|f| !f.followed_username.is_empty())
                .map(|f| ProfileIntent::OpenUser {
                    username: f.followed_username.clone(),
                })
                .unwrap_or(ProfileIntent::None),
            // Guilds are addressed by slug on every `/v1/guilds/…` route, so a
            // row that arrived without one has nothing to open.
            ListTarget::Guilds => self
                .guilds
                .items
                .get(self.guilds.selected)
                .filter(|g| !g.slug.is_empty())
                .map(|g| ProfileIntent::OpenGuild {
                    slug: g.slug.clone(),
                })
                .unwrap_or(ProfileIntent::None),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let title_who = self
            .user
            .as_ref()
            .map(|u| format!("@{}", u.username))
            .or_else(|| self.username.as_ref().map(|u| format!("@{u}")))
            .unwrap_or_else(|| "@…".to_string());
        let title = if self.is_self {
            format!(" cs-tui • profile · {title_who} (you) ")
        } else {
            format!(" cs-tui • profile · {title_who} ")
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // tab row
                Constraint::Min(1),    // content
                Constraint::Length(1), // status
            ])
            .split(inner);

        self.render_tab_row(frame, layout[0], theme);
        match self.tab {
            ProfileTab::Info => self.render_info(frame, layout[1], theme),
            ProfileTab::Posts => self.render_posts(frame, layout[1], theme),
            ProfileTab::Replies => self.render_replies(frame, layout[1], theme),
            ProfileTab::Followers => self.render_followers(frame, layout[1], theme),
            ProfileTab::Following => self.render_following(frame, layout[1], theme),
            ProfileTab::Guilds => self.render_guilds(frame, layout[1], theme),
        }
        self.render_status(frame, layout[2], theme);
    }

    fn render_tab_row(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut spans: Vec<Span<'_>> = Vec::new();
        for (i, t) in ProfileTab::ALL.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", theme.muted_style()));
            }
            let style = if *t == self.tab {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            spans.push(Span::styled(t.label().to_string(), style));
        }
        spans.push(Span::styled("    h/l switch tab", theme.muted_style()));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_info(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if self.loading_user {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading profile…",
                    theme.accent_style(),
                ))),
                area,
            );
            return;
        }
        if let Some(msg) = &self.user_error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style()))),
                area,
            );
            return;
        }
        let Some(u) = &self.user else {
            return;
        };
        let mut lines: Vec<Line<'_>> = Vec::new();
        if let Some(dn) = &u.display_name {
            lines.push(Line::from(Span::styled(dn.clone(), theme.accent_style())));
        }
        lines.push(Line::from(Span::styled(
            format!("@{}", u.username),
            theme.muted_style(),
        )));
        if let Some(bio) = &u.bio {
            lines.push(Line::from(""));
            for line in bio.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), theme.base())));
            }
        }
        lines.push(Line::from(""));
        let counts = format!(
            "{} posts · {} followers · {} following",
            u.posts_count.unwrap_or(0),
            u.followers_count.unwrap_or(0),
            u.following_count.unwrap_or(0),
        );
        lines.push(Line::from(Span::styled(counts, theme.muted_style())));
        // The guild badge, which § Get User Profile says these fields describe:
        // the ONE guild held as founder or member. Apprenticeships are not here,
        // they are on the Guilds tab, so this line is the badge and nothing more.
        if let Some(name) = u.guild_name.as_deref().filter(|n| !n.trim().is_empty()) {
            // `guild_icon` is deliberately not drawn: the API's icon is an icon
            // IDENTIFIER (e.g. "arrows-maximize"), not a glyph, the same reading
            // the guilds index, the guild detail header and the Guilds tab all
            // take. Rendering it here would print that identifier as text.
            lines.push(Line::from(Span::styled(
                format!("guild: {name}"),
                theme.muted_style(),
            )));
        }
        if let Some(loc) = &u.location_name {
            lines.push(Line::from(Span::styled(
                format!("📍 {loc}"),
                theme.muted_style(),
            )));
        }
        if let Some(url) = &u.website_url {
            let label = u.website_name.as_deref().unwrap_or(url.as_str());
            lines.push(Line::from(Span::styled(
                format!("🔗 {label} ({url})"),
                theme.muted_style(),
            )));
        }
        if let Some(pinned) = &u.pinned_post_id {
            lines.push(Line::from(Span::styled(
                format!("📌 pinned: {pinned}"),
                theme.muted_style(),
            )));
        }
        // `is_viewing_self`, not `is_self`: the key handlers and the status line
        // already gate on it, and a profile reached by NAME can be your own, in
        // which case every key promised below would be refused server-side.
        if !self.is_viewing_self() {
            lines.push(Line::from(""));
            let txt = match u.is_following {
                Some(true) => "F to unfollow",
                Some(false) => "F to follow",
                None => "F to toggle follow",
            };
            lines.push(Line::from(Span::styled(txt, theme.accent_style())));
            // Spell the budget out: § Poke a User allows 1/hour and 8/day for
            // every user combined, so a reader who does not know that will read
            // the refusal as a bug rather than as the cap.
            lines.push(Line::from(Span::styled(
                "P to poke (1/hour, 8/day across everyone)",
                theme.accent_style(),
            )));
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "e to edit your profile",
                theme.accent_style(),
            )));
        }
        super::hyperlink::render_linked_paragraph(
            frame,
            area,
            lines,
            0,
            crate::config::get().hyperlinks,
        );
    }

    fn render_posts(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let pinned = self.user.as_ref().and_then(|u| u.pinned_post_id.clone());
        render_list_with_state(
            frame,
            area,
            theme,
            &self.posts,
            "posts",
            move |e: &Entry| {
                let when = e
                    .created_at
                    .map(crate::config::format_list_timestamp)
                    .unwrap_or_default();
                let mut header = vec![
                    Span::styled(format!("@{}", e.author_username), theme.accent_style()),
                    Span::styled(format!(" · {when}"), theme.muted_style()),
                ];
                let edited = super::text::edited_marker(e.edited_at);
                if !edited.is_empty() {
                    header.push(Span::styled(edited, theme.muted_style()));
                }
                if pinned.as_deref() == Some(e.post_id.as_str()) {
                    header.push(Span::styled(" · 📌 pinned", theme.warning_style()));
                }
                vec![
                    Line::from(header),
                    Line::from(Span::styled(
                        super::text::first_line_truncated(&e.content, 160),
                        theme.base(),
                    )),
                    Line::from(""),
                ]
            },
        );
    }

    fn render_replies(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        render_list_with_state(frame, area, theme, &self.replies, "replies", |r: &Reply| {
            let when = r
                .created_at
                .map(crate::config::format_list_timestamp)
                .unwrap_or_default();
            let mut header = vec![
                Span::styled(format!("@{}", r.author_username), theme.accent_style()),
                Span::styled(format!(" · {when} · on {}", r.post_id), theme.muted_style()),
            ];
            let edited = super::text::edited_marker(r.edited_at);
            if !edited.is_empty() {
                header.push(Span::styled(edited, theme.muted_style()));
            }
            vec![
                Line::from(header),
                Line::from(Span::styled(
                    super::text::first_line_truncated(&r.content, 160),
                    theme.base(),
                )),
                Line::from(""),
            ]
        });
    }

    fn render_followers(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        render_list_with_state(
            frame,
            area,
            theme,
            &self.followers,
            "followers",
            |f: &Follow| vec![follow_row(&f.follower_username, &f.follower_id, theme)],
        );
    }

    fn render_following(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        render_list_with_state(
            frame,
            area,
            theme,
            &self.following,
            "following",
            |f: &Follow| vec![follow_row(&f.followed_username, &f.followed_id, theme)],
        );
    }

    fn render_guilds(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        render_list_with_state(
            frame,
            area,
            theme,
            &self.guilds,
            "guilds",
            |g: &UserGuild| vec![guild_row(g, theme)],
        );
    }

    fn render_status(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut parts: Vec<String> = vec![];
        if self.follow_action_pending {
            parts.push("follow pending…".into());
        }
        if self.poke_pending {
            parts.push("poke pending…".into());
        }
        parts.push("tab/shift+tab tabs".into());
        let nav_hint = if self.is_root {
            "backspace quit · esc menu"
        } else {
            "backspace back · esc back"
        };
        parts.push(nav_hint.into());
        // Account-level action — works on every tab, so always surface it.
        // Gated on `is_viewing_self` rather than `is_self`: following, messaging
        // and poking yourself all fail server-side, and a profile reached by
        // name (a search hit, a followers row) can perfectly well be your own.
        if self.is_self {
            parts.push("e edit".into());
        } else if self.user.is_some() && !self.is_viewing_self() {
            parts.push("F follow/unfollow".into());
            parts.push("m message".into());
            parts.push("P poke".into());
        }
        // List actions only apply on the list tabs.
        if self.tab == ProfileTab::Posts && self.is_self {
            parts.push("enter open · E edit · P pin · scroll for more · r refresh".into());
        } else if self.tab == ProfileTab::Guilds {
            // No "scroll for more" here: six rows is the ceiling and there is
            // no cursor, so the whole tab is always already on screen.
            parts.push("enter open · r refresh".into());
        } else if self.tab != ProfileTab::Info {
            // Someone else's Posts tab reads like any other list tab: `E` and
            // `P` are yours-only, so neither is advertised here.
            parts.push("enter open · scroll for more · r refresh".into());
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                parts.join(" · "),
                theme.muted_style(),
            ))),
            area,
        );
    }
}

#[derive(Clone, Copy)]
enum ListTarget {
    Posts,
    Replies,
    Followers,
    Following,
    Guilds,
}

/// Render one row of the Guilds tab: the guild, the role held in it, and, for
/// the badge guild, that it is the one on the profile.
///
/// Only the badge guild is marked. The list is ordered badge-first, but that is
/// invisible once it is on screen, and the role alone doesn't say which of
/// founder/member/apprentice carries the badge, which is the whole distinction
/// v0.8.6 § Guilds draws.
///
/// The guild's `icon` is an icon *identifier* rather than a glyph, the same as
/// on the guilds index, so it is not rendered as text.
fn guild_row(g: &UserGuild, theme: &Theme) -> Line<'static> {
    let name = g.name.trim();
    let head = if !name.is_empty() {
        name.to_string()
    } else if !g.slug.is_empty() {
        format!("#{}", g.slug)
    } else {
        "(unknown guild)".to_string()
    };
    let mut spans = vec![Span::styled(head, theme.accent_style())];

    let mut detail: Vec<String> = Vec::new();
    if !name.is_empty() && !g.slug.is_empty() {
        detail.push(format!("#{}", g.slug));
    }
    // The word for a role is the guilds module's, so a role reads the same here
    // as it does on the guild's own roster.
    if let Some(role) = super::guilds::role_label(g.role) {
        detail.push(role.to_string());
    }
    if let Some(joined) = g.joined_at {
        detail.push(format!(
            "joined {}",
            crate::config::format_list_timestamp(joined)
        ));
    }
    if !detail.is_empty() {
        spans.push(Span::styled(
            format!("  {}", detail.join(" · ")),
            theme.muted_style(),
        ));
    }
    if g.is_badge() {
        spans.push(Span::styled(" · profile badge", theme.warning_style()));
    }
    Line::from(spans)
}

/// Render one follower/following row. The follows API (v0.8.4) returns only
/// user IDs, not `followerUsername`/`followedUsername`, so those fields decode
/// to empty strings. When the username is missing, fall back to a truncated
/// user ID in a muted style rather than printing a bare "@".
fn follow_row(username: &str, user_id: &str, theme: &Theme) -> Line<'static> {
    if username.is_empty() {
        let id = super::text::truncate_to_width(user_id, 18);
        let shown = if id.is_empty() {
            "(unknown user)".to_string()
        } else {
            id
        };
        Line::from(Span::styled(shown, theme.muted_style()))
    } else {
        Line::from(Span::styled(format!("@{username}"), theme.accent_style()))
    }
}

fn render_list_with_state<T, F>(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    state: &TabState<T>,
    empty_label: &str,
    item_lines: F,
) where
    F: Fn(&T) -> Vec<Line<'static>>,
{
    let visible: Vec<usize> = (0..state.items.len()).collect();
    let empty = format!("no {empty_label}");
    super::list::render_body(frame, area, theme, state, &visible, &empty, |t| {
        ListItem::new(item_lines(t))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use time::OffsetDateTime;
    // Only the fixtures name a role: the rows themselves go through the guilds
    // module's shared label.
    use cs_api::GuildRole;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn user(name: &str) -> User {
        User {
            id: "u".into(),
            username: name.into(),
            guild_id: None,
            guild_slug: None,
            guild_icon: None,
            guild_name: None,
            display_name: None,
            email: None,
            bio: None,
            pinned_post_id: None,
            website_url: None,
            website_name: None,
            website_image_url: None,
            location_latitude: None,
            location_longitude: None,
            location_name: None,
            followers_count: None,
            following_count: None,
            posts_count: None,
            is_following: None,
            follow_id: None,
            created_at: None,
        }
    }

    fn profile_entry(post_id: &str) -> Entry {
        Entry {
            post_id: post_id.into(),
            author_id: "u".into(),
            author_username: "me".into(),
            content: "hi".into(),
            title: None,
            slug: None,
            topics: vec![],
            replies_count: 0,
            bookmarks_count: 0,
            is_public: true,
            is_nsfw: false,
            attachments: vec![],
            created_at: None,
            edited_at: None,
            deleted: false,
        }
    }

    fn user_guild(slug: &str, role: GuildRole) -> UserGuild {
        UserGuild {
            guild_id: format!("g-{slug}"),
            slug: slug.into(),
            name: format!("Guild {slug}"),
            role: Some(role),
            ..UserGuild::default()
        }
    }

    fn profile_reply(reply_id: &str) -> Reply {
        Reply {
            reply_id: reply_id.into(),
            post_id: "p1".into(),
            author_id: "u".into(),
            author_username: "me".into(),
            content: "hi back".into(),
            parent_reply_id: None,
            attachments: vec![],
            created_at: None,
            edited_at: None,
            deleted: false,
        }
    }

    fn render_to_string(s: &ProfileScreen, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber()))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn m_messages_another_user() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('m'))),
            ProfileIntent::MessageUser {
                username: "bob".into(),
                user_id: Some("u".into()),
            }
        );
    }

    #[test]
    fn m_does_nothing_on_own_profile() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        assert_eq!(s.handle_key(key(KeyCode::Char('m'))), ProfileIntent::None);
    }

    #[test]
    fn capital_p_pins_an_unpinned_post_on_own_posts_tab() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me"))); // pinned_post_id = None
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Posts;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('P'))),
            ProfileIntent::PinPost {
                post_id: "p1".into(),
                pin: true,
            }
        );
    }

    #[test]
    fn render_makes_the_website_url_clickable() {
        let mut s = ProfileScreen::new_own();
        let mut u = user("me");
        u.website_url = Some("https://example.com".into());
        s.apply_user(Ok(u));
        s.tab = ProfileTab::Info;

        let backend = ratatui::backend::TestBackend::new(60, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber()))
            .expect("draw");

        let buf = terminal.backend().buffer();
        let linked = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| {
                buf[(x, y)]
                    .symbol()
                    .contains("\u{1b}]8;;https://example.com\u{1b}\\")
            })
        });
        assert!(linked, "the profile website URL is an OSC 8 hyperlink");
    }

    #[test]
    fn capital_p_unpins_the_currently_pinned_post() {
        let mut s = ProfileScreen::new_own();
        let mut u = user("me");
        u.pinned_post_id = Some("p1".into());
        s.apply_user(Ok(u));
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Posts;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('P'))),
            ProfileIntent::PinPost {
                post_id: "p1".into(),
                pin: false,
            }
        );
    }

    #[test]
    fn capital_p_pokes_instead_of_pinning_on_another_users_posts_tab() {
        // Pinning is a self-only action, so on someone else's Posts tab the
        // same key is the poke from § Poke a User.
        let mut s = ProfileScreen::new_for("bob".into()); // not self
        s.apply_user(Ok(user("bob")));
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Posts;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('P'))),
            ProfileIntent::PokeUser {
                username: "bob".into()
            }
        );
    }

    #[test]
    fn own_profile_starts_on_info_tab() {
        let s = ProfileScreen::new_own();
        assert!(s.is_self);
        assert!(s.is_root);
        assert_eq!(s.tab, ProfileTab::Info);
    }

    #[test]
    fn other_profile_is_not_root() {
        let s = ProfileScreen::new_for("bob".into());
        assert!(!s.is_self);
        assert!(!s.is_root);
    }

    #[test]
    fn backspace_quits_on_root_back_on_pushed() {
        let mut own = ProfileScreen::new_own();
        assert_eq!(own.handle_key(key(KeyCode::Backspace)), ProfileIntent::Quit);

        let mut other = ProfileScreen::new_for("bob".into());
        assert_eq!(
            other.handle_key(key(KeyCode::Backspace)),
            ProfileIntent::Back
        );
    }

    #[test]
    fn h_and_l_cycle_tabs() {
        let mut s = ProfileScreen::new_own();
        s.handle_key(key(KeyCode::Char('l')));
        assert_eq!(s.tab, ProfileTab::Posts);
        s.handle_key(key(KeyCode::Char('l')));
        assert_eq!(s.tab, ProfileTab::Replies);
        s.handle_key(key(KeyCode::Char('h')));
        assert_eq!(s.tab, ProfileTab::Posts);
    }

    #[test]
    fn shift_number_picks_tab() {
        let mut s = ProfileScreen::new_own();
        s.handle_key(key(KeyCode::Char('@')));
        assert_eq!(s.tab, ProfileTab::Posts);
        s.handle_key(key(KeyCode::Char('%')));
        assert_eq!(s.tab, ProfileTab::Following);
    }

    #[test]
    fn tab_and_shift_tab_cycle_tabs() {
        let mut s = ProfileScreen::new_own();
        s.handle_key(key(KeyCode::Tab));
        assert_eq!(s.tab, ProfileTab::Posts);
        s.handle_key(key(KeyCode::Tab));
        assert_eq!(s.tab, ProfileTab::Replies);
        s.handle_key(key(KeyCode::BackTab));
        assert_eq!(s.tab, ProfileTab::Posts);
    }

    #[test]
    fn j_at_bottom_auto_loads_current_tab() {
        let mut s = ProfileScreen::new_own();
        s.tab = ProfileTab::Posts;
        s.posts.loading = false;
        s.posts.next_cursor = Some("next".into());
        // At the bottom of the active tab with more available, j paginates it.
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, ProfileIntent::LoadMoreCurrentTab);
    }

    #[test]
    fn capital_f_toggles_follow_when_other_user() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('F'))),
            ProfileIntent::ToggleFollow
        );
    }

    #[test]
    fn capital_f_does_nothing_on_own_profile() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        assert_eq!(s.handle_key(key(KeyCode::Char('F'))), ProfileIntent::None);
    }

    #[test]
    fn e_triggers_edit_only_on_self() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'))),
            ProfileIntent::EditOwnProfile
        );

        let mut other = ProfileScreen::new_for("bob".into());
        other.apply_user(Ok(user("bob")));
        assert_eq!(
            other.handle_key(key(KeyCode::Char('e'))),
            ProfileIntent::None
        );
    }

    #[test]
    fn enter_on_posts_emits_open_post() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.tab = ProfileTab::Posts;
        s.posts.items = vec![Entry {
            post_id: "p1".into(),
            author_id: "u".into(),
            author_username: "me".into(),
            content: "x".into(),
            title: None,
            slug: None,
            topics: vec![],
            replies_count: 0,
            bookmarks_count: 0,
            is_public: false,
            is_nsfw: false,
            attachments: vec![],
            created_at: None,
            edited_at: None,
            deleted: false,
        }];
        s.posts.loading = false;
        s.posts.loaded = true;
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            ProfileIntent::OpenPost {
                post_id: "p1".into()
            }
        );
    }

    #[test]
    fn enter_on_following_opens_user() {
        let mut s = ProfileScreen::new_for("alice".into());
        s.apply_user(Ok(user("alice")));
        s.tab = ProfileTab::Following;
        s.following.items = vec![Follow {
            follow_id: "f1".into(),
            follower_id: "u1".into(),
            followed_id: "u2".into(),
            follower_username: "alice".into(),
            followed_username: "bob".into(),
            created_at: None,
        }];
        s.following.loading = false;
        s.following.loaded = true;
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            ProfileIntent::OpenUser {
                username: "bob".into()
            }
        );
    }

    #[test]
    fn enter_on_following_without_username_is_noop() {
        // The follows API returns IDs only (no usernames). Enter must not try
        // to open a profile with an empty username.
        let mut s = ProfileScreen::new_for("alice".into());
        s.apply_user(Ok(user("alice")));
        s.tab = ProfileTab::Following;
        s.following.items = vec![Follow {
            follow_id: "f1".into(),
            follower_id: "u1".into(),
            followed_id: "u2".into(),
            follower_username: String::new(),
            followed_username: String::new(),
            created_at: None,
        }];
        s.following.loading = false;
        s.following.loaded = true;
        assert_eq!(s.handle_key(key(KeyCode::Enter)), ProfileIntent::None);
    }

    #[test]
    fn follow_row_falls_back_to_id_without_username() {
        let theme = Theme::default();
        // Username present -> rendered as "@name".
        let with_name = follow_row("bob", "u2", &theme);
        assert_eq!(with_name.spans[0].content.as_ref(), "@bob");
        // Username missing -> truncated id, not a bare "@".
        let no_name = follow_row("", "user-id-1234", &theme);
        assert_eq!(no_name.spans[0].content.as_ref(), "user-id-1234");
        // Neither username nor id -> explicit placeholder.
        let empty = follow_row("", "", &theme);
        assert_eq!(empty.spans[0].content.as_ref(), "(unknown user)");
    }

    #[test]
    fn apply_user_sets_username_when_none() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        assert_eq!(s.username.as_deref(), Some("me"));
    }

    #[test]
    fn capital_p_pokes_another_user() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('P'))),
            ProfileIntent::PokeUser {
                username: "bob".into()
            }
        );
    }

    #[test]
    fn capital_p_pokes_the_handle_the_server_confirmed() {
        // The screen was opened with one spelling; the loaded profile is the
        // authority on the handle the poke is addressed to.
        let mut s = ProfileScreen::new_for("BOB".into());
        s.apply_user(Ok(user("bob")));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('P'))),
            ProfileIntent::PokeUser {
                username: "bob".into()
            }
        );
    }

    #[test]
    fn capital_p_never_pokes_yourself() {
        // § Poke a User makes a self-poke a 400, so it is not offered at all.
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        assert_eq!(s.handle_key(key(KeyCode::Char('P'))), ProfileIntent::None);
    }

    #[test]
    fn capital_p_waits_for_the_profile_to_load() {
        let mut s = ProfileScreen::new_for("bob".into());
        assert_eq!(s.handle_key(key(KeyCode::Char('P'))), ProfileIntent::None);
    }

    #[test]
    fn capital_e_edits_the_selected_post() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.posts
            .apply_initial(Ok((vec![profile_entry("p1"), profile_entry("p2")], None)));
        s.posts.selected = 1;
        s.tab = ProfileTab::Posts;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('E'))),
            ProfileIntent::EditEntry {
                post_id: "p2".into(),
                content: "hi".into(),
            }
        );
    }

    #[test]
    fn capital_e_is_not_offered_on_someone_elses_post() {
        // Supporter status and the five-minute window stay server-side checks,
        // but every entry on another person's Posts tab is definitively theirs,
        // so the key would promise an action that can only come back `403`.
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Posts;
        assert_eq!(s.handle_key(key(KeyCode::Char('E'))), ProfileIntent::None);
    }

    #[test]
    fn capital_e_is_ignored_off_the_posts_tab() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Replies;
        assert_eq!(s.handle_key(key(KeyCode::Char('E'))), ProfileIntent::None);
    }

    #[test]
    fn capital_e_with_an_empty_posts_tab_is_a_noop() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.tab = ProfileTab::Posts;
        assert_eq!(s.handle_key(key(KeyCode::Char('E'))), ProfileIntent::None);
    }

    #[test]
    fn lowercase_e_still_edits_your_profile_from_the_posts_tab() {
        // The two edits are one keystroke apart, so the old binding has to
        // survive the new one on the tab they share.
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Posts;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'))),
            ProfileIntent::EditOwnProfile
        );
    }

    #[test]
    fn posts_tab_marks_an_edited_entry() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        let mut edited = profile_entry("p1");
        edited.edited_at = Some(OffsetDateTime::now_utc());
        s.posts.apply_initial(Ok((vec![edited], None)));
        s.tab = ProfileTab::Posts;
        assert!(
            render_to_string(&s, 70, 12).contains("(edited)"),
            "an entry with editedAt set is marked as edited"
        );
    }

    #[test]
    fn posts_tab_leaves_an_untouched_entry_unmarked() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Posts;
        assert!(
            !render_to_string(&s, 70, 12).contains("(edited)"),
            "an entry without editedAt carries no marker"
        );
    }

    #[test]
    fn replies_tab_marks_an_edited_reply() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        let mut edited = profile_reply("r1");
        edited.edited_at = Some(OffsetDateTime::now_utc());
        s.replies.apply_initial(Ok((vec![edited], None)));
        s.tab = ProfileTab::Replies;
        assert!(
            render_to_string(&s, 70, 12).contains("(edited)"),
            "a reply with editedAt set is marked as edited"
        );
    }

    #[test]
    fn replies_tab_leaves_an_untouched_reply_unmarked() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.replies
            .apply_initial(Ok((vec![profile_reply("r1")], None)));
        s.tab = ProfileTab::Replies;
        assert!(
            !render_to_string(&s, 70, 12).contains("(edited)"),
            "a reply without editedAt carries no marker"
        );
    }

    #[test]
    fn apply_edited_entry_swaps_the_matching_row() {
        let mut s = ProfileScreen::new_own();
        s.posts
            .apply_initial(Ok((vec![profile_entry("p1"), profile_entry("p2")], None)));
        let mut fresh = profile_entry("p2");
        fresh.content = "corrected".into();
        fresh.edited_at = Some(OffsetDateTime::now_utc());
        assert!(s.apply_edited_entry(fresh));
        assert_eq!(s.posts.items[0].content, "hi", "other rows are untouched");
        assert_eq!(s.posts.items[1].content, "corrected");
        assert!(s.posts.items[1].edited_at.is_some());
    }

    #[test]
    fn apply_edited_entry_reports_a_miss() {
        let mut s = ProfileScreen::new_own();
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        assert!(!s.apply_edited_entry(profile_entry("gone")));
        assert_eq!(s.posts.items.len(), 1);
    }

    #[test]
    fn status_line_offers_poke_on_another_profile_only() {
        let mut other = ProfileScreen::new_for("bob".into());
        other.apply_user(Ok(user("bob")));
        assert!(render_to_string(&other, 160, 12).contains("P poke"));

        let mut own = ProfileScreen::new_own();
        own.apply_user(Ok(user("me")));
        assert!(!render_to_string(&own, 160, 12).contains("P poke"));
    }

    #[test]
    fn your_own_profile_reached_by_name_offers_no_self_directed_actions() {
        // Regression: `is_self` is only true for the ROOT profile, so opening
        // yourself from a search hit or from a row in someone's followers list
        // used to advertise and fire follow, message and poke against your own
        // account, each a guaranteed server-side refusal (§ Poke a User:
        // "Poking yourself returns 400").
        let mut me = ProfileScreen::new_for("me".into());
        me.viewer_user_id = Some("u".into());
        me.apply_user(Ok(user("me")));
        assert!(me.is_viewing_self(), "same id means it is my own profile");

        let text = render_to_string(&me, 160, 12);
        assert!(!text.contains("P poke"), "{text:?}");
        assert!(!text.contains("F follow/unfollow"), "{text:?}");
        assert!(!text.contains("m message"), "{text:?}");
        assert_eq!(
            me.handle_key(key(KeyCode::Char('P'))),
            ProfileIntent::None,
            "poking yourself must not even be attempted",
        );

        // A different account is still fully actionable.
        let mut bob = ProfileScreen::new_for("bob".into());
        bob.viewer_user_id = Some("someone-else".into());
        bob.apply_user(Ok(user("bob")));
        assert!(!bob.is_viewing_self());
        assert!(render_to_string(&bob, 160, 12).contains("P poke"));
    }

    #[test]
    fn status_line_offers_edit_on_your_own_posts_tab_only() {
        let mut own = ProfileScreen::new_own();
        own.apply_user(Ok(user("me")));
        own.tab = ProfileTab::Posts;
        assert!(render_to_string(&own, 160, 12).contains("E edit"));

        let mut other = ProfileScreen::new_for("bob".into());
        other.apply_user(Ok(user("bob")));
        other.tab = ProfileTab::Posts;
        assert!(
            !render_to_string(&other, 160, 12).contains("E edit"),
            "the hint tracks the binding, which is your own posts only"
        );
    }

    #[test]
    fn status_line_shows_a_poke_in_flight() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        assert!(!render_to_string(&s, 160, 12).contains("poke pending"));
        s.poke_pending = true;
        assert!(render_to_string(&s, 160, 12).contains("poke pending"));
    }

    #[test]
    fn shifted_six_selects_the_guilds_tab() {
        let mut s = ProfileScreen::new_own();
        assert_eq!(
            s.handle_key(key(KeyCode::Char('^'))),
            ProfileIntent::SelectTab(ProfileTab::Guilds)
        );
        assert_eq!(s.tab, ProfileTab::Guilds);
    }

    #[test]
    fn guilds_is_the_last_tab_in_the_cycle() {
        let mut s = ProfileScreen::new_own();
        // h from the first tab wraps backwards onto the last one.
        s.handle_key(key(KeyCode::Char('h')));
        assert_eq!(s.tab, ProfileTab::Guilds);
        s.handle_key(key(KeyCode::Char('l')));
        assert_eq!(s.tab, ProfileTab::Info);
    }

    #[test]
    fn apply_guilds_fills_the_tab_and_leaves_no_cursor() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_guilds(Ok(vec![
            user_guild("owls", GuildRole::Member),
            user_guild("divers", GuildRole::Apprentice),
        ]));
        assert!(s.guilds.loaded);
        assert!(!s.guilds.loading);
        assert_eq!(s.guilds.items.len(), 2);
        assert!(
            s.guilds.next_cursor.is_none(),
            "§ List a User's Guilds has no pagination, so nothing may imply a second page"
        );
    }

    #[test]
    fn apply_guilds_surfaces_a_failure() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_guilds(Err("api NotFound (404): no such user".into()));
        assert!(s.guilds.error.is_some());
        assert!(s.guilds.items.is_empty());
        assert!(!s.guilds.loading);
    }

    #[test]
    fn the_guilds_tab_never_asks_for_another_page() {
        let mut s = ProfileScreen::new_own();
        s.tab = ProfileTab::Guilds;
        s.apply_guilds(Ok(vec![user_guild("owls", GuildRole::Founder)]));
        // Sitting on the last row is where every other tab pulls the next page.
        assert_eq!(s.handle_key(key(KeyCode::Char('j'))), ProfileIntent::None);
        assert_eq!(s.handle_key(key(KeyCode::Char('n'))), ProfileIntent::None);
        assert_eq!(s.handle_key(key(KeyCode::PageDown)), ProfileIntent::None);
    }

    #[test]
    fn r_refreshes_the_guilds_tab() {
        let mut s = ProfileScreen::new_own();
        s.tab = ProfileTab::Guilds;
        s.apply_guilds(Ok(vec![user_guild("owls", GuildRole::Founder)]));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('r'))),
            ProfileIntent::RefreshCurrentTab
        );
    }

    #[test]
    fn enter_on_the_guilds_tab_opens_the_selected_guild() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        s.tab = ProfileTab::Guilds;
        s.apply_guilds(Ok(vec![
            user_guild("owls", GuildRole::Member),
            user_guild("divers", GuildRole::Apprentice),
        ]));
        s.guilds.selected = 1;
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            ProfileIntent::OpenGuild {
                slug: "divers".into()
            },
            "an apprenticeship opens the same way the badge guild does"
        );
    }

    #[test]
    fn enter_on_a_guild_row_without_a_slug_is_a_noop() {
        // Every guild route is addressed by slug, so a row that decoded without
        // one would request `/v1/guilds/` and fail.
        let mut s = ProfileScreen::new_for("bob".into());
        s.tab = ProfileTab::Guilds;
        s.apply_guilds(Ok(vec![UserGuild::default()]));
        assert_eq!(s.handle_key(key(KeyCode::Enter)), ProfileIntent::None);
    }

    #[test]
    fn the_guilds_tab_shows_every_guild_with_its_role() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        s.tab = ProfileTab::Guilds;
        s.apply_guilds(Ok(vec![
            user_guild("owls", GuildRole::Member),
            user_guild("divers", GuildRole::Apprentice),
        ]));
        let text = render_to_string(&s, 100, 14);
        assert!(text.contains("Guild owls"), "{text:?}");
        assert!(text.contains("#owls"), "{text:?}");
        assert!(text.contains("member"), "{text:?}");
        // The apprenticeship is listed too: it is not on the profile badge, but
        // the user is in the guild.
        assert!(text.contains("Guild divers"), "{text:?}");
        assert!(text.contains("apprentice"), "{text:?}");
        assert_eq!(
            text.matches("profile badge").count(),
            1,
            "exactly one guild carries the badge: {text:?}"
        );
    }

    #[test]
    fn an_empty_guilds_tab_says_so() {
        let mut s = ProfileScreen::new_for("bob".into());
        s.apply_user(Ok(user("bob")));
        s.tab = ProfileTab::Guilds;
        s.apply_guilds(Ok(vec![]));
        assert!(render_to_string(&s, 100, 14).contains("no guilds"));
    }

    #[test]
    fn the_guilds_tab_status_line_promises_no_further_pages() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        s.tab = ProfileTab::Guilds;
        s.apply_guilds(Ok(vec![user_guild("owls", GuildRole::Founder)]));
        let text = render_to_string(&s, 160, 12);
        assert!(text.contains("enter open"), "{text:?}");
        assert!(
            !text.contains("scroll for more"),
            "the tab cannot page, so it must not offer to: {text:?}"
        );
    }

    #[test]
    fn an_unmodelled_role_is_never_labelled_as_member() {
        // v0.8.6 added `apprentice` to a founder/member vocabulary. Guessing at
        // a role the client doesn't know is how the next addition would show up
        // wearing the wrong word. The rule lives in the guilds module, which
        // pins the vocabulary; this holds the row to it.
        let row = guild_row(
            &UserGuild {
                slug: "owls".into(),
                role: Some(GuildRole::Unknown),
                ..Default::default()
            },
            &Theme::default(),
        );
        let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !text.contains("member"),
            "an unmodelled role must not be dressed up as one: {text:?}"
        );
    }

    #[test]
    fn guild_row_marks_the_badge_guild_only() {
        let theme = Theme::default();
        let row = |g: &UserGuild| -> String {
            guild_row(g, &theme)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert!(row(&user_guild("owls", GuildRole::Founder)).contains("profile badge"));
        assert!(row(&user_guild("owls", GuildRole::Member)).contains("profile badge"));
        assert!(!row(&user_guild("divers", GuildRole::Apprentice)).contains("profile badge"));
        assert!(!row(&UserGuild::default()).contains("profile badge"));
    }

    #[test]
    fn guild_row_names_a_guild_that_arrived_without_a_name() {
        let theme = Theme::default();
        let slug_only = UserGuild {
            slug: "owls".into(),
            ..UserGuild::default()
        };
        assert_eq!(
            guild_row(&slug_only, &theme).spans[0].content.as_ref(),
            "#owls"
        );
        assert_eq!(
            guild_row(&UserGuild::default(), &theme).spans[0]
                .content
                .as_ref(),
            "(unknown guild)"
        );
    }

    #[test]
    fn guild_row_dates_the_membership_when_the_server_sent_one() {
        let theme = Theme::default();
        let mut g = user_guild("owls", GuildRole::Member);
        assert!(
            !guild_row(&g, &theme)
                .spans
                .iter()
                .any(|s| s.content.contains("joined")),
            "no joinedAt, nothing to date"
        );
        g.joined_at = Some(OffsetDateTime::now_utc());
        let dated: String = guild_row(&g, &theme)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(dated.contains("· joined "), "{dated:?}");
    }
}

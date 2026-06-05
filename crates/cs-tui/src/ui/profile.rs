//! Profile screen — 5 tabs (Info, Posts, Replies, Followers, Following).
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Entry, Follow, Reply, User};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use time::OffsetDateTime;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileTab {
    Info,
    Posts,
    Replies,
    Followers,
    Following,
}

impl ProfileTab {
    pub const ALL: [ProfileTab; 5] = [
        Self::Info,
        Self::Posts,
        Self::Replies,
        Self::Followers,
        Self::Following,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Posts => "posts",
            Self::Replies => "replies",
            Self::Followers => "followers",
            Self::Following => "following",
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
    /// Enter edit mode (only meaningful when viewing self).
    EditOwnProfile,
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

    pub follow_action_pending: bool,
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
            follow_action_pending: false,
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
            KeyCode::Char('F') if !self.is_self && self.user.is_some() => {
                return ProfileIntent::ToggleFollow;
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
            _ => {}
        }

        match self.tab {
            ProfileTab::Info => ProfileIntent::None,
            ProfileTab::Posts => self.handle_list_key(key, ListTarget::Posts),
            ProfileTab::Replies => self.handle_list_key(key, ListTarget::Replies),
            ProfileTab::Followers => self.handle_list_key(key, ListTarget::Followers),
            ProfileTab::Following => self.handle_list_key(key, ListTarget::Following),
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
            ListTarget::Followers => self
                .followers
                .items
                .get(self.followers.selected)
                .map(|f| ProfileIntent::OpenUser {
                    username: f.follower_username.clone(),
                })
                .unwrap_or(ProfileIntent::None),
            ListTarget::Following => self
                .following
                .items
                .get(self.following.selected)
                .map(|f| ProfileIntent::OpenUser {
                    username: f.followed_username.clone(),
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
            .title(Span::styled(title, theme.accent_style()));
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
        if !self.is_self {
            lines.push(Line::from(""));
            let txt = match u.is_following {
                Some(true) => "F to unfollow",
                Some(false) => "F to follow",
                None => "F to toggle follow",
            };
            lines.push(Line::from(Span::styled(txt, theme.accent_style())));
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "e to edit your profile",
                theme.accent_style(),
            )));
        }
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(para, area);
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
                let when = e.created_at.map(format_relative).unwrap_or_default();
                let mut header = vec![
                    Span::styled(format!("@{}", e.author_username), theme.accent_style()),
                    Span::styled(format!(" · {when}"), theme.muted_style()),
                ];
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
            let when = r.created_at.map(format_relative).unwrap_or_default();
            vec![
                Line::from(vec![
                    Span::styled(format!("@{}", r.author_username), theme.accent_style()),
                    Span::styled(format!(" · {when} · on {}", r.post_id), theme.muted_style()),
                ]),
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
            |f: &Follow| {
                vec![Line::from(Span::styled(
                    format!("@{}", f.follower_username),
                    theme.accent_style(),
                ))]
            },
        );
    }

    fn render_following(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        render_list_with_state(
            frame,
            area,
            theme,
            &self.following,
            "following",
            |f: &Follow| {
                vec![Line::from(Span::styled(
                    format!("@{}", f.followed_username),
                    theme.accent_style(),
                ))]
            },
        );
    }

    fn render_status(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut parts: Vec<String> = vec![];
        if self.follow_action_pending {
            parts.push("follow pending…".into());
        }
        parts.push("tab tabs".into());
        let nav_hint = if self.is_root {
            "backspace quit · esc menu"
        } else {
            "backspace back · esc back"
        };
        parts.push(nav_hint.into());
        // Account-level action — works on every tab, so always surface it.
        if self.is_self {
            parts.push("e edit".into());
        } else if self.user.is_some() {
            parts.push("F follow/unfollow".into());
        }
        // List actions only apply on the list tabs.
        if self.tab == ProfileTab::Posts && self.is_self {
            parts.push("enter open · P pin · scroll for more · r refresh".into());
        } else if self.tab != ProfileTab::Info {
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

fn format_relative(t: OffsetDateTime) -> String {
    let now = OffsetDateTime::now_utc();
    let secs = (now - t).whole_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else if secs < 30 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else {
        let d = t.date();
        format!("{}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
    }
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

    fn user(name: &str) -> User {
        User {
            id: "u".into(),
            username: name.into(),
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
            deleted: false,
        }
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
    fn capital_p_does_nothing_on_other_users_profile() {
        let mut s = ProfileScreen::new_for("bob".into()); // not self
        s.apply_user(Ok(user("bob")));
        s.posts.apply_initial(Ok((vec![profile_entry("p1")], None)));
        s.tab = ProfileTab::Posts;
        assert_eq!(s.handle_key(key(KeyCode::Char('P'))), ProfileIntent::None);
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
    fn apply_user_sets_username_when_none() {
        let mut s = ProfileScreen::new_own();
        s.apply_user(Ok(user("me")));
        assert_eq!(s.username.as_deref(), Some("me"));
    }
}

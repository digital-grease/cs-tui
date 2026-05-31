//! Top-level App state and event loop.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use cs_api::{
    ApiError, Bookmark, Client, Entry, Follow, FollowsDirection, Guild, GuildMembership,
    GuildThread, JoinedGuild, Note, NoteRevision, Notification, NotificationsFilter, ProfileUpdate,
    Reply, Settings, SettingsUpdate, Topic, User,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use tokio::sync::{mpsc, Notify};
use tokio::time::MissedTickBehavior;

use super::bookmarks::{BookmarksIntent, BookmarksScreen};
use super::compose::{launch_editor, ComposeIntent, ComposeKind, ComposeScreen};
use super::edit_profile::{EditProfileIntent, EditProfileScreen};
use super::feed::{FeedIntent, FeedScreen};
use super::guild_detail::{GuildIntent, GuildScreen, GuildTab};
use super::guilds::{GuildsIntent, GuildsScreen};
use super::journal::{JournalIntent, JournalScreen};
use super::login::{LoginIntent, LoginScreen};
use super::menu::{MenuIntent, MenuOverlay};
use super::nav::{render_tab_bar, RootKind};
use super::notifications::{NotificationsIntent, NotificationsScreen};
use super::post_detail::{PostDetailIntent, PostDetailScreen};
use super::profile::{ProfileIntent, ProfileScreen, ProfileTab};
use super::settings_screen::{SettingsIntent, SettingsScreen};
use super::theme::{Theme, ThemeKind};
use super::toast::Toast;
use super::topic_feed::{TopicFeedIntent, TopicFeedScreen};
use super::topics::{TopicsIntent, TopicsScreen};
use crate::session::Session;

/// Connectivity / auth signal distilled from a background `ApiError`, delivered
/// out-of-band via [`BgEvent::ApiSignal`]. This is the typed side-channel that
/// lets the main loop react to network/session conditions centrally — driving
/// the offline indicator, the rate-limit toast, and session-expiry logout —
/// without every screen re-deriving them from an error string. The per-screen
/// `Result<_, String>` path is left untouched; this rides alongside it.
#[derive(Debug, Clone, Copy)]
pub enum ApiSignal {
    /// A transport failure — we never reached the server.
    Offline,
    /// The server answered but rate-limited us; carries its retry hint.
    RateLimited { retry_after_secs: u64 },
    /// A 401 outlived the client's refresh-once, so the session is dead.
    SessionExpired,
    /// The server answered normally (or with a non-transport error) — proof
    /// we're online; clears any offline state.
    Online,
}

/// Background-task result delivered to the main loop via `mpsc`.
#[derive(Debug)]
pub enum BgEvent {
    /// Out-of-band connectivity/auth signal (see [`ApiSignal`]); rides alongside
    /// the per-screen result events below.
    ApiSignal(ApiSignal),
    LoginResult(Result<String, String>),
    FeedInitial(Result<(Vec<Entry>, Option<String>), String>),
    FeedMore(Result<(Vec<Entry>, Option<String>), String>),
    NotificationsInitial(Result<(Vec<Notification>, Option<String>), String>),
    NotificationsMore(Result<(Vec<Notification>, Option<String>), String>),
    NotificationMarkedRead,
    AllNotificationsMarked,
    BookmarksInitial(Result<(Vec<Bookmark>, Option<String>), String>),
    BookmarksMore(Result<(Vec<Bookmark>, Option<String>), String>),
    BookmarkRemoved,
    TopicsLoaded(Result<Vec<Topic>, String>),
    TopicFeedInitial {
        slug: String,
        result: Result<(Vec<Entry>, Option<String>), String>,
    },
    TopicFeedMore {
        slug: String,
        result: Result<(Vec<Entry>, Option<String>), String>,
    },
    DetailRepliesInitial(Result<(Vec<Reply>, Option<String>), String>),
    DetailRepliesMore(Result<(Vec<Reply>, Option<String>), String>),
    OpenPostDetail {
        result: Result<Entry, String>,
        highlight_reply_id: Option<String>,
    },
    UnreadCount(u32),
    ProfileUser(Result<User, String>),
    ProfilePosts {
        more: bool,
        result: Result<(Vec<Entry>, Option<String>), String>,
    },
    ProfileReplies {
        more: bool,
        result: Result<(Vec<Reply>, Option<String>), String>,
    },
    ProfileFollowers {
        more: bool,
        result: Result<(Vec<Follow>, Option<String>), String>,
    },
    ProfileFollowing {
        more: bool,
        result: Result<(Vec<Follow>, Option<String>), String>,
    },
    ProfileFollowToggled(Result<Option<String>, String>), // Ok(Some(follow_id)) on follow, Ok(None) on unfollow
    ProfileUpdated(Result<User, String>),
    EntryCreated(Result<String, String>),
    ReplyCreated(Result<String, String>),
    EntryDeleted(Result<String, String>),
    NotesInitial(Result<(Vec<Note>, Option<String>), String>),
    NotesMore(Result<(Vec<Note>, Option<String>), String>),
    NoteRevisions {
        note_id: String,
        result: Result<Vec<NoteRevision>, String>,
    },
    NoteCreated(Result<String, String>),
    NoteUpdated(Result<String, String>),
    NoteDeleted,
    SettingsLoaded(Result<Settings, String>),
    SettingsSaved(Result<Settings, String>),
    GuildsInitial(Result<(Vec<Guild>, Option<String>), String>),
    GuildsMore(Result<(Vec<Guild>, Option<String>), String>),
    GuildInfo {
        slug: String,
        result: Result<Guild, String>,
    },
    GuildThreadsInitial {
        slug: String,
        result: Result<(Vec<GuildThread>, Option<String>), String>,
    },
    GuildThreadsMore {
        slug: String,
        result: Result<(Vec<GuildThread>, Option<String>), String>,
    },
    GuildMembersInitial {
        slug: String,
        result: Result<(Vec<GuildMembership>, Option<String>), String>,
    },
    GuildMembersMore {
        slug: String,
        result: Result<(Vec<GuildMembership>, Option<String>), String>,
    },
    GuildJoined {
        slug: String,
        result: Result<JoinedGuild, String>,
    },
    GuildLeft {
        slug: String,
        result: Result<String, String>,
    },
    GuildThreadCreated {
        slug: String,
        result: Result<String, String>,
    },
    ImageFetched {
        post_id: String,
        url: String,
        result: Result<Vec<u8>, String>,
    },
}

#[allow(clippy::large_enum_variant)] // Boxing isn't worth the indirection here.
pub enum Screen {
    Login(LoginScreen),
    Feed(FeedScreen),
    Notifications(NotificationsScreen),
    Bookmarks(BookmarksScreen),
    Topics(TopicsScreen),
    TopicFeed(TopicFeedScreen),
    PostDetail(PostDetailScreen),
    Profile(ProfileScreen),
    EditProfile(EditProfileScreen),
    Compose(ComposeScreen),
    Journal(JournalScreen),
    Settings(SettingsScreen),
    Guilds(GuildsScreen),
    Guild(GuildScreen),
}

impl Screen {
    fn is_login(&self) -> bool {
        matches!(self, Screen::Login(_))
    }

    /// Screens with inline text entry, where printable keys (like `?`) must
    /// reach the focused field rather than triggering global shortcuts.
    fn accepts_text_input(&self) -> bool {
        matches!(
            self,
            Screen::Login(_) | Screen::Compose(_) | Screen::EditProfile(_) | Screen::Settings(_)
        )
    }
}

/// Intent captured from a screen before we drop its borrow on `self.screen`.
enum Action {
    None,
    Quit,
    LoginSubmit {
        email: String,
        password: String,
    },
    FeedRefresh,
    FeedMore {
        cursor: Option<String>,
    },
    NotificationsRefresh,
    NotificationsMore {
        cursor: Option<String>,
    },
    NotificationsMarkOne {
        notification_id: String,
    },
    NotificationsMarkAll,
    BookmarksRefresh,
    BookmarksMore {
        cursor: Option<String>,
    },
    BookmarkRemove {
        bookmark_id: String,
    },
    TopicsRefresh,
    TopicOpen {
        slug: String,
    },
    TopicFeedRefresh {
        slug: String,
    },
    TopicFeedMore {
        slug: String,
        cursor: Option<String>,
    },
    PostDetailRefreshReplies {
        post_id: String,
    },
    PostDetailMoreReplies {
        post_id: String,
        cursor: Option<String>,
    },
    OpenPostDetailById {
        post_id: String,
        highlight_reply_id: Option<String>,
    },
    PopScreen,
    ProfileSelectTab {
        tab: ProfileTab,
        username: String,
    },
    ProfileLoadMore {
        tab: ProfileTab,
        username: String,
        user_id: Option<String>,
        cursor: Option<String>,
    },
    ProfileRefresh {
        tab: ProfileTab,
        username: String,
        user_id: Option<String>,
    },
    ProfileToggleFollow {
        user_id: String,
        follow_id: Option<String>,
    },
    ProfileOpenUser {
        username: String,
    },
    OpenEditProfile,
    SubmitEditProfile {
        update: Box<ProfileUpdate>,
    },
    StartComposeEntry,
    StartComposeReply {
        post_id: String,
        parent_reply_id: Option<String>,
        prefill: String,
    },
    StartComposeNote,
    StartEditNote {
        note_id: String,
        prefill: String,
        topics: Vec<String>,
    },
    ComposeSubmit,
    DeleteEntry {
        post_id: String,
    },
    JournalRefresh,
    JournalMore {
        cursor: Option<String>,
    },
    JournalShowRevisions {
        note_id: String,
    },
    DeleteNote {
        note_id: String,
    },
    SettingsSubmit {
        update: Box<SettingsUpdate>,
    },
    GuildsRefresh,
    GuildsMore {
        cursor: Option<String>,
    },
    GuildOpen {
        slug: String,
    },
    GuildRefresh {
        slug: String,
        tab: GuildTab,
    },
    GuildLoadMore {
        slug: String,
        tab: GuildTab,
        cursor: Option<String>,
    },
    GuildSelectTab {
        slug: String,
        tab: GuildTab,
    },
    GuildJoin {
        slug: String,
    },
    GuildLeave {
        slug: String,
    },
    GuildComposeThread {
        slug: String,
    },
}

pub struct App {
    client: Client,
    theme: Theme,
    theme_kind: ThemeKind,
    screen: Screen,
    back_stack: Vec<Screen>,
    current_root: Option<RootKind>,
    unread_count: u32,
    should_quit: bool,
    bg_tx: mpsc::UnboundedSender<BgEvent>,
    bg_rx: mpsc::UnboundedReceiver<BgEvent>,
    /// Open overlay menu, if any (triggered by Esc).
    menu: Option<MenuOverlay>,
    /// Whether the `?` help overlay is currently shown.
    help: bool,
    /// Terminal image protocol picker, if the terminal supports graphics.
    /// `None` disables image rendering (the text placeholder is shown instead).
    picker: Option<Picker>,
    /// Email cached for re-displaying on the login screen after logout.
    last_email: String,
    /// Whether the last network attempt hit a transport error (no server
    /// reachable). Surfaced as a tab-bar marker; cleared once any call reaches
    /// the server again (heartbeat poll or a server-origin response).
    offline: bool,
    /// Active transient toast (currently the rate-limit countdown), if any.
    toast: Option<Toast>,
    /// Set when a background call proves the session is dead; the run loop
    /// performs the (async) logout and seeds this reason on the login screen.
    pending_logout: Option<String>,
    /// Wakes the unread-count poller early when we go offline, so the offline
    /// marker clears promptly once the connection returns (instead of waiting
    /// out the poller's current sleep).
    offline_notify: Arc<Notify>,
    /// Whether the single long-lived unread-count poller has been spawned. It
    /// outlives logout (idling on the login screen) so re-login reuses it
    /// instead of stacking duplicates.
    poller_started: bool,
}

impl App {
    pub fn with_theme(client: Client, prefill_email: String, theme_kind: ThemeKind) -> Self {
        let (bg_tx, bg_rx) = mpsc::unbounded_channel();
        let last_email = prefill_email.clone();
        Self {
            client,
            theme: theme_kind.theme(),
            theme_kind,
            screen: Screen::Login(LoginScreen::new(prefill_email)),
            back_stack: Vec::new(),
            current_root: None,
            unread_count: 0,
            should_quit: false,
            bg_tx,
            bg_rx,
            menu: None,
            help: false,
            picker: None,
            last_email,
            offline: false,
            toast: None,
            pending_logout: None,
            offline_notify: Arc::new(Notify::new()),
            poller_started: false,
        }
    }

    /// Install the terminal image picker (detected at startup). `None` leaves
    /// image rendering disabled.
    pub fn set_image_picker(&mut self, picker: Option<Picker>) {
        self.picker = picker;
    }

    /// Skip the login screen — used when a valid session was restored at launch.
    pub fn enter_feed_initial(&mut self) {
        self.goto_root(RootKind::Feed);
        if self.poller_started {
            // A poller from a previous session is still alive (it idled on the
            // login screen). Reusing it — rather than spawning a duplicate on
            // every re-login — keeps exactly one heartbeat; nudge it to re-poll
            // now with the fresh tokens.
            self.offline_notify.notify_one();
        } else {
            self.spawn_unread_count_poller();
            self.poller_started = true;
        }
    }

    pub async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        // 1s heartbeat that only fires while a toast is up (see the guarded
        // select arm); it animates the countdown without waking an idle TUI.
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        // One long-lived input reader feeding a channel. The previous approach
        // spawned a fresh `spawn_blocking(event::read)` per select! iteration;
        // because a blocking read can't be cancelled, every time a background
        // event won the select! it orphaned a thread still parked in
        // `event::read()` that then swallowed the next keystroke — the ~2s
        // "unresponsive on startup / after an action" lag (marking a
        // notification read fires two bg events, orphaning two readers). A
        // single reader has nothing to orphan; events queue in the channel and
        // are drained below.
        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
        std::thread::spawn(move || {
            while let Ok(ev) = event::read() {
                if input_tx.send(ev).is_err() {
                    break; // run loop gone
                }
            }
        });

        terminal.draw(|f| self.render(f)).context("terminal draw")?;
        while !self.should_quit {
            tokio::select! {
                maybe_ev = input_rx.recv() => {
                    match maybe_ev {
                        Some(ev) => {
                            self.handle_terminal_event(ev).await;
                            // Drain the rest of the burst (focus events,
                            // capability-query replies) before redrawing, so a
                            // flurry costs one render, not one per event.
                            while let Ok(ev) = input_rx.try_recv() {
                                if self.should_quit {
                                    break;
                                }
                                self.handle_terminal_event(ev).await;
                            }
                        }
                        None => self.should_quit = true, // reader thread ended
                    }
                }
                Some(bg) = self.bg_rx.recv() => {
                    self.handle_bg_event(bg);
                }
                _ = ticker.tick(), if self.toast.is_some() => {
                    self.tick_toast();
                }
            }
            // A background call may have proven the session dead; logging out
            // needs an await, so it happens here rather than in the sync bg
            // handler.
            self.apply_pending_logout().await;
            terminal.draw(|f| self.render(f)).context("terminal draw")?;
        }
        Ok(())
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>) {
        let full_area = frame.area();

        if self.screen.is_login() {
            if let Screen::Login(s) = &self.screen {
                s.render(frame, full_area, &self.theme);
            }
        } else {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(full_area);
            let tab_area = layout[0];
            let screen_area = layout[1];

            // Show the root-of-current-stack in the tab bar (defaulting to Feed
            // if we somehow arrive here without one set).
            let current = self.current_root.unwrap_or(RootKind::Feed);
            render_tab_bar(
                frame,
                tab_area,
                current,
                self.unread_count,
                !self.back_stack.is_empty(),
                self.offline,
                &self.theme,
            );

            match &self.screen {
                Screen::Login(s) => s.render(frame, screen_area, &self.theme),
                Screen::Feed(s) => s.render(frame, screen_area, &self.theme),
                Screen::Notifications(s) => s.render(frame, screen_area, &self.theme),
                Screen::Bookmarks(s) => s.render(frame, screen_area, &self.theme),
                Screen::Topics(s) => s.render(frame, screen_area, &self.theme),
                Screen::TopicFeed(s) => s.render(frame, screen_area, &self.theme),
                Screen::PostDetail(s) => s.render(frame, screen_area, &self.theme),
                Screen::Profile(s) => s.render(frame, screen_area, &self.theme),
                Screen::EditProfile(s) => s.render(frame, screen_area, &self.theme),
                Screen::Compose(s) => s.render(frame, screen_area, &self.theme),
                Screen::Journal(s) => s.render(frame, screen_area, &self.theme),
                Screen::Settings(s) => s.render(frame, screen_area, &self.theme),
                Screen::Guilds(s) => s.render(frame, screen_area, &self.theme),
                Screen::Guild(s) => s.render(frame, screen_area, &self.theme),
            }
        }

        // Transient toast sits above the screen but below the modal overlays.
        if let Some(toast) = &self.toast {
            super::toast::render(frame, full_area, toast, &self.theme);
        }

        // Overlay menu — always drawn last so it sits on top of ANY screen,
        // including login. (Previously the login branch returned early and
        // skipped this, so opening the menu there left keystrokes routed to an
        // undrawn menu and the UI looked frozen.)
        if let Some(menu) = &self.menu {
            menu.render(frame, full_area, &self.theme);
        }
        if self.help {
            super::help::render(frame, full_area, &self.theme);
        }
    }

    async fn handle_terminal_event(&mut self, ev: Event) {
        let key = match ev {
            Event::Key(k) if k.kind == event::KeyEventKind::Press => k,
            // Mouse wheel → one selection step per notch. Button+scroll reporting
            // is enabled in main; motion tracking is not, so the mouse doesn't
            // flood events when moved.
            Event::Mouse(m) => match m.kind {
                event::MouseEventKind::ScrollDown => synthetic_key(KeyCode::Down),
                event::MouseEventKind::ScrollUp => synthetic_key(KeyCode::Up),
                _ => return,
            },
            _ => return,
        };

        // The help overlay swallows the next key to dismiss (Ctrl+C still quits).
        if self.help {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.should_quit = true;
            } else {
                self.help = false;
            }
            return;
        }

        // If the overlay menu is open, route the key there.
        if let Some(menu) = &mut self.menu {
            match menu.handle_key(key) {
                MenuIntent::None => {}
                MenuIntent::Cancel => self.menu = None,
                MenuIntent::Back => {
                    self.menu = None;
                    self.pop_screen();
                }
                MenuIntent::Logout => {
                    self.menu = None;
                    self.logout().await;
                }
                MenuIntent::CycleTheme => {
                    self.cycle_theme();
                    // Keep the menu open with a refreshed label so the user can
                    // cycle repeatedly and watch the palette change live.
                    if let Some(menu) = &mut self.menu {
                        menu.refresh_theme_label(self.theme_kind.name());
                    }
                }
                MenuIntent::Quit => self.should_quit = true,
            }
            return;
        }

        // Esc is the reflexive "back": pop to the previous screen when there is
        // one; on a top-level section (nothing to pop) it opens the overlay menu.
        if key.code == KeyCode::Esc {
            if self.back_stack.is_empty() {
                let authenticated = !self.screen.is_login();
                self.menu =
                    Some(MenuOverlay::build(authenticated, false, self.theme_kind.name()));
            } else {
                self.pop_screen();
            }
            return;
        }

        // `?` opens the help overlay, except where a screen captures text input.
        if key.code == KeyCode::Char('?') && !self.screen.accepts_text_input() {
            self.help = true;
            return;
        }

        // Root hotkeys (1-7) switch sections, but only on screens that don't
        // capture text: otherwise a digit typed into a compose title/topic, a
        // profile-edit field, or a settings field (Settings binds 4/6 itself)
        // would navigate away and discard the in-progress input.
        if !self.screen.accepts_text_input() {
            match key.code {
                KeyCode::Tab | KeyCode::Right => {
                    let next = self.current_root.unwrap_or(RootKind::Feed).next();
                    self.goto_root(next);
                    return;
                }
                KeyCode::BackTab | KeyCode::Left => {
                    let prev = self.current_root.unwrap_or(RootKind::Feed).prev();
                    self.goto_root(prev);
                    return;
                }
                KeyCode::Char(c) => {
                    if let Some(target) = RootKind::from_shortcut(c) {
                        if self.current_root != Some(target) {
                            self.goto_root(target);
                            return;
                        }
                    }
                }
                _ => {}
            }
        }

        // Phase 1: derive an Action with a mutable borrow on the active screen.
        let action = match &mut self.screen {
            Screen::Login(s) => match s.handle_key(key) {
                LoginIntent::Submit => Action::LoginSubmit {
                    email: s.email.trim().to_string(),
                    password: s.password.clone(),
                },
                LoginIntent::Quit => Action::Quit,
                LoginIntent::None => Action::None,
            },
            Screen::Feed(s) => match s.handle_key(key) {
                FeedIntent::Quit => Action::Quit,
                FeedIntent::Refresh => Action::FeedRefresh,
                FeedIntent::LoadMore => Action::FeedMore {
                    cursor: s.next_cursor.clone(),
                },
                FeedIntent::OpenSelected(post_id) => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id: None,
                },
                FeedIntent::Compose => Action::StartComposeEntry,
                FeedIntent::None => Action::None,
            },
            Screen::Notifications(s) => match s.handle_key(key) {
                NotificationsIntent::Quit => Action::Quit,
                NotificationsIntent::Refresh => Action::NotificationsRefresh,
                NotificationsIntent::LoadMore => Action::NotificationsMore {
                    cursor: s.next_cursor.clone(),
                },
                NotificationsIntent::ToggleFilter => Action::NotificationsRefresh,
                NotificationsIntent::MarkSelectedRead { notification_id } => {
                    Action::NotificationsMarkOne { notification_id }
                }
                NotificationsIntent::MarkAllRead => Action::NotificationsMarkAll,
                NotificationsIntent::OpenSelected {
                    post_id,
                    highlight_reply_id,
                } => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id,
                },
                NotificationsIntent::None => Action::None,
            },
            Screen::Bookmarks(s) => match s.handle_key(key) {
                BookmarksIntent::Quit => Action::Quit,
                BookmarksIntent::Refresh => Action::BookmarksRefresh,
                BookmarksIntent::LoadMore => Action::BookmarksMore {
                    cursor: s.next_cursor.clone(),
                },
                BookmarksIntent::RemoveSelected { bookmark_id } => {
                    Action::BookmarkRemove { bookmark_id }
                }
                BookmarksIntent::OpenSelected {
                    post_id,
                    highlight_reply_id,
                } => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id,
                },
                BookmarksIntent::None => Action::None,
            },
            Screen::Topics(s) => match s.handle_key(key) {
                TopicsIntent::Quit => Action::Quit,
                TopicsIntent::Refresh => Action::TopicsRefresh,
                TopicsIntent::OpenSelected { slug } => Action::TopicOpen { slug },
                TopicsIntent::None => Action::None,
            },
            Screen::TopicFeed(s) => match s.handle_key(key) {
                TopicFeedIntent::Quit => Action::Quit,
                TopicFeedIntent::Back => Action::PopScreen,
                TopicFeedIntent::Refresh => Action::TopicFeedRefresh {
                    slug: s.slug.clone(),
                },
                TopicFeedIntent::LoadMore => Action::TopicFeedMore {
                    slug: s.slug.clone(),
                    cursor: s.next_cursor.clone(),
                },
                TopicFeedIntent::OpenSelected { post_id } => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id: None,
                },
                TopicFeedIntent::None => Action::None,
            },
            Screen::PostDetail(s) => match s.handle_key(key) {
                PostDetailIntent::Quit => Action::Quit,
                PostDetailIntent::Back => Action::PopScreen,
                PostDetailIntent::RefreshReplies => Action::PostDetailRefreshReplies {
                    post_id: s.entry.post_id.clone(),
                },
                PostDetailIntent::LoadMoreReplies => Action::PostDetailMoreReplies {
                    post_id: s.entry.post_id.clone(),
                    cursor: s.next_replies_cursor.clone(),
                },
                PostDetailIntent::Reply => Action::StartComposeReply {
                    post_id: s.entry.post_id.clone(),
                    parent_reply_id: None,
                    prefill: format!(
                        "> @{}: {}\n\n",
                        s.entry.author_username,
                        first_line(&s.entry.content)
                    ),
                },
                PostDetailIntent::DeleteEntryConfirmed => Action::DeleteEntry {
                    post_id: s.entry.post_id.clone(),
                },
                PostDetailIntent::None => Action::None,
            },
            Screen::Compose(s) => match s.handle_key(key) {
                ComposeIntent::Quit => Action::Quit,
                ComposeIntent::Submit => Action::ComposeSubmit,
                ComposeIntent::None => Action::None,
            },
            Screen::Settings(s) => match s.handle_key(key) {
                SettingsIntent::Quit => Action::Quit,
                SettingsIntent::Cancel => Action::PopScreen,
                SettingsIntent::Submit { update } => Action::SettingsSubmit { update },
                SettingsIntent::None => Action::None,
            },
            Screen::Journal(s) => match s.handle_key(key) {
                JournalIntent::Quit => Action::Quit,
                JournalIntent::LoadMore => Action::JournalMore {
                    cursor: s.next_cursor.clone(),
                },
                JournalIntent::Refresh => Action::JournalRefresh,
                JournalIntent::Compose => Action::StartComposeNote,
                JournalIntent::EditSelected {
                    note_id,
                    content,
                    topics,
                } => Action::StartEditNote {
                    note_id,
                    prefill: content,
                    topics,
                },
                JournalIntent::DeleteSelected { note_id } => Action::DeleteNote { note_id },
                JournalIntent::ShowRevisions { note_id } => {
                    Action::JournalShowRevisions { note_id }
                }
                JournalIntent::HideRevisions => {
                    // The screen already toggled `mode` back to Current; no spawn needed.
                    Action::None
                }
                JournalIntent::None => Action::None,
            },
            Screen::Profile(s) => match s.handle_key(key) {
                ProfileIntent::Quit => Action::Quit,
                ProfileIntent::Back => Action::PopScreen,
                ProfileIntent::SelectTab(tab) => {
                    let username = s
                        .username
                        .clone()
                        .or_else(|| s.user.as_ref().map(|u| u.username.clone()))
                        .unwrap_or_default();
                    Action::ProfileSelectTab { tab, username }
                }
                ProfileIntent::LoadMoreCurrentTab => {
                    let username = s
                        .username
                        .clone()
                        .or_else(|| s.user.as_ref().map(|u| u.username.clone()))
                        .unwrap_or_default();
                    let user_id = s.user.as_ref().map(|u| u.id.clone());
                    let cursor = match s.tab {
                        ProfileTab::Info => None,
                        ProfileTab::Posts => s.posts.next_cursor.clone(),
                        ProfileTab::Replies => s.replies.next_cursor.clone(),
                        ProfileTab::Followers => s.followers.next_cursor.clone(),
                        ProfileTab::Following => s.following.next_cursor.clone(),
                    };
                    Action::ProfileLoadMore {
                        tab: s.tab,
                        username,
                        user_id,
                        cursor,
                    }
                }
                ProfileIntent::RefreshCurrentTab => {
                    let username = s
                        .username
                        .clone()
                        .or_else(|| s.user.as_ref().map(|u| u.username.clone()))
                        .unwrap_or_default();
                    let user_id = s.user.as_ref().map(|u| u.id.clone());
                    Action::ProfileRefresh {
                        tab: s.tab,
                        username,
                        user_id,
                    }
                }
                ProfileIntent::ToggleFollow => {
                    if let Some(u) = &s.user {
                        Action::ProfileToggleFollow {
                            user_id: u.id.clone(),
                            follow_id: u.follow_id.clone(),
                        }
                    } else {
                        Action::None
                    }
                }
                ProfileIntent::EditOwnProfile => Action::OpenEditProfile,
                ProfileIntent::OpenPost { post_id } => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id: None,
                },
                ProfileIntent::OpenReply { post_id, reply_id } => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id: Some(reply_id),
                },
                ProfileIntent::OpenUser { username } => Action::ProfileOpenUser { username },
                ProfileIntent::None => Action::None,
            },
            Screen::EditProfile(s) => match s.handle_key(key) {
                EditProfileIntent::Quit => Action::Quit,
                EditProfileIntent::Cancel => Action::PopScreen,
                EditProfileIntent::Submit { update } => Action::SubmitEditProfile { update },
                EditProfileIntent::None => Action::None,
            },
            Screen::Guilds(s) => match s.handle_key(key) {
                GuildsIntent::Quit => Action::Quit,
                GuildsIntent::Refresh => Action::GuildsRefresh,
                GuildsIntent::LoadMore => Action::GuildsMore {
                    cursor: s.next_cursor.clone(),
                },
                GuildsIntent::OpenSelected { slug } => Action::GuildOpen { slug },
                GuildsIntent::None => Action::None,
            },
            Screen::Guild(s) => match s.handle_key(key) {
                GuildIntent::Quit => Action::Quit,
                GuildIntent::Back => Action::PopScreen,
                GuildIntent::Refresh => Action::GuildRefresh {
                    slug: s.slug.clone(),
                    tab: s.tab,
                },
                GuildIntent::LoadMore => Action::GuildLoadMore {
                    slug: s.slug.clone(),
                    tab: s.tab,
                    cursor: match s.tab {
                        GuildTab::Threads => s.threads_cursor.clone(),
                        GuildTab::Members => s.members_cursor.clone(),
                    },
                },
                GuildIntent::SelectTab(tab) => Action::GuildSelectTab {
                    slug: s.slug.clone(),
                    tab,
                },
                GuildIntent::OpenThread { post_id } => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id: None,
                },
                GuildIntent::Join => Action::GuildJoin {
                    slug: s.slug.clone(),
                },
                GuildIntent::Leave => Action::GuildLeave {
                    slug: s.slug.clone(),
                },
                GuildIntent::Compose => Action::GuildComposeThread {
                    slug: s.slug.clone(),
                },
                GuildIntent::None => Action::None,
            },
        };

        // Phase 2: apply the action with full mutable access to self.
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::LoginSubmit { email, password } => self.spawn_login(email, password),
            Action::FeedRefresh => self.spawn_feed_initial(),
            Action::FeedMore { cursor } => self.spawn_feed_more(cursor),
            Action::NotificationsRefresh => {
                let filter = if let Screen::Notifications(s) = &self.screen {
                    s.filter
                } else {
                    NotificationsFilter::All
                };
                self.spawn_notifications_initial(filter);
            }
            Action::NotificationsMore { cursor } => {
                let filter = if let Screen::Notifications(s) = &self.screen {
                    s.filter
                } else {
                    NotificationsFilter::All
                };
                self.spawn_notifications_more(filter, cursor);
            }
            Action::NotificationsMarkOne { notification_id } => {
                if let Screen::Notifications(s) = &mut self.screen {
                    s.mark_local(&notification_id);
                }
                self.unread_count = self.unread_count.saturating_sub(1);
                self.spawn_mark_notification_read(notification_id);
            }
            Action::NotificationsMarkAll => {
                if let Screen::Notifications(s) = &mut self.screen {
                    s.mark_all_local();
                }
                self.unread_count = 0;
                self.spawn_mark_all_notifications_read();
            }
            Action::BookmarksRefresh => self.spawn_bookmarks_initial(),
            Action::BookmarksMore { cursor } => self.spawn_bookmarks_more(cursor),
            Action::BookmarkRemove { bookmark_id } => {
                if let Screen::Bookmarks(s) = &mut self.screen {
                    s.remove_local(&bookmark_id);
                }
                self.spawn_delete_bookmark(bookmark_id);
            }
            Action::TopicsRefresh => self.spawn_topics_load(),
            Action::TopicOpen { slug } => {
                let new_screen = Screen::TopicFeed(TopicFeedScreen::new(slug.clone()));
                self.push_screen(new_screen);
                self.spawn_topic_feed_initial(&slug);
            }
            Action::TopicFeedRefresh { slug } => self.spawn_topic_feed_initial(&slug),
            Action::TopicFeedMore { slug, cursor } => self.spawn_topic_feed_more(&slug, cursor),
            Action::PostDetailRefreshReplies { post_id } => {
                self.spawn_detail_replies_initial(&post_id);
            }
            Action::PostDetailMoreReplies { post_id, cursor } => {
                self.spawn_detail_replies_more(&post_id, cursor);
            }
            Action::OpenPostDetailById {
                post_id,
                highlight_reply_id,
            } => {
                // Fast path: if the entry is already in the current Feed, use it.
                if let Screen::Feed(s) = &self.screen {
                    if let Some(entry) = s.entries.iter().find(|e| e.post_id == post_id).cloned() {
                        self.enter_post_detail(entry, highlight_reply_id);
                        return;
                    }
                }
                if let Screen::TopicFeed(s) = &self.screen {
                    if let Some(entry) = s.entries.iter().find(|e| e.post_id == post_id).cloned() {
                        self.enter_post_detail(entry, highlight_reply_id);
                        return;
                    }
                }
                // Slow path: fetch entry first.
                self.spawn_open_post_detail_by_id(post_id, highlight_reply_id);
            }
            Action::PopScreen => self.pop_screen(),
            Action::ProfileSelectTab { tab, username } => {
                self.spawn_profile_tab_fetch(tab, username, None, None);
            }
            Action::ProfileLoadMore {
                tab,
                username,
                user_id,
                cursor,
            } => {
                self.spawn_profile_tab_fetch(tab, username, user_id, cursor);
            }
            Action::ProfileRefresh {
                tab,
                username,
                user_id,
            } => {
                if let Screen::Profile(s) = &mut self.screen {
                    match tab {
                        ProfileTab::Info => s.loading_user = true,
                        ProfileTab::Posts => {
                            s.posts.loading = true;
                            s.posts.items.clear();
                            s.posts.next_cursor = None;
                        }
                        ProfileTab::Replies => {
                            s.replies.loading = true;
                            s.replies.items.clear();
                            s.replies.next_cursor = None;
                        }
                        ProfileTab::Followers => {
                            s.followers.loading = true;
                            s.followers.items.clear();
                            s.followers.next_cursor = None;
                        }
                        ProfileTab::Following => {
                            s.following.loading = true;
                            s.following.items.clear();
                            s.following.next_cursor = None;
                        }
                    }
                }
                self.spawn_profile_tab_fetch(tab, username, user_id, None);
            }
            Action::ProfileToggleFollow { user_id, follow_id } => {
                if let Screen::Profile(s) = &mut self.screen {
                    s.follow_action_pending = true;
                }
                self.spawn_toggle_follow(user_id, follow_id);
            }
            Action::ProfileOpenUser { username } => {
                let mut screen = ProfileScreen::new_for(username.clone());
                screen.is_self = false;
                screen.is_root = false;
                self.push_screen(Screen::Profile(screen));
                self.spawn_profile_user(username);
            }
            Action::OpenEditProfile => {
                if let Screen::Profile(s) = &self.screen {
                    if let Some(u) = &s.user {
                        let screen = EditProfileScreen::from_user(u);
                        self.push_screen(Screen::EditProfile(screen));
                    }
                }
            }
            Action::SubmitEditProfile { update } => {
                self.spawn_update_own_profile(*update);
            }
            Action::StartComposeEntry => {
                self.start_compose(ComposeKind::NewEntry, String::new())
                    .await;
            }
            Action::StartComposeReply {
                post_id,
                parent_reply_id,
                prefill,
            } => {
                self.start_compose(
                    ComposeKind::Reply {
                        post_id,
                        parent_reply_id,
                    },
                    prefill,
                )
                .await;
            }
            Action::ComposeSubmit => {
                self.spawn_compose_submit();
            }
            Action::DeleteEntry { post_id } => {
                self.spawn_delete_entry(post_id);
            }
            Action::StartComposeNote => {
                self.start_compose(ComposeKind::NewNote, String::new())
                    .await;
            }
            Action::StartEditNote {
                note_id,
                prefill,
                topics,
            } => {
                self.start_compose_note_edit(note_id, prefill, topics).await;
            }
            Action::JournalRefresh => {
                if let Screen::Journal(s) = &mut self.screen {
                    s.notes.clear();
                    s.next_cursor = None;
                    s.selected = 0;
                    s.loading = true;
                    s.error = None;
                }
                self.spawn_notes_initial();
            }
            Action::JournalMore { cursor } => self.spawn_notes_more(cursor),
            Action::JournalShowRevisions { note_id } => {
                if let Screen::Journal(s) = &mut self.screen {
                    s.loading_revisions = true;
                }
                self.spawn_note_revisions(note_id);
            }
            Action::DeleteNote { note_id } => {
                if let Screen::Journal(s) = &mut self.screen {
                    s.remove_local(&note_id);
                }
                self.spawn_delete_note(note_id);
            }
            Action::SettingsSubmit { update } => {
                self.spawn_settings_save(*update);
            }
            Action::GuildsRefresh => self.spawn_guilds_initial(),
            Action::GuildsMore { cursor } => self.spawn_guilds_more(cursor),
            Action::GuildOpen { slug } => {
                self.push_screen(Screen::Guild(GuildScreen::new(slug.clone())));
                self.spawn_guild_open(slug);
            }
            Action::GuildRefresh { slug, tab } => self.spawn_guild_tab_initial(&slug, tab),
            Action::GuildSelectTab { slug, tab } => self.spawn_guild_tab_initial(&slug, tab),
            Action::GuildLoadMore { slug, tab, cursor } => {
                self.spawn_guild_tab_more(&slug, tab, cursor)
            }
            Action::GuildJoin { slug } => self.spawn_guild_join(slug),
            Action::GuildLeave { slug } => self.spawn_guild_leave(slug),
            Action::GuildComposeThread { slug } => {
                self.start_compose(ComposeKind::GuildThread { guild_slug: slug }, String::new())
                    .await;
            }
        }
    }

    fn handle_bg_event(&mut self, ev: BgEvent) {
        match ev {
            BgEvent::ApiSignal(signal) => self.handle_api_signal(signal),
            BgEvent::LoginResult(Ok(email)) => {
                let tokens = block_on(self.client.tokens());
                let session = Session {
                    tokens,
                    email: email.clone(),
                };
                if let Err(e) = session.save() {
                    tracing::warn!(error = %e, "session save failed");
                }
                self.last_email = email;
                self.offline = false;
                self.enter_feed_initial();
            }
            BgEvent::LoginResult(Err(msg)) => {
                if let Screen::Login(s) = &mut self.screen {
                    s.finish_submit(Err(msg));
                }
            }
            BgEvent::FeedInitial(result) => {
                if let Screen::Feed(s) = &mut self.screen {
                    s.apply_initial(result);
                }
            }
            BgEvent::FeedMore(result) => {
                if let Screen::Feed(s) = &mut self.screen {
                    s.apply_more(result);
                }
            }
            BgEvent::NotificationsInitial(result) => {
                if let Screen::Notifications(s) = &mut self.screen {
                    s.apply_initial(result);
                }
            }
            BgEvent::NotificationsMore(result) => {
                if let Screen::Notifications(s) = &mut self.screen {
                    s.apply_more(result);
                }
            }
            BgEvent::NotificationMarkedRead | BgEvent::AllNotificationsMarked => {
                // Server confirmed the mark; local UI already updated optimistically.
                // Refresh unread count to converge on truth.
                self.spawn_unread_count_once();
            }
            BgEvent::BookmarksInitial(result) => {
                if let Screen::Bookmarks(s) = &mut self.screen {
                    s.apply_initial(result);
                }
            }
            BgEvent::BookmarksMore(result) => {
                if let Screen::Bookmarks(s) = &mut self.screen {
                    s.apply_more(result);
                }
            }
            BgEvent::BookmarkRemoved => {
                // Local state already removed optimistically.
            }
            BgEvent::TopicsLoaded(result) => {
                if let Screen::Topics(s) = &mut self.screen {
                    s.apply(result);
                }
            }
            BgEvent::TopicFeedInitial { slug, result } => {
                if let Screen::TopicFeed(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_initial(result);
                    }
                }
            }
            BgEvent::TopicFeedMore { slug, result } => {
                if let Screen::TopicFeed(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_more(result);
                    }
                }
            }
            BgEvent::DetailRepliesInitial(result) => {
                if let Screen::PostDetail(s) = &mut self.screen {
                    s.apply_replies_initial(result);
                }
            }
            BgEvent::DetailRepliesMore(result) => {
                if let Screen::PostDetail(s) = &mut self.screen {
                    s.apply_replies_more(result);
                }
            }
            BgEvent::OpenPostDetail {
                result,
                highlight_reply_id,
            } => match result {
                Ok(entry) => self.enter_post_detail(entry, highlight_reply_id),
                Err(msg) => {
                    tracing::warn!(error = msg, "open-post-detail fetch failed");
                }
            },
            BgEvent::UnreadCount(n) => {
                // A successful poll doubles as an online heartbeat.
                self.offline = false;
                self.unread_count = n;
            }
            BgEvent::ProfileUser(result) => {
                if let Screen::Profile(s) = &mut self.screen {
                    s.apply_user(result);
                    // If the user just loaded and we're on a non-Info tab, kick off its fetch.
                    if let Some(u) = s.user.clone() {
                        let username = u.username.clone();
                        let user_id = Some(u.id.clone());
                        let tab = s.tab;
                        if tab != ProfileTab::Info {
                            self.spawn_profile_tab_fetch(tab, username, user_id, None);
                        }
                    }
                }
            }
            BgEvent::ProfilePosts { more, result } => {
                if let Screen::Profile(s) = &mut self.screen {
                    if more {
                        s.posts.apply_more(result);
                    } else {
                        s.posts.apply_initial(result);
                    }
                }
            }
            BgEvent::ProfileReplies { more, result } => {
                if let Screen::Profile(s) = &mut self.screen {
                    if more {
                        s.replies.apply_more(result);
                    } else {
                        s.replies.apply_initial(result);
                    }
                }
            }
            BgEvent::ProfileFollowers { more, result } => {
                if let Screen::Profile(s) = &mut self.screen {
                    if more {
                        s.followers.apply_more(result);
                    } else {
                        s.followers.apply_initial(result);
                    }
                }
            }
            BgEvent::ProfileFollowing { more, result } => {
                if let Screen::Profile(s) = &mut self.screen {
                    if more {
                        s.following.apply_more(result);
                    } else {
                        s.following.apply_initial(result);
                    }
                }
            }
            BgEvent::ProfileFollowToggled(result) => {
                if let Screen::Profile(s) = &mut self.screen {
                    s.follow_action_pending = false;
                    match result {
                        Ok(new_follow_id) => {
                            if let Some(u) = &mut s.user {
                                if let Some(fid) = new_follow_id {
                                    u.follow_id = Some(fid);
                                    u.is_following = Some(true);
                                    u.followers_count =
                                        u.followers_count.map(|c| c.saturating_add(1));
                                } else {
                                    u.follow_id = None;
                                    u.is_following = Some(false);
                                    u.followers_count =
                                        u.followers_count.map(|c| c.saturating_sub(1));
                                }
                            }
                        }
                        Err(msg) => {
                            tracing::warn!(error = msg, "follow toggle failed");
                            s.user_error = Some(msg);
                        }
                    }
                }
            }
            BgEvent::ProfileUpdated(result) => match result {
                Ok(u) => {
                    if matches!(self.screen, Screen::EditProfile(_)) {
                        self.pop_screen();
                    }
                    if let Screen::Profile(p) = &mut self.screen {
                        p.user = Some(u);
                        p.loading_user = false;
                        p.user_error = None;
                    }
                }
                Err(msg) => {
                    if let Screen::EditProfile(s) = &mut self.screen {
                        s.finish_submit(Err(msg));
                    }
                }
            },
            BgEvent::EntryCreated(result) => match result {
                Ok(_new_post_id) => {
                    if matches!(self.screen, Screen::Compose(_)) {
                        self.pop_screen();
                    }
                    // If the underlying screen is the feed, refresh it.
                    if matches!(self.screen, Screen::Feed(_)) {
                        self.spawn_feed_initial();
                    }
                }
                Err(msg) => {
                    if let Screen::Compose(s) = &mut self.screen {
                        s.finish_submit(Err(msg));
                    }
                }
            },
            BgEvent::ReplyCreated(result) => match result {
                Ok(_new_reply_id) => {
                    if matches!(self.screen, Screen::Compose(_)) {
                        self.pop_screen();
                    }
                    // If the underlying screen is a PostDetail, refresh replies.
                    if let Screen::PostDetail(d) = &self.screen {
                        let post_id = d.entry.post_id.clone();
                        self.spawn_detail_replies_initial(&post_id);
                    }
                }
                Err(msg) => {
                    if let Screen::Compose(s) = &mut self.screen {
                        s.finish_submit(Err(msg));
                    }
                }
            },
            BgEvent::EntryDeleted(result) => match result {
                Ok(_post_id) => {
                    if matches!(self.screen, Screen::PostDetail(_)) {
                        self.pop_screen();
                    }
                    if matches!(self.screen, Screen::Feed(_)) {
                        self.spawn_feed_initial();
                    }
                }
                Err(msg) => {
                    if let Screen::PostDetail(s) = &mut self.screen {
                        s.error = Some(format!("delete failed: {msg}"));
                    }
                }
            },
            BgEvent::NotesInitial(result) => {
                if let Screen::Journal(s) = &mut self.screen {
                    s.apply_initial(result);
                }
            }
            BgEvent::NotesMore(result) => {
                if let Screen::Journal(s) = &mut self.screen {
                    s.apply_more(result);
                }
            }
            BgEvent::NoteRevisions { note_id, result } => {
                if let Screen::Journal(s) = &mut self.screen {
                    s.apply_revisions(note_id, result);
                }
            }
            BgEvent::NoteCreated(result) => match result {
                Ok(_) => {
                    if matches!(self.screen, Screen::Compose(_)) {
                        self.pop_screen();
                    }
                    if matches!(self.screen, Screen::Journal(_)) {
                        self.spawn_notes_initial();
                    }
                }
                Err(msg) => {
                    if let Screen::Compose(s) = &mut self.screen {
                        s.finish_submit(Err(msg));
                    }
                }
            },
            BgEvent::NoteUpdated(result) => match result {
                Ok(_) => {
                    if matches!(self.screen, Screen::Compose(_)) {
                        self.pop_screen();
                    }
                    if matches!(self.screen, Screen::Journal(_)) {
                        self.spawn_notes_initial();
                    }
                }
                Err(msg) => {
                    if let Screen::Compose(s) = &mut self.screen {
                        s.finish_submit(Err(msg));
                    }
                }
            },
            BgEvent::NoteDeleted => {
                // Already removed optimistically; no further action.
            }
            BgEvent::SettingsLoaded(result) => {
                if let Screen::Settings(s) = &mut self.screen {
                    s.apply_loaded(result);
                }
            }
            BgEvent::SettingsSaved(result) => match result {
                Ok(s) => {
                    if let Screen::Settings(screen) = &mut self.screen {
                        screen.apply_loaded(Ok(s));
                        screen.finish_submit(Ok(()));
                    }
                }
                Err(msg) => {
                    if let Screen::Settings(screen) = &mut self.screen {
                        screen.finish_submit(Err(msg));
                    }
                }
            },
            BgEvent::GuildsInitial(result) => {
                if let Screen::Guilds(s) = &mut self.screen {
                    s.apply_initial(result);
                }
            }
            BgEvent::GuildsMore(result) => {
                if let Screen::Guilds(s) = &mut self.screen {
                    s.apply_more(result);
                }
            }
            BgEvent::GuildInfo { slug, result } => {
                if let Screen::Guild(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_guild(result);
                    }
                }
            }
            BgEvent::GuildThreadsInitial { slug, result } => {
                if let Screen::Guild(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_threads_initial(result);
                    }
                }
            }
            BgEvent::GuildThreadsMore { slug, result } => {
                if let Screen::Guild(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_threads_more(result);
                    }
                }
            }
            BgEvent::GuildMembersInitial { slug, result } => {
                if let Screen::Guild(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_members_initial(result);
                    }
                }
            }
            BgEvent::GuildMembersMore { slug, result } => {
                if let Screen::Guild(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_members_more(result);
                    }
                }
            }
            BgEvent::GuildJoined { slug, result } => {
                if let Screen::Guild(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_joined(result);
                    }
                }
            }
            BgEvent::GuildLeft { slug, result } => {
                if let Screen::Guild(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_left(result);
                    }
                }
            }
            BgEvent::GuildThreadCreated { slug, result } => match result {
                Ok(_post_id) => {
                    if matches!(self.screen, Screen::Compose(_)) {
                        self.pop_screen();
                    }
                    // If we're back on the guild that got the thread, reload it.
                    let on_guild = matches!(&self.screen, Screen::Guild(s) if s.slug == slug);
                    if on_guild {
                        if let Screen::Guild(s) = &mut self.screen {
                            s.tab = GuildTab::Threads;
                            s.loading = true;
                        }
                        self.spawn_guild_tab_initial(&slug, GuildTab::Threads);
                    }
                }
                Err(msg) => {
                    if let Screen::Compose(s) = &mut self.screen {
                        s.finish_submit(Err(msg));
                    }
                }
            },
            BgEvent::ImageFetched {
                post_id,
                url,
                result,
            } => match result {
                Ok(bytes) => {
                    if let (Some(picker), Screen::PostDetail(s)) = (&self.picker, &self.screen) {
                        if s.entry.post_id == post_id {
                            match image::load_from_memory(&bytes) {
                                Ok(img) => s.set_image(picker.new_resize_protocol(img)),
                                Err(e) => tracing::debug!(error = %e, url, "image decode failed"),
                            }
                        }
                    }
                }
                Err(msg) => tracing::debug!(error = msg, url, "image fetch failed"),
            },
        }
    }

    // Navigation helpers ------------------------------------------------------

    fn push_screen(&mut self, new: Screen) {
        let prev = std::mem::replace(&mut self.screen, new);
        self.back_stack.push(prev);
    }

    fn pop_screen(&mut self) {
        if let Some(prev) = self.back_stack.pop() {
            self.screen = prev;
        }
        // Pop from the bottom of the stack (a root screen) is a no-op now;
        // the user picks Quit explicitly from the menu instead.
    }

    /// Clear session state and return to the login screen. Used by the menu's
    /// `Logout` action (also reachable when an API call repeatedly fails and the
    /// user wants to bail).
    async fn logout(&mut self) {
        self.client.clear_tokens().await;
        if let Err(e) = crate::session::Session::clear() {
            tracing::warn!(error = %e, "session clear failed");
        }
        self.back_stack.clear();
        self.current_root = None;
        self.unread_count = 0;
        self.offline = false;
        self.toast = None;
        let email = self.last_email.clone();
        self.screen = Screen::Login(LoginScreen::new(email));
    }

    /// React to a connectivity/auth signal distilled from a background error
    /// (see [`ApiSignal`]). This is the single funnel the three reliability
    /// behaviors hang off of.
    fn handle_api_signal(&mut self, signal: ApiSignal) {
        match signal {
            ApiSignal::Offline => {
                // Only nudge the poller on the online→offline *transition*. The
                // poller emits Offline itself on each failed retry, so notifying
                // on every signal would defeat its 5s backoff and busy-loop a
                // down connection. The first transition (often from another
                // task's request) wakes it to start fast-checking; from there it
                // self-paces until a poll succeeds and clears the marker.
                if !self.offline {
                    self.offline = true;
                    self.offline_notify.notify_one();
                }
            }
            ApiSignal::Online => self.offline = false,
            ApiSignal::RateLimited { retry_after_secs } => {
                // Getting a rate-limit *response* proves we're online.
                self.offline = false;
                self.toast = Some(Toast::rate_limited(retry_after_secs));
            }
            ApiSignal::SessionExpired => {
                // Ignore once we're already on login (we've logged out), so a
                // burst of in-flight 401s doesn't loop.
                if !self.screen.is_login() {
                    self.pending_logout =
                        Some("Session expired — please sign in again.".to_string());
                }
            }
        }
    }

    /// Expire the active toast once its countdown elapses. Driven by the 1s
    /// ticker while a toast is shown.
    fn tick_toast(&mut self) {
        if self.toast.as_ref().is_some_and(Toast::is_expired) {
            self.toast = None;
        }
    }

    /// If a background call proved the session is dead, log out and surface the
    /// reason on the login screen. Runs in the async loop because `logout`
    /// awaits; the sync bg handler only sets `pending_logout`.
    async fn apply_pending_logout(&mut self) {
        if let Some(reason) = self.pending_logout.take() {
            self.logout().await;
            if let Screen::Login(s) = &mut self.screen {
                s.error = Some(reason);
            }
        }
    }

    /// Advance to the next theme palette, apply it live, and persist the choice
    /// to local prefs so it survives restarts. A failed save is non-fatal.
    fn cycle_theme(&mut self) {
        self.theme_kind = self.theme_kind.next();
        self.theme = self.theme_kind.theme();
        let prefs = crate::prefs::Prefs {
            theme: Some(self.theme_kind.name().to_string()),
        };
        if let Err(e) = prefs.save() {
            tracing::warn!(error = %e, "theme prefs save failed");
        }
    }

    fn goto_root(&mut self, target: RootKind) {
        self.back_stack.clear();
        self.current_root = Some(target);
        match target {
            RootKind::Feed => {
                self.screen = Screen::Feed(FeedScreen::new());
                self.spawn_feed_initial();
            }
            RootKind::Notifications => {
                let mut s = NotificationsScreen::new();
                s.filter = NotificationsFilter::All;
                self.screen = Screen::Notifications(s);
                self.spawn_notifications_initial(NotificationsFilter::All);
            }
            RootKind::Bookmarks => {
                self.screen = Screen::Bookmarks(BookmarksScreen::new());
                self.spawn_bookmarks_initial();
            }
            RootKind::Topics => {
                self.screen = Screen::Topics(TopicsScreen::new());
                self.spawn_topics_load();
            }
            RootKind::Profile => {
                self.screen = Screen::Profile(ProfileScreen::new_own());
                self.spawn_profile_user_me();
            }
            RootKind::Journal => {
                self.screen = Screen::Journal(JournalScreen::new());
                self.spawn_notes_initial();
            }
            RootKind::Settings => {
                self.screen = Screen::Settings(SettingsScreen::new());
                self.spawn_settings_load();
            }
            RootKind::Guilds => {
                self.screen = Screen::Guilds(GuildsScreen::new());
                self.spawn_guilds_initial();
            }
        }
    }

    // Spawn helpers -----------------------------------------------------------

    fn spawn_login(&self, email: String, password: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .login(&email, &password)
                .await
                .map(|_| email)
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::LoginResult(result));
        });
    }

    fn spawn_feed_initial(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_entries(None, None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::FeedInitial(result));
        });
    }

    fn spawn_feed_more(&self, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_entries(cursor.as_deref(), None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::FeedMore(result));
        });
    }

    fn spawn_notifications_initial(&self, filter: NotificationsFilter) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_notifications(None, None, filter, &[])
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::NotificationsInitial(result));
        });
    }

    fn spawn_notifications_more(&self, filter: NotificationsFilter, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_notifications(cursor.as_deref(), None, filter, &[])
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::NotificationsMore(result));
        });
    }

    fn spawn_mark_notification_read(&self, notification_id: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            match client.mark_notification_read(&notification_id).await {
                Ok(()) => {
                    let _ = tx.send(BgEvent::NotificationMarkedRead);
                }
                Err(e) => {
                    let msg = note_api_err(&tx, e);
                    tracing::warn!(error = %msg, notification_id, "mark_notification_read failed");
                }
            }
        });
    }

    fn spawn_mark_all_notifications_read(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            match client.mark_all_notifications_read().await {
                Ok(_) => {
                    let _ = tx.send(BgEvent::AllNotificationsMarked);
                }
                Err(e) => {
                    let msg = note_api_err(&tx, e);
                    tracing::warn!(error = %msg, "mark_all_notifications_read failed");
                }
            }
        });
    }

    fn spawn_bookmarks_initial(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_bookmarks(None, None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::BookmarksInitial(result));
        });
    }

    fn spawn_bookmarks_more(&self, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_bookmarks(cursor.as_deref(), None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::BookmarksMore(result));
        });
    }

    fn spawn_delete_bookmark(&self, bookmark_id: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            match client.delete_bookmark(&bookmark_id).await {
                Ok(()) => {
                    let _ = tx.send(BgEvent::BookmarkRemoved);
                }
                Err(e) => {
                    let msg = note_api_err(&tx, e);
                    tracing::warn!(error = %msg, bookmark_id, "delete_bookmark failed");
                }
            }
        });
    }

    fn spawn_topics_load(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.list_topics().await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::TopicsLoaded(result));
        });
    }

    fn spawn_topic_feed_initial(&self, slug: &str) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            let result = client
                .list_topic_posts(&slug, None, None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::TopicFeedInitial { slug, result });
        });
    }

    fn spawn_topic_feed_more(&self, slug: &str, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            let result = client
                .list_topic_posts(&slug, cursor.as_deref(), None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::TopicFeedMore { slug, result });
        });
    }

    fn spawn_guilds_initial(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.list_guilds(None, None).await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::GuildsInitial(result));
        });
    }

    fn spawn_guilds_more(&self, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_guilds(cursor.as_deref(), None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::GuildsMore(result));
        });
    }

    /// Open a guild: fetch its header/membership and the first page of threads.
    fn spawn_guild_open(&self, slug: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let info_slug = slug.clone();
        tokio::spawn(async move {
            let result = client.get_guild(&info_slug).await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::GuildInfo {
                slug: info_slug,
                result,
            });
        });
        self.spawn_guild_tab_initial(&slug, GuildTab::Threads);
    }

    fn spawn_guild_tab_initial(&self, slug: &str, tab: GuildTab) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            match tab {
                GuildTab::Threads => {
                    let result = client
                        .list_guild_threads(&slug, None, None)
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::GuildThreadsInitial { slug, result });
                }
                GuildTab::Members => {
                    let result = client
                        .list_guild_members(&slug, None, None)
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::GuildMembersInitial { slug, result });
                }
            }
        });
    }

    fn spawn_guild_tab_more(&self, slug: &str, tab: GuildTab, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let slug = slug.to_string();
        tokio::spawn(async move {
            match tab {
                GuildTab::Threads => {
                    let result = client
                        .list_guild_threads(&slug, cursor.as_deref(), None)
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::GuildThreadsMore { slug, result });
                }
                GuildTab::Members => {
                    let result = client
                        .list_guild_members(&slug, cursor.as_deref(), None)
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::GuildMembersMore { slug, result });
                }
            }
        });
    }

    fn spawn_guild_join(&self, slug: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.join_guild(&slug).await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::GuildJoined { slug, result });
        });
    }

    fn spawn_guild_leave(&self, slug: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.leave_guild(&slug).await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::GuildLeft { slug, result });
        });
    }

    fn spawn_detail_replies_initial(&self, post_id: &str) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let post_id = post_id.to_string();
        tokio::spawn(async move {
            let result = client
                .list_replies(&post_id, None, None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::DetailRepliesInitial(result));
        });
    }

    fn spawn_detail_replies_more(&self, post_id: &str, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let post_id = post_id.to_string();
        tokio::spawn(async move {
            let result = client
                .list_replies(&post_id, cursor.as_deref(), None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::DetailRepliesMore(result));
        });
    }

    fn spawn_open_post_detail_by_id(&self, post_id: String, highlight_reply_id: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.get_entry(&post_id).await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::OpenPostDetail {
                result,
                highlight_reply_id,
            });
        });
    }

    /// Push a post-detail screen for `entry`, load its replies, and (when the
    /// terminal supports graphics) start fetching its first image.
    fn enter_post_detail(&mut self, entry: Entry, highlight_reply_id: Option<String>) {
        let id = entry.post_id.clone();
        let first_image = if self.picker.is_some() {
            super::images::entry_image_urls(&entry).into_iter().next()
        } else {
            None
        };
        let mut screen = PostDetailScreen::new(entry);
        screen.highlight_reply_id = highlight_reply_id;
        self.push_screen(Screen::PostDetail(screen));
        self.spawn_detail_replies_initial(&id);
        if let Some(url) = first_image {
            self.spawn_fetch_image(id, url);
        }
    }

    fn spawn_fetch_image(&self, post_id: String, url: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.fetch_image(&url).await.map_err(|e| e.to_string());
            let _ = tx.send(BgEvent::ImageFetched {
                post_id,
                url,
                result,
            });
        });
    }

    fn spawn_unread_count_once(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            match client.unread_notification_count().await {
                Ok(n) => {
                    let _ = tx.send(BgEvent::UnreadCount(n));
                }
                Err(e) => {
                    let msg = note_api_err(&tx, e);
                    tracing::debug!(error = %msg, "unread_count one-shot failed");
                }
            }
        });
    }

    fn spawn_profile_user_me(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.get_own_profile().await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::ProfileUser(result));
        });
    }

    fn spawn_profile_user(&self, username: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .get_profile(&username)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::ProfileUser(result));
        });
    }

    fn spawn_profile_tab_fetch(
        &self,
        tab: ProfileTab,
        username: String,
        user_id: Option<String>,
        cursor: Option<String>,
    ) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let more = cursor.is_some();
        tokio::spawn(async move {
            match tab {
                ProfileTab::Info => {} // Info uses the User fetch.
                ProfileTab::Posts => {
                    let result = client
                        .list_user_posts(&username, cursor.as_deref(), None)
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::ProfilePosts { more, result });
                }
                ProfileTab::Replies => {
                    let result = client
                        .list_user_replies(&username, cursor.as_deref(), None)
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::ProfileReplies { more, result });
                }
                ProfileTab::Followers => {
                    let result = client
                        .list_follows(
                            FollowsDirection::Followers,
                            user_id.as_deref(),
                            cursor.as_deref(),
                            None,
                        )
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::ProfileFollowers { more, result });
                }
                ProfileTab::Following => {
                    let result = client
                        .list_follows(
                            FollowsDirection::Following,
                            user_id.as_deref(),
                            cursor.as_deref(),
                            None,
                        )
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::ProfileFollowing { more, result });
                }
            }
        });
    }

    fn spawn_toggle_follow(&self, user_id: String, follow_id: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            if let Some(fid) = follow_id {
                // Currently following — unfollow.
                let result = client
                    .unfollow(&fid)
                    .await
                    .map(|()| None)
                    .map_err(|e| note_api_err(&tx, e));
                let _ = tx.send(BgEvent::ProfileFollowToggled(result));
            } else {
                // Not following — follow.
                let result = client
                    .follow_user(&user_id)
                    .await
                    .map(Some)
                    .map_err(|e| note_api_err(&tx, e));
                let _ = tx.send(BgEvent::ProfileFollowToggled(result));
            }
        });
    }

    fn spawn_update_own_profile(&self, update: ProfileUpdate) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .update_own_profile(&update)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::ProfileUpdated(result));
        });
    }

    /// Launch $EDITOR on a tempfile (synchronously, off the runtime thread),
    /// then push a `Compose` screen with the resulting content. Empty content
    /// cancels the flow.
    async fn start_compose(&mut self, kind: ComposeKind, prefill: String) {
        let initial = if prefill.is_empty() {
            String::new()
        } else {
            prefill
        };
        let editor_result = tokio::task::spawn_blocking(move || launch_editor(&initial, ".md"))
            .await
            .map_err(|e| format!("editor task panicked: {e}"))
            .and_then(|r| r.map_err(|e| e.to_string()));

        let content = match editor_result {
            Ok(c) => c,
            Err(msg) => {
                tracing::warn!(error = %msg, "compose: editor failed");
                return;
            }
        };
        if content.trim().is_empty() {
            tracing::debug!("compose: empty content, cancelled");
            return;
        }
        let screen = ComposeScreen::new(kind, content);
        self.push_screen(Screen::Compose(screen));
    }

    fn spawn_compose_submit(&self) {
        let (kind, content, title, topics, is_public, is_nsfw) = match &self.screen {
            Screen::Compose(s) => (
                s.kind.clone(),
                s.content.clone(),
                s.title_to_send(),
                s.parse_topics(),
                s.is_public,
                s.is_nsfw,
            ),
            _ => return,
        };
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            match kind {
                ComposeKind::NewEntry => {
                    let result = client
                        .create_entry(
                            &content,
                            title.as_deref(),
                            None,
                            &topics,
                            is_public,
                            is_nsfw,
                        )
                        .await
                        .map(|created| created.post_id)
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::EntryCreated(result));
                }
                ComposeKind::Reply {
                    post_id,
                    parent_reply_id,
                } => {
                    let result = client
                        .create_reply(&post_id, &content, parent_reply_id.as_deref())
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::ReplyCreated(result));
                }
                ComposeKind::NewNote => {
                    let result = client
                        .create_note(&content, &topics)
                        .await
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::NoteCreated(result));
                }
                ComposeKind::UpdateNote { note_id } => {
                    let result = client
                        .update_note(&note_id, &content, &topics)
                        .await
                        .map(|()| note_id)
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::NoteUpdated(result));
                }
                ComposeKind::GuildThread { guild_slug } => {
                    let result = client
                        .create_guild_thread(&guild_slug, &content, title.as_deref(), None, &topics)
                        .await
                        .map(|created| created.post_id)
                        .map_err(|e| note_api_err(&tx, e));
                    let _ = tx.send(BgEvent::GuildThreadCreated {
                        slug: guild_slug,
                        result,
                    });
                }
            }
        });
    }

    fn spawn_delete_entry(&self, post_id: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .delete_entry(&post_id)
                .await
                .map(|()| post_id)
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::EntryDeleted(result));
        });
    }

    async fn start_compose_note_edit(
        &mut self,
        note_id: String,
        prefill: String,
        topics: Vec<String>,
    ) {
        let editor_result = tokio::task::spawn_blocking(move || launch_editor(&prefill, ".md"))
            .await
            .map_err(|e| format!("editor task panicked: {e}"))
            .and_then(|r| r.map_err(|e| e.to_string()));
        let content = match editor_result {
            Ok(c) => c,
            Err(msg) => {
                tracing::warn!(error = %msg, "compose-note-edit: editor failed");
                return;
            }
        };
        if content.trim().is_empty() {
            return;
        }
        let mut screen = ComposeScreen::new(ComposeKind::UpdateNote { note_id }, content);
        screen.topics_input = topics.join(", ");
        self.push_screen(Screen::Compose(screen));
    }

    fn spawn_notes_initial(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_notes(None, None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::NotesInitial(result));
        });
    }

    fn spawn_notes_more(&self, cursor: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_notes(cursor.as_deref(), None)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::NotesMore(result));
        });
    }

    fn spawn_note_revisions(&self, note_id: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_note_revisions(&note_id, None, None)
                .await
                .map(|(items, _cursor)| items)
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::NoteRevisions { note_id, result });
        });
    }

    fn spawn_settings_load(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client.get_settings().await.map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::SettingsLoaded(result));
        });
    }

    fn spawn_settings_save(&self, update: SettingsUpdate) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .update_settings(&update)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::SettingsSaved(result));
        });
    }

    fn spawn_delete_note(&self, note_id: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            match client.delete_note(&note_id).await {
                Ok(()) => {
                    let _ = tx.send(BgEvent::NoteDeleted);
                }
                Err(e) => {
                    let msg = note_api_err(&tx, e);
                    tracing::warn!(error = %msg, note_id, "delete_note failed");
                }
            }
        });
    }

    fn spawn_unread_count_poller(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let wake = self.offline_notify.clone();
        tokio::spawn(async move {
            // Brief settle delay so the initial render lands before the first poll.
            tokio::time::sleep(Duration::from_secs(3)).await;
            // Doubles as a connectivity / session heartbeat: a successful poll
            // clears the offline marker, while a failure is funnelled through
            // `note_api_err` exactly like every instrumented request — so a
            // transport drop raises the offline marker and a terminal 401 logs
            // an *idle* user out (the poller is their only traffic). While
            // offline we poll faster (5s vs 60s), and a `wake` notification cuts
            // the sleep short so the marker clears promptly on reconnect.
            loop {
                let next_delay = match client.unread_notification_count().await {
                    Ok(n) => {
                        if tx.send(BgEvent::UnreadCount(n)).is_err() {
                            return; // app gone
                        }
                        60
                    }
                    Err(e) => {
                        let transport = e.is_transport();
                        let msg = note_api_err(&tx, e);
                        tracing::debug!(error = %msg, "unread_count poll failed");
                        if transport {
                            5
                        } else {
                            60
                        }
                    }
                };
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(next_delay)) => {}
                    () = wake.notified() => {}
                }
            }
        });
    }
}

/// Build a synthetic key-press event (used to translate mouse-wheel scrolls into
/// the same one-step navigation as the arrow keys).
fn synthetic_key(code: KeyCode) -> event::KeyEvent {
    event::KeyEvent::new(code, KeyModifiers::empty())
}

/// Block on a future from within the App run-loop task. Safe here because
/// `Client::tokens()` only reads a `RwLock` — it does not itself await on
/// anything that would re-enter the runtime.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
}

/// Classify a background `ApiError`, emit the matching [`ApiSignal`] to the main
/// loop, and flatten the error to its display string for the per-screen path.
/// This replaces the bare `.map_err(|e| note_api_err(&tx, e))` at every authenticated
/// API spawn site, so connectivity/auth conditions reach the central funnel
/// without disturbing any screen's `Result<_, String>` handling.
fn note_api_err(tx: &mpsc::UnboundedSender<BgEvent>, e: ApiError) -> String {
    let signal = if e.is_transport() {
        ApiSignal::Offline
    } else if e.is_rate_limited() {
        ApiSignal::RateLimited {
            retry_after_secs: e.retry_after_secs().unwrap_or(5),
        }
    } else if e.is_unauthorized() {
        // Any 401 that reaches us has already outlived the client's
        // refresh-once, so the session is genuinely dead.
        ApiSignal::SessionExpired
    } else {
        // A server-origin error (404, validation, …) still proves we're online.
        ApiSignal::Online
    };
    let _ = tx.send(BgEvent::ApiSignal(signal));
    e.to_string()
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 100 {
        line.to_string()
    } else {
        let truncated: String = line.chars().take(99).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a rendered test buffer into one string for substring assertions.
    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        buf.content.iter().map(|c| c.symbol()).collect()
    }

    fn test_app() -> App {
        let client = cs_api::Client::new().expect("client builds");
        App::with_theme(client, "you@example.com".into(), ThemeKind::Cyber)
    }

    fn render_to_string(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|f| app.render(f)).expect("draw");
        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn menu_overlay_is_drawn_over_the_login_screen() {
        // Regression: opening the Esc menu on the login screen used to be
        // skipped by an early return in render(), so keystrokes routed to an
        // invisible menu and the UI appeared frozen.
        let mut app = test_app();
        assert!(app.screen.is_login());
        app.menu = Some(MenuOverlay::build(false, false, "cyber"));
        let text = render_to_string(&app);
        assert!(text.contains("menu"), "menu title not drawn: {text:?}");
        assert!(text.contains("Quit"), "Quit item not drawn");
        assert!(text.contains("Cancel"), "Cancel item not drawn");
    }

    #[test]
    fn login_screen_without_menu_draws_no_menu_chrome() {
        let app = test_app();
        let text = render_to_string(&app);
        assert!(!text.contains("Cancel"), "menu chrome leaked with no menu open");
    }

    fn key_event(code: KeyCode) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::empty()))
    }

    #[tokio::test]
    async fn question_mark_toggles_help_on_read_screens() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new()); // not a text-input screen
        app.handle_terminal_event(key_event(KeyCode::Char('?'))).await;
        assert!(app.help, "? should open help on the feed");
        app.handle_terminal_event(key_event(KeyCode::Char('j'))).await;
        assert!(!app.help, "any key should dismiss help");
    }

    #[tokio::test]
    async fn question_mark_is_text_on_the_login_screen() {
        let mut app = test_app(); // starts on Login (text input)
        app.handle_terminal_event(key_event(KeyCode::Char('?'))).await;
        assert!(!app.help, "? must not open help while typing into login");
    }

    #[test]
    fn help_overlay_renders_over_a_screen() {
        let mut app = test_app();
        app.help = true;
        let text = render_to_string(&app);
        assert!(text.contains("help"), "help title not drawn");
        assert!(text.contains("Sections"), "help body not drawn");
    }

    #[tokio::test]
    async fn digit_keys_navigate_from_read_screens() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.handle_terminal_event(key_event(KeyCode::Char('2'))).await;
        assert!(
            matches!(app.screen, Screen::Notifications(_)),
            "2 should switch to notifications from a read screen"
        );
    }

    #[tokio::test]
    async fn digit_keys_do_not_navigate_away_from_text_input_screens() {
        let mut app = test_app();
        app.screen = Screen::Settings(SettingsScreen::new());
        app.current_root = Some(RootKind::Settings);
        app.handle_terminal_event(key_event(KeyCode::Char('2'))).await;
        assert!(
            matches!(app.screen, Screen::Settings(_)),
            "a digit on a text-input screen must reach the screen, not navigate"
        );
    }

    // --- Phase 7.3: reliability signals -------------------------------------

    fn drain_signal(rx: &mut mpsc::UnboundedReceiver<BgEvent>) -> ApiSignal {
        match rx.try_recv() {
            Ok(BgEvent::ApiSignal(s)) => s,
            other => panic!("expected an ApiSignal, got {other:?}"),
        }
    }

    #[test]
    fn note_api_err_classifies_and_preserves_message() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        // Rate limited → carries the retry hint; the display string still flows
        // through to the per-screen path unchanged.
        let msg = note_api_err(&tx, ApiError::RateLimited { retry_after_secs: 12 });
        assert!(msg.contains("retry after 12s"), "display string lost: {msg}");
        assert!(matches!(
            drain_signal(&mut rx),
            ApiSignal::RateLimited { retry_after_secs: 12 }
        ));

        // Unauthorized → terminal session-expiry (refresh already failed upstream).
        let _ = note_api_err(&tx, ApiError::Unauthorized);
        assert!(matches!(drain_signal(&mut rx), ApiSignal::SessionExpired));

        // A server-origin error proves we're online.
        let _ = note_api_err(&tx, ApiError::NotImplemented);
        assert!(matches!(drain_signal(&mut rx), ApiSignal::Online));
    }

    #[test]
    fn offline_signal_toggles_indicator() {
        let mut app = test_app();
        app.handle_api_signal(ApiSignal::Offline);
        assert!(app.offline);
        app.handle_api_signal(ApiSignal::Online);
        assert!(!app.offline);
    }

    #[test]
    fn rate_limited_signal_shows_toast_and_is_online() {
        let mut app = test_app();
        app.offline = true;
        app.handle_api_signal(ApiSignal::RateLimited { retry_after_secs: 8 });
        assert!(app.toast.is_some(), "rate-limit signal should raise a toast");
        assert!(!app.offline, "a rate-limit response proves we're online");
    }

    #[test]
    fn unread_count_event_clears_offline() {
        let mut app = test_app();
        app.offline = true;
        app.handle_bg_event(BgEvent::UnreadCount(4));
        assert!(!app.offline, "a successful poll is an online heartbeat");
        assert_eq!(app.unread_count, 4);
    }

    #[test]
    fn session_expiry_arms_logout_only_when_authenticated() {
        // On the login screen the signal is a no-op (we're already logged out).
        let mut app = test_app();
        assert!(app.screen.is_login());
        app.handle_api_signal(ApiSignal::SessionExpired);
        assert!(app.pending_logout.is_none());

        // On an authenticated screen it arms a logout carrying a reason.
        app.screen = Screen::Feed(FeedScreen::new());
        app.handle_api_signal(ApiSignal::SessionExpired);
        assert!(app
            .pending_logout
            .as_deref()
            .is_some_and(|r| r.contains("expired")));
    }

    #[test]
    fn offline_marker_renders_in_tab_bar() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.offline = true;
        let text = render_to_string(&app);
        assert!(
            text.contains("offline"),
            "offline marker missing from tab bar: {text:?}"
        );
    }

    #[test]
    fn rate_limit_toast_renders_over_a_screen() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.toast = Some(Toast::rate_limited(10));
        let text = render_to_string(&app);
        assert!(
            text.contains("rate limited"),
            "toast text missing: {text:?}"
        );
    }

    #[test]
    fn tick_does_not_clear_a_live_toast() {
        let mut app = test_app();
        app.toast = Some(Toast::rate_limited(30));
        app.tick_toast();
        assert!(app.toast.is_some(), "a live toast must survive a tick");
    }
}

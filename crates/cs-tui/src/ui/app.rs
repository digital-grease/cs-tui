//! Top-level App state and event loop.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use cs_api::{
    ApiError, Bookmark, Client, EndpointKey, Entry, Follow, FollowsDirection, Guild,
    GuildMembership, GuildThread, JoinedGuild, Note, NoteRevision, Notification, NotificationType,
    NotificationsFilter, ProfileUpdate, Reply, Settings, SettingsUpdate, Topic, User,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::DefaultTerminal;
use ratatui_image::picker::Picker;
use tokio::sync::{mpsc, Notify};
use tokio::time::MissedTickBehavior;

use super::bookmarks::{BookmarksIntent, BookmarksScreen};
use super::compose::{launch_editor, ComposeIntent, ComposeKind, ComposeScreen};
use super::edit_profile::{EditProfileIntent, EditProfileScreen};
use super::editor::{EditorIntent, EditorPurpose, EditorScreen};
use super::feed::{FeedIntent, FeedScreen, HeadUpdate};
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
use super::shuffle::ShufflePool;
use super::theme::{ColorMode, Theme, ThemeKind};
use super::toast::Toast;
use super::topic_feed::{TopicFeedIntent, TopicFeedScreen};
use super::topics::{TopicsIntent, TopicsScreen};
use crate::session::Session;

/// A shuffled track that ends with less than this much reported progress is
/// counted as a failed play (yt-dlp resolution errors kill mpv near-instantly,
/// before any position report lands) — but only when the wall-clock check
/// below agrees, since a broken IPC socket also leaves the position at zero
/// for a track that actually played in full.
const SUSPECT_END_SECS: f64 = 5.0;

/// Wall-clock corroboration for [`SUSPECT_END_SECS`]: an end only counts as a
/// failure when the mpv process also lived for less than this long.
const SUSPECT_WALL_TIME: Duration = Duration::from_secs(10);

/// Turn shuffle off after this many consecutive suspect endings, so a mass
/// failure (network down, yt-dlp broken) can't spin mpv in a loop.
const SUSPECT_END_LIMIT: u8 = 3;

/// How many played tracks `<` / `>` can navigate back through.
const PLAY_HISTORY_CAP: usize = 50;

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
    /// The newest feed page from the background poll — prepended without moving
    /// the user's scroll position. Only emitted while the feed is on screen.
    FeedHead(Vec<Entry>),
    NotificationsInitial(Result<(Vec<Notification>, Option<String>), String>),
    NotificationsMore(Result<(Vec<Notification>, Option<String>), String>),
    NotificationMarkedRead,
    /// A single mark-read failed; roll back the optimistic local change.
    NotificationMarkFailed {
        notification_id: String,
    },
    AllNotificationsMarked,
    /// Mark-all-read failed; the optimistic "all read" must be resynced.
    AllNotificationsMarkFailed,
    BookmarksInitial(Result<(Vec<Bookmark>, Option<String>), String>),
    BookmarksMore(Result<(Vec<Bookmark>, Option<String>), String>),
    BookmarkRemoved,
    /// A bookmark removal failed; the optimistic local removal must be undone.
    BookmarkRemoveFailed,
    /// Result of bookmarking a post from the feed / post detail.
    BookmarkCreated(Result<String, String>),
    /// The jukebox player for generation `token` stopped (track ended, stopped,
    /// or mpv exited). Clears the now-playing bar if it's still the current one.
    PlaybackEnded {
        token: u64,
    },
    /// A progress update (position/duration in seconds) polled from mpv for
    /// generation `token`. Feeds the now-playing bar's time readout and gauge.
    PlaybackProgress {
        token: u64,
        position_secs: f64,
        duration_secs: f64,
    },
    /// Tracks collected by the background shuffle refill (a bounded walk of the
    /// global feed, filtered client-side for audio attachments), plus the feed
    /// cursor where the next walk should resume. `Err` only when a whole walk
    /// produced nothing (a partial walk that found tracks reports `Ok`).
    /// `epoch` guards against a walk superseded by logout or shuffle-off.
    ShuffleTracks {
        epoch: u64,
        result: Result<(Vec<super::audio::JukeboxTrack>, Option<String>), String>,
    },
    /// A page from the background topics warm-up. `complete` is true on the last
    /// page (or when the fill gives up); `epoch` guards against a superseded run.
    TopicsPrefetched {
        epoch: u64,
        topics: Vec<Topic>,
        complete: bool,
    },
    /// The user's followed/muted topic slugs, fetched from settings.
    TopicPrefsLoaded(Result<(Vec<String>, Vec<String>), String>),
    /// Result of a follow/mute PATCH; on failure we resync from the server.
    TopicPrefsSaved(Result<(), String>),
    TopicFeedInitial {
        slug: String,
        result: Result<(Vec<Entry>, Option<String>), String>,
    },
    TopicFeedMore {
        slug: String,
        result: Result<(Vec<Entry>, Option<String>), String>,
    },
    DetailRepliesInitial {
        post_id: String,
        result: Result<(Vec<Reply>, Option<String>), String>,
    },
    DetailRepliesMore {
        post_id: String,
        result: Result<(Vec<Reply>, Option<String>), String>,
    },
    OpenPostDetail {
        result: Result<Entry, String>,
        highlight_reply_id: Option<String>,
    },
    /// Current watch state for an open post detail, fetched on open. `Err` is
    /// ignored (the indicator just stays unknown); connectivity is noted via the
    /// usual signal path.
    WatchStatus {
        post_id: String,
        result: Result<bool, String>,
    },
    /// Result of a watch/unwatch toggle. `Ok(watching)` is the authoritative new
    /// state; `Err` rolls back the optimistic flip and warns.
    WatchToggled {
        post_id: String,
        result: Result<bool, String>,
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
    /// Ok carries (post_id, final slug) — the server may suffix the slug on
    /// collision, so we echo what was actually stored.
    EntryCreated(Result<(String, Option<String>), String>),
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
    Editor(EditorScreen),
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
        match self {
            Screen::Login(_) | Screen::Compose(_) | Screen::EditProfile(_) | Screen::Editor(_) => {
                true
            }
            // The topics search box captures printable keys while open.
            Screen::Topics(s) => s.is_filtering(),
            // Settings has only toggles, cyclable choices, and read-only fields —
            // no free text — so global nav (1-8 / ←→) always stays active there.
            _ => false,
        }
    }
}

/// Intent captured from a screen before we drop its borrow on `self.screen`.
#[derive(Debug, PartialEq)]
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
    ToggleTopicFollow {
        slug: String,
    },
    ToggleTopicMute {
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
    PinPost {
        post_id: String,
        pin: bool,
    },
    StartComposeEntry,
    OpenUrl {
        url: String,
    },
    /// `p` pressed on a screen: start/switch/toggle based on the focused track.
    /// The other player controls (pause/stop/volume) are handled inline in the
    /// global key block, not as actions, since they don't touch a screen.
    PlayPressed {
        track: Option<super::audio::JukeboxTrack>,
    },
    BookmarkPost {
        post_id: String,
    },
    BookmarkReply {
        reply_id: String,
    },
    /// Watch (`watch == true`) or unwatch the given thread.
    SetThreadWatch {
        post_id: String,
        watch: bool,
    },
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
    ComposeReEdit,
    /// Built-in editor: Ctrl+D accepted the body.
    EditorSave,
    /// Built-in editor: Esc/Ctrl+C discarded.
    EditorCancel,
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
    /// User-defined palette from `config.toml`, if any. Enables the `Custom`
    /// theme in the cycle and resolves `ThemeKind::Custom`.
    custom_theme: Option<Theme>,
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
    /// Runtime gate for inline image rendering, toggled with `i`. Starts on.
    /// Turning it off forces a screen clear and suppresses both fetching and
    /// drawing — the escape hatch when a terminal over-reports its graphics
    /// support and an image post renders as a screenful of garbage. Independent
    /// of `picker`: effective rendering needs `picker.is_some() && images_on`.
    images_on: bool,
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
    /// True while the Feed is the active screen — gates the background feed
    /// poller so it only fetches while the user is actually viewing the feed.
    feed_active: Arc<AtomicBool>,
    /// Whether the long-lived feed head-poller has been spawned (mirrors
    /// `poller_started`; outlives logout).
    feed_poller_started: bool,
    /// When set, the input reader thread stops touching crossterm so an external
    /// `$EDITOR` owns the terminal exclusively (otherwise the reader steals the
    /// editor's keystrokes, which then replay onto the TUI when it exits).
    input_paused: Arc<AtomicBool>,
    /// Request a full repaint on the next frame. Set after an external editor
    /// re-enters the alternate screen, since ratatui's diff renderer would
    /// otherwise skip cells (e.g. the background fill) it believes unchanged,
    /// leaving the editor's blank screen showing through.
    force_clear: bool,
    /// Session cache of the topics list, warmed in the background from login by
    /// a gentle paginated fill (the topics section can run to thousands, so we
    /// trickle them in rather than blocking a search on loading every page). The
    /// topics screen is a pure view over this; search filters it.
    topics_cache: Vec<Topic>,
    /// Whether the background warm-up has loaded every topic.
    topics_complete: bool,
    /// Bumped on refresh to invalidate the in-flight warm-up task (its remaining
    /// pages are discarded by epoch check) before a fresh one starts.
    topics_epoch: Arc<AtomicU64>,
    /// The user's followed / muted topic slugs (from settings), used for the
    /// topics-list markers and the follow/mute toggles. Loaded lazily the first
    /// time the topics section is opened.
    topic_follows: Vec<String>,
    topic_mutes: Vec<String>,
    /// True once we've fetched the follow/mute prefs (reset to retry on failure).
    topic_prefs_loaded: bool,
    /// What the terminal can render (truecolor / 256 / NO_COLOR), detected once
    /// at startup; every resolved theme is adapted to it.
    color_mode: ColorMode,
    /// The currently playing jukebox track (mpv background player), if any.
    /// Drives the now-playing bar and the playback control keys.
    now_playing: Option<super::player::Handle>,
    /// Memoized `mpv --version` probe: `None` until the first play attempt, then
    /// the cached answer. Keeps tests and non-music sessions from spawning mpv.
    mpv_available: Option<bool>,
    /// Memoized yt-dlp/youtube-dl probe (mpv needs one for YouTube URLs).
    ytdlp_available: Option<bool>,
    /// Generation counter for playback; each new track gets a fresh token so a
    /// previous track's exit can't clear the bar for the current one.
    next_play_token: u64,
    /// Volume carried across tracks (0..=130), updated by the volume keys.
    player_volume: i64,
    /// Shuffle mode (`S`): when the current track ends naturally, play a random
    /// jukebox post instead of stopping. Session-scoped, like the volume.
    shuffle: bool,
    /// Candidate tracks for shuffle plus the refill bookkeeping (in-flight
    /// flag, feed cursor, play-on-arrival latch).
    shuffle_pool: ShufflePool,
    /// Consecutive shuffled tracks that ended without ever reporting progress —
    /// almost always failed URL resolution. Breaker that turns shuffle off
    /// rather than spinning mpv through an endless run of dead tracks.
    shuffle_suspect_ends: u8,
    /// Generation counter for the shuffle refill walk (mirrors `topics_epoch`):
    /// bumped on logout and on shuffle-off so a superseded walk aborts and its
    /// result event is dropped instead of mutating the reset pool.
    shuffle_epoch: Arc<AtomicU64>,
    /// Tracks in the order they actually played (manual picks and shuffled
    /// alike), capped at [`PLAY_HISTORY_CAP`]. `<` / `>` navigate it;
    /// `play_history_pos` is the index of the current (or latest) track.
    play_history: Vec<super::audio::JukeboxTrack>,
    play_history_pos: usize,
}

/// The background override for the active `background_mode`, given the palette's
/// own background. `None` leaves it alone; `Some(Reset)` lets the terminal's
/// transparency show through; `Some(Black)` forces a solid backdrop for a theme
/// that would otherwise be transparent (an already-opaque theme is left as-is).
fn background_override(base_bg: ratatui::style::Color) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    match crate::config::get().background_mode {
        crate::config::BackgroundMode::Theme => None,
        crate::config::BackgroundMode::Transparent => Some(Color::Reset),
        crate::config::BackgroundMode::Opaque => (base_bg == Color::Reset).then_some(Color::Black),
    }
}

/// Resolve a theme kind to its concrete palette, apply the `background_mode`
/// override, then adapt to the terminal's color capability. Shared by startup
/// ([`App::with_theme`]) and runtime theme-cycling ([`App::resolve_theme`]) so
/// the transparency preference applies in both.
fn build_theme(kind: ThemeKind, custom: Option<&Theme>, color_mode: ColorMode) -> Theme {
    let base = match kind {
        ThemeKind::Custom => custom.cloned().unwrap_or_else(Theme::cyber),
        k => k.theme(),
    };
    let bg = background_override(base.background);
    base.with_background(bg).adapt(color_mode)
}

impl App {
    pub fn with_theme(
        client: Client,
        prefill_email: String,
        theme_kind: ThemeKind,
        custom_theme: Option<Theme>,
    ) -> Self {
        let (bg_tx, bg_rx) = mpsc::unbounded_channel();
        let last_email = prefill_email.clone();
        let color_mode = ColorMode::detect();
        let theme = build_theme(theme_kind, custom_theme.as_ref(), color_mode);
        Self {
            client,
            theme,
            theme_kind,
            custom_theme,
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
            images_on: true,
            last_email,
            offline: false,
            toast: None,
            pending_logout: None,
            offline_notify: Arc::new(Notify::new()),
            poller_started: false,
            feed_active: Arc::new(AtomicBool::new(false)),
            feed_poller_started: false,
            input_paused: Arc::new(AtomicBool::new(false)),
            force_clear: false,
            topics_cache: Vec::new(),
            topics_complete: false,
            topics_epoch: Arc::new(AtomicU64::new(0)),
            topic_follows: Vec::new(),
            topic_mutes: Vec::new(),
            topic_prefs_loaded: false,
            color_mode,
            now_playing: None,
            mpv_available: None,
            ytdlp_available: None,
            next_play_token: 0,
            player_volume: crate::config::get().audio_volume,
            shuffle: false,
            shuffle_pool: ShufflePool::new(),
            shuffle_suspect_ends: 0,
            shuffle_epoch: Arc::new(AtomicU64::new(0)),
            play_history: Vec::new(),
            play_history_pos: 0,
        }
    }

    /// Install the terminal image picker (detected at startup). `None` leaves
    /// image rendering disabled.
    pub fn set_image_picker(&mut self, picker: Option<Picker>) {
        self.picker = picker;
    }

    /// Toggle inline image rendering at runtime (the `i` key). Forces a full
    /// terminal clear so a mis-rendered image (raw graphics-protocol bytes from a
    /// terminal that over-reported its support) is wiped immediately. When
    /// re-enabling, kick fetches for any post-detail images that never loaded
    /// (the post was opened while images were off) so they appear without
    /// reopening it.
    fn toggle_images(&mut self) {
        self.images_on = !self.images_on;
        self.force_clear = true;
        self.ensure_detail_images_fetched();
    }

    /// Spawn fetches for every image the post-detail screen can show — the post's
    /// and each loaded reply's — skipping any already cached or already requested
    /// (`mark_requested` dedups, so this is cheap to call repeatedly). The render
    /// pass decodes and overlays each image inline once its gap scrolls into view.
    /// No-op off the post-detail screen, with images disabled, or on a terminal
    /// without graphics support.
    fn ensure_detail_images_fetched(&self) {
        if !self.images_on || self.picker.is_none() {
            return;
        }
        let Screen::PostDetail(s) = &self.screen else {
            return;
        };
        let post_id = s.entry.post_id.clone();
        for url in s.all_image_urls() {
            if s.has_image_bytes(&url) {
                continue;
            }
            if s.mark_requested(url.clone()) {
                self.spawn_fetch_image(post_id.clone(), url);
            }
        }
    }

    /// Skip the login screen — used when a valid session was restored at launch.
    pub fn enter_feed_initial(&mut self) {
        // Seed shuffle from config at session start (re-login included, since
        // logout disarms it). Armed only — playback still needs a first track
        // started by hand; chaining takes over from there.
        self.shuffle = crate::config::get().shuffle;
        self.goto_root(crate::config::get().start_section);
        if self.poller_started {
            // A poller from a previous session is still alive (it idled on the
            // login screen). Reusing it — rather than spawning a duplicate on
            // every re-login — keeps exactly one heartbeat; nudge it to re-poll
            // now with the fresh tokens.
            self.offline_notify.notify_one();
        } else {
            self.spawn_unread_count_poller();
            self.poller_started = true;
            // Populate the badge right away: the poller idles a few seconds
            // before its first tick, so without this the count would stay blank
            // for that whole settle window at launch.
            self.spawn_unread_count_once();
        }
        // The topics warm-up IS re-spawned every login: logout cleared the cache
        // and bumped the epoch, so this starts a fresh fill for the new session
        // (and the epoch bump means any prior run's pages are already discarded).
        self.spawn_topics_prefetch();
        // Background feed auto-refresh (config-gated). Like the unread poller,
        // it's a single long-lived task reused across re-logins.
        if crate::config::get().feed_autorefresh && !self.feed_poller_started {
            self.spawn_feed_head_poller();
            self.feed_poller_started = true;
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
        //
        // The reader uses `poll` + a pause flag rather than a bare blocking
        // `read()` so that while an external `$EDITOR` owns the terminal it makes
        // NO crossterm calls — otherwise it competes with the editor for stdin,
        // dropping the editor's keystrokes and replaying them onto the TUI on
        // exit.
        let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
        let input_paused = self.input_paused.clone();
        std::thread::spawn(move || loop {
            if input_paused.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            match event::poll(Duration::from_millis(50)) {
                Ok(true) => {
                    // An editor may have grabbed the terminal between the poll
                    // and now — leave the pending event for it.
                    if input_paused.load(Ordering::SeqCst) {
                        continue;
                    }
                    match event::read() {
                        Ok(ev) => {
                            if input_tx.send(ev).is_err() {
                                break; // run loop gone
                            }
                        }
                        Err(_) => break,
                    }
                }
                Ok(false) => {} // poll timeout — loop and re-check the pause flag
                Err(_) => break,
            }
        });

        terminal.draw(|f| self.render(f)).context("terminal draw")?;
        while !self.should_quit {
            tokio::select! {
                maybe_ev = input_rx.recv() => {
                    match maybe_ev {
                        Some(ev) => {
                            // Drain the whole burst (focus events, capability
                            // replies) before redrawing, so a flurry costs one
                            // render, not one per event; then collapse repeated
                            // wheel events (see `coalesce_scroll`).
                            let mut batch = vec![ev];
                            while let Ok(next) = input_rx.try_recv() {
                                batch.push(next);
                            }
                            for ev in coalesce_scroll(batch) {
                                self.handle_terminal_event(ev).await;
                                if self.should_quit {
                                    break;
                                }
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
            if self.force_clear {
                terminal.clear().context("terminal clear")?;
                self.force_clear = false;
            }
            // Gate the background feed poller: it only fetches while the feed is
            // the screen on top.
            self.feed_active
                .store(matches!(self.screen, Screen::Feed(_)), Ordering::Relaxed);
            terminal.draw(|f| self.render(f)).context("terminal draw")?;
        }
        Ok(())
    }

    fn render(&self, frame: &mut ratatui::Frame<'_>) {
        let full_area = frame.area();

        // Paint a uniform backdrop first so `background_mode` is authoritative.
        // `self.theme.background` already encodes the mode (Reset → let the
        // terminal's transparency show through; an opaque color → solid backdrop;
        // otherwise the palette's own background). Without this, only cells that a
        // widget paints with `base()` get the theme background, so a translucent
        // terminal shows through everywhere else regardless of theme.
        frame.render_widget(
            ratatui::widgets::Block::default()
                .style(ratatui::style::Style::default().bg(self.theme.background)),
            full_area,
        );

        if self.screen.is_login() {
            if let Screen::Login(s) = &self.screen {
                s.render(frame, full_area, &self.theme);
            }
        } else {
            // Reserve a bottom row for the now-playing bar while a track is
            // loaded — or while shuffle is hunting for one, so the armed mode
            // is never invisible (music starting with no on-screen cue is the
            // surprise we want to avoid).
            let playing =
                self.now_playing.is_some() || (self.shuffle && self.shuffle_pool.pending_play);
            let mut constraints = vec![Constraint::Length(1), Constraint::Min(1)];
            if playing {
                constraints.push(Constraint::Length(1));
            }
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
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
                Screen::PostDetail(s) => s.render(
                    frame,
                    screen_area,
                    &self.theme,
                    self.images_on,
                    self.picker.as_ref(),
                ),
                Screen::Profile(s) => s.render(frame, screen_area, &self.theme),
                Screen::EditProfile(s) => s.render(frame, screen_area, &self.theme),
                Screen::Compose(s) => s.render(frame, screen_area, &self.theme),
                Screen::Editor(s) => s.render(frame, screen_area, &self.theme),
                Screen::Journal(s) => s.render(frame, screen_area, &self.theme),
                Screen::Settings(s) => s.render(frame, screen_area, &self.theme),
                Screen::Guilds(s) => s.render(frame, screen_area, &self.theme),
                Screen::Guild(s) => s.render(frame, screen_area, &self.theme),
            }

            // Now-playing bar in the reserved bottom row (added to `constraints`).
            if let Some(handle) = &self.now_playing {
                super::player::render_bar(frame, layout[2], handle, self.shuffle, &self.theme);
            } else if playing {
                // Shuffle armed with nothing loaded yet (refill in flight).
                super::player::render_search_bar(frame, layout[2], &self.theme);
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

    /// Phase 1 of input handling: map a key on the active screen to an Action.
    /// Only touches the screen it mutates, so it can be unit-tested directly.
    fn route_key(screen: &mut Screen, key: event::KeyEvent) -> Action {
        match screen {
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
                    cursor: s.list.next_cursor.clone(),
                },
                FeedIntent::OpenSelected(post_id) => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id: None,
                },
                FeedIntent::Compose => Action::StartComposeEntry,
                FeedIntent::Bookmark(post_id) => Action::BookmarkPost { post_id },
                FeedIntent::PlayJukebox(track) => Action::PlayPressed { track },
                FeedIntent::OpenJukebox(url) => Action::OpenUrl { url },
                FeedIntent::None => Action::None,
            },
            Screen::Notifications(s) => match s.handle_key(key) {
                NotificationsIntent::Quit => Action::Quit,
                NotificationsIntent::Refresh => Action::NotificationsRefresh,
                NotificationsIntent::LoadMore => Action::NotificationsMore {
                    cursor: s.list.next_cursor.clone(),
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
                    cursor: s.list.next_cursor.clone(),
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
                BookmarksIntent::PlayJukebox(track) => Action::PlayPressed { track },
                BookmarksIntent::OpenJukebox(url) => Action::OpenUrl { url },
                BookmarksIntent::None => Action::None,
            },
            Screen::Topics(s) => match s.handle_key(key) {
                TopicsIntent::Quit => Action::Quit,
                TopicsIntent::Refresh => Action::TopicsRefresh,
                TopicsIntent::OpenSelected { slug } => Action::TopicOpen { slug },
                TopicsIntent::ToggleFollow { slug } => Action::ToggleTopicFollow { slug },
                TopicsIntent::ToggleMute { slug } => Action::ToggleTopicMute { slug },
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
                    cursor: s.list.next_cursor.clone(),
                },
                TopicFeedIntent::OpenSelected { post_id } => Action::OpenPostDetailById {
                    post_id,
                    highlight_reply_id: None,
                },
                TopicFeedIntent::ToggleFollow { slug } => Action::ToggleTopicFollow { slug },
                TopicFeedIntent::ToggleMute { slug } => Action::ToggleTopicMute { slug },
                TopicFeedIntent::PlayJukebox(track) => Action::PlayPressed { track },
                TopicFeedIntent::OpenJukebox(url) => Action::OpenUrl { url },
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
                    prefill: String::new(),
                },
                PostDetailIntent::QuoteReply => Action::StartComposeReply {
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
                PostDetailIntent::Bookmark => Action::BookmarkPost {
                    post_id: s.entry.post_id.clone(),
                },
                PostDetailIntent::BookmarkReply { reply_id } => Action::BookmarkReply { reply_id },
                PostDetailIntent::ToggleWatch => Action::SetThreadWatch {
                    post_id: s.entry.post_id.clone(),
                    // Unknown state defaults to "start watching".
                    watch: !s.watching.unwrap_or(false),
                },
                PostDetailIntent::OpenUrl(url) => Action::OpenUrl { url },
                PostDetailIntent::PlayJukebox(track) => Action::PlayPressed { track },
                PostDetailIntent::None => Action::None,
            },
            Screen::Compose(s) => match s.handle_key(key) {
                ComposeIntent::Quit => Action::Quit,
                ComposeIntent::Submit => Action::ComposeSubmit,
                ComposeIntent::Edit => Action::ComposeReEdit,
                ComposeIntent::None => Action::None,
            },
            Screen::Editor(s) => match s.handle_key(key) {
                EditorIntent::Save => Action::EditorSave,
                EditorIntent::Cancel => Action::EditorCancel,
                EditorIntent::None => Action::None,
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
                ProfileIntent::PinPost { post_id, pin } => Action::PinPost { post_id, pin },
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
                    cursor: s.list.next_cursor.clone(),
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
        }
    }

    async fn handle_terminal_event(&mut self, ev: Event) {
        // Bracketed paste (enabled in main) arrives as one atomic event. The
        // editor inserts it verbatim; single-line fields take it with newlines
        // collapsed so a multi-line clipboard can't break out of the field or
        // trigger Enter/submit. Handled before the key conversion below since a
        // Paste carries no KeyEvent.
        if let Event::Paste(data) = ev {
            match &mut self.screen {
                Screen::Editor(s) => s.paste(&data),
                Screen::Login(s) => s.paste_into_focused(&data),
                Screen::Compose(s) => s.paste_into_focused(&data),
                Screen::EditProfile(s) => s.paste_into_focused(&data),
                Screen::Topics(s) if s.is_filtering() => {
                    s.paste_filter(&super::input::collapse_newlines(&data));
                }
                _ => {}
            }
            return;
        }
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

        // Esc closes the topics search box first, before its "back/menu" role.
        if key.code == KeyCode::Esc {
            if let Screen::Topics(s) = &mut self.screen {
                if s.clear_filter() {
                    return;
                }
            }
        }

        // Esc is the reflexive "back": pop to the previous screen when there is
        // one; on a top-level section (nothing to pop) it opens the overlay menu.
        if key.code == KeyCode::Esc {
            if self.back_stack.is_empty() {
                let authenticated = !self.screen.is_login();
                self.menu = Some(MenuOverlay::build(
                    authenticated,
                    false,
                    self.theme_kind.name(),
                ));
            } else {
                self.pop_screen();
            }
            return;
        }

        // Backspace mirrors Esc's "back" globally (so the help overlay can
        // advertise it honestly), but only where there's somewhere to return to.
        // It's also a text-delete key, so defer to the focused field on input
        // screens, and it has no menu role at the top level.
        if key.code == KeyCode::Backspace
            && !self.screen.accepts_text_input()
            && !self.back_stack.is_empty()
        {
            self.pop_screen();
            return;
        }

        // `?` opens the help overlay, except where a screen captures text input.
        if key.code == KeyCode::Char('?') && !self.screen.accepts_text_input() {
            self.help = true;
            return;
        }

        // Section nav: ←/→ cycle and 1-8 jump, but only on screens that don't
        // capture text (a digit typed into a compose title must reach the field,
        // not navigate). Tab is deliberately NOT a section key — it's reserved
        // for switching sub-tabs within a screen (profile tabs, guild tabs,
        // settings fields).
        if !self.screen.accepts_text_input() {
            match key.code {
                KeyCode::Right => {
                    let next = self.current_root.unwrap_or(RootKind::Feed).next();
                    self.goto_root(next);
                    return;
                }
                KeyCode::Left => {
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

        // Shuffle toggle (`S`): global on any non-text screen, and unlike the
        // other player keys it works while idle too — enabling shuffle with
        // nothing playing starts a random jukebox track. Crossterm reports an
        // uppercase letter with a SHIFT modifier (or none, terminal-dependent),
        // so accept both.
        if !self.screen.accepts_text_input()
            && key.code == KeyCode::Char('S')
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        {
            self.toggle_shuffle();
            return;
        }

        // Image toggle (`i`): global on any non-text screen. Flips inline image
        // rendering off/on and forces a full clear — both a personal preference
        // and the recovery key when a terminal mis-reports its graphics support
        // and an image post renders as a screenful of raw protocol bytes.
        if !self.screen.accepts_text_input() && key.code == KeyCode::Char('i') {
            self.toggle_images();
            return;
        }

        // Jukebox player controls, active only while something is playing and no
        // field is capturing text. `p` is left to the browse screens that bind it
        // to play/switch a focused track; on every other screen it toggles pause.
        // Allow no modifiers (so `Ctrl+d` Settings-save etc. still pass through)
        // or a bare SHIFT — `<` / `>` arrive shifted, and shifted letters are
        // distinct `Char` values, so the existing lowercase arms can't collide.
        if self.now_playing.is_some()
            && !self.screen.accepts_text_input()
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        {
            let on_browse_screen = matches!(
                self.screen,
                Screen::Feed(_)
                    | Screen::PostDetail(_)
                    | Screen::TopicFeed(_)
                    | Screen::Bookmarks(_)
            );
            match key.code {
                KeyCode::Char('s') => {
                    self.player_stop();
                    return;
                }
                KeyCode::Char('[') => {
                    self.player_volume(-5);
                    return;
                }
                KeyCode::Char(']') => {
                    self.player_volume(5);
                    return;
                }
                // mpv's own playlist-navigation keys.
                KeyCode::Char('<') => {
                    self.player_prev();
                    return;
                }
                KeyCode::Char('>') => {
                    self.player_next();
                    return;
                }
                KeyCode::Char('p') if !on_browse_screen => {
                    self.player_toggle_pause();
                    return;
                }
                _ => {}
            }
        }

        // Phase 1: derive an Action with a mutable borrow on the active screen.
        let action = Self::route_key(&mut self.screen, key);

        // Phase 2: apply the action with full mutable access to self.
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::LoginSubmit { email, password } => self.spawn_login(email, password),
            Action::FeedRefresh => self.spawn_feed_initial(),
            Action::FeedMore { cursor } => self.spawn_feed_more(cursor),
            Action::NotificationsRefresh => {
                let (filter, types) = self.notification_query();
                self.spawn_notifications_initial(filter, types);
            }
            Action::NotificationsMore { cursor } => {
                let (filter, types) = self.notification_query();
                self.spawn_notifications_more(filter, types, cursor);
            }
            Action::NotificationsMarkOne { notification_id } => {
                if self.block_write_if_offline() {
                    return;
                }
                if let Screen::Notifications(s) = &mut self.screen {
                    s.mark_local(&notification_id);
                }
                self.unread_count = self.unread_count.saturating_sub(1);
                self.spawn_mark_notification_read(notification_id);
            }
            Action::NotificationsMarkAll => {
                if self.block_write_if_offline() {
                    return;
                }
                if let Screen::Notifications(s) = &mut self.screen {
                    s.mark_all_local();
                }
                self.unread_count = 0;
                self.spawn_mark_all_notifications_read();
            }
            Action::BookmarksRefresh => self.spawn_bookmarks_initial(),
            Action::BookmarksMore { cursor } => self.spawn_bookmarks_more(cursor),
            Action::BookmarkRemove { bookmark_id } => {
                if self.block_write_if_offline() {
                    return;
                }
                if let Screen::Bookmarks(s) = &mut self.screen {
                    s.remove_local(&bookmark_id);
                }
                self.spawn_delete_bookmark(bookmark_id);
            }
            Action::TopicsRefresh => {
                // Invalidate the running warm-up, clear the cache, and re-warm.
                self.topics_epoch.fetch_add(1, Ordering::SeqCst);
                self.topics_cache.clear();
                self.topics_complete = false;
                if let Screen::Topics(s) = &mut self.screen {
                    s.set_topics(Vec::new(), false);
                }
                self.spawn_topics_prefetch();
                // Refresh the follow/mute prefs too.
                self.topic_prefs_loaded = true;
                self.spawn_topic_prefs_load();
            }
            Action::TopicOpen { slug } => {
                let mut new = TopicFeedScreen::new(slug.clone());
                new.set_topic_state(
                    self.topic_follows.contains(&slug),
                    self.topic_mutes.contains(&slug),
                );
                self.push_screen(Screen::TopicFeed(new));
                self.spawn_topic_feed_initial(&slug);
                // Opening a topic directly (e.g. from search) may precede ever
                // visiting the topics list, so make sure prefs get loaded.
                if !self.topic_prefs_loaded {
                    self.topic_prefs_loaded = true;
                    self.spawn_topic_prefs_load();
                }
            }
            Action::ToggleTopicFollow { slug } => {
                if self.block_write_if_offline() {
                    return;
                }
                // Optimistic toggle; the marker flipping is the feedback. A failed
                // PATCH resyncs from the server (see TopicPrefsSaved).
                if self.topic_follows.contains(&slug) {
                    self.topic_follows.retain(|s| *s != slug);
                } else {
                    self.topic_follows.push(slug);
                }
                self.push_topic_prefs();
                self.spawn_save_topic_prefs(SettingsUpdate {
                    followed_topics: Some(self.topic_follows.clone()),
                    ..Default::default()
                });
            }
            Action::ToggleTopicMute { slug } => {
                if self.block_write_if_offline() {
                    return;
                }
                if self.topic_mutes.contains(&slug) {
                    self.topic_mutes.retain(|s| *s != slug);
                } else {
                    self.topic_mutes.push(slug);
                }
                self.push_topic_prefs();
                self.spawn_save_topic_prefs(SettingsUpdate {
                    muted_topics: Some(self.topic_mutes.clone()),
                    ..Default::default()
                });
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
                // Fast path: reuse the entry already in the current feed — but
                // skip a tombstoned one (a refresh may have marked it deleted),
                // so we fall through and fetch fresh rather than open a stale shell.
                if let Screen::Feed(s) = &self.screen {
                    if let Some(entry) = s
                        .list
                        .items
                        .iter()
                        .find(|e| e.post_id == post_id && !e.deleted)
                        .cloned()
                    {
                        self.enter_post_detail(entry, highlight_reply_id);
                        return;
                    }
                }
                if let Screen::TopicFeed(s) = &self.screen {
                    if let Some(entry) = s
                        .list
                        .items
                        .iter()
                        .find(|e| e.post_id == post_id && !e.deleted)
                        .cloned()
                    {
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
                if self.block_write_if_offline() {
                    return;
                }
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
            Action::PinPost { post_id, pin } => {
                if self.block_write_if_offline() {
                    return;
                }
                // Optimistic: flip the marker now (the ★/📌 is the feedback). A
                // failed PATCH resyncs via the ProfileUpdated error path.
                if let Screen::Profile(p) = &mut self.screen {
                    if let Some(u) = &mut p.user {
                        u.pinned_post_id = pin.then(|| post_id.clone());
                    }
                }
                self.spawn_update_own_profile(ProfileUpdate {
                    pinned_post_id: if pin {
                        cs_api::Patch::Set(post_id)
                    } else {
                        cs_api::Patch::Clear
                    },
                    ..Default::default()
                });
            }
            Action::StartComposeEntry => {
                self.start_compose(ComposeKind::NewEntry, String::new())
                    .await;
            }
            Action::OpenUrl { url } => {
                // Hands the link to the desktop browser. No network of ours, so
                // it's fine offline; just report success/failure via a toast.
                match super::open::open_url(&url) {
                    Ok(()) => self.toast = Some(Toast::confirmation("opening in browser…")),
                    Err(e) => {
                        tracing::debug!(error = %e, %url, "failed to open url");
                        self.toast = Some(Toast::warning("couldn't open your browser"));
                    }
                }
            }
            Action::PlayPressed { track } => self.play_pressed(track),
            Action::BookmarkPost { post_id } => {
                if self.block_write_if_offline() {
                    return;
                }
                self.spawn_bookmark_post(post_id);
            }
            Action::BookmarkReply { reply_id } => {
                if self.block_write_if_offline() {
                    return;
                }
                self.spawn_bookmark_reply(reply_id);
            }
            Action::SetThreadWatch { post_id, watch } => {
                if self.block_write_if_offline() {
                    return;
                }
                // Optimistically reflect the new state; the toggle result
                // reconciles (or rolls back) once it lands.
                if let Screen::PostDetail(s) = &mut self.screen {
                    if s.entry.post_id == post_id {
                        s.set_watching(watch);
                    }
                }
                self.spawn_set_thread_watch(post_id, watch);
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
                self.warn_if_compose_throttled();
                self.spawn_compose_submit();
            }
            Action::ComposeReEdit => self.re_edit_compose().await,
            Action::EditorSave => self.editor_save(),
            Action::EditorCancel => self.pop_screen(),
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
                // Every entry page the user browses feeds the shuffle pool's
                // candidate list (here and in the other entry-carrying arms) —
                // free material for shuffle mode, at zero extra API cost.
                if let Ok((entries, _)) = &result {
                    self.shuffle_pool.harvest(entries);
                }
                if let Screen::Feed(s) = &mut self.screen {
                    s.apply_initial(result);
                }
            }
            BgEvent::FeedMore(result) => {
                if let Ok((entries, _)) = &result {
                    self.shuffle_pool.harvest(entries);
                }
                if let Screen::Feed(s) = &mut self.screen {
                    s.apply_more(result);
                }
            }
            BgEvent::FeedHead(entries) => {
                self.shuffle_pool.harvest(&entries);
                // Apply only if the feed is still on screen (the user may have
                // navigated away between the poll and now).
                let mut reload_head = false;
                if let Screen::Feed(s) = &mut self.screen {
                    match s.apply_new_head(entries) {
                        HeadUpdate::Prepended(n) => {
                            self.toast = Some(Toast::confirmation(format!("↑ {n} new")));
                        }
                        HeadUpdate::Gap => {
                            // More than a page of new posts arrived, so a clean
                            // prepend would leave a hole in the timeline. If the
                            // user is parked at the very top, reload from scratch
                            // so the newest posts surface without a manual `r`;
                            // if they're scrolled down reading, keep their place
                            // and just hint that a refresh is available.
                            if s.is_at_top() {
                                reload_head = true;
                            } else {
                                self.toast = Some(Toast::confirmation("new posts · r to refresh"));
                            }
                        }
                        HeadUpdate::None => {}
                    }
                }
                if reload_head {
                    self.spawn_feed_initial();
                    self.toast = Some(Toast::confirmation("↑ new posts"));
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
                // Converge on truth with a *delayed* re-read: the count endpoint
                // is cached server-side (~5s), so an immediate poll would return
                // the pre-mark value and flick the just-cleared badge back to its
                // old number. The resync waits past that cache window.
                self.spawn_unread_count_resync();
            }
            BgEvent::NotificationMarkFailed { notification_id } => {
                // Undo the optimistic mark so the UI matches the server.
                if let Screen::Notifications(s) = &mut self.screen {
                    s.unmark_local(&notification_id);
                }
                self.unread_count = self.unread_count.saturating_add(1);
                self.warn_toast_unless_signalled("couldn't mark as read");
            }
            BgEvent::AllNotificationsMarkFailed => {
                // We can't reconstruct which were unread, so resync from server.
                self.warn_toast_unless_signalled("couldn't mark all as read");
                if matches!(self.screen, Screen::Notifications(_)) {
                    let (filter, types) = self.notification_query();
                    self.spawn_notifications_initial(filter, types);
                }
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
            BgEvent::BookmarkRemoveFailed => {
                // The optimistic removal didn't take; resync the list from server
                // (we discarded the removed item, so we can't re-insert it).
                self.warn_toast_unless_signalled("couldn't remove bookmark");
                if matches!(self.screen, Screen::Bookmarks(_)) {
                    self.spawn_bookmarks_initial();
                }
            }
            BgEvent::BookmarkCreated(result) => {
                self.toast = Some(match result {
                    Ok(_) => Toast::confirmation("bookmarked"),
                    Err(msg) => Toast::warning(format!("bookmark failed: {}", first_line(&msg))),
                });
            }
            BgEvent::WatchStatus { post_id, result } => {
                // Passive fetch on open: update the indicator on success, stay
                // quiet on failure (no toast — the key still works).
                if let Ok(watching) = result {
                    if let Screen::PostDetail(s) = &mut self.screen {
                        if s.entry.post_id == post_id {
                            s.set_watching(watching);
                        }
                    }
                }
            }
            BgEvent::WatchToggled { post_id, result } => match result {
                Ok(watching) => {
                    if let Screen::PostDetail(s) = &mut self.screen {
                        if s.entry.post_id == post_id {
                            s.set_watching(watching);
                        }
                    }
                    self.toast = Some(Toast::confirmation(if watching {
                        "watching thread"
                    } else {
                        "unwatched thread"
                    }));
                }
                Err(msg) => {
                    // Roll back the optimistic flip from a fresh status fetch so
                    // the indicator can't lie about the server state.
                    self.warn_toast_unless_signalled(&format!(
                        "watch failed: {}",
                        first_line(&msg)
                    ));
                    if matches!(&self.screen, Screen::PostDetail(s) if s.entry.post_id == post_id) {
                        self.spawn_watch_status(&post_id);
                    }
                }
            },
            BgEvent::PlaybackEnded { token } => {
                // Clear the now-playing bar only if this is still the current
                // track (a superseded track's exit must not clear a newer one).
                // An explicit stop or track switch invalidates the token before
                // this arrives, so a matching token means the track ended on
                // its own — exactly when shuffle should chain to the next one.
                if let Some(ended) = self.now_playing.take_if(|h| h.token == token) {
                    if self.shuffle {
                        // A track that dies quickly without reporting progress
                        // almost certainly failed to resolve (dead link,
                        // yt-dlp error). Both clauses matter: position alone
                        // would also flag a full song played with a broken IPC
                        // socket (position never updates), so wall-clock time
                        // must corroborate. Skipping past one bad track is the
                        // point of chaining; skipping forever through a mass
                        // failure (network down, yt-dlp broken) would spin
                        // mpv in a loop, so give up after a few in a row.
                        if ended.position_secs < SUSPECT_END_SECS
                            && ended.started_at.elapsed() < SUSPECT_WALL_TIME
                        {
                            self.shuffle_suspect_ends += 1;
                            // The link is likely dead for good; stop offering
                            // it (it stays in the seen-set, so refills won't
                            // re-add it either).
                            self.shuffle_pool.remove(&ended.url);
                        } else {
                            self.shuffle_suspect_ends = 0;
                        }
                        if self.shuffle_suspect_ends >= SUSPECT_END_LIMIT {
                            self.shuffle = false;
                            self.toast =
                                Some(Toast::warning("shuffle off: tracks keep failing to play"));
                        } else if self.play_history_pos + 1 < self.play_history.len() {
                            // `<` rewound into the play history: natural ends
                            // replay forward through the remembered sequence
                            // before fresh random picks resume at the tip.
                            self.play_history_pos += 1;
                            let track = self.play_history[self.play_history_pos].clone();
                            self.start_playback_at(track, false);
                            if self.now_playing.is_none() {
                                // Same no-dead-armed-mode rule as the random
                                // chain: a start that bailed will never emit
                                // PlaybackEnded to continue from.
                                self.shuffle = false;
                                self.toast =
                                    Some(Toast::warning("shuffle off: couldn't start playback"));
                            }
                        } else {
                            self.shuffle_advance(Some(&ended.url));
                        }
                    }
                }
            }
            BgEvent::ShuffleTracks { epoch, result } => {
                // A walk from a superseded shuffle generation (logout, or the
                // mode toggled off and the bookkeeping reset) must not touch
                // the pool — mirrors the topics warm-up's epoch guard.
                if epoch != self.shuffle_epoch.load(Ordering::SeqCst) {
                    return;
                }
                match result {
                    Ok((tracks, cursor)) => {
                        let added = self.shuffle_pool.add_tracks(tracks);
                        self.shuffle_pool.finish_refill(added, cursor);
                        // The play-on-arrival latch: shuffle wanted a track
                        // while the pool was empty. Don't hijack a track the
                        // user has started by hand in the meantime.
                        if std::mem::take(&mut self.shuffle_pool.pending_play)
                            && self.shuffle
                            && self.now_playing.is_none()
                        {
                            self.shuffle_advance(None);
                        }
                    }
                    Err(_) => {
                        self.shuffle_pool.fetch_inflight = false;
                        let was_pending = std::mem::take(&mut self.shuffle_pool.pending_play);
                        // We promised music we can't deliver; don't leave a
                        // silent mode armed — and when the mode does flip off,
                        // say so unconditionally (warn_toast_unless_signalled
                        // can be swallowed by a rate-limit/offline toast,
                        // which is exactly when this path fires).
                        if was_pending && self.now_playing.is_none() {
                            self.shuffle = false;
                            self.toast = Some(Toast::warning("shuffle off: couldn't fetch tracks"));
                        } else {
                            self.warn_toast_unless_signalled("shuffle: couldn't fetch tracks");
                        }
                    }
                }
            }
            BgEvent::PlaybackProgress {
                token,
                position_secs,
                duration_secs,
            } => {
                // Ignore progress from a superseded track (token mismatch).
                if let Some(h) = self.now_playing.as_mut() {
                    if h.token == token {
                        h.position_secs = position_secs;
                        h.duration_secs = duration_secs;
                    }
                }
            }
            BgEvent::TopicsPrefetched {
                epoch,
                topics,
                complete,
            } => {
                // Ignore pages from a warm-up that a refresh has superseded.
                if epoch == self.topics_epoch.load(Ordering::SeqCst) {
                    self.topics_cache.extend(topics);
                    if complete {
                        self.topics_complete = true;
                    }
                    if let Screen::Topics(s) = &mut self.screen {
                        s.set_topics(self.topics_cache.clone(), self.topics_complete);
                    }
                }
            }
            BgEvent::TopicPrefsLoaded(result) => match result {
                Ok((follows, mutes)) => {
                    self.topic_follows = follows;
                    self.topic_mutes = mutes;
                    self.topic_prefs_loaded = true;
                    self.push_topic_prefs();
                }
                Err(msg) => {
                    // Allow a retry next time the section opens / on refresh.
                    self.topic_prefs_loaded = false;
                    tracing::warn!(error = %msg, "topic prefs load failed");
                }
            },
            BgEvent::TopicPrefsSaved(result) => {
                if let Err(msg) = result {
                    // The optimistic toggle didn't persist — resync from server.
                    self.warn_toast_unless_signalled("couldn't update topic");
                    tracing::warn!(error = %msg, "topic prefs save failed");
                    self.spawn_topic_prefs_load();
                }
            }
            BgEvent::TopicFeedInitial { slug, result } => {
                if let Ok((entries, _)) = &result {
                    self.shuffle_pool.harvest(entries);
                }
                if let Screen::TopicFeed(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_initial(result);
                    }
                }
            }
            BgEvent::TopicFeedMore { slug, result } => {
                if let Ok((entries, _)) = &result {
                    self.shuffle_pool.harvest(entries);
                }
                if let Screen::TopicFeed(s) = &mut self.screen {
                    if s.slug == slug {
                        s.apply_more(result);
                    }
                }
            }
            BgEvent::DetailRepliesInitial { post_id, result } => {
                // Guard against a stale reply page landing on a different post
                // (open A, pop, open B before A's replies arrive).
                if let Screen::PostDetail(s) = &mut self.screen {
                    if s.entry.post_id == post_id {
                        s.apply_replies_initial(result);
                    }
                }
                // The replies (and so their images) are now known — fetch them.
                self.ensure_detail_images_fetched();
            }
            BgEvent::DetailRepliesMore { post_id, result } => {
                if let Screen::PostDetail(s) = &mut self.screen {
                    if s.entry.post_id == post_id {
                        s.apply_replies_more(result);
                    }
                }
                self.ensure_detail_images_fetched();
            }
            BgEvent::OpenPostDetail {
                result,
                highlight_reply_id,
            } => match result {
                Ok(entry) => {
                    self.shuffle_pool.harvest(std::slice::from_ref(&entry));
                    self.enter_post_detail(entry, highlight_reply_id);
                }
                Err(msg) => {
                    // Don't swallow it: a notification pointing at a missing /
                    // non-post target would otherwise look like a dead key.
                    tracing::warn!(error = %msg, "open-post-detail fetch failed");
                    self.warn_toast_unless_signalled("couldn't open that post");
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
                if let Ok((entries, _)) = &result {
                    self.shuffle_pool.harvest(entries);
                }
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
                    } else if matches!(self.screen, Screen::Profile(_)) {
                        // A pin/unpin from the profile failed — warn and resync
                        // the marker from the server.
                        tracing::warn!(error = %msg, "pin update failed");
                        self.warn_toast_unless_signalled("couldn't update pin");
                        self.spawn_profile_user_me();
                    }
                }
            },
            BgEvent::EntryCreated(result) => match result {
                Ok((_post_id, slug)) => {
                    if matches!(self.screen, Screen::Compose(_)) {
                        self.pop_screen();
                    }
                    // If the underlying screen is the feed, refresh it.
                    if matches!(self.screen, Screen::Feed(_)) {
                        self.spawn_feed_initial();
                    }
                    // Echo the stored slug so the user sees any collision suffix.
                    self.toast = Some(match slug {
                        Some(s) => Toast::confirmation(format!("posted · /{s}")),
                        None => Toast::confirmation("posted"),
                    });
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
                    // Cache the bytes on the matching post-detail screen; the
                    // render pass decodes and overlays them inline once the
                    // image's gap scrolls into view. If the user navigated away,
                    // the post id won't match and the bytes are simply dropped.
                    if let Screen::PostDetail(s) = &self.screen {
                        if s.entry.post_id == post_id {
                            s.cache_image_bytes(url, bytes);
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
        // Stop any music so it doesn't keep playing on the login screen, and
        // drop shuffle's session-scoped state with it (the pool was built from
        // this account's view of the feed). The epoch bump cancels any
        // in-flight refill walk so its result can't repopulate the cleared
        // pool after (re-)login.
        self.player_stop();
        self.shuffle = false;
        self.shuffle_pool.clear();
        self.shuffle_epoch.fetch_add(1, Ordering::SeqCst);
        self.play_history.clear();
        self.play_history_pos = 0;
        self.back_stack.clear();
        self.current_root = None;
        self.unread_count = 0;
        self.offline = false;
        self.toast = None;
        // Invalidate the topics warm-up: bump the epoch so any in-flight prefetch
        // bails (and its pages are dropped) instead of 401-spinning, and drop the
        // cache so the next login re-warms fresh rather than showing stale data.
        self.topics_epoch.fetch_add(1, Ordering::SeqCst);
        self.topics_cache.clear();
        self.topics_complete = false;
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

    /// Show a brief warning toast for a failed action — but don't clobber a more
    /// specific toast the preceding `ApiSignal` already raised (e.g. the
    /// rate-limit countdown), which is queued just ahead of the failure event.
    fn warn_toast_unless_signalled(&mut self, msg: &str) {
        if self.toast.is_none() {
            self.toast = Some(Toast::warning(msg.to_string()));
        }
    }

    /// Short-circuit an optimistic write while we know we're offline: give the
    /// user instant feedback instead of an optimistic flicker that rolls back a
    /// few seconds later (or a hang until the request times out). Returns true
    /// when the caller should skip the action.
    fn block_write_if_offline(&mut self) -> bool {
        if self.offline {
            self.toast = Some(Toast::warning(
                "you're offline — try again when reconnected",
            ));
            true
        } else {
            false
        }
    }

    /// When a compose submit would block on the client-side write limiter, show
    /// a visible countdown instead of letting `acquire` hang silently. The post
    /// still sends once the window opens.
    fn warn_if_compose_throttled(&mut self) {
        let (key, verb) = match &self.screen {
            Screen::Compose(s) => match &s.kind {
                ComposeKind::NewEntry => (EndpointKey::EntriesCreate, "posting"),
                ComposeKind::Reply { .. } => (EndpointKey::RepliesCreate, "replying"),
                ComposeKind::NewNote => (EndpointKey::NotesCreate, "saving"),
                ComposeKind::UpdateNote { .. } => (EndpointKey::NotesUpdate, "saving"),
                ComposeKind::GuildThread { .. } => (EndpointKey::GuildsThreadsCreate, "posting"),
            },
            _ => return,
        };
        let secs = self.client.time_until_writable(key).as_secs();
        if secs == 0 {
            return;
        }
        self.toast = Some(if secs <= 90 {
            Toast::countdown(format!("rate limited — {verb} in"), secs)
        } else {
            Toast::warning(format!("rate limit reached — try {verb} again later"))
        });
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

    /// The palettes the cycle steps through: the built-ins, plus the user's
    /// `Custom` when `config.toml` defines one.
    fn available_theme_kinds(&self) -> Vec<ThemeKind> {
        let mut kinds = ThemeKind::ALL.to_vec();
        if self.custom_theme.is_some() {
            kinds.push(ThemeKind::Custom);
        }
        kinds
    }

    /// Resolve a kind to its concrete palette (`Custom` comes from `config.toml`),
    /// adapted to the terminal's color capability.
    fn resolve_theme(&self, kind: ThemeKind) -> Theme {
        build_theme(kind, self.custom_theme.as_ref(), self.color_mode)
    }

    /// Advance to the next theme palette, apply it live, and persist the choice
    /// to local prefs so it survives restarts. A failed save is non-fatal.
    fn cycle_theme(&mut self) {
        let kinds = self.available_theme_kinds();
        let idx = kinds
            .iter()
            .position(|k| *k == self.theme_kind)
            .unwrap_or(0);
        self.theme_kind = kinds[(idx + 1) % kinds.len()];
        self.theme = self.resolve_theme(self.theme_kind);
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
                self.spawn_notifications_initial(NotificationsFilter::All, Vec::new());
            }
            RootKind::Bookmarks => {
                self.screen = Screen::Bookmarks(BookmarksScreen::new());
                self.spawn_bookmarks_initial();
            }
            RootKind::Topics => {
                // Pure view over the background-warmed cache (it keeps filling
                // while open via `TopicsPrefetched`); the screen never fetches.
                let mut s = TopicsScreen::new();
                s.set_topics(self.topics_cache.clone(), self.topics_complete);
                s.set_topic_prefs(self.topic_follows.clone(), self.topic_mutes.clone());
                self.screen = Screen::Topics(s);
                // Lazily fetch follow/mute prefs the first time topics is opened.
                if !self.topic_prefs_loaded {
                    self.topic_prefs_loaded = true;
                    self.spawn_topic_prefs_load();
                }
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

    /// The active read filter + type-bucket from the notifications screen
    /// (defaults when it isn't the current screen).
    fn notification_query(&self) -> (NotificationsFilter, Vec<NotificationType>) {
        if let Screen::Notifications(s) = &self.screen {
            (s.filter, s.selected_types())
        } else {
            (NotificationsFilter::All, Vec::new())
        }
    }

    fn spawn_notifications_initial(
        &self,
        filter: NotificationsFilter,
        types: Vec<NotificationType>,
    ) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_notifications(None, None, filter, &types)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::NotificationsInitial(result));
        });
    }

    fn spawn_notifications_more(
        &self,
        filter: NotificationsFilter,
        types: Vec<NotificationType>,
        cursor: Option<String>,
    ) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_notifications(cursor.as_deref(), None, filter, &types)
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
                    let _ = tx.send(BgEvent::NotificationMarkFailed { notification_id });
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
                    let _ = tx.send(BgEvent::AllNotificationsMarkFailed);
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
                    let _ = tx.send(BgEvent::BookmarkRemoveFailed);
                }
            }
        });
    }

    fn spawn_bookmark_post(&self, post_id: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .bookmark_post(&post_id)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::BookmarkCreated(result));
        });
    }

    fn spawn_bookmark_reply(&self, reply_id: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .bookmark_reply(&reply_id)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::BookmarkCreated(result));
        });
    }

    /// Fetch the current watch state for an open post detail (human-driven: runs
    /// when the user opens the thread). Non-blocking; the indicator updates when
    /// `WatchStatus` lands.
    fn spawn_watch_status(&self, post_id: &str) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let post_id = post_id.to_string();
        tokio::spawn(async move {
            let result = client
                .watch_status(&post_id)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::WatchStatus { post_id, result });
        });
    }

    /// Watch or unwatch a thread, reporting the authoritative new state via
    /// `WatchToggled`.
    fn spawn_set_thread_watch(&self, post_id: String, watch: bool) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = if watch {
                client.watch_thread(&post_id).await
            } else {
                client.unwatch_thread(&post_id).await
            }
            .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::WatchToggled { post_id, result });
        });
    }

    /// Warm the topics cache in the background: page through every topic with a
    /// gentle trickle so a later search covers them all without a foreground
    /// load. Self-paced and rate-limited; resumes through transient errors and
    /// gives up after a sustained outage (a manual refresh re-warms). Its pages
    /// carry the current epoch so a refresh can discard a superseded run.
    fn spawn_topics_prefetch(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let epoch_arc = self.topics_epoch.clone();
        let my_epoch = epoch_arc.load(Ordering::SeqCst);
        tokio::spawn(async move {
            // Settle so the warm-up doesn't compete with the initial feed load.
            tokio::time::sleep(Duration::from_secs(2)).await;
            let mut cursor: Option<String> = None;
            let mut errors: u32 = 0;
            loop {
                if epoch_arc.load(Ordering::SeqCst) != my_epoch {
                    return; // superseded by a refresh
                }
                match client.list_topics(cursor.as_deref(), Some(50)).await {
                    Ok((topics, next)) => {
                        errors = 0;
                        let complete = next.is_none();
                        let sent = tx.send(BgEvent::TopicsPrefetched {
                            epoch: my_epoch,
                            topics,
                            complete,
                        });
                        if sent.is_err() || complete {
                            return; // app gone, or all pages loaded
                        }
                        cursor = next;
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    Err(e) => {
                        if tx.is_closed() {
                            return; // app gone — don't keep retrying
                        }
                        errors += 1;
                        tracing::debug!(error = %e, "topics prefetch page failed");
                        if errors >= 10 {
                            // Sustained failure (likely offline): stop the
                            // "loading…" hint with what we have; `r` retries.
                            let _ = tx.send(BgEvent::TopicsPrefetched {
                                epoch: my_epoch,
                                topics: Vec::new(),
                                complete: true,
                            });
                            return;
                        }
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
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
            let result = client
                .list_guilds(None, None)
                .await
                .map_err(|e| note_api_err(&tx, e));
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
            let result = client
                .get_guild(&info_slug)
                .await
                .map_err(|e| note_api_err(&tx, e));
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
            let result = client
                .join_guild(&slug)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::GuildJoined { slug, result });
        });
    }

    fn spawn_guild_leave(&self, slug: String) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .leave_guild(&slug)
                .await
                .map_err(|e| note_api_err(&tx, e));
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
            let _ = tx.send(BgEvent::DetailRepliesInitial { post_id, result });
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
            let _ = tx.send(BgEvent::DetailRepliesMore { post_id, result });
        });
    }

    fn spawn_open_post_detail_by_id(&self, post_id: String, highlight_reply_id: Option<String>) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .get_entry(&post_id)
                .await
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::OpenPostDetail {
                result,
                highlight_reply_id,
            });
        });
    }

    /// Push a post-detail screen for `entry`, load its replies, and (when the
    /// terminal supports graphics) start fetching the post's inline image.
    fn enter_post_detail(&mut self, entry: Entry, highlight_reply_id: Option<String>) {
        let id = entry.post_id.clone();
        let mut screen = PostDetailScreen::new(entry);
        screen.highlight_reply_id = highlight_reply_id;
        self.push_screen(Screen::PostDetail(screen));
        self.spawn_detail_replies_initial(&id);
        // Resolve the watch indicator (subscribed to thread_reply notifications?).
        self.spawn_watch_status(&id);
        // Kick off the fetch for the post's own image now; the replies' images
        // follow once `DetailRepliesInitial` lands. Both are decoded and drawn
        // inline by the render pass as they scroll into view.
        self.ensure_detail_images_fetched();
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

    /// Whether mpv is usable, probed once (spawns `mpv --version`) and cached.
    /// The probe blocks on a subprocess, so run it via `block_in_place` to avoid
    /// stalling the event loop on the first play attempt.
    fn mpv_available(&mut self) -> bool {
        *self
            .mpv_available
            .get_or_insert_with(|| tokio::task::block_in_place(super::player::mpv_available))
    }

    /// Whether a YouTube resolver (yt-dlp/youtube-dl) is usable, probed once.
    /// Same blocking-subprocess caveat as [`Self::mpv_available`].
    fn ytdlp_available(&mut self) -> bool {
        *self
            .ytdlp_available
            .get_or_insert_with(|| tokio::task::block_in_place(super::player::ytdlp_available))
    }

    /// Handle `p` on a screen given the track under the cursor: play it, switch
    /// to it, or — when it's already the current track, or there's no track here
    /// — toggle pause on whatever is playing.
    fn play_pressed(&mut self, track: Option<super::audio::JukeboxTrack>) {
        match track {
            Some(t) => {
                if self.now_playing.as_ref().is_some_and(|h| h.url == t.url) {
                    self.player_toggle_pause();
                } else {
                    self.start_playback(t);
                }
            }
            None => {
                if self.now_playing.is_some() {
                    self.player_toggle_pause();
                } else {
                    self.toast = Some(Toast::warning("no jukebox track here"));
                }
            }
        }
    }

    /// Start (or replace) playback of `track` via the mpv background player,
    /// recording it in the play history (so `<` can return to it).
    fn start_playback(&mut self, track: super::audio::JukeboxTrack) {
        self.start_playback_at(track, true);
    }

    /// [`Self::start_playback`], minus the history push — used when the track
    /// IS a history entry (`<` / `>` navigation), where re-pushing would
    /// duplicate it and orphan the forward entries.
    fn start_playback_at(&mut self, track: super::audio::JukeboxTrack, push_history: bool) {
        if !self.mpv_available() {
            self.toast = Some(Toast::warning(
                "install mpv + yt-dlp to play audio · o opens it in your browser",
            ));
            return;
        }
        // mpv needs yt-dlp to resolve YouTube; without it playback would fail
        // instantly, so warn rather than flash an empty now-playing bar.
        if super::audio::is_youtube(&track.url) && !self.ytdlp_available() {
            self.toast = Some(Toast::warning(
                "install yt-dlp to play YouTube tracks · o opens it in your browser",
            ));
            return;
        }
        // Replace any current track (its task still emits PlaybackEnded for the
        // old token, which the handler ignores once the token has moved on).
        if let Some(handle) = self.now_playing.take() {
            handle.stop();
        }
        self.next_play_token += 1;
        let token = self.next_play_token;
        match super::player::play(
            &track.url,
            track.artist.clone(),
            track.title.clone(),
            token,
            self.player_volume,
            self.bg_tx.clone(),
        ) {
            Ok(handle) => {
                self.now_playing = Some(handle);
                if push_history {
                    self.push_play_history(track);
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "failed to spawn mpv");
                self.toast = Some(Toast::warning("couldn't start mpv"));
            }
        }
    }

    /// Append a successfully started track to the play history and point the
    /// cursor at it. Starting a new track while rewound into history abandons
    /// the forward entries (the usual media-player branching rule); replaying
    /// the track already at the cursor is a no-op so pause-style restarts
    /// don't pile up duplicates.
    fn push_play_history(&mut self, track: super::audio::JukeboxTrack) {
        if self.play_history.get(self.play_history_pos) == Some(&track) {
            return;
        }
        self.play_history.truncate(self.play_history_pos + 1);
        self.play_history.push(track);
        if self.play_history.len() > PLAY_HISTORY_CAP {
            self.play_history.remove(0);
        }
        self.play_history_pos = self.play_history.len() - 1;
    }

    /// `<` — replay the previous track from the play history.
    fn player_prev(&mut self) {
        if self.play_history_pos > 0 && !self.play_history.is_empty() {
            self.play_history_pos -= 1;
            let track = self.play_history[self.play_history_pos].clone();
            self.start_playback_at(track, false);
        } else {
            self.toast = Some(Toast::warning("no previous track"));
        }
    }

    /// `>` — skip forward: through the history when `<` rewound into it,
    /// otherwise to a fresh random pick from the shuffle pool. The skip is
    /// just a pick, not a mode change — shuffle stays however it was.
    fn player_next(&mut self) {
        if self.play_history_pos + 1 < self.play_history.len() {
            self.play_history_pos += 1;
            let track = self.play_history[self.play_history_pos].clone();
            self.start_playback_at(track, false);
            return;
        }
        let current = self.now_playing.as_ref().map(|h| h.url.clone());
        if self.shuffle {
            // Full machinery: refills, the pending latch, disarm on failure.
            self.shuffle_advance(current.as_deref());
        } else {
            // One-shot random pick; with no material, point at shuffle rather
            // than silently spinning up its refill walk for a single skip.
            match self.shuffle_pool.pick(current.as_deref()) {
                Some(track) => self.start_playback(track),
                None => {
                    self.toast = Some(Toast::warning(
                        "no other jukebox tracks known yet · S starts shuffle",
                    ));
                }
            }
        }
    }

    fn player_toggle_pause(&mut self) {
        if let Some(handle) = self.now_playing.as_mut() {
            handle.toggle_pause();
        }
    }

    fn player_stop(&mut self) {
        if let Some(handle) = self.now_playing.take() {
            handle.stop();
            // An explicit stop also ends shuffle — "keep playing random
            // tracks" is exactly what the user just declined.
            if self.shuffle {
                self.shuffle = false;
                self.toast = Some(Toast::confirmation("stopped · shuffle off"));
            } else {
                self.toast = Some(Toast::confirmation("stopped"));
            }
        }
    }

    /// Toggle shuffle mode (`S`). Turning it on mid-track lets the current
    /// song finish and chains from there; turning it on while idle starts a
    /// random jukebox track right away — the "jukebox radio" entry point.
    fn toggle_shuffle(&mut self) {
        if self.shuffle {
            self.shuffle = false;
            self.shuffle_pool.pending_play = false;
            // Cancel any in-flight refill walk: bumping the epoch makes the
            // task bail at its next page (and its result be dropped), so the
            // inflight flag can be reset here without risking a stale event
            // flipping state later.
            self.shuffle_epoch.fetch_add(1, Ordering::SeqCst);
            self.shuffle_pool.fetch_inflight = false;
            self.toast = Some(Toast::confirmation("shuffle off"));
            return;
        }
        // Without mpv (and yt-dlp — jukebox tracks are nearly always YouTube
        // links) shuffle could never play anything; say so instead of turning
        // on a mode that silently does nothing.
        if !self.mpv_available() || !self.ytdlp_available() {
            self.toast = Some(Toast::warning("install mpv + yt-dlp to use shuffle"));
            return;
        }
        self.shuffle = true;
        self.shuffle_suspect_ends = 0;
        // Re-enabling is an explicit "try again", even if the last refill walk
        // came up dry.
        self.shuffle_pool.retry_refills();
        if self.now_playing.is_some() {
            self.toast = Some(Toast::confirmation("shuffle on"));
            if self.shuffle_pool.needs_refill() {
                self.spawn_shuffle_refill();
            }
        } else {
            self.shuffle_advance(None);
        }
    }

    /// Start the next random jukebox track, topping the pool up in the
    /// background when it runs low. `just_ended` is the URL of the track whose
    /// end triggered the chain, so the pick can avoid an instant repeat (the
    /// handle is already taken out of `now_playing` by then). With an empty
    /// pool, latch `pending_play` so the refill's arrival starts playback.
    fn shuffle_advance(&mut self, just_ended: Option<&str>) {
        match self.shuffle_pool.pick(just_ended) {
            Some(track) => {
                self.start_playback(track);
                if self.now_playing.is_none() {
                    // start_playback bailed (player gone mid-session, spawn
                    // failure). No PlaybackEnded will ever arrive to continue
                    // the chain, so don't leave a dead mode armed and
                    // invisible.
                    self.shuffle = false;
                    self.toast = Some(Toast::warning("shuffle off: couldn't start playback"));
                } else if self.shuffle_pool.needs_refill() {
                    self.spawn_shuffle_refill();
                }
            }
            None if self.shuffle_pool.fetch_inflight => {
                self.shuffle_pool.pending_play = true;
                self.toast = Some(Toast::confirmation("shuffle on · finding a jukebox post…"));
            }
            None if self.shuffle_pool.needs_refill() => {
                self.shuffle_pool.pending_play = true;
                self.toast = Some(Toast::confirmation("shuffle on · finding a jukebox post…"));
                self.spawn_shuffle_refill();
            }
            None => {
                // Empty pool and the last walk was dry: the feed has no (new)
                // jukebox posts to offer. Turn the mode back off rather than
                // leaving it armed and silent.
                self.shuffle = false;
                self.toast = Some(Toast::warning("shuffle off: no jukebox posts found"));
            }
        }
    }

    /// Walk a few pages of the global feed in the background, collecting posts
    /// with audio attachments for the shuffle pool. Bounded (at most
    /// [`super::shuffle::REFILL_MAX_PAGES`] pages per walk, early-out at
    /// [`super::shuffle::REFILL_TARGET`] finds) and paced, on top of the
    /// client's own per-endpoint rate limiting — shuffle must stay a music
    /// mode, not a crawler.
    fn spawn_shuffle_refill(&mut self) {
        if self.shuffle_pool.fetch_inflight {
            return;
        }
        self.shuffle_pool.fetch_inflight = true;
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let mut cursor = self.shuffle_pool.cursor.clone();
        // Generation guard: logout / toggling shuffle off bumps the epoch, so
        // a superseded walk stops fetching and its result is dropped by the
        // handler instead of repopulating a cleared pool.
        let epoch_ref = Arc::clone(&self.shuffle_epoch);
        let epoch = epoch_ref.load(Ordering::SeqCst);
        tokio::spawn(async move {
            let mut found: Vec<super::audio::JukeboxTrack> = Vec::new();
            for page in 0..super::shuffle::REFILL_MAX_PAGES {
                if page > 0 {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                if epoch_ref.load(Ordering::SeqCst) != epoch {
                    return;
                }
                match client.list_entries(cursor.as_deref(), Some(50)).await {
                    Ok((entries, next)) => {
                        found.extend(
                            entries
                                .iter()
                                .filter_map(|e| super::audio::jukebox_track(&e.attachments)),
                        );
                        cursor = next;
                        if cursor.is_none() || found.len() >= super::shuffle::REFILL_TARGET {
                            break;
                        }
                    }
                    Err(e) => {
                        let msg = note_api_err(&tx, e);
                        // A partial walk that found tracks still counts; only
                        // a walk that produced nothing surfaces as an error.
                        if found.is_empty() {
                            let _ = tx.send(BgEvent::ShuffleTracks {
                                epoch,
                                result: Err(msg),
                            });
                            return;
                        }
                        break;
                    }
                }
            }
            let _ = tx.send(BgEvent::ShuffleTracks {
                epoch,
                result: Ok((found, cursor)),
            });
        });
    }

    fn player_volume(&mut self, delta: i64) {
        if let Some(handle) = self.now_playing.as_mut() {
            handle.step_volume(delta);
            self.player_volume = handle.volume;
        }
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

    /// Re-read the unread count after a short delay, to converge on truth
    /// following an optimistic mark-read. The count endpoint is cached
    /// server-side (~5s); reading it immediately would return the pre-mark
    /// value and clobber the optimistic update (the badge would flick back to
    /// its old number until the next poll). Waiting past the cache window reads
    /// the post-mark truth instead.
    fn spawn_unread_count_resync(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(6)).await;
            match client.unread_notification_count().await {
                Ok(n) => {
                    let _ = tx.send(BgEvent::UnreadCount(n));
                }
                Err(e) => {
                    let msg = note_api_err(&tx, e);
                    tracing::debug!(error = %msg, "unread_count resync failed");
                }
            }
        });
    }

    fn spawn_profile_user_me(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .get_own_profile()
                .await
                .map_err(|e| note_api_err(&tx, e));
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

    /// Launch the external editor on `initial` (off the runtime thread), pausing
    /// the input reader so the editor owns the terminal exclusively (otherwise it
    /// loses keystrokes that then replay onto the TUI). Returns the edited content
    /// or an error string. Only reached on the `external_editor_set()` path.
    async fn run_editor(&mut self, initial: String) -> Result<String, String> {
        self.input_paused.store(true, Ordering::SeqCst);
        let result = tokio::task::spawn_blocking(move || launch_editor(&initial, ".md"))
            .await
            .map_err(|e| format!("editor task panicked: {e}"))
            .and_then(|r| r.map_err(|e| e.to_string()));
        self.input_paused.store(false, Ordering::SeqCst);
        // The editor re-entered a blank alternate screen; force a full repaint.
        self.force_clear = true;
        result
    }

    /// Whether the user opted into an external editor by setting `editor` in the
    /// config. When unset (the default) the built-in editor is used. The implicit
    /// `$VISUAL`/`$EDITOR` auto-launch was deliberately dropped: an environment
    /// editor that GUI-forks or is missing was the cause of "compose flashes and
    /// does nothing", so shelling out is now opt-in only.
    fn external_editor_set() -> bool {
        crate::config::get().editor.is_some()
    }

    fn toast_editor_failed(&mut self, err: &str) {
        tracing::warn!(error = %err, "compose: external editor failed");
        self.toast = Some(Toast::warning(
            "editor failed — check the `editor` config (GUI editors need a blocking flag, e.g. `code --wait`)",
        ));
    }

    fn toast_editor_empty(&mut self) {
        self.toast = Some(Toast::warning("editor returned nothing — post discarded"));
    }

    async fn start_compose(&mut self, kind: ComposeKind, prefill: String) {
        if Self::external_editor_set() {
            match self.run_editor(prefill).await {
                Ok(content) if !content.trim().is_empty() => {
                    self.push_screen(Screen::Compose(ComposeScreen::new(kind, content)));
                }
                Ok(_) => self.toast_editor_empty(),
                Err(msg) => self.toast_editor_failed(&msg),
            }
            return;
        }
        let screen = EditorScreen::new(
            EditorPurpose::NewBody {
                kind,
                prefill_topics: Vec::new(),
            },
            &prefill,
        );
        self.push_screen(Screen::Editor(screen));
    }

    /// Apply a built-in editor save, routing its content to the next screen by
    /// the editor's purpose: a fresh body opens the compose confirm view; a
    /// Ctrl+E re-edit returns to the compose screen it came from.
    fn editor_save(&mut self) {
        let (content, purpose) = match &self.screen {
            Screen::Editor(s) => (s.content(), s.purpose().clone()),
            _ => return,
        };
        match purpose {
            EditorPurpose::NewBody {
                kind,
                prefill_topics,
            } => {
                let mut screen = ComposeScreen::new(kind, content);
                if !prefill_topics.is_empty() {
                    screen.topics_input = prefill_topics.join(", ");
                }
                // Replace the editor with the confirm screen; back_stack already
                // holds the originating screen, so Esc from Compose returns there.
                self.screen = Screen::Compose(screen);
            }
            EditorPurpose::ReEditBody => {
                // The compose screen we re-edited is the top of the back stack.
                self.pop_screen();
                if let Screen::Compose(c) = &mut self.screen {
                    c.content = content;
                    c.error = None;
                }
            }
        }
    }

    /// Re-edit the body of the active compose screen (Ctrl+E), preserving the
    /// title/slug/topics/visibility fields. Uses the built-in editor by default,
    /// or the configured external editor.
    async fn re_edit_compose(&mut self) {
        let Screen::Compose(s) = &self.screen else {
            return;
        };
        let current = s.content.clone();
        if Self::external_editor_set() {
            match self.run_editor(current).await {
                Ok(content) => {
                    if let Screen::Compose(s) = &mut self.screen {
                        s.content = content;
                        s.error = None;
                    }
                }
                Err(msg) => self.toast_editor_failed(&msg),
            }
            return;
        }
        let screen = EditorScreen::new(EditorPurpose::ReEditBody, &current);
        self.push_screen(Screen::Editor(screen));
    }

    fn spawn_compose_submit(&self) {
        let (kind, content, title, slug, topics, is_public, is_nsfw) = match &self.screen {
            Screen::Compose(s) => (
                s.kind.clone(),
                s.content.clone(),
                s.title_to_send(),
                s.slug_to_send(),
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
                            slug.as_deref(),
                            &topics,
                            is_public,
                            is_nsfw,
                        )
                        .await
                        .map(|created| (created.post_id, created.slug))
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
                        .create_guild_thread(
                            &guild_slug,
                            &content,
                            title.as_deref(),
                            slug.as_deref(),
                            &topics,
                        )
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
        if Self::external_editor_set() {
            match self.run_editor(prefill).await {
                Ok(content) if !content.trim().is_empty() => {
                    let mut screen =
                        ComposeScreen::new(ComposeKind::UpdateNote { note_id }, content);
                    screen.topics_input = topics.join(", ");
                    self.push_screen(Screen::Compose(screen));
                }
                Ok(_) => self.toast_editor_empty(),
                Err(msg) => self.toast_editor_failed(&msg),
            }
            return;
        }
        let screen = EditorScreen::new(
            EditorPurpose::NewBody {
                kind: ComposeKind::UpdateNote { note_id },
                prefill_topics: topics,
            },
            &prefill,
        );
        self.push_screen(Screen::Editor(screen));
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
            let result = client
                .get_settings()
                .await
                .map_err(|e| note_api_err(&tx, e));
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

    /// Push the current follow/mute sets into whichever topic screen is active.
    fn push_topic_prefs(&mut self) {
        match &mut self.screen {
            Screen::Topics(s) => {
                s.set_topic_prefs(self.topic_follows.clone(), self.topic_mutes.clone());
            }
            Screen::TopicFeed(s) => {
                let followed = self.topic_follows.contains(&s.slug);
                let muted = self.topic_mutes.contains(&s.slug);
                s.set_topic_state(followed, muted);
            }
            _ => {}
        }
    }

    /// Fetch the user's followed/muted topic slugs from settings (lazy, once).
    fn spawn_topic_prefs_load(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .get_settings()
                .await
                .map(|s| {
                    (
                        s.followed_topics.unwrap_or_default(),
                        s.muted_topics.unwrap_or_default(),
                    )
                })
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::TopicPrefsLoaded(result));
        });
    }

    /// PATCH a follow/mute change to settings (the optimistic local change was
    /// already applied; a failure triggers a resync).
    fn spawn_save_topic_prefs(&self, update: SettingsUpdate) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            let result = client
                .update_settings(&update)
                .await
                .map(|_| ())
                .map_err(|e| note_api_err(&tx, e));
            let _ = tx.send(BgEvent::TopicPrefsSaved(result));
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
        let online_delay = crate::config::get().notifications_refresh_secs;
        tokio::spawn(async move {
            // Brief settle delay so the initial render lands before the first poll.
            tokio::time::sleep(Duration::from_secs(3)).await;
            // Doubles as a connectivity / session heartbeat: a successful poll
            // clears the offline marker, while a failure is funnelled through
            // `note_api_err` exactly like every instrumented request — so a
            // transport drop raises the offline marker and a terminal 401 logs
            // an *idle* user out (the poller is their only traffic). While
            // offline we poll faster (5s vs the configured interval), and a
            // `wake` notification cuts the sleep short so the marker clears
            // promptly on reconnect.
            loop {
                let next_delay = match client.unread_notification_count().await {
                    Ok(n) => {
                        if tx.send(BgEvent::UnreadCount(n)).is_err() {
                            return; // app gone
                        }
                        online_delay
                    }
                    Err(e) => {
                        let transport = e.is_transport();
                        let msg = note_api_err(&tx, e);
                        tracing::debug!(error = %msg, "unread_count poll failed");
                        if transport {
                            5
                        } else {
                            online_delay
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

    /// A slow background poll of the feed head. While the user is viewing the
    /// feed, it fetches the newest page and the UI prepends genuinely-new
    /// entries without moving the scroll position (see
    /// [`FeedScreen::apply_new_head`]). Gated by `feed_active` so it never
    /// fetches off-screen; the interval comes from config (`feed_refresh_secs`).
    /// One long-lived task, like the unread poller. Errors are funnelled through
    /// `note_api_err` for the connectivity side-channel but otherwise stay
    /// silent — a background poll must never nag the user.
    fn spawn_feed_head_poller(&self) {
        let client = self.client.clone();
        let tx = self.bg_tx.clone();
        let active = self.feed_active.clone();
        let interval = crate::config::get().feed_refresh_secs;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(interval)).await;
                if !active.load(Ordering::Relaxed) {
                    continue; // not viewing the feed — don't fetch
                }
                match client.list_entries(None, None).await {
                    Ok((entries, _cursor)) => {
                        if tx.send(BgEvent::FeedHead(entries)).is_err() {
                            return; // app gone
                        }
                    }
                    Err(e) => {
                        let msg = note_api_err(&tx, e);
                        tracing::debug!(error = %msg, "feed head poll failed");
                    }
                }
            }
        });
    }
}

/// Build a synthetic key-press event (used to translate mouse-wheel scrolls into
/// the same one-step navigation as the arrow keys).
/// Collapse runs of same-direction mouse-wheel events within one input burst,
/// keeping the first of each run. Some terminals/compositors emit several wheel
/// events per physical notch (high-resolution scrolling); without this, one
/// notch would move the selection by several rows. Distinct notches arrive in
/// separate bursts and so still register one move each. Non-scroll events pass
/// through unchanged and break a run.
fn coalesce_scroll(batch: Vec<Event>) -> Vec<Event> {
    let mut out = Vec::with_capacity(batch.len());
    let mut prev_scroll: Option<event::MouseEventKind> = None;
    for ev in batch {
        let scroll_kind = match &ev {
            Event::Mouse(m)
                if matches!(
                    m.kind,
                    event::MouseEventKind::ScrollDown | event::MouseEventKind::ScrollUp
                ) =>
            {
                Some(m.kind)
            }
            _ => None,
        };
        if scroll_kind.is_some() && scroll_kind == prev_scroll {
            continue; // repeat from the same physical notch — drop it
        }
        prev_scroll = scroll_kind;
        out.push(ev);
    }
    out
}

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
    e.user_message()
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
        App::with_theme(client, "you@example.com".into(), ThemeKind::Cyber, None)
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
        assert!(
            !text.contains("Cancel"),
            "menu chrome leaked with no menu open"
        );
    }

    fn key_event(code: KeyCode) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::empty()))
    }

    fn test_entry(post_id: &str) -> cs_api::Entry {
        cs_api::Entry {
            post_id: post_id.into(),
            author_id: "u".into(),
            author_username: "alice".into(),
            content: "hi".into(),
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
        }
    }

    fn test_reply(reply_id: &str, post_id: &str) -> cs_api::Reply {
        cs_api::Reply {
            reply_id: reply_id.into(),
            post_id: post_id.into(),
            author_id: "u".into(),
            author_username: "alice".into(),
            content: "yo".into(),
            parent_reply_id: None,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    #[test]
    fn reply_page_for_a_different_post_is_ignored() {
        // Regression: a stale reply page (open A, pop, open B before A's replies
        // arrive) used to land on whatever PostDetail was active.
        let mut app = test_app();
        app.screen = Screen::PostDetail(PostDetailScreen::new(test_entry("B")));

        app.handle_bg_event(BgEvent::DetailRepliesInitial {
            post_id: "A".into(),
            result: Ok((vec![test_reply("r1", "A")], None)),
        });
        let Screen::PostDetail(s) = &app.screen else {
            panic!("expected PostDetail");
        };
        assert!(
            s.replies.is_empty(),
            "stale page for post A must not land on B"
        );

        // The matching page for B applies normally.
        app.handle_bg_event(BgEvent::DetailRepliesInitial {
            post_id: "B".into(),
            result: Ok((vec![test_reply("r1", "B")], None)),
        });
        let Screen::PostDetail(s) = &app.screen else {
            panic!("expected PostDetail");
        };
        assert_eq!(s.replies.len(), 1, "matching reply page should apply");
    }

    #[tokio::test]
    async fn backspace_pops_a_pushed_screen() {
        let mut app = test_app();
        app.push_screen(Screen::PostDetail(PostDetailScreen::new(test_entry("p1"))));
        assert!(matches!(app.screen, Screen::PostDetail(_)));
        app.handle_terminal_event(key_event(KeyCode::Backspace))
            .await;
        assert!(
            !matches!(app.screen, Screen::PostDetail(_)),
            "backspace should pop a pushed screen (global back)"
        );
    }

    fn test_notification(id: &str) -> cs_api::Notification {
        cs_api::Notification {
            notification_id: id.into(),
            kind: cs_api::NotificationType::Reply,
            read: false,
            created_at: None,
            actor_id: None,
            actor_username: None,
            target_id: None,
            target_type: None,
            reason: None,
            metadata: cs_api::NotificationMetadata::default(),
        }
    }

    #[test]
    fn mark_failed_rolls_back_read_flag_and_unread_count() {
        let mut app = test_app();
        let mut screen = NotificationsScreen::new();
        screen.apply_initial(Ok((vec![test_notification("n1")], None)));
        screen.mark_local("n1"); // optimistic read
        app.screen = Screen::Notifications(screen);
        app.unread_count = 2; // pretend 3 → 2 was applied optimistically

        app.handle_bg_event(BgEvent::NotificationMarkFailed {
            notification_id: "n1".into(),
        });

        let Screen::Notifications(s) = &app.screen else {
            panic!("expected Notifications");
        };
        assert!(!s.list.items[0].read, "read flag should roll back");
        assert_eq!(app.unread_count, 3, "unread count should be restored");
        assert!(app.toast.is_some(), "a warning toast should be shown");
    }

    #[test]
    fn bookmark_remove_failed_raises_a_warning_toast() {
        let mut app = test_app();
        app.handle_bg_event(BgEvent::BookmarkRemoveFailed);
        assert!(app.toast.is_some(), "a failed removal should warn the user");
    }

    fn kev(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn route_key_maps_feed_intents_to_actions() {
        // Direct, synchronous coverage of the Phase-1 router (no side effects).
        let mut feed = FeedScreen::new();
        feed.apply_initial(Ok((vec![test_entry("p1")], None)));
        let mut screen = Screen::Feed(feed);

        assert_eq!(
            App::route_key(&mut screen, kev(KeyCode::Enter)),
            Action::OpenPostDetailById {
                post_id: "p1".into(),
                highlight_reply_id: None,
            }
        );
        assert_eq!(
            App::route_key(&mut screen, kev(KeyCode::Char('c'))),
            Action::StartComposeEntry
        );
        assert_eq!(
            App::route_key(&mut screen, kev(KeyCode::Char('b'))),
            Action::BookmarkPost {
                post_id: "p1".into()
            }
        );
        assert_eq!(
            App::route_key(&mut screen, kev(KeyCode::Char('x'))),
            Action::None
        );
    }

    fn kev_ctrl(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn editor(initial: &str) -> Screen {
        Screen::Editor(EditorScreen::new(EditorPurpose::ReEditBody, initial))
    }

    #[test]
    fn editor_screen_accepts_text_input() {
        // Printable keys (digits, ?, i, S, ...) must reach the editor, not the
        // global shortcuts.
        assert!(editor("").accepts_text_input());
    }

    #[test]
    fn route_key_maps_editor_intents_to_actions() {
        let mut screen = editor("hello");
        assert_eq!(
            App::route_key(&mut screen, kev_ctrl(KeyCode::Char('d'))),
            Action::EditorSave
        );
        assert_eq!(
            App::route_key(&mut screen, kev_ctrl(KeyCode::Char('c'))),
            Action::EditorCancel
        );
        assert_eq!(
            App::route_key(&mut screen, kev(KeyCode::Char('x'))),
            Action::None
        );
    }

    #[tokio::test]
    async fn compose_entry_opens_the_builtin_editor_over_the_feed() {
        let mut app = test_app();
        let mut feed = FeedScreen::new();
        feed.apply_initial(Ok((vec![test_entry("p1")], None)));
        app.screen = Screen::Feed(feed);
        app.current_root = Some(RootKind::Feed);
        app.handle_terminal_event(key_event(KeyCode::Char('c')))
            .await;
        assert!(
            matches!(app.screen, Screen::Editor(_)),
            "c opens the built-in editor"
        );
        assert!(
            matches!(app.back_stack.last(), Some(Screen::Feed(_))),
            "the originating feed is preserved beneath the editor"
        );
    }

    #[test]
    fn editor_save_newbody_opens_compose_confirm_and_keeps_originator() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        let mut ed = EditorScreen::new(
            EditorPurpose::NewBody {
                kind: ComposeKind::NewEntry,
                prefill_topics: vec!["music".into()],
            },
            "",
        );
        ed.paste("hello world");
        app.push_screen(Screen::Editor(ed));
        app.editor_save();
        let Screen::Compose(c) = &app.screen else {
            panic!("expected compose confirm after save");
        };
        assert_eq!(c.content, "hello world");
        assert_eq!(c.topics_input, "music");
        assert!(
            matches!(app.back_stack.last(), Some(Screen::Feed(_))),
            "Esc from the confirm screen must return to the feed"
        );
    }

    #[test]
    fn editor_reedit_roundtrip_preserves_compose_fields() {
        let mut app = test_app();
        let mut compose = ComposeScreen::new(ComposeKind::NewEntry, "old body".to_string());
        compose.title_input = "My Title".into();
        compose.topics_input = "a, b".into();
        app.screen = Screen::Compose(compose);
        // Ctrl+E pushed the editor over the compose screen.
        app.push_screen(editor("old body"));
        if let Screen::Editor(s) = &mut app.screen {
            s.paste(" + new");
        }
        app.editor_save();
        let Screen::Compose(c) = &app.screen else {
            panic!("expected to land back on compose");
        };
        assert_eq!(c.content, "old body + new");
        assert_eq!(c.title_input, "My Title", "title preserved");
        assert_eq!(c.topics_input, "a, b", "topics preserved");
    }

    #[tokio::test]
    async fn paste_event_routes_to_the_editor() {
        let mut app = test_app();
        app.screen = editor("");
        app.handle_terminal_event(Event::Paste("a\nb".into())).await;
        let Screen::Editor(s) = &app.screen else {
            panic!("still on editor");
        };
        assert_eq!(s.content(), "a\nb");
    }

    #[tokio::test]
    async fn paste_event_into_login_stays_single_line() {
        // Bracketed paste into a single-line field collapses newlines so it can't
        // break out of the field or trigger submit.
        let mut app = test_app();
        assert!(app.screen.is_login());
        app.handle_terminal_event(Event::Paste("a\nb".into())).await;
        let text = render_to_string(&app);
        assert!(
            text.contains("a b"),
            "newline collapsed to a space: {text:?}"
        );
    }

    #[tokio::test]
    async fn editor_ctrl_c_cancels_back_to_originator() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.push_screen(editor("draft"));
        app.handle_terminal_event(Event::Key(kev_ctrl(KeyCode::Char('c'))))
            .await;
        assert!(
            matches!(app.screen, Screen::Feed(_)),
            "Ctrl+C discards and returns to the feed"
        );
    }

    #[tokio::test]
    async fn editor_esc_cancels_back_to_originator() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.push_screen(editor("draft"));
        app.handle_terminal_event(key_event(KeyCode::Esc)).await;
        assert!(
            matches!(app.screen, Screen::Feed(_)),
            "Esc pops the editor via the global back handler"
        );
    }

    #[tokio::test]
    async fn feed_enter_opens_the_post_via_fast_path() {
        let mut app = test_app();
        let mut feed = FeedScreen::new();
        feed.apply_initial(Ok((vec![test_entry("p1")], None)));
        app.screen = Screen::Feed(feed);
        app.current_root = Some(RootKind::Feed);
        app.handle_terminal_event(key_event(KeyCode::Enter)).await;
        assert!(
            matches!(app.screen, Screen::PostDetail(_)),
            "enter routes through OpenPostDetailById to the post detail"
        );
    }

    #[tokio::test]
    async fn topic_feed_enter_opens_the_post() {
        let mut app = test_app();
        let mut tf = TopicFeedScreen::new("music".into());
        tf.apply_initial(Ok((vec![test_entry("p1")], None)));
        app.screen = Screen::TopicFeed(tf);
        app.handle_terminal_event(key_event(KeyCode::Enter)).await;
        assert!(matches!(app.screen, Screen::PostDetail(_)));
    }

    #[tokio::test]
    async fn enter_on_a_deleted_cached_entry_skips_the_fast_path() {
        // #22: a refresh can tombstone a cached entry; opening it must fetch
        // fresh rather than show a stale shell, so the fast path is skipped and
        // the (async) slow fetch leaves the screen on the feed for now.
        let mut app = test_app();
        let mut feed = FeedScreen::new();
        let mut e = test_entry("p1");
        e.deleted = true;
        feed.apply_initial(Ok((vec![e], None)));
        app.screen = Screen::Feed(feed);
        app.current_root = Some(RootKind::Feed);
        app.handle_terminal_event(key_event(KeyCode::Enter)).await;
        assert!(
            matches!(app.screen, Screen::Feed(_)),
            "deleted cached entry must not fast-path into a stale detail view"
        );
    }

    #[tokio::test]
    async fn toggling_topic_follow_updates_state_optimistically() {
        let mut app = test_app();
        let mut s = TopicsScreen::new();
        s.set_topics(
            vec![cs_api::Topic {
                slug: "music".into(),
                post_count: 1,
            }],
            true,
        );
        app.screen = Screen::Topics(s);
        app.current_root = Some(RootKind::Topics);

        app.handle_terminal_event(key_event(KeyCode::Char('f')))
            .await;
        assert!(
            app.topic_follows.iter().any(|s| s == "music"),
            "follow applied optimistically"
        );

        app.handle_terminal_event(key_event(KeyCode::Char('f')))
            .await;
        assert!(
            !app.topic_follows.iter().any(|s| s == "music"),
            "pressing f again unfollows"
        );
    }

    #[test]
    fn topic_prefs_loaded_populates_state() {
        let mut app = test_app();
        app.screen = Screen::Topics(TopicsScreen::new());
        app.handle_bg_event(BgEvent::TopicPrefsLoaded(Ok((
            vec!["music".into()],
            vec!["spam".into()],
        ))));
        assert_eq!(app.topic_follows, vec!["music".to_string()]);
        assert_eq!(app.topic_mutes, vec!["spam".to_string()]);
        assert!(app.topic_prefs_loaded);
    }

    #[tokio::test]
    async fn offline_blocks_an_optimistic_write_with_a_toast() {
        let mut app = test_app();
        let mut screen = NotificationsScreen::new();
        screen.apply_initial(Ok((vec![test_notification("n1")], None)));
        app.screen = Screen::Notifications(screen);
        app.current_root = Some(RootKind::Notifications);
        app.unread_count = 3;
        app.offline = true;

        // `m` on the unread item would normally mark it read optimistically.
        app.handle_terminal_event(key_event(KeyCode::Char('m')))
            .await;

        let Screen::Notifications(s) = &app.screen else {
            panic!("expected Notifications");
        };
        assert!(
            !s.list.items[0].read,
            "offline write must not optimistically mark"
        );
        assert_eq!(app.unread_count, 3, "unread count unchanged while offline");
        assert!(app.toast.is_some(), "offline write surfaces a toast");
    }

    #[test]
    fn rate_limit_countdown_is_not_clobbered_by_a_failure_event() {
        // The ApiSignal (rate-limit countdown) is queued just ahead of the
        // failure event; the generic warning must not overwrite it.
        let mut app = test_app();
        app.handle_api_signal(ApiSignal::RateLimited {
            retry_after_secs: 8,
        });
        app.handle_bg_event(BgEvent::BookmarkRemoveFailed);
        let text = render_to_string(&app);
        assert!(
            text.contains("rate limited"),
            "rate-limit countdown should survive the failure event: {text}"
        );
    }

    #[tokio::test]
    async fn question_mark_toggles_help_on_read_screens() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new()); // not a text-input screen
        app.handle_terminal_event(key_event(KeyCode::Char('?')))
            .await;
        assert!(app.help, "? should open help on the feed");
        app.handle_terminal_event(key_event(KeyCode::Char('j')))
            .await;
        assert!(!app.help, "any key should dismiss help");
    }

    #[tokio::test]
    async fn question_mark_is_text_on_the_login_screen() {
        let mut app = test_app(); // starts on Login (text input)
        app.handle_terminal_event(key_event(KeyCode::Char('?')))
            .await;
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
        app.handle_terminal_event(key_event(KeyCode::Char('2')))
            .await;
        assert!(
            matches!(app.screen, Screen::Notifications(_)),
            "2 should switch to notifications from a read screen"
        );
    }

    #[tokio::test]
    async fn digit_keys_do_not_navigate_away_from_text_input_screens() {
        // Compose is unconditionally text-input: a digit must reach the editor,
        // not navigate.
        let mut app = test_app();
        app.screen = Screen::Compose(ComposeScreen::new(ComposeKind::NewEntry, String::new()));
        app.current_root = Some(RootKind::Feed);
        app.handle_terminal_event(key_event(KeyCode::Char('2')))
            .await;
        assert!(
            matches!(app.screen, Screen::Compose(_)),
            "a digit on a text-input screen must reach the screen, not navigate"
        );
    }

    /// Build a loaded Settings screen focused on the field at `idx`.
    fn settings_focused(idx: usize) -> Screen {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        s.focused = idx;
        Screen::Settings(s)
    }

    #[tokio::test]
    async fn settings_toggle_lets_section_keys_through() {
        // On a toggle field (the default), header nav must leave Settings like
        // any read screen — both digits and ←/→.
        let mut app = test_app();
        app.screen = settings_focused(0); // filterNSFW — a Bool field
        app.current_root = Some(RootKind::Settings);
        app.handle_terminal_event(key_event(KeyCode::Char('2')))
            .await;
        assert!(
            matches!(app.screen, Screen::Notifications(_)),
            "a digit on a settings toggle should jump to that section"
        );

        let mut app = test_app();
        app.screen = settings_focused(0);
        app.current_root = Some(RootKind::Settings);
        app.handle_terminal_event(key_event(KeyCode::Left)).await;
        assert!(
            matches!(app.screen, Screen::Journal(_)),
            "Left on a settings toggle should cycle to the previous section"
        );
    }

    #[tokio::test]
    async fn settings_choice_field_lets_section_keys_through() {
        // Settings has no free-text fields, so a digit always navigates — even
        // when a cyclable choice field is focused (space cycles it, not digits).
        let mut app = test_app();
        app.screen = settings_focused(12); // timeDisplayFormat — a Choice field
        app.current_root = Some(RootKind::Settings);
        app.handle_terminal_event(key_event(KeyCode::Char('2')))
            .await;
        assert!(
            matches!(app.screen, Screen::Notifications(_)),
            "a digit on a settings choice field should jump to that section"
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
        let msg = note_api_err(
            &tx,
            ApiError::RateLimited {
                retry_after_secs: 12,
            },
        );
        assert!(
            msg.contains("retry after 12s"),
            "display string lost: {msg}"
        );
        assert!(matches!(
            drain_signal(&mut rx),
            ApiSignal::RateLimited {
                retry_after_secs: 12
            }
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
        app.handle_api_signal(ApiSignal::RateLimited {
            retry_after_secs: 8,
        });
        assert!(
            app.toast.is_some(),
            "rate-limit signal should raise a toast"
        );
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

    #[test]
    fn custom_theme_joins_the_cycle_when_configured() {
        let mut app = test_app();
        // No custom palette → only the built-ins.
        let built_in = ThemeKind::ALL.len();
        assert_eq!(app.available_theme_kinds().len(), built_in);
        assert!(!app.available_theme_kinds().contains(&ThemeKind::Custom));

        // With a custom palette → Custom is appended and resolves to it.
        let custom = Theme::dark();
        app.custom_theme = Some(custom.clone());
        let kinds = app.available_theme_kinds();
        assert_eq!(kinds.len(), built_in + 1);
        assert_eq!(kinds.last(), Some(&ThemeKind::Custom));
        assert_eq!(app.resolve_theme(ThemeKind::Custom).accent, custom.accent);

        // Without one, resolving Custom safely falls back to cyber.
        app.custom_theme = None;
        assert_eq!(
            app.resolve_theme(ThemeKind::Custom).accent,
            Theme::cyber().accent
        );
    }

    #[test]
    fn bookmark_result_raises_a_toast() {
        let mut app = test_app();
        app.handle_bg_event(BgEvent::BookmarkCreated(Ok("bm1".into())));
        assert!(app.toast.is_some(), "a successful bookmark should confirm");
        app.toast = None;
        app.handle_bg_event(BgEvent::BookmarkCreated(Err("conflict".into())));
        assert!(app.toast.is_some(), "a failed bookmark should warn");
    }

    #[tokio::test]
    async fn topics_prefetch_fills_cache_and_revisit_uses_it() {
        // tokio runtime needed: revisiting Topics now lazily spawns a prefs load.
        let mut app = test_app();
        app.screen = Screen::Topics(TopicsScreen::new());
        app.current_root = Some(RootKind::Topics);
        let epoch = app.topics_epoch.load(Ordering::SeqCst);

        // First warm-up page (not complete): cache grows, screen updates live.
        app.handle_bg_event(BgEvent::TopicsPrefetched {
            epoch,
            topics: vec![Topic {
                slug: "music".into(),
                post_count: 5,
            }],
            complete: false,
        });
        assert_eq!(app.topics_cache.len(), 1);
        assert!(!app.topics_complete);

        // Final page: completes the cache.
        app.handle_bg_event(BgEvent::TopicsPrefetched {
            epoch,
            topics: vec![Topic {
                slug: "linux".into(),
                post_count: 9,
            }],
            complete: true,
        });
        assert_eq!(app.topics_cache.len(), 2);
        assert!(app.topics_complete);

        // A stale page from a superseded run (wrong epoch) is ignored.
        app.handle_bg_event(BgEvent::TopicsPrefetched {
            epoch: epoch.wrapping_add(99),
            topics: vec![Topic {
                slug: "ghost".into(),
                post_count: 1,
            }],
            complete: false,
        });
        assert_eq!(
            app.topics_cache.len(),
            2,
            "stale-epoch page must be dropped"
        );

        // Revisiting topics fills the screen from the cache — no loading, no fetch.
        app.goto_root(RootKind::Topics);
        match &app.screen {
            Screen::Topics(s) => {
                assert!(!s.loading);
                assert!(s.complete);
                assert_eq!(s.items.len(), 2);
            }
            _ => panic!("expected the Topics screen"),
        }
    }

    #[test]
    fn coalesce_scroll_collapses_same_direction_runs() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let scroll = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::empty(),
            })
        };
        let down = || scroll(MouseEventKind::ScrollDown);
        let up = || scroll(MouseEventKind::ScrollUp);
        let keyev = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::empty(),
        ));

        // Five wheel events from one physical notch collapse to a single move.
        assert_eq!(
            coalesce_scroll(vec![down(), down(), down(), down(), down()]).len(),
            1
        );

        // Direction changes are kept; a key breaks the run and passes through.
        let out = coalesce_scroll(vec![down(), down(), up(), up(), keyev, down()]);
        assert_eq!(out.len(), 4, "expected down, up, key, down");
    }

    // Shuffle mode ------------------------------------------------------------

    /// A test entry carrying a jukebox (audio) attachment.
    fn audio_entry(post_id: &str, url: &str) -> cs_api::Entry {
        let mut e = test_entry(post_id);
        e.attachments = vec![cs_api::Attachment::Audio {
            src: url.into(),
            origin: "youtube".into(),
            artist: "a".into(),
            title: "t".into(),
            genre: String::new(),
        }];
        e
    }

    fn jukebox_track(url: &str) -> super::super::audio::JukeboxTrack {
        super::super::audio::JukeboxTrack {
            url: url.into(),
            artist: "a".into(),
            title: "t".into(),
        }
    }

    #[test]
    fn entry_pages_harvest_into_the_shuffle_pool() {
        let mut app = test_app();
        // Harvesting must work regardless of the active screen — pages are
        // scanned as they arrive, not when shuffle is enabled.
        app.handle_bg_event(BgEvent::FeedInitial(Ok((
            vec![
                audio_entry("p1", "https://youtu.be/one"),
                test_entry("p2"), // no attachment
            ],
            None,
        ))));
        assert_eq!(app.shuffle_pool.len(), 1);
        // A topic-feed page adds (and dedups against) the same pool.
        app.handle_bg_event(BgEvent::TopicFeedInitial {
            slug: "music".into(),
            result: Ok((
                vec![
                    audio_entry("p1", "https://youtu.be/one"), // already seen
                    audio_entry("p3", "https://youtu.be/two"),
                ],
                None,
            )),
        });
        assert_eq!(app.shuffle_pool.len(), 2);
    }

    #[tokio::test]
    async fn shuffle_toggle_without_mpv_warns_and_stays_off() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.mpv_available = Some(false);
        app.ytdlp_available = Some(false);
        app.handle_terminal_event(key_event(KeyCode::Char('S')))
            .await;
        assert!(!app.shuffle, "shuffle must not arm without a player");
        let text = render_to_string(&app);
        assert!(text.contains("mpv"), "missing-player toast: {text:?}");
    }

    #[tokio::test]
    async fn i_toggles_inline_images_and_forces_a_clear() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        assert!(app.images_on, "images start on");

        app.handle_terminal_event(key_event(KeyCode::Char('i')))
            .await;
        assert!(!app.images_on, "i turns images off");
        assert!(
            app.force_clear,
            "toggling forces a clear so mis-rendered garbage is wiped"
        );

        app.force_clear = false;
        app.handle_terminal_event(key_event(KeyCode::Char('i')))
            .await;
        assert!(app.images_on, "i turns images back on");
        assert!(app.force_clear, "re-enabling also forces a clear");
    }

    #[tokio::test]
    async fn i_is_ignored_while_a_text_field_is_focused() {
        // On a text-capturing screen `i` must reach the field, not toggle images.
        let mut app = test_app();
        assert!(app.screen.accepts_text_input(), "login captures text");
        app.handle_terminal_event(key_event(KeyCode::Char('i')))
            .await;
        assert!(app.images_on, "i typed into a field must not toggle images");
    }

    #[tokio::test]
    async fn shuffle_chains_when_a_track_ends_and_disarms_on_a_failed_start() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        // mpv "missing" makes the chained start_playback bail — observable
        // proof the chain reached the play step without spawning a real
        // process, and exercising the no-dead-armed-mode guard in one go.
        app.mpv_available = Some(false);
        app.ytdlp_available = Some(false);
        app.shuffle = true;
        app.shuffle_pool
            .add_tracks(vec![jukebox_track("https://youtu.be/next")]);
        let mut h = super::super::player::test_handle("https://youtu.be/current", 7);
        h.position_secs = 200.0; // played well past the suspect threshold
        app.now_playing = Some(h);

        app.handle_bg_event(BgEvent::PlaybackEnded { token: 7 });

        assert!(app.now_playing.is_none(), "ended track clears the bar");
        assert!(
            !app.shuffle,
            "a chain that cannot start playback must not stay armed"
        );
        let text = render_to_string(&app);
        assert!(
            text.contains("shuffle off"),
            "disarm reaches start_playback and is announced: {text:?}"
        );
    }

    #[test]
    fn shuffle_ignores_a_superseded_tracks_end() {
        let mut app = test_app();
        app.shuffle = true;
        app.shuffle_pool
            .add_tracks(vec![jukebox_track("https://youtu.be/next")]);
        app.now_playing = Some(super::super::player::test_handle("u", 9));
        // Token 8 is a previous track's exit racing in; the current track (9)
        // keeps playing and no advance happens.
        app.handle_bg_event(BgEvent::PlaybackEnded { token: 8 });
        assert!(app.now_playing.is_some(), "current track must survive");
    }

    #[tokio::test]
    async fn shuffle_gives_up_after_repeated_instant_failures() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.mpv_available = Some(false);
        app.ytdlp_available = Some(false);
        app.shuffle = true;
        app.shuffle_pool.add_tracks(vec![
            jukebox_track("https://youtu.be/a"),
            jukebox_track("https://youtu.be/b"),
        ]);
        for token in 1..=u64::from(SUSPECT_END_LIMIT) {
            // Re-arm by hand each round: with mpv stubbed out the chained
            // start_playback fails and disarms the mode (covered by its own
            // test), which would otherwise mask the breaker under test here.
            // The suspect counter survives, which is the part that matters.
            app.shuffle = true;
            // position_secs stays 0.0 and the handle is seconds old: the
            // track died before reporting progress — a suspect ending.
            app.now_playing = Some(super::super::player::test_handle("u", token));
            app.handle_bg_event(BgEvent::PlaybackEnded { token });
        }
        assert!(!app.shuffle, "breaker must trip after consecutive failures");
        let text = render_to_string(&app);
        assert!(text.contains("failing"), "breaker toast: {text:?}");
    }

    #[tokio::test]
    async fn shuffle_plays_on_refill_arrival_when_latched() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.mpv_available = Some(false);
        app.ytdlp_available = Some(false);
        app.shuffle = true;
        app.shuffle_pool.fetch_inflight = true;
        app.shuffle_pool.pending_play = true;

        app.handle_bg_event(BgEvent::ShuffleTracks {
            epoch: 0,
            result: Ok((
                vec![jukebox_track("https://youtu.be/found")],
                Some("c123".into()),
            )),
        });

        assert!(!app.shuffle_pool.pending_play, "latch consumed");
        assert_eq!(app.shuffle_pool.cursor.as_deref(), Some("c123"));
        let text = render_to_string(&app);
        assert!(
            text.contains("shuffle off"),
            "latch triggered playback (which then failed and disarmed, mpv \
             being stubbed out): {text:?}"
        );
    }

    #[test]
    fn shuffle_stale_refill_results_are_dropped() {
        let mut app = test_app();
        app.shuffle_pool.fetch_inflight = true;
        // The walk was superseded (logout, or shuffle toggled off) before its
        // result landed.
        app.shuffle_epoch.fetch_add(1, Ordering::SeqCst);
        app.handle_bg_event(BgEvent::ShuffleTracks {
            epoch: 0,
            result: Ok((
                vec![jukebox_track("https://youtu.be/stale")],
                Some("stale-cursor".into()),
            )),
        });
        assert_eq!(
            app.shuffle_pool.len(),
            0,
            "stale tracks must not repopulate the pool"
        );
        assert!(
            app.shuffle_pool.cursor.is_none(),
            "stale cursor must not install"
        );
    }

    #[test]
    fn toggling_shuffle_off_cancels_the_refill_walk() {
        let mut app = test_app();
        app.shuffle = true;
        app.shuffle_pool.fetch_inflight = true;
        let epoch_before = app.shuffle_epoch.load(Ordering::SeqCst);
        app.toggle_shuffle();
        assert!(!app.shuffle);
        assert!(
            !app.shuffle_pool.fetch_inflight,
            "the cancelled walk's flag must reset so re-enabling can refill"
        );
        assert!(
            app.shuffle_epoch.load(Ordering::SeqCst) > epoch_before,
            "the epoch bump is what invalidates the in-flight walk"
        );
    }

    #[test]
    fn play_history_push_dedupes_restarts_and_branches() {
        let mut app = test_app();
        app.push_play_history(jukebox_track("a"));
        app.push_play_history(jukebox_track("b"));
        app.push_play_history(jukebox_track("b")); // restart of the current track
        assert_eq!(app.play_history.len(), 2, "restart must not duplicate");
        assert_eq!(app.play_history_pos, 1);
        // Rewound to "a", then a new track: the forward branch ("b") is gone.
        app.play_history_pos = 0;
        app.push_play_history(jukebox_track("c"));
        let urls: Vec<_> = app.play_history.iter().map(|t| t.url.as_str()).collect();
        assert_eq!(urls, ["a", "c"]);
        assert_eq!(app.play_history_pos, 1);
    }

    #[test]
    fn prev_and_next_navigate_the_play_history() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        // With mpv stubbed out the nav playback bails, but the cursor logic —
        // what this test is about — still runs.
        app.mpv_available = Some(false);
        app.ytdlp_available = Some(false);
        app.play_history = vec![
            jukebox_track("https://youtu.be/a"),
            jukebox_track("https://youtu.be/b"),
            jukebox_track("https://youtu.be/c"),
        ];
        app.play_history_pos = 2;

        app.player_prev();
        assert_eq!(app.play_history_pos, 1);
        app.player_prev();
        assert_eq!(app.play_history_pos, 0);
        app.player_prev(); // at the oldest entry: stays put and warns
        assert_eq!(app.play_history_pos, 0);
        let text = render_to_string(&app);
        assert!(text.contains("no previous track"), "{text:?}");

        app.player_next(); // forward through history, not a random pick
        assert_eq!(app.play_history_pos, 1);
    }

    #[test]
    fn next_at_the_tip_picks_a_random_track_without_enabling_shuffle() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.mpv_available = Some(false);
        app.ytdlp_available = Some(false);
        app.shuffle_pool
            .add_tracks(vec![jukebox_track("https://youtu.be/other")]);
        app.now_playing = Some(super::super::player::test_handle("https://youtu.be/cur", 1));

        app.player_next();

        assert!(!app.shuffle, "a skip is not a mode change");
        // The pick reached start_playback, which bailed on the stubbed mpv.
        let text = render_to_string(&app);
        assert!(text.contains("mpv"), "skip reached the play step: {text:?}");
    }

    #[test]
    fn next_with_no_material_and_no_shuffle_points_at_shuffle() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.now_playing = Some(super::super::player::test_handle("https://youtu.be/cur", 1));
        app.player_next();
        let text = render_to_string(&app);
        assert!(text.contains("S starts shuffle"), "{text:?}");
    }

    #[test]
    fn natural_end_replays_forward_through_history_when_rewound() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.mpv_available = Some(false);
        app.ytdlp_available = Some(false);
        app.shuffle = true;
        app.play_history = vec![
            jukebox_track("https://youtu.be/a"),
            jukebox_track("https://youtu.be/b"),
        ];
        app.play_history_pos = 0; // rewound: "a" is playing, "b" is ahead
        let mut h = super::super::player::test_handle("https://youtu.be/a", 4);
        h.position_secs = 100.0; // a genuine full play, not a suspect end
        app.now_playing = Some(h);

        app.handle_bg_event(BgEvent::PlaybackEnded { token: 4 });

        assert_eq!(
            app.play_history_pos, 1,
            "mid-history end must replay forward, not pick randomly"
        );
    }

    #[test]
    fn armed_idle_shuffle_shows_the_search_bar() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.shuffle = true;
        app.shuffle_pool.pending_play = true;
        let text = render_to_string(&app);
        assert!(
            text.contains("finding a jukebox post"),
            "armed-idle shuffle must stay visible: {text:?}"
        );
    }

    #[test]
    fn shuffle_refill_error_disarms_an_idle_shuffle() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.shuffle = true;
        app.shuffle_pool.fetch_inflight = true;
        app.shuffle_pool.pending_play = true;
        app.handle_bg_event(BgEvent::ShuffleTracks {
            epoch: 0,
            result: Err("boom".into()),
        });
        assert!(
            !app.shuffle,
            "no music to deliver — mode must not stay armed"
        );
        assert!(!app.shuffle_pool.fetch_inflight);
        let text = render_to_string(&app);
        assert!(
            text.contains("shuffle off"),
            "the self-disarm must be announced, not just the fetch failure: {text:?}"
        );
    }

    #[test]
    fn explicit_stop_turns_shuffle_off() {
        let mut app = test_app();
        app.screen = Screen::Feed(FeedScreen::new());
        app.current_root = Some(RootKind::Feed);
        app.shuffle = true;
        app.now_playing = Some(super::super::player::test_handle("u", 1));
        app.player_stop();
        assert!(app.now_playing.is_none());
        assert!(!app.shuffle, "stop means stop — no chained follow-up");
        let text = render_to_string(&app);
        assert!(text.contains("shuffle off"), "stop toast: {text:?}");
    }
}

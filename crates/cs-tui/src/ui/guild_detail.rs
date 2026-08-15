//! Guild detail screen: one guild, with Threads and Members tabs. Reached by
//! pressing Enter on the guilds index; Enter on a thread opens it in the post
//! detail view.
//!
//! A user is no longer in at most one guild (API v0.8.6 § Guilds). They hold
//! one badge guild, as founder or member, which is the guild on their profile,
//! plus up to [`MAX_APPRENTICESHIPS`] apprenticeships. That reshapes all three
//! membership keys:
//!
//! - `J` join asks for a place and reads back which one the server gave, since
//!   it decides between member and apprentice (§ Join a Guild).
//! - `P` moves the profile badge onto an apprenticeship, demoting the guild
//!   the user was a member of to an apprenticeship rather than leaving it
//!   (§ Change Your Guild Badge).
//! - `L` leave means "leave this one": an apprenticeship goes without touching
//!   the badge, while leaving the badge guild clears the badge and promotes
//!   nothing (§ Leave a Guild).
//!
//! All three are armed by their key and sent by `y`: each is a 3/min, 15/day
//! write on a budget of its own (§ Rate Limits), so a mistyped key would spend
//! a third of that minute's allowance on an action nobody asked for.
//!
//! The Members tab lists members and apprentices in one list, oldest joined
//! first, exactly as the server pages them (§ List Guild Members), with every
//! row carrying its role. The website groups the two; grouping here would mean
//! sorting a roster that is still being paged in, and re-ordering it under the
//! reader as later pages land, so the split is reported in the header counts
//! instead.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{
    Guild, GuildMembership, GuildRole, GuildThread, JoinedGuild, PromotedGuild, UserGuild,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

/// The most apprenticeships one user can hold (API v0.8.6 § Guilds). A join
/// past this is refused with a 409, so the screen checks the cap itself rather
/// than spending one of the three joins a minute learning it.
pub const MAX_APPRENTICESHIPS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildTab {
    Threads,
    Members,
}

/// A membership write that has been armed by its key and is waiting for `y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildAction {
    /// Join this guild, as whichever role the server picks.
    Join,
    /// Make this apprenticeship the profile badge.
    Promote,
    /// Leave this guild.
    Leave,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuildIntent {
    /// Back to the guilds index.
    Back,
    /// Reload the active tab.
    Refresh,
    /// Next page of the active tab.
    LoadMore,
    /// Switch tab (emitted only when the new tab still needs its first fetch).
    SelectTab(GuildTab),
    /// Open the selected thread in the post-detail view.
    OpenThread {
        post_id: String,
    },
    /// Join this guild. The server decides between member and apprentice, so
    /// the reply has to be read rather than assumed.
    Join,
    /// Make this guild the viewer's profile badge, demoting the guild they are
    /// currently a member of to an apprenticeship.
    Promote,
    /// Leave this guild.
    Leave,
    /// Compose a new thread in this guild (members only).
    Compose,
    Quit,
    None,
}

#[derive(Debug)]
pub struct GuildScreen {
    pub slug: String,
    pub guild: Option<Guild>,
    pub tab: GuildTab,
    pub threads: Vec<GuildThread>,
    pub threads_cursor: Option<String>,
    pub threads_selected: usize,
    pub threads_loaded: bool,
    pub members: Vec<GuildMembership>,
    pub members_cursor: Option<String>,
    pub members_selected: usize,
    pub members_loaded: bool,
    pub loading: bool,
    /// True while a join/promote/leave request is in flight (prevents
    /// double-submit).
    pub action_pending: bool,
    /// The membership write armed by its key, waiting for `y` to send it.
    pub confirming: Option<GuildAction>,
    /// How the last membership action turned out, when it went through. Shown
    /// in the status bar; the API picks the role on a join, so what happened is
    /// worth stating outright.
    pub notice: Option<String>,
    /// Why the last membership action was refused, either by the screen or by
    /// the server. Kept out of `error` so a refused join leaves the thread list
    /// on screen instead of replacing it.
    pub action_error: Option<String>,
    /// The viewer's own guilds, from `GET /v1/users/me/guilds` when the shell
    /// has supplied them (API v0.8.6 § List a User's Guilds). Optional: without
    /// them the prompts fall back to wording that names no other guild.
    pub own_guilds: Option<Vec<UserGuild>>,
    pub error: Option<String>,
}

impl GuildScreen {
    pub fn new(slug: String) -> Self {
        Self {
            slug,
            guild: None,
            tab: GuildTab::Threads,
            threads: Vec::new(),
            threads_cursor: None,
            threads_selected: 0,
            threads_loaded: false,
            members: Vec::new(),
            members_cursor: None,
            members_selected: 0,
            members_loaded: false,
            loading: true,
            action_pending: false,
            confirming: None,
            notice: None,
            action_error: None,
            own_guilds: None,
            error: None,
        }
    }

    fn cur_len(&self) -> usize {
        match self.tab {
            GuildTab::Threads => self.threads.len(),
            GuildTab::Members => self.members.len(),
        }
    }

    fn cur_has_more(&self) -> bool {
        match self.tab {
            GuildTab::Threads => self.threads_cursor.is_some(),
            GuildTab::Members => self.members_cursor.is_some(),
        }
    }

    fn cur_sel_mut(&mut self) -> &mut usize {
        match self.tab {
            GuildTab::Threads => &mut self.threads_selected,
            GuildTab::Members => &mut self.members_selected,
        }
    }

    fn select_tab(&mut self, tab: GuildTab) -> GuildIntent {
        if self.tab == tab {
            return GuildIntent::None;
        }
        self.tab = tab;
        let loaded = match tab {
            GuildTab::Threads => self.threads_loaded,
            GuildTab::Members => self.members_loaded,
        };
        if loaded {
            GuildIntent::None
        } else {
            self.loading = true;
            GuildIntent::SelectTab(tab)
        }
    }

    /// The viewer's role here, which since v0.8.6 may be
    /// [`GuildRole::Apprentice`] (API v0.8.6 § Get Guild).
    fn viewer_role(&self) -> Option<GuildRole> {
        self.guild.as_ref().and_then(|g| g.role)
    }

    /// Whether the viewer is in this guild in any capacity.
    ///
    /// `isMember` and `role` both come from § Get Guild. The role is read as
    /// well as the flag so an apprentice still counts as being in the guild
    /// even if a server treats "member" narrowly, which matters because an
    /// apprentice may leave and may promote.
    fn viewer_is_in_guild(&self) -> bool {
        self.guild.as_ref().is_some_and(|g| {
            g.is_member
                || matches!(
                    g.role,
                    Some(GuildRole::Founder | GuildRole::Member | GuildRole::Apprentice)
                )
        })
    }

    /// The viewer's badge guild, when their own guild list has been supplied.
    fn badge_guild(&self) -> Option<&UserGuild> {
        self.own_guilds.as_ref()?.iter().find(|g| g.is_badge())
    }

    /// How many apprenticeships the viewer already holds, or `None` while their
    /// own guild list is unknown.
    fn apprenticeships_held(&self) -> Option<usize> {
        Some(
            self.own_guilds
                .as_ref()?
                .iter()
                .filter(|g| g.role == Some(GuildRole::Apprentice))
                .count(),
        )
    }

    /// Record how a membership action turned out.
    fn set_notice(&mut self, text: String) {
        self.notice = Some(text);
        self.action_error = None;
    }

    /// Record why a membership action was refused.
    fn set_action_error(&mut self, text: String) {
        self.action_error = Some(text);
        self.notice = None;
    }

    /// Arm `action`, clearing the last outcome so the prompt stands alone.
    fn arm(&mut self, action: GuildAction) -> GuildIntent {
        self.notice = None;
        self.action_error = None;
        self.confirming = Some(action);
        GuildIntent::None
    }

    /// Arm the join confirmation, or say why joining is not on offer.
    ///
    /// The server picks the role (§ Join a Guild), so the only thing worth
    /// checking first is the apprenticeship cap, which is the one refusal the
    /// screen can see coming.
    fn arm_join(&mut self) -> GuildIntent {
        if self.action_pending || self.guild.is_none() || self.viewer_is_in_guild() {
            return GuildIntent::None;
        }
        // The cap only bites when this join would BE an apprenticeship. § Join a
        // Guild states the role rule before the 409s: a viewer with no badge
        // guild joins as a member, which spends no apprenticeship slot. That
        // state is reachable and not exotic, since § Leave a Guild says leaving
        // your badge guild "clears the badge and promotes nothing", so refusing
        // on the count alone would strand such a user with no way back to a
        // badge guild short of abandoning an apprenticeship.
        if self.badge_guild().is_some()
            && self
                .apprenticeships_held()
                .is_some_and(|held| held >= MAX_APPRENTICESHIPS)
        {
            self.set_action_error(format!(
                "you already hold {MAX_APPRENTICESHIPS} apprenticeships, leave one before joining #{}",
                self.slug
            ));
            return GuildIntent::None;
        }
        self.arm(GuildAction::Join)
    }

    /// Arm the badge move, which is only meaningful for an apprenticeship.
    ///
    /// Promoting the guild the viewer already wears is the 200-with-nothing-
    /// changed case (§ Change Your Guild Badge), so it is answered on the spot
    /// instead of being sent, and a founder's badge cannot move at all, which
    /// is the 403.
    fn arm_promote(&mut self) -> GuildIntent {
        if self.action_pending {
            return GuildIntent::None;
        }
        match self.viewer_role() {
            Some(GuildRole::Apprentice) => {}
            Some(GuildRole::Founder | GuildRole::Member) => {
                self.set_notice(format!("#{} is already your profile badge", self.slug));
                return GuildIntent::None;
            }
            _ => return GuildIntent::None,
        }
        let founded = self
            .badge_guild()
            .filter(|b| b.role == Some(GuildRole::Founder))
            .map(|b| b.slug.clone());
        if let Some(slug) = founded {
            self.set_action_error(format!(
                "you founded #{slug}, so the badge can't move; hand that guild over on the web first"
            ));
            return GuildIntent::None;
        }
        self.arm(GuildAction::Promote)
    }

    /// Arm the leave confirmation. Apprentices may leave; founders may not,
    /// which the API answers with a 403 (§ Leave a Guild).
    fn arm_leave(&mut self) -> GuildIntent {
        if self.action_pending || !self.viewer_is_in_guild() {
            return GuildIntent::None;
        }
        if self.viewer_role() == Some(GuildRole::Founder) {
            self.set_action_error(
                "founders can't leave through the API, manage the guild on the web".to_string(),
            );
            return GuildIntent::None;
        }
        self.arm(GuildAction::Leave)
    }

    /// The question `y` answers, phrased with whatever the screen actually
    /// knows about the viewer's guilds. Without their own guild list it states
    /// the rule rather than guessing at an outcome.
    fn confirm_prompt(&self, action: GuildAction) -> String {
        let slug = &self.slug;
        let tail = "y=yes, any other key=cancel";
        match action {
            GuildAction::Join => match (self.badge_guild(), self.apprenticeships_held()) {
                (Some(badge), Some(held)) => format!(
                    "join #{slug} as an apprentice ({} of {MAX_APPRENTICESHIPS})? your #{} badge is unchanged. {tail}",
                    held + 1,
                    badge.slug
                ),
                (None, Some(_)) => {
                    format!("join #{slug} as a member? it becomes your profile badge. {tail}")
                }
                _ => format!(
                    "join #{slug}? you join as a member if you're in no guild yet, as an apprentice otherwise. {tail}"
                ),
            },
            // badge_guild() answers None for two different states, and they call
            // for different copy: the list is not loaded yet, or the list IS
            // loaded and the viewer holds no badge guild. The second is what
            // § Leave a Guild steers people into, and there is no guild to step
            // down, so promising one would be a lie.
            GuildAction::Promote => match (self.badge_guild(), self.own_guilds.is_some()) {
                (Some(badge), _) => format!(
                    "make #{slug} your profile badge? #{} becomes an apprenticeship, so you stay in it. {tail}",
                    badge.slug
                ),
                (None, true) => format!(
                    "make #{slug} your profile badge? you hold no guild as a member, so nothing steps down. {tail}"
                ),
                (None, false) => format!(
                    "make #{slug} your profile badge? if you're a member of another guild it becomes an apprenticeship, so you stay in it. {tail}"
                ),
            },
            GuildAction::Leave => {
                if self.viewer_role() == Some(GuildRole::Apprentice) {
                    format!(
                        "leave #{slug}? it's an apprenticeship, so your profile badge is unchanged. {tail}"
                    )
                } else {
                    format!(
                        "leave #{slug}? this clears your profile badge and promotes nothing in its place. {tail}"
                    )
                }
            }
        }
    }

    /// The membership keys worth advertising for the viewer's role here.
    fn action_hints(&self) -> &'static str {
        if self.action_pending {
            return " · working…";
        }
        if self.guild.is_none() {
            return "";
        }
        if !self.viewer_is_in_guild() {
            return " · c new · J join";
        }
        match self.viewer_role() {
            Some(GuildRole::Founder) => " · c new",
            Some(GuildRole::Apprentice) => " · c new · P badge · L leave",
            _ => " · c new · L leave",
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> GuildIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return GuildIntent::Quit;
        }
        // An armed membership write owns the next keystroke: `y` sends it, any
        // other key cancels. Join, promote and leave are each 3/min, 15/day, so
        // a slip of the finger would spend a third of the minute's allowance.
        if let Some(action) = self.confirming {
            self.confirming = None;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.action_pending = true;
                return match action {
                    GuildAction::Join => GuildIntent::Join,
                    GuildAction::Promote => GuildIntent::Promote,
                    GuildAction::Leave => GuildIntent::Leave,
                };
            }
            return GuildIntent::None;
        }
        if key.code == KeyCode::Backspace {
            return GuildIntent::Back;
        }
        // Tab switching is allowed even while a tab is loading. Tab/Shift+Tab
        // toggle the two tabs; h/l jump directly (vim aliases).
        match key.code {
            KeyCode::Char('h') => {
                return self.select_tab(GuildTab::Threads);
            }
            KeyCode::Char('l') => {
                return self.select_tab(GuildTab::Members);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let other = match self.tab {
                    GuildTab::Threads => GuildTab::Members,
                    GuildTab::Members => GuildTab::Threads,
                };
                return self.select_tab(other);
            }
            KeyCode::Char('J') => return self.arm_join(),
            KeyCode::Char('P') => return self.arm_promote(),
            KeyCode::Char('L') => return self.arm_leave(),
            KeyCode::Char('c') => {
                // v0.8.4: guild forums are open, any authenticated user can
                // start a thread, membership not required.
                if self.guild.is_some() {
                    return GuildIntent::Compose;
                }
                return GuildIntent::None;
            }
            _ => {}
        }
        if self.loading {
            return GuildIntent::None;
        }
        // Pre-compute len/has_more so the &mut from `cur_sel_mut` doesn't clash
        // with the immutable reads inside the shared nav call.
        let len = self.cur_len();
        let has_more = self.cur_has_more();
        match super::list_nav::navigate(key.code, self.cur_sel_mut(), len, has_more) {
            super::list_nav::ListNav::LoadMore => {
                self.loading = true;
                return GuildIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return GuildIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        match key.code {
            KeyCode::Char('r') => {
                self.loading = true;
                self.error = None;
                self.notice = None;
                self.action_error = None;
                return GuildIntent::Refresh;
            }
            KeyCode::Enter if self.tab == GuildTab::Threads => {
                if let Some(t) = self.threads.get(self.threads_selected) {
                    return GuildIntent::OpenThread {
                        post_id: t.entry.post_id.clone(),
                    };
                }
            }
            _ => {}
        }
        GuildIntent::None
    }

    pub fn apply_guild(&mut self, result: Result<Guild, String>) {
        match result {
            Ok(g) => self.guild = Some(g),
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_threads_initial(
        &mut self,
        result: Result<(Vec<GuildThread>, Option<String>), String>,
    ) {
        self.loading = false;
        self.threads_loaded = true;
        match result {
            Ok((items, cursor)) => {
                self.threads = items;
                self.threads_cursor = cursor;
                if self.threads_selected >= self.threads.len() {
                    self.threads_selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_threads_more(
        &mut self,
        result: Result<(Vec<GuildThread>, Option<String>), String>,
    ) {
        self.loading = false;
        match result {
            Ok((mut items, cursor)) => {
                self.threads.append(&mut items);
                self.threads_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_members_initial(
        &mut self,
        result: Result<(Vec<GuildMembership>, Option<String>), String>,
    ) {
        self.loading = false;
        self.members_loaded = true;
        match result {
            Ok((items, cursor)) => {
                self.members = items;
                self.members_cursor = cursor;
                if self.members_selected >= self.members.len() {
                    self.members_selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_members_more(
        &mut self,
        result: Result<(Vec<GuildMembership>, Option<String>), String>,
    ) {
        self.loading = false;
        match result {
            Ok((mut items, cursor)) => {
                self.members.append(&mut items);
                self.members_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    /// Fold in the viewer's own guilds (API v0.8.6 § List a User's Guilds).
    ///
    /// Enrichment, not a dependency: it lets the prompts name the badge guild
    /// and count apprenticeships, and lets the screen refuse a join that would
    /// pass the cap. A failure therefore leaves the plainer wording in place
    /// rather than putting an error on a screen that works without it.
    pub fn apply_own_guilds(&mut self, result: Result<Vec<UserGuild>, String>) {
        if let Ok(guilds) = result {
            self.own_guilds = Some(guilds);
        }
    }

    /// Fold in the result of a join (API v0.8.6 § Join a Guild).
    ///
    /// The role in the reply is the answer, not a formality: a join is a
    /// membership only when the viewer had no badge guild, and an
    /// apprenticeship otherwise, which counts against `apprenticeCount` and
    /// leaves the profile badge alone.
    pub fn apply_joined(&mut self, result: Result<JoinedGuild, String>) {
        self.action_pending = false;
        match result {
            Ok(j) => {
                let role = j.role.or_else(|| self.joined_role_fallback());
                if let Some(g) = &mut self.guild {
                    g.is_member = true;
                    g.role = role;
                    // Anything but a stated apprenticeship is counted as a
                    // member, which is what a join meant before v0.8.6. A
                    // refresh replaces the guess with the server's counts.
                    if role == Some(GuildRole::Apprentice) {
                        g.apprentice_count = g.apprentice_count.saturating_add(1);
                    } else {
                        g.member_count = g.member_count.saturating_add(1);
                    }
                }
                self.record_own_membership(j.guild_id, role);
                self.set_notice(match role {
                    Some(GuildRole::Apprentice) => format!(
                        "joined #{} as an apprentice, your profile badge is unchanged",
                        self.slug
                    ),
                    Some(GuildRole::Founder | GuildRole::Member) => format!(
                        "joined #{} as a member, it is now your profile badge",
                        self.slug
                    ),
                    _ => format!("joined #{}", self.slug),
                });
                self.error = None;
            }
            Err(msg) => self.set_action_error(format!("couldn't join #{}: {msg}", self.slug)),
        }
    }

    /// Fold in the result of a badge move (API v0.8.6 § Change Your Guild
    /// Badge).
    ///
    /// A promotion turns this apprenticeship into the membership and pushes the
    /// old membership down to an apprenticeship, so it moves one head between
    /// the two counts rather than adding one. Promoting the guild the viewer
    /// already wears changes nothing, and the reply says so by reporting the
    /// role they already held.
    pub fn apply_promoted(&mut self, result: Result<PromotedGuild, String>) {
        self.action_pending = false;
        match result {
            Ok(p) => {
                let was = self.viewer_role();
                let now = p.role.unwrap_or(GuildRole::Member);
                let moved = was == Some(GuildRole::Apprentice) && now != GuildRole::Apprentice;
                if let Some(g) = &mut self.guild {
                    g.is_member = true;
                    g.role = Some(now);
                    if moved {
                        g.apprentice_count = g.apprentice_count.saturating_sub(1);
                        g.member_count = g.member_count.saturating_add(1);
                    }
                }
                if moved {
                    // Read this BEFORE promote_own_guilds rewrites the roles.
                    // With no badge guild there was nothing to step down, which
                    // is the state § Leave a Guild leaves you in.
                    let had_badge = self.badge_guild().is_some();
                    self.promote_own_guilds();
                    self.set_notice(if had_badge {
                        format!(
                            "#{} is your profile badge now, your old guild became an apprenticeship",
                            self.slug
                        )
                    } else {
                        format!("#{} is your profile badge now", self.slug)
                    });
                } else {
                    self.set_notice(format!(
                        "#{} was already your profile badge, nothing changed",
                        self.slug
                    ));
                }
                self.error = None;
            }
            Err(msg) => {
                self.set_action_error(format!("couldn't make #{} your badge: {msg}", self.slug))
            }
        }
    }

    /// Fold in the result of a leave (API v0.8.6 § Leave a Guild).
    ///
    /// Which membership went is decided by the slug, not by the badge: leaving
    /// an apprenticeship leaves the badge alone, and leaving the badge guild
    /// clears it and promotes nothing, so the viewer is told which happened.
    pub fn apply_left(&mut self, result: Result<String, String>) {
        self.action_pending = false;
        match result {
            Ok(_) => {
                let was = self.viewer_role();
                let apprenticeship = was == Some(GuildRole::Apprentice);
                if let Some(g) = &mut self.guild {
                    g.is_member = false;
                    g.role = None;
                    if apprenticeship {
                        g.apprentice_count = g.apprentice_count.saturating_sub(1);
                    } else {
                        g.member_count = g.member_count.saturating_sub(1);
                    }
                }
                let slug = self.slug.clone();
                if let Some(own) = &mut self.own_guilds {
                    own.retain(|g| g.slug != slug);
                }
                self.set_notice(if apprenticeship {
                    format!("left #{slug}, your profile badge is unchanged")
                } else {
                    format!("left #{slug}, you have no profile badge now, promote an apprenticeship to fill it")
                });
                self.error = None;
            }
            Err(msg) => self.set_action_error(format!("couldn't leave #{}: {msg}", self.slug)),
        }
    }

    /// What a join must have made the viewer when the reply omitted the role:
    /// an apprentice if they already hold a badge guild, a member if they are
    /// known to hold none, and nothing at all while their guilds are unknown.
    fn joined_role_fallback(&self) -> Option<GuildRole> {
        let own = self.own_guilds.as_ref()?;
        if own.iter().any(UserGuild::is_badge) {
            Some(GuildRole::Apprentice)
        } else {
            Some(GuildRole::Member)
        }
    }

    /// Mirror a fresh membership into the viewer's own guild list so the
    /// prompts stay right without waiting for a refetch.
    ///
    /// The server orders that list badge guild first (§ List a User's Guilds);
    /// an appended entry can break that order, which is harmless here because
    /// the badge is found by role rather than by position.
    fn record_own_membership(&mut self, guild_id: String, role: Option<GuildRole>) {
        if self.own_guilds.is_none() {
            return;
        }
        let guild = self.guild.as_ref();
        let entry = UserGuild {
            guild_id,
            slug: self.slug.clone(),
            name: guild.map(|g| g.name.clone()).unwrap_or_default(),
            icon: guild.and_then(|g| g.icon.clone()),
            profile_picture_url: guild.and_then(|g| g.profile_picture_url.clone()),
            role,
            joined_at: None,
        };
        if let Some(own) = &mut self.own_guilds {
            own.retain(|g| g.slug != entry.slug);
            own.push(entry);
        }
    }

    /// Mirror a badge move into the viewer's own guild list: this guild takes
    /// the badge, and the guild that held it becomes an apprenticeship rather
    /// than being left (§ Change Your Guild Badge).
    fn promote_own_guilds(&mut self) {
        let slug = self.slug.clone();
        let Some(own) = &mut self.own_guilds else {
            return;
        };
        for g in own.iter_mut() {
            if g.slug == slug {
                g.role = Some(GuildRole::Member);
            } else if g.is_badge() {
                g.role = Some(GuildRole::Apprentice);
            }
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let name = self
            .guild
            .as_ref()
            .map(|g| g.name.as_str())
            .unwrap_or(&self.slug);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(
                format!(" cs-tui • {name} "),
                theme.heading_style(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Length(1), // tab bar
                Constraint::Min(1),    // list
                Constraint::Length(1), // status
            ])
            .split(inner);

        frame.render_widget(self.header_line(theme), layout[0]);
        frame.render_widget(self.tab_line(theme), layout[1]);

        if self.loading && self.cur_len() == 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled("loading…", theme.accent_style()))),
                layout[2],
            );
        } else if let Some(msg) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style()))),
                layout[2],
            );
        } else {
            self.render_list(frame, layout[2], theme);
        }

        let base = match self.tab {
            GuildTab::Threads => "tab/h/l tabs · enter open · scroll for more · r refresh",
            GuildTab::Members => "tab/h/l tabs · scroll for more · r refresh",
        };
        // An armed prompt outranks the last outcome, which outranks the keys:
        // whichever is showing, the line answers "what happens next?".
        let (status, style) = if let Some(action) = self.confirming {
            (self.confirm_prompt(action), theme.warning_style())
        } else if let Some(msg) = &self.action_error {
            (msg.clone(), theme.error_style())
        } else if let Some(msg) = &self.notice {
            (msg.clone(), theme.success_style())
        } else {
            (
                format!("{base}{} · esc back", self.action_hints()),
                theme.muted_style(),
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status, style))),
            layout[3],
        );
    }

    fn header_line(&self, theme: &Theme) -> Paragraph<'static> {
        let text = match &self.guild {
            Some(g) => {
                // Spelling out the badge for the two badge roles is what tells
                // an apprentice their profile is unaffected by this guild.
                let membership = match g.role {
                    Some(GuildRole::Founder) => "  · you: founder, your profile badge",
                    Some(GuildRole::Member) => "  · you: member, your profile badge",
                    Some(GuildRole::Apprentice) => "  · you: apprentice",
                    _ if g.is_member => "  · you: member",
                    _ => "",
                };
                // `icon` is an identifier string, not a glyph, so it isn't
                // rendered as text.
                format!(
                    "#{} · {}{}",
                    g.slug,
                    super::guilds::headcount_label(g),
                    membership
                )
            }
            None => format!("#{}", self.slug),
        };
        Paragraph::new(Line::from(Span::styled(text, theme.muted_style())))
    }

    fn tab_line(&self, theme: &Theme) -> Paragraph<'static> {
        let tab_span = |label: &'static str, active: bool| {
            let style = if active {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            Span::styled(label, style)
        };
        Paragraph::new(Line::from(vec![
            tab_span("Threads", self.tab == GuildTab::Threads),
            Span::styled("  │  ", theme.muted_style()),
            tab_span("Members", self.tab == GuildTab::Members),
        ]))
    }

    fn render_list(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        match self.tab {
            GuildTab::Threads => {
                if self.threads.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            "no threads yet",
                            theme.muted_style(),
                        ))),
                        area,
                    );
                    return;
                }
                let items: Vec<ListItem<'_>> =
                    self.threads.iter().map(|t| thread_item(t, theme)).collect();
                let list = List::new(items)
                    .highlight_style(theme.accent_style())
                    .highlight_symbol("▌ ");
                let mut state = ListState::default();
                state.select(Some(
                    self.threads_selected
                        .min(self.threads.len().saturating_sub(1)),
                ));
                frame.render_stateful_widget(list, area, &mut state);
            }
            GuildTab::Members => {
                if self.members.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled("no members", theme.muted_style()))),
                        area,
                    );
                    return;
                }
                let items: Vec<ListItem<'_>> =
                    self.members.iter().map(|m| member_item(m, theme)).collect();
                let list = List::new(items)
                    .highlight_style(theme.accent_style())
                    .highlight_symbol("▌ ");
                let mut state = ListState::default();
                state.select(Some(
                    self.members_selected
                        .min(self.members.len().saturating_sub(1)),
                ));
                frame.render_stateful_widget(list, area, &mut state);
            }
        }
    }
}

fn thread_item<'a>(t: &'a GuildThread, theme: &Theme) -> ListItem<'a> {
    let e = &t.entry;
    let when = e
        .created_at
        .map(crate::config::format_list_timestamp)
        .unwrap_or_default();
    let mut header_spans = vec![
        Span::styled(format!("@{}", e.author_username), theme.accent_style()),
        Span::styled(
            format!(" · {when} · {} replies", e.replies_count),
            theme.muted_style(),
        ),
    ];
    if super::images::has_image(e) {
        header_spans.push(Span::styled(" · [image]", theme.accent_style()));
    }
    let mut lines = vec![Line::from(header_spans)];
    if let Some(title) = e.title.as_deref() {
        let title = title.trim();
        if !title.is_empty() {
            lines.push(Line::from(Span::styled(
                super::text::first_line_truncated(title, 200),
                theme.accent_style(),
            )));
        }
    }
    let snippet = super::markdown::content_preview(&e.content, crate::config::get().preview_length);
    if !snippet.is_empty() {
        lines.push(Line::from(Span::styled(snippet, theme.base())));
    }
    if !crate::config::get().compact {
        lines.push(Line::from(""));
    }
    ListItem::new(lines)
}

fn member_item<'a>(m: &'a GuildMembership, theme: &Theme) -> ListItem<'a> {
    // Members and apprentices arrive in one list (API v0.8.6 § List Guild
    // Members), so the role is the only thing telling them apart. The word for
    // it is the guilds module's, so a role reads the same here as on a profile.
    let mut meta = String::new();
    if let Some(role) = super::guilds::role_label(m.role) {
        meta.push_str("  ");
        meta.push_str(role);
    }
    if let Some(name) = m
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        meta.push_str(if meta.is_empty() { "  " } else { " · " });
        meta.push_str(name);
    }
    let line = Line::from(vec![
        Span::styled(format!("@{}", m.username), theme.accent_style()),
        Span::styled(meta, theme.muted_style()),
    ]);
    ListItem::new(vec![line])
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

    fn thread(post_id: &str) -> GuildThread {
        let entry = cs_api::Entry {
            post_id: post_id.into(),
            author_username: "alice".into(),
            content: format!("thread {post_id}"),
            ..Default::default()
        };
        GuildThread {
            entry,
            guild_id: Some("g1".into()),
            guild_slug: Some("owls".into()),
            is_guild_thread: true,
        }
    }

    #[test]
    fn starts_on_threads_tab_loading() {
        let s = GuildScreen::new("owls".into());
        assert_eq!(s.tab, GuildTab::Threads);
        assert!(s.loading);
    }

    #[test]
    fn backspace_returns_back() {
        let mut s = GuildScreen::new("owls".into());
        assert_eq!(s.handle_key(key(KeyCode::Backspace)), GuildIntent::Back);
    }

    #[test]
    fn switching_to_members_first_time_requests_fetch() {
        let mut s = GuildScreen::new("owls".into());
        s.apply_threads_initial(Ok((vec![thread("p1")], None))); // clears loading
        let intent = s.handle_key(key(KeyCode::Char('l')));
        assert_eq!(intent, GuildIntent::SelectTab(GuildTab::Members));
        assert_eq!(s.tab, GuildTab::Members);
        assert!(s.loading);
    }

    #[test]
    fn switching_back_to_loaded_tab_does_not_refetch() {
        let mut s = GuildScreen::new("owls".into());
        s.apply_threads_initial(Ok((vec![thread("p1")], None)));
        s.apply_members_initial(Ok((vec![], None)));
        s.tab = GuildTab::Members;
        let intent = s.handle_key(key(KeyCode::Char('h'))); // back to Threads (loaded)
        assert_eq!(intent, GuildIntent::None);
        assert_eq!(s.tab, GuildTab::Threads);
    }

    #[test]
    fn tab_toggles_between_tabs() {
        let mut s = GuildScreen::new("owls".into());
        s.apply_threads_initial(Ok((vec![thread("p1")], None)));
        assert_eq!(s.tab, GuildTab::Threads);
        s.handle_key(key(KeyCode::Tab));
        assert_eq!(s.tab, GuildTab::Members);
        s.handle_key(key(KeyCode::Tab));
        assert_eq!(s.tab, GuildTab::Threads);
    }

    #[test]
    fn j_at_bottom_auto_loads_current_tab() {
        let mut s = GuildScreen::new("owls".into());
        s.apply_threads_initial(Ok((vec![thread("p1")], Some("cur".into()))));
        // One thread, selection at the bottom, cursor present → j paginates.
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, GuildIntent::LoadMore);
        assert!(s.loading);
    }

    #[test]
    fn enter_on_thread_opens_it() {
        let mut s = GuildScreen::new("owls".into());
        s.apply_threads_initial(Ok((vec![thread("p1"), thread("p2")], None)));
        s.threads_selected = 1;
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            GuildIntent::OpenThread {
                post_id: "p2".into()
            }
        );
    }

    #[test]
    fn enter_on_members_tab_does_nothing() {
        let mut s = GuildScreen::new("owls".into());
        s.apply_threads_initial(Ok((vec![thread("p1")], None)));
        s.apply_members_initial(Ok((vec![], None)));
        s.tab = GuildTab::Members;
        assert_eq!(s.handle_key(key(KeyCode::Enter)), GuildIntent::None);
    }

    #[test]
    fn j_advances_within_threads() {
        let mut s = GuildScreen::new("owls".into());
        s.apply_threads_initial(Ok((vec![thread("p1"), thread("p2"), thread("p3")], None)));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.threads_selected, 2);
    }

    fn with_guild(is_member: bool, role: Option<GuildRole>) -> GuildScreen {
        let mut s = GuildScreen::new("owls".into());
        s.guild = Some(Guild {
            id: "g1".into(),
            slug: "owls".into(),
            member_count: 5,
            is_member,
            role,
            ..Default::default()
        });
        s
    }

    fn own(slug: &str, role: GuildRole) -> UserGuild {
        UserGuild {
            guild_id: format!("{slug}-id"),
            slug: slug.into(),
            name: slug.into(),
            role: Some(role),
            ..Default::default()
        }
    }

    fn membership(username: &str, role: GuildRole) -> GuildMembership {
        GuildMembership {
            membership_id: format!("g1_{username}"),
            guild_id: "g1".into(),
            guild_slug: "owls".into(),
            user_id: username.into(),
            username: username.into(),
            role: Some(role),
            joined_at: None,
            display_name: None,
            profile_picture_url: None,
        }
    }

    fn rendered(s: &GuildScreen, width: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, 10);
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
    fn j_arms_the_join_and_y_sends_it() {
        let mut s = with_guild(false, None);
        assert_eq!(s.handle_key(key(KeyCode::Char('J'))), GuildIntent::None);
        assert_eq!(s.confirming, Some(GuildAction::Join));
        assert!(!s.action_pending, "arming spends nothing");

        assert_eq!(s.handle_key(key(KeyCode::Char('y'))), GuildIntent::Join);
        assert!(s.action_pending);
        assert!(s.confirming.is_none());
    }

    #[test]
    fn any_other_key_cancels_an_armed_action() {
        let mut s = with_guild(false, None);
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(s.handle_key(key(KeyCode::Char('h'))), GuildIntent::None);
        assert!(s.confirming.is_none());
        assert!(!s.action_pending);
        assert_eq!(s.tab, GuildTab::Threads, "the cancelling key isn't reused");
    }

    #[test]
    fn join_is_not_offered_to_someone_already_in_the_guild() {
        // An apprenticeship counts as being in the guild even if a server
        // reserves `isMember` for the badge roles.
        let mut apprentice = with_guild(false, Some(GuildRole::Apprentice));
        assert_eq!(
            apprentice.handle_key(key(KeyCode::Char('J'))),
            GuildIntent::None
        );
        assert!(apprentice.confirming.is_none());

        let mut member = with_guild(true, Some(GuildRole::Member));
        assert_eq!(
            member.handle_key(key(KeyCode::Char('J'))),
            GuildIntent::None
        );
        assert!(member.confirming.is_none());
    }

    #[test]
    fn join_is_refused_before_it_spends_a_request_at_five_apprenticeships() {
        let mut s = with_guild(false, None);
        let mut mine = vec![own("home", GuildRole::Member)];
        for i in 0..MAX_APPRENTICESHIPS {
            mine.push(own(&format!("a{i}"), GuildRole::Apprentice));
        }
        s.apply_own_guilds(Ok(mine));

        assert_eq!(s.handle_key(key(KeyCode::Char('J'))), GuildIntent::None);
        assert!(s.confirming.is_none(), "nothing is armed, nothing is sent");
        let msg = s.action_error.expect("the cap is explained");
        assert!(msg.contains("already hold 5 apprenticeships"), "{msg}");
    }

    #[test]
    fn the_join_prompt_says_which_role_the_join_would_be() {
        // With a badge guild already, the API makes this an apprenticeship.
        let mut s = with_guild(false, None);
        s.apply_own_guilds(Ok(vec![
            own("night-owls", GuildRole::Member),
            own("divers", GuildRole::Apprentice),
        ]));
        s.handle_key(key(KeyCode::Char('J')));
        let prompt = s.confirm_prompt(GuildAction::Join);
        assert!(prompt.contains("as an apprentice (2 of 5)"), "{prompt}");
        assert!(
            prompt.contains("#night-owls badge is unchanged"),
            "{prompt}"
        );

        // With no guild at all, it is a membership and carries the badge.
        let mut fresh = with_guild(false, None);
        fresh.apply_own_guilds(Ok(vec![]));
        assert!(fresh
            .confirm_prompt(GuildAction::Join)
            .contains("as a member? it becomes your profile badge"));
    }

    #[test]
    fn the_join_prompt_states_the_rule_when_the_viewers_guilds_are_unknown() {
        // No "leave your current guild first": being in a guild is no longer a
        // reason to refuse a join, so the prompt describes the server's choice.
        let s = with_guild(false, None);
        let prompt = s.confirm_prompt(GuildAction::Join);
        assert!(
            prompt.contains(
                "you join as a member if you're in no guild yet, as an apprentice otherwise"
            ),
            "{prompt}"
        );
    }

    #[test]
    fn p_arms_a_promotion_only_for_an_apprenticeship() {
        let mut apprentice = with_guild(true, Some(GuildRole::Apprentice));
        assert_eq!(
            apprentice.handle_key(key(KeyCode::Char('P'))),
            GuildIntent::None
        );
        assert_eq!(apprentice.confirming, Some(GuildAction::Promote));
        assert_eq!(
            apprentice.handle_key(key(KeyCode::Char('y'))),
            GuildIntent::Promote
        );
        assert!(apprentice.action_pending);

        // Already the badge guild: the 200-with-nothing-changed case, answered
        // here rather than sent.
        let mut badge = with_guild(true, Some(GuildRole::Member));
        assert_eq!(badge.handle_key(key(KeyCode::Char('P'))), GuildIntent::None);
        assert!(badge.confirming.is_none());
        assert!(badge
            .notice
            .as_deref()
            .is_some_and(|m| m.contains("already your profile badge")));

        // Not in the guild at all: nothing to promote, and nothing to say.
        let mut outsider = with_guild(false, None);
        assert_eq!(
            outsider.handle_key(key(KeyCode::Char('P'))),
            GuildIntent::None
        );
        assert!(outsider.confirming.is_none());
        assert!(outsider.notice.is_none());
    }

    #[test]
    fn promotion_is_refused_when_the_viewer_founded_their_badge_guild() {
        let mut s = with_guild(true, Some(GuildRole::Apprentice));
        s.apply_own_guilds(Ok(vec![
            own("night-owls", GuildRole::Founder),
            own("owls", GuildRole::Apprentice),
        ]));
        assert_eq!(s.handle_key(key(KeyCode::Char('P'))), GuildIntent::None);
        assert!(s.confirming.is_none());
        let msg = s.action_error.expect("the 403 is explained up front");
        assert!(msg.contains("you founded #night-owls"), "{msg}");
    }

    #[test]
    fn the_promotion_prompt_names_the_guild_that_steps_down() {
        let mut s = with_guild(true, Some(GuildRole::Apprentice));
        s.apply_own_guilds(Ok(vec![
            own("night-owls", GuildRole::Member),
            own("owls", GuildRole::Apprentice),
        ]));
        let prompt = s.confirm_prompt(GuildAction::Promote);
        assert!(
            prompt.contains("#night-owls becomes an apprenticeship, so you stay in it"),
            "{prompt}"
        );
    }

    #[test]
    fn a_badgeless_viewer_is_told_nothing_steps_down() {
        // § Leave a Guild: leaving your badge guild "clears the badge and
        // promotes nothing", so this state is exactly what the spec's own
        // leave-then-promote flow produces. Promising that a guild steps down
        // would name a guild the user is no longer in.
        let mut s = with_guild(true, Some(GuildRole::Apprentice));
        s.apply_own_guilds(Ok(vec![own("owls", GuildRole::Apprentice)]));
        let prompt = s.confirm_prompt(GuildAction::Promote);
        assert!(
            prompt.contains("you hold no guild as a member, so nothing steps down"),
            "{prompt}"
        );
    }

    #[test]
    fn a_badgeless_viewer_at_the_apprenticeship_cap_may_still_join() {
        // § Join a Guild states the role rule before the 409s: with no badge
        // guild you join as a MEMBER, which spends no apprenticeship slot.
        // Refusing on the count alone stranded such a user with no way back to
        // a badge guild short of abandoning an apprenticeship.
        let mut s = with_guild(false, None);
        s.apply_own_guilds(Ok((0..MAX_APPRENTICESHIPS)
            .map(|i| own(&format!("g{i}"), GuildRole::Apprentice))
            .collect()));
        assert_eq!(s.arm_join(), GuildIntent::None, "arming returns None");
        assert!(
            s.action_error.is_none(),
            "the join must not be refused client-side: {:?}",
            s.action_error,
        );
        assert_eq!(
            s.confirming,
            Some(GuildAction::Join),
            "it should be armed for confirm",
        );
    }

    #[test]
    fn l_arms_a_leave_for_members_and_apprentices_but_not_founders() {
        for role in [GuildRole::Member, GuildRole::Apprentice] {
            let mut s = with_guild(true, Some(role));
            assert_eq!(s.handle_key(key(KeyCode::Char('L'))), GuildIntent::None);
            assert_eq!(s.confirming, Some(GuildAction::Leave), "{role:?}");
            assert_eq!(s.handle_key(key(KeyCode::Char('y'))), GuildIntent::Leave);
        }

        let mut founder = with_guild(true, Some(GuildRole::Founder));
        assert_eq!(
            founder.handle_key(key(KeyCode::Char('L'))),
            GuildIntent::None
        );
        assert!(founder.confirming.is_none());
        assert!(founder
            .action_error
            .as_deref()
            .is_some_and(|m| m.contains("founders can't leave through the API")));
    }

    #[test]
    fn the_leave_prompt_says_what_happens_to_the_badge() {
        let apprentice = with_guild(true, Some(GuildRole::Apprentice));
        assert!(apprentice
            .confirm_prompt(GuildAction::Leave)
            .contains("it's an apprenticeship, so your profile badge is unchanged"));

        let member = with_guild(true, Some(GuildRole::Member));
        assert!(member
            .confirm_prompt(GuildAction::Leave)
            .contains("clears your profile badge and promotes nothing"));
    }

    #[test]
    fn apply_joined_as_a_member_counts_a_member_and_takes_the_badge() {
        let mut s = with_guild(false, None);
        s.action_pending = true;
        s.apply_joined(Ok(JoinedGuild {
            guild_id: "g1".into(),
            role: Some(GuildRole::Member),
        }));
        assert!(!s.action_pending);
        let notice = s.notice.clone().expect("the outcome is stated");
        assert!(notice.contains("as a member"), "{notice}");
        let g = s.guild.unwrap();
        assert!(g.is_member);
        assert_eq!(g.role, Some(GuildRole::Member));
        assert_eq!(g.member_count, 6);
        assert_eq!(g.apprentice_count, 0);
    }

    #[test]
    fn apply_joined_as_an_apprentice_counts_an_apprentice_and_keeps_the_badge() {
        let mut s = with_guild(false, None);
        s.apply_own_guilds(Ok(vec![own("night-owls", GuildRole::Member)]));
        s.action_pending = true;
        s.apply_joined(Ok(JoinedGuild {
            guild_id: "g1".into(),
            role: Some(GuildRole::Apprentice),
        }));
        let notice = s.notice.clone().expect("the outcome is stated");
        assert!(
            notice.contains("your profile badge is unchanged"),
            "{notice}"
        );
        let mine = s.own_guilds.clone().expect("own guilds tracked");
        assert_eq!(mine.len(), 2, "the apprenticeship joins the viewer's list");
        assert_eq!(mine[1].role, Some(GuildRole::Apprentice));
        let g = s.guild.unwrap();
        assert_eq!(g.role, Some(GuildRole::Apprentice));
        assert_eq!(g.member_count, 5, "an apprentice is not a member");
        assert_eq!(g.apprentice_count, 1);
    }

    #[test]
    fn a_join_with_no_stated_role_is_read_from_the_viewers_guilds() {
        let mut s = with_guild(false, None);
        s.apply_own_guilds(Ok(vec![own("night-owls", GuildRole::Member)]));
        s.apply_joined(Ok(JoinedGuild {
            guild_id: "g1".into(),
            role: None,
        }));
        assert_eq!(
            s.guild.unwrap().role,
            Some(GuildRole::Apprentice),
            "a badge guild is already held, so this can only be an apprenticeship"
        );
    }

    #[test]
    fn apply_promoted_moves_one_head_from_apprentice_to_member() {
        let mut s = with_guild(true, Some(GuildRole::Apprentice));
        if let Some(g) = &mut s.guild {
            g.apprentice_count = 2;
        }
        s.apply_own_guilds(Ok(vec![
            own("night-owls", GuildRole::Member),
            own("owls", GuildRole::Apprentice),
        ]));
        s.action_pending = true;
        s.apply_promoted(Ok(PromotedGuild {
            guild_id: "g1".into(),
            role: Some(GuildRole::Member),
        }));

        assert!(!s.action_pending);
        let g = s.guild.clone().unwrap();
        assert_eq!(g.role, Some(GuildRole::Member));
        assert_eq!(g.member_count, 6);
        assert_eq!(g.apprentice_count, 1);
        let mine = s.own_guilds.clone().unwrap();
        assert_eq!(
            mine[0].role,
            Some(GuildRole::Apprentice),
            "the old badge guild is demoted, not left"
        );
        assert_eq!(mine[1].role, Some(GuildRole::Member));
        assert!(s
            .notice
            .as_deref()
            .is_some_and(|m| m.contains("became an apprenticeship")));
    }

    #[test]
    fn apply_promoted_on_your_own_badge_guild_changes_no_counts() {
        let mut s = with_guild(true, Some(GuildRole::Member));
        s.apply_promoted(Ok(PromotedGuild {
            guild_id: "g1".into(),
            role: Some(GuildRole::Member),
        }));
        let g = s.guild.clone().unwrap();
        assert_eq!(g.member_count, 5);
        assert_eq!(g.apprentice_count, 0);
        assert!(s
            .notice
            .as_deref()
            .is_some_and(|m| m.contains("nothing changed")));
    }

    #[test]
    fn apply_left_clears_membership_and_drops_the_member_count() {
        let mut s = with_guild(true, Some(GuildRole::Member));
        s.apply_own_guilds(Ok(vec![own("owls", GuildRole::Member)]));
        s.apply_left(Ok("g1".into()));
        let g = s.guild.clone().unwrap();
        assert!(!g.is_member);
        assert!(g.role.is_none());
        assert_eq!(g.member_count, 4);
        assert!(
            s.own_guilds.as_ref().is_some_and(|mine| mine.is_empty()),
            "the guild leaves the viewer's own list"
        );
        assert!(s
            .notice
            .as_deref()
            .is_some_and(|m| m.contains("no profile badge now")));
    }

    #[test]
    fn leaving_an_apprenticeship_drops_an_apprentice_and_keeps_the_badge() {
        let mut s = with_guild(true, Some(GuildRole::Apprentice));
        if let Some(g) = &mut s.guild {
            g.apprentice_count = 3;
        }
        s.apply_left(Ok("g1".into()));
        let g = s.guild.clone().unwrap();
        assert_eq!(g.member_count, 5, "a member's seat was never taken");
        assert_eq!(g.apprentice_count, 2);
        assert!(s
            .notice
            .as_deref()
            .is_some_and(|m| m.contains("your profile badge is unchanged")));
    }

    #[test]
    fn a_refused_join_stays_out_of_the_body() {
        let mut s = with_guild(false, None);
        s.action_pending = true;
        s.apply_joined(Err("that already exists".into()));
        assert!(!s.action_pending);
        assert!(s.error.is_none(), "the thread list stays on screen");
        let msg = s.action_error.expect("the refusal is shown");
        assert!(msg.contains("couldn't join #owls"), "{msg}");
        assert!(!s.guild.unwrap().is_member);
    }

    #[test]
    fn a_failed_own_guilds_fetch_is_not_an_error_on_this_screen() {
        let mut s = with_guild(false, None);
        s.apply_own_guilds(Err("can't reach the server".into()));
        assert!(s.own_guilds.is_none());
        assert!(s.error.is_none());
        assert!(s.action_error.is_none());
    }

    #[test]
    fn the_header_reports_members_and_apprentices_separately() {
        let mut s = with_guild(true, Some(GuildRole::Apprentice));
        if let Some(g) = &mut s.guild {
            g.member_count = 12;
            g.apprentice_count = 3;
        }
        s.apply_threads_initial(Ok((vec![thread("p1")], None)));
        let text = rendered(&s, 120);
        assert!(
            text.contains("12 members · 3 apprentices · 15 total"),
            "{text}"
        );
        assert!(text.contains("you: apprentice"), "{text}");
    }

    #[test]
    fn the_member_list_tells_members_and_apprentices_apart() {
        let mut s = with_guild(true, Some(GuildRole::Member));
        s.apply_threads_initial(Ok((vec![], None)));
        s.apply_members_initial(Ok((
            vec![
                membership("alice", GuildRole::Member),
                membership("bob", GuildRole::Apprentice),
            ],
            None,
        )));
        s.tab = GuildTab::Members;
        let text = rendered(&s, 120);
        assert!(text.contains("@alice  member"), "{text}");
        assert!(text.contains("@bob  apprentice"), "{text}");
    }

    #[test]
    fn an_armed_action_takes_over_the_status_line() {
        let mut s = with_guild(true, Some(GuildRole::Apprentice));
        s.apply_threads_initial(Ok((vec![thread("p1")], None)));
        s.handle_key(key(KeyCode::Char('P')));
        let text = rendered(&s, 160);
        assert!(text.contains("make #owls your profile badge?"), "{text}");
        assert!(text.contains("y=yes, any other key=cancel"), "{text}");
    }

    #[test]
    fn the_status_line_offers_the_keys_the_viewers_role_allows() {
        let mut apprentice = with_guild(true, Some(GuildRole::Apprentice));
        apprentice.apply_threads_initial(Ok((vec![thread("p1")], None)));
        assert!(rendered(&apprentice, 120).contains("P badge · L leave"));

        let mut founder = with_guild(true, Some(GuildRole::Founder));
        founder.apply_threads_initial(Ok((vec![thread("p1")], None)));
        let text = rendered(&founder, 120);
        assert!(!text.contains("L leave"), "founders can't leave: {text}");
        assert!(
            !text.contains("P badge"),
            "a founder wears the badge: {text}"
        );

        let mut outsider = with_guild(false, None);
        outsider.apply_threads_initial(Ok((vec![thread("p1")], None)));
        assert!(rendered(&outsider, 120).contains("J join"));
    }

    #[test]
    fn c_requests_compose_for_members_and_outsiders() {
        // v0.8.4: guild forums are open, so non-members can start threads too.
        let mut member = with_guild(true, Some(GuildRole::Member));
        assert_eq!(
            member.handle_key(key(KeyCode::Char('c'))),
            GuildIntent::Compose
        );
        let mut outsider = with_guild(false, None);
        assert_eq!(
            outsider.handle_key(key(KeyCode::Char('c'))),
            GuildIntent::Compose
        );
    }
}

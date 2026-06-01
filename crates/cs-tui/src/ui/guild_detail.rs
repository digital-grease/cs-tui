//! Guild detail screen — one guild, with Threads and Members tabs. Reached by
//! pressing Enter on the guilds index. Enter on a thread opens it in the post
//! detail view. (Join/leave and thread composition land in a follow-up.)
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Guild, GuildMembership, GuildRole, GuildThread, JoinedGuild};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildTab {
    Threads,
    Members,
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
    OpenThread { post_id: String },
    /// Join this guild.
    Join,
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
    /// True while a join/leave request is in flight (prevents double-submit).
    pub action_pending: bool,
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

    pub fn handle_key(&mut self, key: KeyEvent) -> GuildIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return GuildIntent::Quit;
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
            KeyCode::Char('J') => {
                let can_join =
                    self.guild.as_ref().is_some_and(|g| !g.is_member) && !self.action_pending;
                if can_join {
                    self.action_pending = true;
                    return GuildIntent::Join;
                }
                return GuildIntent::None;
            }
            KeyCode::Char('L') => {
                let can_leave = self
                    .guild
                    .as_ref()
                    .is_some_and(|g| g.is_member && g.role != Some(GuildRole::Founder))
                    && !self.action_pending;
                if can_leave {
                    self.action_pending = true;
                    return GuildIntent::Leave;
                }
                return GuildIntent::None;
            }
            KeyCode::Char('c') => {
                if self.guild.as_ref().is_some_and(|g| g.is_member) {
                    return GuildIntent::Compose;
                }
                return GuildIntent::None;
            }
            _ => {}
        }
        if self.loading {
            return GuildIntent::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                let len = self.cur_len();
                let at_bottom = {
                    let s = self.cur_sel_mut();
                    if len > 0 && *s < len - 1 {
                        *s += 1;
                        false
                    } else {
                        true
                    }
                };
                // At the bottom, scrolling down pulls the next page automatically.
                if at_bottom && self.cur_has_more() {
                    self.loading = true;
                    return GuildIntent::LoadMore;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let s = self.cur_sel_mut();
                *s = s.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => *self.cur_sel_mut() = 0,
            KeyCode::Char('G') | KeyCode::End => {
                let len = self.cur_len();
                *self.cur_sel_mut() = len.saturating_sub(1);
            }
            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::PageDown if self.cur_has_more() => {
                self.loading = true;
                return GuildIntent::LoadMore;
            }
            KeyCode::Char('r') => {
                self.loading = true;
                self.error = None;
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

    pub fn apply_joined(&mut self, result: Result<JoinedGuild, String>) {
        self.action_pending = false;
        match result {
            Ok(j) => {
                if let Some(g) = &mut self.guild {
                    g.is_member = true;
                    g.role = j.role.or(Some(GuildRole::Member));
                    g.member_count = g.member_count.saturating_add(1);
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_left(&mut self, result: Result<String, String>) {
        self.action_pending = false;
        match result {
            Ok(_) => {
                if let Some(g) = &mut self.guild {
                    g.is_member = false;
                    g.role = None;
                    g.member_count = g.member_count.saturating_sub(1);
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let name = self.guild.as_ref().map(|g| g.name.as_str()).unwrap_or(&self.slug);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(format!(" cs-tui • {name} "), theme.accent_style()));
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
            GuildTab::Threads => "tab tabs · enter open · scroll for more · r refresh",
            GuildTab::Members => "tab tabs · scroll for more · r refresh",
        };
        let action = match &self.guild {
            _ if self.action_pending => " · working…",
            Some(g) if !g.is_member => " · J join",
            Some(g) if g.role != Some(GuildRole::Founder) => " · c new · L leave",
            Some(_) => " · c new",
            None => "",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{base}{action} · esc back"),
                theme.muted_style(),
            ))),
            layout[3],
        );
    }

    fn header_line(&self, theme: &Theme) -> Paragraph<'static> {
        let text = match &self.guild {
            Some(g) => {
                let membership = if g.is_member {
                    match g.role {
                        Some(GuildRole::Founder) => "  · you: founder",
                        _ => "  · you: member",
                    }
                } else {
                    ""
                };
                // `icon` is an identifier string, not a glyph — don't render it.
                format!("#{} · {} members{}", g.slug, g.member_count, membership)
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
                        Paragraph::new(Line::from(Span::styled(
                            "no members",
                            theme.muted_style(),
                        ))),
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
                first_line_truncated(title, 200),
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
    let role = match m.role {
        Some(GuildRole::Founder) => "founder",
        _ => "member",
    };
    let suffix = m
        .display_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|n| format!(" · {n}"))
        .unwrap_or_default();
    let line = Line::from(vec![
        Span::styled(format!("@{}", m.username), theme.accent_style()),
        Span::styled(format!("  {role}{suffix}"), theme.muted_style()),
    ]);
    ListItem::new(vec![line])
}

fn first_line_truncated(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    if first.chars().count() <= max {
        first.to_string()
    } else {
        let truncated: String = first.chars().take(max - 1).collect();
        format!("{truncated}…")
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

    #[test]
    fn j_requests_join_only_when_not_a_member() {
        let mut s = with_guild(false, None);
        assert_eq!(s.handle_key(key(KeyCode::Char('J'))), GuildIntent::Join);
        assert!(s.action_pending);

        let mut member = with_guild(true, Some(GuildRole::Member));
        assert_eq!(member.handle_key(key(KeyCode::Char('J'))), GuildIntent::None);
    }

    #[test]
    fn l_requests_leave_for_member_but_not_founder() {
        let mut member = with_guild(true, Some(GuildRole::Member));
        assert_eq!(member.handle_key(key(KeyCode::Char('L'))), GuildIntent::Leave);

        let mut founder = with_guild(true, Some(GuildRole::Founder));
        assert_eq!(founder.handle_key(key(KeyCode::Char('L'))), GuildIntent::None);
    }

    #[test]
    fn apply_joined_sets_membership_and_bumps_count() {
        let mut s = with_guild(false, None);
        s.action_pending = true;
        s.apply_joined(Ok(JoinedGuild {
            guild_id: "g1".into(),
            role: Some(GuildRole::Member),
        }));
        assert!(!s.action_pending);
        let g = s.guild.unwrap();
        assert!(g.is_member);
        assert_eq!(g.role, Some(GuildRole::Member));
        assert_eq!(g.member_count, 6);
    }

    #[test]
    fn apply_left_clears_membership_and_drops_count() {
        let mut s = with_guild(true, Some(GuildRole::Member));
        s.apply_left(Ok("g1".into()));
        let g = s.guild.unwrap();
        assert!(!g.is_member);
        assert!(g.role.is_none());
        assert_eq!(g.member_count, 4);
    }

    #[test]
    fn join_error_surfaces_and_clears_pending() {
        let mut s = with_guild(false, None);
        s.action_pending = true;
        s.apply_joined(Err("api Conflict (409): already in a guild".into()));
        assert!(!s.action_pending);
        assert!(s.error.is_some());
        assert!(!s.guild.unwrap().is_member);
    }

    #[test]
    fn c_requests_compose_only_for_members() {
        let mut member = with_guild(true, Some(GuildRole::Member));
        assert_eq!(
            member.handle_key(key(KeyCode::Char('c'))),
            GuildIntent::Compose
        );
        let mut outsider = with_guild(false, None);
        assert_eq!(
            outsider.handle_key(key(KeyCode::Char('c'))),
            GuildIntent::None
        );
    }
}

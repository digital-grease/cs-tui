//! Guilds index screen (root `9`): a paginated list of guilds, most populated
//! first. Enter opens the selected guild's detail screen.
//!
//! Ordering is by `memberCount` alone (API v0.8.6 § List Guilds), so the
//! apprentices a guild has never move it up the list. Rows therefore spell out
//! both counts rather than showing a single number: neither one is the guild's
//! headcount on its own, and a lone total would make the ordering look broken.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Guild, GuildRole};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::list::{self, TabState};
use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuildsIntent {
    Refresh,
    LoadMore,
    /// Open the selected guild's detail screen.
    OpenSelected {
        slug: String,
    },
    Quit,
    None,
}

#[derive(Debug)]
pub struct GuildsScreen {
    pub list: TabState<Guild>,
}

impl GuildsScreen {
    pub fn new() -> Self {
        Self {
            list: TabState::loading(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> GuildsIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return GuildsIntent::Quit;
        }
        if self.list.loading {
            return GuildsIntent::None;
        }
        match super::list_nav::navigate(
            key.code,
            &mut self.list.selected,
            self.list.items.len(),
            self.list.next_cursor.is_some(),
        ) {
            super::list_nav::ListNav::LoadMore => {
                self.list.loading = true;
                return GuildsIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return GuildsIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        match key.code {
            KeyCode::Char('r') => {
                self.list.items.clear();
                self.list.next_cursor = None;
                self.list.selected = 0;
                self.list.loading = true;
                self.list.error = None;
                return GuildsIntent::Refresh;
            }
            KeyCode::Enter => {
                if let Some(g) = self.list.items.get(self.list.selected) {
                    return GuildsIntent::OpenSelected {
                        slug: g.slug.clone(),
                    };
                }
            }
            _ => {}
        }
        GuildsIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Guild>, Option<String>), String>) {
        self.list.apply_initial(result);
    }

    pub fn apply_more(&mut self, result: Result<(Vec<Guild>, Option<String>), String>) {
        self.list.apply_more(result);
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • guilds ", theme.heading_style()));
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
            "no guilds",
            |g| guild_item(g, theme),
        );

        let (status, style) = if let Some(msg) = list::load_more_error(&self.list) {
            (msg, theme.error_style())
        } else if self.list.next_cursor.is_some() {
            (
                format!(
                    "{} guilds · scroll down for more · enter open · r refresh · esc menu",
                    self.list.items.len()
                ),
                theme.muted_style(),
            )
        } else {
            (
                format!(
                    "{} guilds · enter open · r refresh · esc menu",
                    self.list.items.len()
                ),
                theme.muted_style(),
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status, style))),
            layout[1],
        );
    }
}

impl Default for GuildsScreen {
    fn default() -> Self {
        Self::new()
    }
}

/// A guild's headcount, spelled out honestly (API v0.8.6 § List Guilds).
///
/// `memberCount` counts founders and members, `apprenticeCount` counts
/// apprentices, and the guild's headcount is the sum, so neither field on its
/// own is the number of people in the guild and neither may be printed as if
/// it were. Guilds that predate apprenticeships omit `apprenticeCount`, which
/// reads as 0 and prints as the plain member count they have always shown.
///
/// Shared with the guild detail screen so both surfaces say the same thing.
pub fn headcount_label(g: &Guild) -> String {
    if g.apprentice_count == 0 {
        return count_label(g.member_count, "member");
    }
    format!(
        "{} · {} · {} total",
        count_label(g.member_count, "member"),
        count_label(g.apprentice_count, "apprentice"),
        g.headcount()
    )
}

/// The word for a role somebody holds in a guild (API v0.8.6 § Guilds).
///
/// `None` for a role this client doesn't model and for a row that arrived
/// without one, so neither is printed as a guess. v0.8.6 widened the vocabulary
/// from founder/member to founder/member/apprentice, and a client that fills an
/// unrecognized role in with "member" is exactly how an apprentice comes to be
/// shown wearing a badge they don't have.
///
/// Shared by the guild roster and the profile's guilds tab, so one person's
/// role reads the same wherever it is shown.
#[must_use]
pub fn role_label(role: Option<GuildRole>) -> Option<&'static str> {
    match role {
        Some(GuildRole::Founder) => Some("founder"),
        Some(GuildRole::Member) => Some("member"),
        Some(GuildRole::Apprentice) => Some("apprentice"),
        Some(GuildRole::Unknown) | None => None,
    }
}

/// `n` with its noun, singular when there is exactly one of them.
fn count_label(n: u32, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn guild_item(g: &Guild, theme: &Theme) -> ListItem<'static> {
    // The API's `icon` is an icon *identifier* (e.g. "arrows-maximize"), not a
    // glyph, so it's not rendered as text.
    let header = Line::from(vec![
        Span::styled(g.name.clone(), theme.accent_style()),
        Span::styled(
            format!("  #{} · {}", g.slug, headcount_label(g)),
            theme.muted_style(),
        ),
    ]);
    let mut lines = vec![header];
    if let Some(bio) = g.bio.as_deref() {
        let bio = bio.trim();
        if !bio.is_empty() {
            lines.push(Line::from(Span::styled(
                super::text::first_line_truncated(bio, 200),
                theme.base(),
            )));
        }
    }
    lines.push(Line::from(""));
    ListItem::new(lines)
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

    fn guild(slug: &str) -> Guild {
        Guild {
            id: slug.into(),
            name: format!("Guild {slug}"),
            slug: slug.into(),
            member_count: 3,
            ..Default::default()
        }
    }

    fn counted(members: u32, apprentices: u32) -> Guild {
        Guild {
            member_count: members,
            apprentice_count: apprentices,
            ..guild("owls")
        }
    }

    #[test]
    fn apply_initial_populates_and_threads_cursor() {
        let mut s = GuildsScreen::new();
        s.apply_initial(Ok((vec![guild("a"), guild("b")], Some("c1".into()))));
        assert!(!s.list.loading);
        assert_eq!(s.list.items.len(), 2);
        assert_eq!(s.list.next_cursor.as_deref(), Some("c1"));
    }

    #[test]
    fn apply_more_appends() {
        let mut s = GuildsScreen::new();
        s.apply_initial(Ok((vec![guild("a")], Some("c".into()))));
        s.apply_more(Ok((vec![guild("b")], None)));
        assert_eq!(s.list.items.len(), 2);
        assert!(s.list.next_cursor.is_none());
    }

    #[test]
    fn enter_opens_selected_slug() {
        let mut s = GuildsScreen::new();
        s.apply_initial(Ok((vec![guild("owls"), guild("cats")], None)));
        s.list.selected = 1;
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            GuildsIntent::OpenSelected {
                slug: "cats".into()
            }
        );
    }

    #[test]
    fn load_more_only_when_cursor_present() {
        let mut s = GuildsScreen::new();
        s.apply_initial(Ok((vec![guild("a")], None)));
        assert_eq!(s.handle_key(key(KeyCode::Char('n'))), GuildsIntent::None);
        s.apply_initial(Ok((vec![guild("a")], Some("c".into()))));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('n'))),
            GuildsIntent::LoadMore
        );
    }

    #[test]
    fn headcount_label_reads_a_guild_with_no_apprentices_as_before() {
        // `apprenticeCount` is missing on guilds that predate apprenticeships,
        // which decodes to 0, so such a guild reads as it always did.
        let g = counted(12, 0);
        assert_eq!(headcount_label(&g), "12 members");
    }

    #[test]
    fn headcount_label_names_both_counts_and_the_total() {
        let g = counted(12, 3);
        assert_eq!(headcount_label(&g), "12 members · 3 apprentices · 15 total");
    }

    #[test]
    fn headcount_label_uses_singular_nouns_for_one() {
        let g = counted(1, 1);
        assert_eq!(headcount_label(&g), "1 member · 1 apprentice · 2 total");
    }

    #[test]
    fn role_labels_cover_the_v0_8_6_vocabulary() {
        // v0.8.6 added `apprentice` to a founder/member vocabulary. Guessing at
        // a role the client doesn't know is how the next addition would show up
        // wearing the wrong word.
        assert_eq!(role_label(Some(GuildRole::Founder)), Some("founder"));
        assert_eq!(role_label(Some(GuildRole::Member)), Some("member"));
        assert_eq!(role_label(Some(GuildRole::Apprentice)), Some("apprentice"));
        assert_eq!(
            role_label(Some(GuildRole::Unknown)),
            None,
            "an unmodelled role isn't called a member"
        );
        assert_eq!(role_label(None), None);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut s = GuildsScreen::new();
        let kev = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(s.handle_key(kev), GuildsIntent::Quit);
    }
}

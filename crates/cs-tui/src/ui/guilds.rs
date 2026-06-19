//! Guilds index screen (root `8`) — a paginated list of guilds, most populated
//! first. Enter opens the selected guild's detail screen.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Guild;
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

fn guild_item(g: &Guild, theme: &Theme) -> ListItem<'static> {
    // The API's `icon` is an icon *identifier* (e.g. "arrows-maximize"), not a
    // glyph, so it's not rendered as text.
    let header = Line::from(vec![
        Span::styled(g.name.clone(), theme.accent_style()),
        Span::styled(
            format!("  #{} · {} members", g.slug, g.member_count),
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

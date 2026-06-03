//! Guilds index screen (root `8`) — a paginated list of guilds, most populated
//! first. Enter opens the selected guild's detail screen.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Guild;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuildsIntent {
    Refresh,
    LoadMore,
    /// Open the selected guild's detail screen.
    OpenSelected { slug: String },
    Quit,
    None,
}

#[derive(Debug)]
pub struct GuildsScreen {
    pub items: Vec<Guild>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

impl GuildsScreen {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: true,
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> GuildsIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return GuildsIntent::Quit;
        }
        if self.loading {
            return GuildsIntent::None;
        }
        match super::list_nav::navigate(
            key.code,
            &mut self.selected,
            self.items.len(),
            self.next_cursor.is_some(),
        ) {
            super::list_nav::ListNav::LoadMore => {
                self.loading = true;
                return GuildsIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return GuildsIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        match key.code {
            KeyCode::Char('r') => {
                self.items.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return GuildsIntent::Refresh;
            }
            KeyCode::Enter => {
                if let Some(g) = self.items.get(self.selected) {
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
        self.loading = false;
        match result {
            Ok((items, cursor)) => {
                self.items = items;
                self.next_cursor = cursor;
                if self.selected >= self.items.len() {
                    self.selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_more(&mut self, result: Result<(Vec<Guild>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((mut items, cursor)) => {
                self.items.append(&mut items);
                self.next_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • guilds ", theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        if self.loading && self.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading guilds…",
                    theme.accent_style(),
                ))),
                layout[0],
            );
        } else if let Some(msg) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style()))),
                layout[0],
            );
        } else if self.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled("no guilds", theme.muted_style()))),
                layout[0],
            );
        } else {
            let items: Vec<ListItem<'_>> =
                self.items.iter().map(|g| guild_item(g, theme)).collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(self.selected.min(self.items.len().saturating_sub(1))));
            frame.render_stateful_widget(list, layout[0], &mut state);
        }

        let status = if self.next_cursor.is_some() {
            format!(
                "{} guilds · scroll down for more · enter open · r refresh · esc menu",
                self.items.len()
            )
        } else {
            format!(
                "{} guilds · enter open · r refresh · esc menu",
                self.items.len()
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status, theme.muted_style()))),
            layout[1],
        );
    }
}

impl Default for GuildsScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn guild_item<'a>(g: &'a Guild, theme: &Theme) -> ListItem<'a> {
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
        assert!(!s.loading);
        assert_eq!(s.items.len(), 2);
        assert_eq!(s.next_cursor.as_deref(), Some("c1"));
    }

    #[test]
    fn apply_more_appends() {
        let mut s = GuildsScreen::new();
        s.apply_initial(Ok((vec![guild("a")], Some("c".into()))));
        s.apply_more(Ok((vec![guild("b")], None)));
        assert_eq!(s.items.len(), 2);
        assert!(s.next_cursor.is_none());
    }

    #[test]
    fn enter_opens_selected_slug() {
        let mut s = GuildsScreen::new();
        s.apply_initial(Ok((vec![guild("owls"), guild("cats")], None)));
        s.selected = 1;
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
        assert_eq!(s.handle_key(key(KeyCode::Char('n'))), GuildsIntent::LoadMore);
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

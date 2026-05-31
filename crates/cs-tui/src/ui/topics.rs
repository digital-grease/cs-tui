//! Topics index screen — list of topics, sorted by post count.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Topic;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicsIntent {
    Refresh,
    LoadMore,
    /// Open the topic feed for the selected slug.
    OpenSelected {
        slug: String,
    },
    Quit,
    None,
}

#[derive(Debug)]
pub struct TopicsScreen {
    pub items: Vec<Topic>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

impl TopicsScreen {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: true,
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TopicsIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return TopicsIntent::Quit;
        }
        if self.loading {
            return TopicsIntent::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down
                if !self.items.is_empty() && self.selected < self.items.len() - 1 =>
            {
                self.selected += 1;
            }
            // At the bottom, scrolling down pulls the next page automatically.
            KeyCode::Char('j') | KeyCode::Down if self.next_cursor.is_some() => {
                self.loading = true;
                return TopicsIntent::LoadMore;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End if !self.items.is_empty() => {
                self.selected = self.items.len() - 1;
            }
            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::PageDown
                if self.next_cursor.is_some() =>
            {
                self.loading = true;
                return TopicsIntent::LoadMore;
            }
            KeyCode::Char('r') => {
                self.items.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return TopicsIntent::Refresh;
            }
            KeyCode::Enter => {
                if let Some(t) = self.items.get(self.selected) {
                    return TopicsIntent::OpenSelected {
                        slug: t.slug.clone(),
                    };
                }
            }
            _ => {}
        }
        TopicsIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Topic>, Option<String>), String>) {
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

    pub fn apply_more(&mut self, result: Result<(Vec<Topic>, Option<String>), String>) {
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
            .title(Span::styled(" cs-tui • topics ", theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        if self.loading && self.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading topics…",
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
                Paragraph::new(Line::from(Span::styled("no topics", theme.muted_style()))),
                layout[0],
            );
        } else {
            let items: Vec<ListItem<'_>> =
                self.items.iter().map(|t| topic_item(t, theme)).collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(self.selected.min(self.items.len().saturating_sub(1))));
            frame.render_stateful_widget(list, layout[0], &mut state);
        }

        let more = if self.next_cursor.is_some() {
            " · scroll down for more"
        } else {
            ""
        };
        let status = Paragraph::new(Line::from(Span::styled(
            format!(
                "{} topics{more} · enter open · r refresh · esc menu",
                self.items.len()
            ),
            theme.muted_style(),
        )));
        frame.render_widget(status, layout[1]);
    }
}

impl Default for TopicsScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn topic_item<'a>(t: &'a Topic, theme: &Theme) -> ListItem<'a> {
    let line = Line::from(vec![
        Span::styled(format!("#{}", t.slug), theme.accent_style()),
        Span::styled(format!("  ({} posts)", t.post_count), theme.muted_style()),
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

    fn topic(slug: &str, count: u32) -> Topic {
        Topic {
            slug: slug.into(),
            post_count: count,
        }
    }

    #[test]
    fn apply_populates() {
        let mut s = TopicsScreen::new();
        s.apply_initial(Ok((vec![topic("music", 42), topic("linux", 17)], None)));
        assert!(!s.loading);
        assert_eq!(s.items.len(), 2);
    }

    #[test]
    fn apply_more_appends_and_tracks_cursor() {
        let mut s = TopicsScreen::new();
        s.apply_initial(Ok((vec![topic("music", 42)], Some("c1".into()))));
        assert_eq!(s.next_cursor.as_deref(), Some("c1"));
        s.apply_more(Ok((vec![topic("linux", 17)], None)));
        assert_eq!(s.items.len(), 2);
        assert!(s.next_cursor.is_none());
    }

    #[test]
    fn enter_emits_open_with_slug() {
        let mut s = TopicsScreen::new();
        s.apply_initial(Ok((vec![topic("music", 42), topic("linux", 17)], None)));
        s.selected = 1;
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            TopicsIntent::OpenSelected {
                slug: "linux".into()
            }
        );
    }

    #[test]
    fn j_advances_bounded() {
        let mut s = TopicsScreen::new();
        s.apply_initial(Ok((vec![topic("a", 1), topic("b", 2), topic("c", 3)], None)));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn j_at_bottom_auto_loads() {
        let mut s = TopicsScreen::new();
        s.apply_initial(Ok((vec![topic("a", 1)], Some("next".into()))));
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, TopicsIntent::LoadMore);
        assert!(s.loading);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut s = TopicsScreen::new();
        let kev = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(s.handle_key(kev), TopicsIntent::Quit);
    }
}

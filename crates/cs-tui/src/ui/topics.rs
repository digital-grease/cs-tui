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
    pub loading: bool,
    pub error: Option<String>,
}

impl TopicsScreen {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
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
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End if !self.items.is_empty() => {
                self.selected = self.items.len() - 1;
            }
            KeyCode::Char('r') => {
                self.items.clear();
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

    pub fn apply(&mut self, result: Result<Vec<Topic>, String>) {
        self.loading = false;
        match result {
            Ok(items) => {
                self.items = items;
                if self.selected >= self.items.len() {
                    self.selected = 0;
                }
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

        let status = Paragraph::new(Line::from(Span::styled(
            format!(
                "{} topics · enter open · r refresh · esc menu",
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
        s.apply(Ok(vec![topic("music", 42), topic("linux", 17)]));
        assert!(!s.loading);
        assert_eq!(s.items.len(), 2);
    }

    #[test]
    fn enter_emits_open_with_slug() {
        let mut s = TopicsScreen::new();
        s.apply(Ok(vec![topic("music", 42), topic("linux", 17)]));
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
        s.apply(Ok(vec![topic("a", 1), topic("b", 2), topic("c", 3)]));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 2);
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

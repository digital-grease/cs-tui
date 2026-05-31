//! Topics index screen — a searchable view over the topic list.
//!
//! The screen never fetches: the App warms a cache of every topic in the
//! background (started at login) and pushes it here via [`TopicsScreen::set_topics`].
//! `/` opens a live filter over whatever's loaded so far; matches grow as the
//! background fill continues.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Topic;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicsIntent {
    /// Re-warm the cache from scratch.
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
    /// Cache empty and still warming.
    pub loading: bool,
    /// Background warm-up has loaded every topic.
    pub complete: bool,
    /// Active search query (`Some` while the `/` filter box is open). The list
    /// narrows to slugs containing it (case-insensitive).
    pub filter: Option<String>,
    /// Browse selection saved when the search box opens, restored if it's closed
    /// without picking a result — so an aborted `/` doesn't lose your place.
    pre_search_selected: usize,
}

impl TopicsScreen {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            loading: true,
            complete: false,
            filter: None,
            pre_search_selected: 0,
        }
    }

    /// Push the latest cache snapshot in from the App as the background warm-up
    /// progresses. `complete` is true once every topic has loaded.
    pub fn set_topics(&mut self, items: Vec<Topic>, complete: bool) {
        self.items = items;
        self.complete = complete;
        self.loading = self.items.is_empty() && !complete;
        let view_len = self.view().len();
        if view_len > 0 && self.selected >= view_len {
            self.selected = view_len - 1;
        }
    }

    /// Whether the search box is open (printable keys go to the query).
    #[must_use]
    pub fn is_filtering(&self) -> bool {
        self.filter.is_some()
    }

    /// Indices into `items` matching the current filter (all when not filtering).
    fn view(&self) -> Vec<usize> {
        match &self.filter {
            Some(q) if !q.is_empty() => {
                let q = q.to_lowercase();
                self.items
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.slug.to_lowercase().contains(&q))
                    .map(|(i, _)| i)
                    .collect()
            }
            _ => (0..self.items.len()).collect(),
        }
    }

    /// Exit the search box, clearing the query and restoring the pre-search
    /// browse position. Returns `true` if it was open.
    pub fn clear_filter(&mut self) -> bool {
        if self.filter.take().is_some() {
            self.selected = self
                .pre_search_selected
                .min(self.items.len().saturating_sub(1));
            true
        } else {
            false
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TopicsIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return TopicsIntent::Quit;
        }

        // Search box open: printable keys edit the query; ↑/↓ + Enter navigate
        // the narrowed list. (Esc, intercepted by the app, closes the box.)
        if self.filter.is_some() {
            match key.code {
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(q) = self.filter.as_mut() {
                        q.push(c);
                    }
                    self.selected = 0;
                }
                KeyCode::Backspace => {
                    if let Some(q) = self.filter.as_mut() {
                        q.pop();
                    }
                    self.selected = 0;
                }
                KeyCode::Down => {
                    let n = self.view().len();
                    if n > 0 && self.selected + 1 < n {
                        self.selected += 1;
                    }
                }
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Enter => {
                    if let Some(&i) = self.view().get(self.selected) {
                        return TopicsIntent::OpenSelected {
                            slug: self.items[i].slug.clone(),
                        };
                    }
                }
                _ => {}
            }
            return TopicsIntent::None;
        }

        // Browse mode — navigate the (possibly still-filling) list.
        let len = self.items.len();
        match key.code {
            KeyCode::Char('/') => {
                // Keep the current selection (empty query shows all), but
                // remember it so an aborted search returns here.
                self.pre_search_selected = self.selected;
                self.filter = Some(String::new());
            }
            KeyCode::Char('j') | KeyCode::Down if len > 0 && self.selected + 1 < len => {
                self.selected += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End if len > 0 => {
                self.selected = len - 1;
            }
            KeyCode::Char('r') => {
                self.selected = 0;
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

        let view = self.view();
        if self.loading && self.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading topics…",
                    theme.accent_style(),
                ))),
                layout[0],
            );
        } else if view.is_empty() {
            let msg = if self.is_filtering() {
                "no matching topics"
            } else {
                "no topics"
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg, theme.muted_style()))),
                layout[0],
            );
        } else {
            let items: Vec<ListItem<'_>> = view
                .iter()
                .map(|&i| topic_item(&self.items[i], theme))
                .collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(self.selected.min(view.len().saturating_sub(1))));
            frame.render_stateful_widget(list, layout[0], &mut state);
        }

        // Status line: a search box when filtering, else the topic count. The
        // background warm-up shows a "loading…" suffix until complete.
        let warming = if self.complete { "" } else { " · loading…" };
        let status_text = if let Some(q) = &self.filter {
            format!(
                "/{q}█ · {} match{}{warming} · ↑↓ enter · esc clear",
                view.len(),
                if view.len() == 1 { "" } else { "es" },
            )
        } else {
            format!(
                "{} topics{warming} · / search · enter open · r refresh · esc menu",
                self.items.len()
            )
        };
        let status = Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style())));
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
    fn set_topics_populates_and_tracks_completion() {
        let mut s = TopicsScreen::new();
        assert!(s.loading);
        s.set_topics(vec![topic("music", 42)], false);
        assert!(!s.loading); // has items now
        assert!(!s.complete);
        s.set_topics(vec![topic("music", 42), topic("linux", 17)], true);
        assert_eq!(s.items.len(), 2);
        assert!(s.complete);
    }

    #[test]
    fn enter_emits_open_with_slug() {
        let mut s = TopicsScreen::new();
        s.set_topics(vec![topic("music", 42), topic("linux", 17)], true);
        s.selected = 1;
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            TopicsIntent::OpenSelected {
                slug: "linux".into()
            }
        );
    }

    #[test]
    fn j_advances_bounded() {
        let mut s = TopicsScreen::new();
        s.set_topics(vec![topic("a", 1), topic("b", 2), topic("c", 3)], true);
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn r_requests_refresh() {
        let mut s = TopicsScreen::new();
        s.set_topics(vec![topic("a", 1)], true);
        assert_eq!(s.handle_key(key(KeyCode::Char('r'))), TopicsIntent::Refresh);
    }

    #[test]
    fn slash_opens_search_and_filters_to_match() {
        let mut s = TopicsScreen::new();
        s.set_topics(
            vec![topic("music", 5), topic("musings", 3), topic("linux", 9)],
            true,
        );
        assert_eq!(s.handle_key(key(KeyCode::Char('/'))), TopicsIntent::None);
        assert!(s.is_filtering());
        for c in "mus".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(s.filter.as_deref(), Some("mus"));
        // Enter opens the first match (music); linux is filtered out.
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            TopicsIntent::OpenSelected {
                slug: "music".into()
            }
        );
    }

    #[test]
    fn search_arrows_navigate_matches_and_esc_clears() {
        let mut s = TopicsScreen::new();
        s.set_topics(vec![topic("music", 5), topic("musings", 3)], true);
        s.handle_key(key(KeyCode::Char('/')));
        for c in "mus".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        s.handle_key(key(KeyCode::Down)); // second match
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            TopicsIntent::OpenSelected {
                slug: "musings".into()
            }
        );
        assert!(s.clear_filter());
        assert!(!s.is_filtering());
        assert!(!s.clear_filter(), "already cleared");
    }

    #[test]
    fn search_works_on_a_still_filling_list() {
        // Filtering is available before the warm-up completes.
        let mut s = TopicsScreen::new();
        s.set_topics(vec![topic("music", 5)], false); // not complete
        s.handle_key(key(KeyCode::Char('/')));
        for c in "mus".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert!(s.is_filtering());
        // A later chunk arrives and a new match appears.
        s.set_topics(vec![topic("music", 5), topic("musings", 3)], true);
        s.handle_key(key(KeyCode::Down));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            TopicsIntent::OpenSelected {
                slug: "musings".into()
            }
        );
    }

    #[test]
    fn aborted_search_restores_browse_position() {
        let mut s = TopicsScreen::new();
        s.set_topics(vec![topic("a", 1), topic("b", 2), topic("c", 3)], true);
        s.selected = 2; // browsing the third item
        s.handle_key(key(KeyCode::Char('/')));
        s.handle_key(key(KeyCode::Char('a'))); // narrows; selection re-anchors to 0
        assert_eq!(s.selected, 0);
        assert!(s.clear_filter()); // Esc closes the box
        assert_eq!(s.selected, 2, "aborting a search returns to the browse spot");
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

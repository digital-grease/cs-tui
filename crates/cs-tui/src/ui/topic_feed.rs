//! Topic feed screen — entries tagged with a specific topic.
//!
//! Visually identical to the home feed except for the title and the data source.
//! Reuses the navigation pattern from [`super::feed::FeedScreen`] but stays a
//! separate type so navigation can distinguish "home feed" from "topic feed"
//! when popping back from a child screen.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Entry;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use time::OffsetDateTime;

use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicFeedIntent {
    /// Return to the topics index.
    Back,
    LoadMore,
    Refresh,
    OpenSelected {
        post_id: String,
    },
    Quit,
    None,
}

#[derive(Debug)]
pub struct TopicFeedScreen {
    pub slug: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

impl TopicFeedScreen {
    pub fn new(slug: String) -> Self {
        Self {
            slug,
            entries: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: true,
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> TopicFeedIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return TopicFeedIntent::Quit;
        }
        if key.code == KeyCode::Backspace {
            return TopicFeedIntent::Back;
        }
        if self.loading {
            return TopicFeedIntent::None;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down
                if !self.entries.is_empty() && self.selected < self.entries.len() - 1 =>
            {
                self.selected += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End if !self.entries.is_empty() => {
                self.selected = self.entries.len() - 1;
            }
            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::PageDown
                if self.next_cursor.is_some() =>
            {
                self.loading = true;
                return TopicFeedIntent::LoadMore;
            }
            KeyCode::Char('r') => {
                self.entries.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return TopicFeedIntent::Refresh;
            }
            KeyCode::Enter => {
                if let Some(e) = self.entries.get(self.selected) {
                    return TopicFeedIntent::OpenSelected {
                        post_id: e.post_id.clone(),
                    };
                }
            }
            _ => {}
        }
        TopicFeedIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Entry>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((entries, cursor)) => {
                self.entries = entries;
                self.next_cursor = cursor;
                if self.selected >= self.entries.len() {
                    self.selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_more(&mut self, result: Result<(Vec<Entry>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((mut entries, cursor)) => {
                self.entries.append(&mut entries);
                self.next_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let title = format!(" cs-tui • #{} ", self.slug);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        if self.loading && self.entries.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading topic…",
                    theme.accent_style(),
                ))),
                layout[0],
            );
        } else if let Some(msg) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style()))),
                layout[0],
            );
        } else if self.entries.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "no entries in this topic",
                    theme.muted_style(),
                ))),
                layout[0],
            );
        } else {
            let items: Vec<ListItem<'_>> =
                self.entries.iter().map(|e| entry_item(e, theme)).collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(
                self.selected.min(self.entries.len().saturating_sub(1)),
            ));
            frame.render_stateful_widget(list, layout[0], &mut state);
        }

        let status_text = if self.loading {
            "loading… · j/k · enter open · n next · r refresh · esc back".to_string()
        } else if self.next_cursor.is_some() {
            format!(
                "{} entries · more — n · j/k · enter open · r refresh · esc back",
                self.entries.len()
            )
        } else {
            format!(
                "{} entries · end · j/k · enter open · r refresh · esc back",
                self.entries.len()
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style()))),
            layout[1],
        );
    }
}

fn entry_item<'a>(entry: &'a Entry, theme: &Theme) -> ListItem<'a> {
    let when = entry
        .created_at
        .map(format_timestamp_relative)
        .unwrap_or_default();
    let counts = format!(
        " · {} replies · {} bookmarks",
        entry.replies_count, entry.bookmarks_count
    );
    let header = Line::from(vec![
        Span::styled(format!("@{}", entry.author_username), theme.accent_style()),
        Span::styled(format!(" · {when}{counts}"), theme.muted_style()),
    ]);
    let snippet = super::markdown::content_preview(&entry.content, 200);
    let body = Line::from(Span::styled(snippet, theme.base()));
    ListItem::new(vec![header, body, Line::from("")])
}

fn format_timestamp_relative(t: OffsetDateTime) -> String {
    let now = OffsetDateTime::now_utc();
    let delta = now - t;
    let secs = delta.whole_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else if secs < 30 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else {
        let dt = t.date();
        format!("{}-{:02}-{:02}", dt.year(), u8::from(dt.month()), dt.day())
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

    fn entry(post_id: &str) -> Entry {
        Entry {
            post_id: post_id.into(),
            author_id: "a".into(),
            author_username: "alice".into(),
            content: format!("entry {post_id}"),
            title: None,
            slug: None,
            topics: vec!["music".into()],
            replies_count: 0,
            bookmarks_count: 0,
            is_public: false,
            is_nsfw: false,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    #[test]
    fn backspace_returns_back_to_index() {
        let mut s = TopicFeedScreen::new("music".into());
        assert_eq!(s.handle_key(key(KeyCode::Backspace)), TopicFeedIntent::Back);
    }

    #[test]
    fn enter_opens_selected_post() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1"), entry("p2")], None)));
        s.selected = 1;
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            TopicFeedIntent::OpenSelected {
                post_id: "p2".into()
            }
        );
    }

    #[test]
    fn apply_more_appends() {
        let mut s = TopicFeedScreen::new("linux".into());
        s.apply_initial(Ok((vec![entry("p1")], Some("c".into()))));
        s.apply_more(Ok((vec![entry("p2")], None)));
        assert_eq!(s.entries.len(), 2);
        assert!(s.next_cursor.is_none());
    }
}

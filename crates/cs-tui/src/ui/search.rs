//! Search overlay — full-text search across users, posts, and replies.
//!
//! Opened globally with Ctrl+F and pushed like any transient screen. Shows the
//! grouped preview (`type=all`): up to 8 hits per group, navigable as one
//! flattened list, opening the selected hit into its post or profile.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{PostHit, ReplyHit, SearchPreview, UserHit};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::cmail::one_line_preview;
use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchIntent {
    Run {
        query: String,
    },
    OpenPost {
        post_id: String,
        highlight_reply_id: Option<String>,
    },
    OpenUser {
        username: String,
    },
    Back,
    Quit,
    None,
}

/// One navigable result row (borrowed from the preview groups).
enum Row<'a> {
    User(&'a UserHit),
    Post(&'a PostHit),
    Reply(&'a ReplyHit),
}

#[derive(Debug)]
pub struct SearchScreen {
    query: String,
    /// Whether the query box is focused (typing) vs. browsing results.
    editing: bool,
    results: Option<SearchPreview>,
    loading: bool,
    error: Option<String>,
    /// Index into the flattened result rows.
    selected: usize,
}

impl SearchScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            query: String::new(),
            editing: true,
            results: None,
            loading: false,
            error: None,
            selected: 0,
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    pub fn paste_text(&mut self, text: &str) {
        if self.editing {
            self.query.push_str(&super::input::collapse_newlines(text));
        }
    }

    fn row_count(&self) -> usize {
        self.results
            .as_ref()
            .map_or(0, |r| r.users.len() + r.posts.len() + r.replies.len())
    }

    fn rows(&self) -> Vec<Row<'_>> {
        let mut out = Vec::new();
        if let Some(r) = &self.results {
            out.extend(r.users.iter().map(Row::User));
            out.extend(r.posts.iter().map(Row::Post));
            out.extend(r.replies.iter().map(Row::Reply));
        }
        out
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SearchIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return SearchIntent::Quit;
        }
        if self.editing {
            return self.handle_editing_key(key);
        }
        match key.code {
            KeyCode::Esc => SearchIntent::Back,
            KeyCode::Char('/') | KeyCode::Char('e') => {
                self.editing = true;
                SearchIntent::None
            }
            KeyCode::Enter => self.open_selected(),
            code => {
                let len = self.row_count();
                super::list_nav::navigate(code, &mut self.selected, len, false);
                SearchIntent::None
            }
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> SearchIntent {
        match key.code {
            KeyCode::Esc => SearchIntent::Back,
            KeyCode::Enter => {
                let query = self.query.trim().to_string();
                if query.is_empty() {
                    return SearchIntent::None;
                }
                self.editing = false;
                self.loading = true;
                self.error = None;
                self.selected = 0;
                SearchIntent::Run { query }
            }
            KeyCode::Backspace => {
                self.query.pop();
                SearchIntent::None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(c);
                SearchIntent::None
            }
            _ => SearchIntent::None,
        }
    }

    fn open_selected(&self) -> SearchIntent {
        match self.rows().into_iter().nth(self.selected) {
            Some(Row::User(u)) => SearchIntent::OpenUser {
                username: u.username.clone(),
            },
            Some(Row::Post(p)) => SearchIntent::OpenPost {
                post_id: p.post_id.clone(),
                highlight_reply_id: None,
            },
            Some(Row::Reply(r)) => SearchIntent::OpenPost {
                post_id: r.post_id.clone(),
                highlight_reply_id: Some(r.reply_id.clone()),
            },
            None => SearchIntent::None,
        }
    }

    pub fn apply_results(&mut self, result: Result<SearchPreview, String>) {
        self.loading = false;
        match result {
            Ok(preview) => {
                self.results = Some(preview);
                self.selected = 0;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • search ", theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);

        // Query line.
        let query_line = if self.editing {
            Line::from(vec![
                Span::styled("search: ", theme.muted_style()),
                Span::styled(self.query.clone(), theme.base()),
                Span::styled("▏", theme.accent_style()),
            ])
        } else {
            Line::from(vec![
                Span::styled("search: ", theme.muted_style()),
                Span::styled(self.query.clone(), theme.base()),
            ])
        };
        frame.render_widget(Paragraph::new(query_line), layout[0]);

        // Body: grouped results (or status).
        frame.render_widget(Paragraph::new(self.body_lines(theme)), layout[1]);

        // Footer hint.
        let hint = if self.editing {
            "enter search · esc close"
        } else {
            "↑↓ move · enter open · / edit · esc close"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, theme.muted_style()))),
            layout[2],
        );
    }

    fn body_lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        if self.loading {
            return vec![Line::from(Span::styled("searching…", theme.accent_style()))];
        }
        if let Some(msg) = &self.error {
            return vec![Line::from(Span::styled(msg.clone(), theme.error_style()))];
        }
        let Some(r) = &self.results else {
            return vec![Line::from(Span::styled(
                "type a query and press enter",
                theme.muted_style(),
            ))];
        };
        if r.is_empty() {
            return vec![Line::from(Span::styled("no results", theme.muted_style()))];
        }

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut idx = 0usize;
        let sel = self.selected;
        if !r.users.is_empty() {
            lines.push(section_header("Users", theme));
            for u in &r.users {
                let label = match &u.display_name {
                    Some(d) if !d.is_empty() => format!("@{} · {d}", u.username),
                    _ => format!("@{}", u.username),
                };
                lines.push(result_line(&label, idx == sel, theme));
                idx += 1;
            }
        }
        if !r.posts.is_empty() {
            lines.push(section_header("Posts", theme));
            for p in &r.posts {
                let title = p
                    .title
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| one_line_preview(&p.content, 60));
                let label = format!("{title}  · @{}", p.author_username);
                lines.push(result_line(&label, idx == sel, theme));
                idx += 1;
            }
        }
        if !r.replies.is_empty() {
            lines.push(section_header("Replies", theme));
            for reply in &r.replies {
                let label = one_line_preview(&reply.content, 70);
                lines.push(result_line(&label, idx == sel, theme));
                idx += 1;
            }
        }
        lines
    }
}

impl Default for SearchScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn section_header(label: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(format!("── {label} ──"), theme.muted_style()))
}

fn result_line(label: &str, selected: bool, theme: &Theme) -> Line<'static> {
    let (marker, style) = if selected {
        ("▌ ", theme.accent_style())
    } else {
        ("  ", theme.base())
    };
    Line::from(vec![
        Span::styled(marker, theme.accent_style()),
        Span::styled(label.to_string(), style),
    ])
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

    fn preview() -> SearchPreview {
        SearchPreview {
            users: vec![UserHit {
                username: "neo".into(),
                ..UserHit::default()
            }],
            posts: vec![PostHit {
                post_id: "p1".into(),
                author_username: "trinity".into(),
                content: "hello".into(),
                ..PostHit::default()
            }],
            replies: vec![ReplyHit {
                reply_id: "r1".into(),
                post_id: "p2".into(),
                content: "re".into(),
                ..ReplyHit::default()
            }],
        }
    }

    #[test]
    fn typing_and_enter_runs_the_search() {
        let mut s = SearchScreen::new();
        for c in "neon".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            SearchIntent::Run {
                query: "neon".into()
            }
        );
        assert!(!s.is_editing());
    }

    #[test]
    fn navigating_and_opening_hits() {
        let mut s = SearchScreen::new();
        s.editing = false;
        s.apply_results(Ok(preview()));
        // Row 0 = user "neo".
        assert_eq!(
            s.open_selected(),
            SearchIntent::OpenUser {
                username: "neo".into()
            }
        );
        // Down to the post row.
        s.handle_key(key(KeyCode::Down));
        assert_eq!(
            s.open_selected(),
            SearchIntent::OpenPost {
                post_id: "p1".into(),
                highlight_reply_id: None,
            }
        );
        // Down to the reply row → opens its parent post with the reply highlighted.
        s.handle_key(key(KeyCode::Down));
        assert_eq!(
            s.open_selected(),
            SearchIntent::OpenPost {
                post_id: "p2".into(),
                highlight_reply_id: Some("r1".into()),
            }
        );
    }

    #[test]
    fn slash_returns_to_editing() {
        let mut s = SearchScreen::new();
        s.editing = false;
        assert_eq!(s.handle_key(key(KeyCode::Char('/'))), SearchIntent::None);
        assert!(s.is_editing());
    }
}

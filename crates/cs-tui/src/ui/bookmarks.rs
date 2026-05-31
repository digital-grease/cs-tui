//! Bookmarks screen.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Bookmark, BookmarkKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use time::OffsetDateTime;

use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarksIntent {
    LoadMore,
    Refresh,
    /// Remove the selected bookmark.
    RemoveSelected {
        bookmark_id: String,
    },
    /// Open the underlying post (or the parent post of a reply bookmark).
    OpenSelected {
        post_id: String,
        highlight_reply_id: Option<String>,
    },
    Quit,
    None,
}

#[derive(Debug)]
pub struct BookmarksScreen {
    pub items: Vec<Bookmark>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

impl BookmarksScreen {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: true,
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> BookmarksIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return BookmarksIntent::Quit;
        }
        if self.loading {
            return BookmarksIntent::None;
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
                return BookmarksIntent::LoadMore;
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
                return BookmarksIntent::LoadMore;
            }
            KeyCode::Char('r') => {
                self.items.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return BookmarksIntent::Refresh;
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(b) = self.items.get(self.selected) {
                    return BookmarksIntent::RemoveSelected {
                        bookmark_id: b.bookmark_id.clone(),
                    };
                }
            }
            KeyCode::Enter => {
                if let Some(b) = self.items.get(self.selected) {
                    let (post_id, highlight) = match b.kind {
                        BookmarkKind::Post => (b.post_id.clone(), None),
                        BookmarkKind::Reply => {
                            let post_id =
                                b.reply.as_ref().map(|r| r.post_id.clone()).or_else(|| {
                                    b.post_id.clone() // best-effort fallback
                                });
                            (post_id, b.reply_id.clone())
                        }
                    };
                    if let Some(pid) = post_id {
                        return BookmarksIntent::OpenSelected {
                            post_id: pid,
                            highlight_reply_id: highlight,
                        };
                    }
                }
            }
            _ => {}
        }
        BookmarksIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Bookmark>, Option<String>), String>) {
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

    pub fn apply_more(&mut self, result: Result<(Vec<Bookmark>, Option<String>), String>) {
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

    /// Optimistically remove a bookmark from local state.
    pub fn remove_local(&mut self, bookmark_id: &str) {
        if let Some(idx) = self.items.iter().position(|b| b.bookmark_id == bookmark_id) {
            self.items.remove(idx);
            if self.selected >= self.items.len() {
                self.selected = self.items.len().saturating_sub(1);
            }
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • bookmarks ", theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        if self.loading && self.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading bookmarks…",
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
                Paragraph::new(Line::from(Span::styled(
                    "no bookmarks",
                    theme.muted_style(),
                ))),
                layout[0],
            );
        } else {
            let items: Vec<ListItem<'_>> =
                self.items.iter().map(|b| bookmark_item(b, theme)).collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(self.selected.min(self.items.len().saturating_sub(1))));
            frame.render_stateful_widget(list, layout[0], &mut state);
        }

        let status = status_line(self, theme);
        frame.render_widget(status, layout[1]);
    }
}

impl Default for BookmarksScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn bookmark_item<'a>(b: &'a Bookmark, theme: &Theme) -> ListItem<'a> {
    let (kind_label, author, snippet, when) = match (b.kind, &b.post, &b.reply) {
        (BookmarkKind::Post, Some(p), _) => (
            "post",
            p.author_username.clone(),
            first_line_truncated(&p.content, 160),
            p.created_at,
        ),
        (BookmarkKind::Reply, _, Some(r)) => (
            "reply",
            r.author_username.clone(),
            first_line_truncated(&r.content, 160),
            r.created_at,
        ),
        (BookmarkKind::Post, None, _) => (
            "post",
            "?".to_string(),
            format!(
                "[deleted post — id {}]",
                b.post_id.as_deref().unwrap_or("?")
            ),
            None,
        ),
        (BookmarkKind::Reply, _, None) => (
            "reply",
            "?".to_string(),
            format!(
                "[deleted reply — id {}]",
                b.reply_id.as_deref().unwrap_or("?")
            ),
            None,
        ),
    };
    let when_str = when.map(format_timestamp_relative).unwrap_or_default();
    let header = Line::from(vec![
        Span::styled(format!("[{kind_label}] "), theme.muted_style()),
        Span::styled(format!("@{author}"), theme.accent_style()),
        Span::styled(format!(" · {when_str}"), theme.muted_style()),
    ]);
    let body = Line::from(Span::styled(snippet, theme.base()));
    ListItem::new(vec![header, body, Line::from("")])
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

fn status_line<'a>(s: &'a BookmarksScreen, theme: &Theme) -> Paragraph<'a> {
    let text = if s.loading {
        "loading… · enter open · d remove · n next · r refresh · esc menu".to_string()
    } else if s.next_cursor.is_some() {
        format!(
            "{} bookmarks · scroll down for more · enter open · d remove · r refresh · esc menu",
            s.items.len()
        )
    } else {
        format!(
            "{} bookmarks · end · enter open · d remove · r refresh · esc menu",
            s.items.len()
        )
    };
    Paragraph::new(Line::from(Span::styled(text, theme.muted_style())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use cs_api::{Entry, Reply};

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
            content: "hi".into(),
            title: None,
            slug: None,
            topics: vec![],
            replies_count: 0,
            bookmarks_count: 0,
            is_public: false,
            is_nsfw: false,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    fn reply(reply_id: &str, post_id: &str) -> Reply {
        Reply {
            reply_id: reply_id.into(),
            post_id: post_id.into(),
            author_id: "a".into(),
            author_username: "alice".into(),
            content: "yo".into(),
            parent_reply_id: None,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    fn post_bookmark(id: &str, post_id: &str) -> Bookmark {
        Bookmark {
            bookmark_id: id.into(),
            kind: BookmarkKind::Post,
            post_id: Some(post_id.into()),
            reply_id: None,
            post: Some(entry(post_id)),
            reply: None,
            created_at: None,
        }
    }

    fn reply_bookmark(id: &str, reply_id: &str, post_id: &str) -> Bookmark {
        Bookmark {
            bookmark_id: id.into(),
            kind: BookmarkKind::Reply,
            post_id: None,
            reply_id: Some(reply_id.into()),
            post: None,
            reply: Some(reply(reply_id, post_id)),
            created_at: None,
        }
    }

    #[test]
    fn enter_on_post_bookmark_opens_post() {
        let mut s = BookmarksScreen::new();
        s.apply_initial(Ok((vec![post_bookmark("b1", "p1")], None)));
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            BookmarksIntent::OpenSelected {
                post_id: "p1".into(),
                highlight_reply_id: None,
            }
        );
    }

    #[test]
    fn enter_on_reply_bookmark_opens_parent_post_with_highlight() {
        let mut s = BookmarksScreen::new();
        s.apply_initial(Ok((vec![reply_bookmark("b1", "r1", "p1")], None)));
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            BookmarksIntent::OpenSelected {
                post_id: "p1".into(),
                highlight_reply_id: Some("r1".into()),
            }
        );
    }

    #[test]
    fn d_removes_selected() {
        let mut s = BookmarksScreen::new();
        s.apply_initial(Ok((vec![post_bookmark("b1", "p1")], None)));
        let intent = s.handle_key(key(KeyCode::Char('d')));
        assert_eq!(
            intent,
            BookmarksIntent::RemoveSelected {
                bookmark_id: "b1".into()
            }
        );
    }

    #[test]
    fn remove_local_drops_the_item() {
        let mut s = BookmarksScreen::new();
        s.apply_initial(Ok((
            vec![post_bookmark("b1", "p1"), post_bookmark("b2", "p2")],
            None,
        )));
        s.selected = 1;
        s.remove_local("b1");
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].bookmark_id, "b2");
        // Selection clamped to remaining items.
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn apply_more_appends() {
        let mut s = BookmarksScreen::new();
        s.apply_initial(Ok((vec![post_bookmark("b1", "p1")], Some("c".into()))));
        s.apply_more(Ok((vec![post_bookmark("b2", "p2")], None)));
        assert_eq!(s.items.len(), 2);
        assert!(s.next_cursor.is_none());
    }
}

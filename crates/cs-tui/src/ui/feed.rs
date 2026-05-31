//! Feed screen — paginated list of entries with cursor-driven scroll.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Entry;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use time::OffsetDateTime;

use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedIntent {
    /// Load the next cursor page.
    LoadMore,
    /// Re-fetch from cursor=None.
    Refresh,
    /// Open the post detail for the selected entry's `post_id`.
    OpenSelected(String),
    /// Bookmark the selected entry (`post_id`).
    Bookmark(String),
    /// Start composing a new entry.
    Compose,
    /// Exit the app.
    Quit,
    None,
}

#[derive(Debug)]
pub struct FeedScreen {
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub include_nsfw: bool,
}

impl FeedScreen {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: true,
            error: None,
            include_nsfw: false,
        }
    }

    /// Number of entries currently visible after NSFW filtering.
    fn visible_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.include_nsfw || !e.is_nsfw)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FeedIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return FeedIntent::Quit;
        }
        if self.loading {
            return FeedIntent::None;
        }
        let visible = self.visible_indices();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down
                if !visible.is_empty() && self.selected < visible.len() - 1 =>
            {
                self.selected += 1;
            }
            // At the bottom of the loaded list, scrolling down pulls the next
            // page automatically (no need to know about `n`).
            KeyCode::Char('j') | KeyCode::Down if self.next_cursor.is_some() => {
                self.loading = true;
                return FeedIntent::LoadMore;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.selected = 0;
            }
            KeyCode::Char('G') | KeyCode::End if !visible.is_empty() => {
                self.selected = visible.len() - 1;
            }
            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::PageDown
                if self.next_cursor.is_some() =>
            {
                self.loading = true;
                return FeedIntent::LoadMore;
            }
            KeyCode::Char('r') => {
                self.entries.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return FeedIntent::Refresh;
            }
            KeyCode::Char('c') => {
                return FeedIntent::Compose;
            }
            KeyCode::Char('b') => {
                if let Some(idx) = visible.get(self.selected) {
                    if let Some(entry) = self.entries.get(*idx) {
                        return FeedIntent::Bookmark(entry.post_id.clone());
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(idx) = visible.get(self.selected) {
                    if let Some(entry) = self.entries.get(*idx) {
                        return FeedIntent::OpenSelected(entry.post_id.clone());
                    }
                }
            }
            _ => {}
        }
        FeedIntent::None
    }

    /// Apply the result of an initial load or refresh.
    pub fn apply_initial(&mut self, result: Result<(Vec<Entry>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((entries, cursor)) => {
                self.entries = entries;
                self.next_cursor = cursor;
                if self.selected >= self.visible_indices().len() {
                    self.selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    /// Append the result of a load-more page.
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
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • feed ", theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let list_area = layout[0];
        let status_area = layout[1];

        let visible_indices = self.visible_indices();

        if self.loading && self.entries.is_empty() {
            let para = Paragraph::new(Line::from(Span::styled(
                "loading feed…",
                theme.accent_style(),
            )));
            frame.render_widget(para, list_area);
        } else if let Some(msg) = &self.error {
            let para = Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style())));
            frame.render_widget(para, list_area);
        } else if visible_indices.is_empty() {
            let para = Paragraph::new(Line::from(Span::styled(
                "no entries to show",
                theme.muted_style(),
            )));
            frame.render_widget(para, list_area);
        } else {
            let items: Vec<ListItem<'_>> = visible_indices
                .iter()
                .map(|i| entry_item(&self.entries[*i], list_area.width, theme))
                .collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(
                self.selected.min(visible_indices.len().saturating_sub(1)),
            ));
            frame.render_stateful_widget(list, list_area, &mut state);
        }

        let status = status_line(self, theme);
        frame.render_widget(status, status_area);
    }
}

impl Default for FeedScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn entry_item<'a>(entry: &'a Entry, width: u16, theme: &Theme) -> ListItem<'a> {
    let when = entry
        .created_at
        .map(format_timestamp_relative)
        .unwrap_or_default();
    let topics = if entry.topics.is_empty() {
        String::new()
    } else {
        format!(" · #{}", entry.topics.join(" #"))
    };
    let counts = format!(
        " · {} replies · {} bookmarks",
        entry.replies_count, entry.bookmarks_count
    );

    let header = Line::from(vec![
        Span::styled(format!("@{}", entry.author_username), theme.accent_style()),
        Span::styled(format!(" · {when}{topics}{counts}"), theme.muted_style()),
    ]);

    let mut lines = vec![header];

    // v0.3.7: surface the entry title (when set) on its own line above the
    // content snippet. Skipped for None/whitespace-only titles.
    if let Some(title) = entry.title.as_deref() {
        let title = title.trim();
        if !title.is_empty() {
            lines.push(Line::from(Span::styled(
                first_line_truncated(title, 200),
                theme.accent_style(),
            )));
        }
    }

    let snippet = super::markdown::content_preview(&entry.content, 200);
    lines.push(Line::from(Span::styled(snippet, theme.base())));

    // Rule between posts so it's clear where one ends and the next begins.
    // `width - 2` accounts for the list's highlight-symbol gutter.
    let rule = "─".repeat(width.saturating_sub(2).max(1) as usize);
    lines.push(Line::from(Span::styled(rule, theme.muted_style())));

    ListItem::new(lines)
}

fn first_line_truncated(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= max {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(max - 1).collect();
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

fn status_line<'a>(s: &'a FeedScreen, theme: &Theme) -> Paragraph<'a> {
    let text = if s.loading {
        "loading… · enter open · b bookmark · r refresh · esc menu".to_string()
    } else if s.next_cursor.is_some() {
        format!(
            "{} entries · scroll down for more · enter open · b bookmark · r refresh · esc menu",
            s.entries.len()
        )
    } else {
        format!(
            "{} entries · end of feed · enter open · b bookmark · r refresh · esc menu",
            s.entries.len()
        )
    };
    Paragraph::new(Line::from(Span::styled(text, theme.muted_style())))
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

    fn entry(id: &str, author: &str, nsfw: bool) -> Entry {
        Entry {
            post_id: id.into(),
            author_id: "u1".into(),
            author_username: author.into(),
            content: format!("content of {id}"),
            title: None,
            slug: None,
            topics: vec![],
            replies_count: 0,
            bookmarks_count: 0,
            is_public: false,
            is_nsfw: nsfw,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    fn render_entry_item(entry: &Entry) -> String {
        use ratatui::widgets::List;
        let theme = Theme::cyber();
        let item = entry_item(entry, 80, &theme);
        let backend = ratatui::backend::TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(List::new(vec![item]), area);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn entry_item_renders_title_only_when_present() {
        let marker = "ZZTITLEMARKER";
        let mut with = entry("a", "alice", false);
        with.title = Some(marker.into());
        assert!(
            render_entry_item(&with).contains(marker),
            "title should render in the feed item"
        );

        let without = entry("a", "alice", false); // title: None
        assert!(
            !render_entry_item(&without).contains(marker),
            "no title line should render when title is None"
        );
    }

    #[test]
    fn entry_item_skips_whitespace_only_title() {
        let mut e = entry("a", "alice", false);
        e.title = Some("   ".into());
        let text = render_entry_item(&e);
        assert!(
            text.contains("content of a"),
            "content snippet still renders"
        );
    }

    #[test]
    fn new_starts_loading() {
        let s = FeedScreen::new();
        assert!(s.loading);
        assert!(s.entries.is_empty());
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn apply_initial_clears_loading_and_populates() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "alice", false)], None)));
        assert!(!s.loading);
        assert_eq!(s.entries.len(), 1);
        assert!(s.next_cursor.is_none());
        assert!(s.error.is_none());
    }

    #[test]
    fn apply_initial_error_sets_error_and_clears_loading() {
        let mut s = FeedScreen::new();
        s.apply_initial(Err("boom".into()));
        assert!(!s.loading);
        assert_eq!(s.error.as_deref(), Some("boom"));
    }

    #[test]
    fn j_advances_selection_bounded() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![
                entry("a", "a", false),
                entry("b", "b", false),
                entry("c", "c", false),
            ],
            None,
        )));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 1);
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 2);
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 2, "should not advance past last");
    }

    #[test]
    fn k_decrements_selection_bounded() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        s.selected = 1;
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.selected, 0);
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn b_bookmarks_selected_entry() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("p1", "a", false), entry("p2", "b", false)],
            None,
        )));
        s.selected = 1;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('b'))),
            FeedIntent::Bookmark("p2".into())
        );
    }

    #[test]
    fn enter_emits_open_selected_with_post_id() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("p1", "a", false), entry("p2", "b", false)],
            None,
        )));
        s.selected = 1;
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, FeedIntent::OpenSelected("p2".into()));
    }

    #[test]
    fn n_requests_load_more_only_when_cursor_present() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], Some("next".into()))));
        let intent = s.handle_key(key(KeyCode::Char('n')));
        assert_eq!(intent, FeedIntent::LoadMore);
        assert!(s.loading);

        s.loading = false;
        s.next_cursor = None;
        let intent = s.handle_key(key(KeyCode::Char('n')));
        assert_eq!(intent, FeedIntent::None);
    }

    #[test]
    fn j_at_bottom_auto_loads_next_page() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            Some("next".into()),
        )));
        // Move to the last entry, then one more `j` paginates instead of stalling.
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.selected, 1);
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, FeedIntent::LoadMore);
        assert!(s.loading);
    }

    #[test]
    fn j_at_bottom_without_cursor_does_nothing() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], None)));
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, FeedIntent::None);
        assert_eq!(s.selected, 0);
        assert!(!s.loading);
    }

    #[test]
    fn r_resets_and_requests_refresh() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], Some("cur".into()))));
        s.selected = 0;
        let intent = s.handle_key(key(KeyCode::Char('r')));
        assert_eq!(intent, FeedIntent::Refresh);
        assert!(s.loading);
        assert!(s.entries.is_empty());
        assert!(s.next_cursor.is_none());
    }

    #[test]
    fn nsfw_entries_hidden_by_default() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![
                entry("a", "a", false),
                entry("b", "b", true),
                entry("c", "c", false),
            ],
            None,
        )));
        assert_eq!(s.visible_indices(), vec![0, 2]);
    }

    #[test]
    fn nsfw_entries_visible_when_enabled() {
        let mut s = FeedScreen::new();
        s.include_nsfw = true;
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", true)],
            None,
        )));
        assert_eq!(s.visible_indices(), vec![0, 1]);
    }

    #[test]
    fn ctrl_c_emits_quit() {
        let mut s = FeedScreen::new();
        let kev = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(s.handle_key(kev), FeedIntent::Quit);
    }

    #[test]
    fn q_is_just_a_letter() {
        // q is no longer a quit shortcut — must not return Quit.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![], None)));
        let intent = s.handle_key(key(KeyCode::Char('q')));
        assert_eq!(intent, FeedIntent::None);
    }

    #[test]
    fn apply_more_appends_entries() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], Some("c1".into()))));
        s.apply_more(Ok((vec![entry("b", "b", false)], None)));
        assert_eq!(s.entries.len(), 2);
        assert!(s.next_cursor.is_none());
    }

    #[test]
    fn truncation_handles_short_content() {
        assert_eq!(first_line_truncated("hi", 50), "hi");
    }

    #[test]
    fn truncation_truncates_long_content() {
        let s = "x".repeat(300);
        let out = first_line_truncated(&s, 200);
        assert_eq!(out.chars().count(), 200);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncation_uses_only_first_line() {
        let s = "first line\nsecond line";
        assert_eq!(first_line_truncated(s, 100), "first line");
    }
}

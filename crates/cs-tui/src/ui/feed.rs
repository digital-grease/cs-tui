//! Feed screen — paginated list of entries with cursor-driven scroll.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Entry;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::list::{self, TabState};
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
    /// Play (or toggle) the selected entry's jukebox track. `None` when it has
    /// none — the app then treats `p` as pause for whatever is already playing.
    PlayJukebox(Option<super::audio::JukeboxTrack>),
    /// Open the selected entry's jukebox link in the browser.
    OpenJukebox(String),
    /// Start composing a new entry.
    Compose,
    /// Exit the app.
    Quit,
    None,
}

#[derive(Debug)]
pub struct FeedScreen {
    pub list: TabState<Entry>,
    pub include_nsfw: bool,
}

/// Outcome of folding a background head-poll into the feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadUpdate {
    /// Nothing visibly changed.
    None,
    /// `n` new (visible) entries were prepended at the top.
    Prepended(usize),
    /// The head page didn't overlap the current top — more than a page of new
    /// entries arrived, so prepending would leave a gap; a manual refresh is
    /// needed to catch up cleanly.
    Gap,
}

impl FeedScreen {
    pub fn new() -> Self {
        Self {
            list: TabState::loading(),
            include_nsfw: crate::config::get().nsfw,
        }
    }

    /// Number of entries currently visible after NSFW filtering.
    fn visible_indices(&self) -> Vec<usize> {
        self.list
            .items
            .iter()
            .enumerate()
            .filter(|(_, e)| self.include_nsfw || !e.is_nsfw)
            .map(|(i, _)| i)
            .collect()
    }

    /// The currently highlighted entry (after NSFW filtering), if any.
    fn selected_entry(&self) -> Option<&Entry> {
        self.visible_indices()
            .get(self.list.selected)
            .and_then(|idx| self.list.items.get(*idx))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FeedIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return FeedIntent::Quit;
        }
        if self.list.loading {
            return FeedIntent::None;
        }
        let visible = self.visible_indices();
        match super::list_nav::navigate(
            key.code,
            &mut self.list.selected,
            visible.len(),
            self.list.next_cursor.is_some(),
        ) {
            super::list_nav::ListNav::LoadMore => {
                self.list.loading = true;
                return FeedIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return FeedIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        match key.code {
            KeyCode::Char('r') => {
                self.list.items.clear();
                self.list.next_cursor = None;
                self.list.selected = 0;
                self.list.loading = true;
                self.list.error = None;
                return FeedIntent::Refresh;
            }
            KeyCode::Char('c') => {
                return FeedIntent::Compose;
            }
            KeyCode::Char('b') => {
                if let Some(idx) = visible.get(self.list.selected) {
                    if let Some(entry) = self.list.items.get(*idx) {
                        return FeedIntent::Bookmark(entry.post_id.clone());
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(idx) = visible.get(self.list.selected) {
                    if let Some(entry) = self.list.items.get(*idx) {
                        return FeedIntent::OpenSelected(entry.post_id.clone());
                    }
                }
            }
            KeyCode::Char('p') => {
                let track = visible
                    .get(self.list.selected)
                    .and_then(|idx| self.list.items.get(*idx))
                    .and_then(|e| super::audio::jukebox_track(&e.attachments));
                return FeedIntent::PlayJukebox(track);
            }
            KeyCode::Char('o') => {
                if let Some(url) = visible
                    .get(self.list.selected)
                    .and_then(|idx| self.list.items.get(*idx))
                    .and_then(|e| super::audio::jukebox_url(&e.attachments))
                {
                    return FeedIntent::OpenJukebox(url);
                }
            }
            _ => {}
        }
        FeedIntent::None
    }

    /// Apply the result of an initial load or refresh. Selection clamps to the
    /// NSFW-filtered view, not the raw item count.
    pub fn apply_initial(&mut self, result: Result<(Vec<Entry>, Option<String>), String>) {
        self.list.apply_initial(result);
        if self.list.selected >= self.visible_indices().len() {
            self.list.selected = 0;
        }
    }

    /// Append the result of a load-more page.
    pub fn apply_more(&mut self, result: Result<(Vec<Entry>, Option<String>), String>) {
        self.list.apply_more(result);
    }

    /// Fold a background head-poll (the newest page) into the feed without
    /// moving the user's scroll position. Strictly-new entries (the prefix of
    /// `head` ahead of the first entry we already have) are prepended at the
    /// top, and `selected` — a NSFW-filtered *view* index — shifts by the number
    /// of *visible* new entries so the row under the cursor stays put. If the
    /// user is at the very top, the newest is revealed (selection stays at 0).
    /// If the page doesn't overlap our current top (more than a page of new
    /// entries), returns `Gap` and changes nothing, so we never hide entries.
    pub fn apply_new_head(&mut self, head: Vec<Entry>) -> HeadUpdate {
        use std::collections::HashSet;
        if head.is_empty() || self.list.items.is_empty() {
            return HeadUpdate::None;
        }
        let existing: HashSet<&str> = self.list.items.iter().map(|e| e.post_id.as_str()).collect();
        let new_count = match head
            .iter()
            .position(|e| existing.contains(e.post_id.as_str()))
        {
            Some(0) => return HeadUpdate::None, // head's newest already at our top
            Some(k) => k,                       // head[0..k] are strictly newer
            None => return HeadUpdate::Gap,     // a full page of new entries: gap
        };
        let new: Vec<Entry> = head.into_iter().take(new_count).collect();
        let visible_new = new
            .iter()
            .filter(|e| self.include_nsfw || !e.is_nsfw)
            .count();
        self.list.items.splice(0..0, new);
        if self.list.selected != 0 {
            self.list.selected += visible_new;
            // Keep the viewport on the same rows: the visible list grew by
            // `visible_new` at the top, so the persisted scroll offset shifts too.
            self.list.shift_offset(visible_new);
        }
        if visible_new == 0 {
            HeadUpdate::None
        } else {
            HeadUpdate::Prepended(visible_new)
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

        let visible = self.visible_indices();
        let width = list_area.width;
        list::render_body(
            frame,
            list_area,
            theme,
            &self.list,
            &visible,
            "no entries to show",
            |e| entry_item(e, width, theme),
        );

        let status = status_line(self, theme);
        frame.render_widget(status, status_area);
    }
}

impl Default for FeedScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn entry_item(entry: &Entry, width: u16, theme: &Theme) -> ListItem<'static> {
    let when = entry
        .created_at
        .map(crate::config::format_list_timestamp)
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

    let mut header_spans = vec![
        Span::styled(format!("@{}", entry.author_username), theme.accent_style()),
        Span::styled(format!(" · {when}{topics}{counts}"), theme.muted_style()),
    ];
    // Flag any image (markdown link OR attachment) — the snippet only sees
    // markdown, so attachment-only posts would otherwise look image-less.
    if super::images::has_image(entry) {
        header_spans.push(Span::styled(" · [image]", theme.accent_style()));
    }
    // Likewise flag a jukebox (audio) attachment so it's visible from the list.
    if super::audio::has_audio(entry) {
        header_spans.push(Span::styled(" · [jukebox]", theme.accent_style()));
    }

    let mut lines = vec![Line::from(header_spans)];

    // v0.3.7: surface the entry title (when set) on its own line above the
    // content snippet. Skipped for None/whitespace-only titles.
    if let Some(title) = entry.title.as_deref() {
        let title = title.trim();
        if !title.is_empty() {
            lines.push(Line::from(Span::styled(
                super::text::first_line_truncated(title, 200),
                theme.accent_style(),
            )));
        }
    }

    let snippet =
        super::markdown::content_preview(&entry.content, crate::config::get().preview_length);
    if !snippet.is_empty() {
        lines.push(Line::from(Span::styled(snippet, theme.base())));
    }

    // Rule between posts so it's clear where one ends and the next begins
    // (omitted in compact mode). `width - 2` accounts for the highlight gutter.
    if !crate::config::get().compact {
        let rule = "─".repeat(width.saturating_sub(2).max(1) as usize);
        lines.push(Line::from(Span::styled(rule, theme.muted_style())));
    }

    ListItem::new(lines)
}

fn status_line<'a>(s: &'a FeedScreen, theme: &Theme) -> Paragraph<'a> {
    if let Some(msg) = list::load_more_error(&s.list) {
        return Paragraph::new(Line::from(Span::styled(msg, theme.error_style())));
    }
    // Surface the jukebox keys only when the highlighted post has a track.
    let media = if s
        .selected_entry()
        .is_some_and(super::audio::has_audio)
    {
        " · p play · o open"
    } else {
        ""
    };
    let text = if s.list.loading {
        "loading… · c new post · enter open · b bookmark · r refresh · esc menu".to_string()
    } else if s.list.next_cursor.is_some() {
        format!(
            "{} entries · scroll down for more · c new post · enter open · b bookmark{media} · r refresh · esc menu",
            s.list.items.len()
        )
    } else {
        format!(
            "{} entries · end of feed · c new post · enter open · b bookmark{media} · r refresh · esc menu",
            s.list.items.len()
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
    fn entry_item_flags_an_attachment_image() {
        // The reported bug: a post with text + an image ATTACHMENT (no markdown
        // image link) showed no `[image]` tag in the feed, yet rendered an image
        // on open. It must be flagged now.
        let mut e = entry("a", "alice", false); // content "content of a"
        e.attachments = vec![cs_api::Attachment::Image {
            src: "https://x/a.png".into(),
            width: 0,
            height: 0,
        }];
        let text = render_entry_item(&e);
        assert!(
            text.contains("[image]"),
            "attachment image must be flagged: {text:?}"
        );
        assert!(text.contains("content of a"), "text snippet still renders");
    }

    #[test]
    fn p_plays_the_highlighted_entrys_jukebox() {
        let mut s = FeedScreen::new();
        let mut e = entry("a", "alice", false);
        e.attachments = vec![cs_api::Attachment::Audio {
            src: "https://youtu.be/abc".into(),
            origin: "youtube".into(),
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            genre: "electronic".into(),
        }];
        s.apply_initial(Ok((vec![e], None)));
        match s.handle_key(key(KeyCode::Char('p'))) {
            FeedIntent::PlayJukebox(Some(t)) => {
                assert_eq!(t.url, "https://youtu.be/abc");
                assert_eq!(t.title, "Paranoimia");
            }
            other => panic!("expected PlayJukebox(Some), got {other:?}"),
        }
    }

    #[test]
    fn p_with_no_jukebox_yields_play_none() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "alice", false)], None)));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('p'))),
            FeedIntent::PlayJukebox(None)
        );
    }

    #[test]
    fn o_opens_the_highlighted_entrys_jukebox() {
        let mut s = FeedScreen::new();
        let mut e = entry("a", "alice", false);
        e.attachments = vec![cs_api::Attachment::Audio {
            src: "https://youtu.be/abc".into(),
            origin: "youtube".into(),
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            genre: "electronic".into(),
        }];
        s.apply_initial(Ok((vec![e], None)));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            FeedIntent::OpenJukebox("https://youtu.be/abc".into())
        );
    }

    #[test]
    fn o_without_a_jukebox_is_a_noop() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "alice", false)], None)));
        assert_eq!(s.handle_key(key(KeyCode::Char('o'))), FeedIntent::None);
    }

    #[test]
    fn entry_item_flags_a_jukebox_attachment() {
        let mut e = entry("a", "alice", false); // content "content of a"
        e.attachments = vec![cs_api::Attachment::Audio {
            src: "https://www.youtube.com/watch?v=abc".into(),
            origin: "youtube".into(),
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            genre: "electronic".into(),
        }];
        let text = render_entry_item(&e);
        assert!(
            text.contains("[jukebox]"),
            "audio attachment must be flagged: {text:?}"
        );
        assert!(text.contains("content of a"), "text snippet still renders");
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
        assert!(s.list.loading);
        assert!(s.list.items.is_empty());
        assert_eq!(s.list.selected, 0);
    }

    #[test]
    fn apply_initial_clears_loading_and_populates() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "alice", false)], None)));
        assert!(!s.list.loading);
        assert_eq!(s.list.items.len(), 1);
        assert!(s.list.next_cursor.is_none());
        assert!(s.list.error.is_none());
    }

    #[test]
    fn apply_initial_error_sets_error_and_clears_loading() {
        let mut s = FeedScreen::new();
        s.apply_initial(Err("boom".into()));
        assert!(!s.list.loading);
        assert_eq!(s.list.error.as_deref(), Some("boom"));
    }

    #[test]
    fn apply_new_head_prepends_and_preserves_scroll() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![
                entry("a", "a", false),
                entry("b", "b", false),
                entry("c", "c", false),
            ],
            None,
        )));
        s.list.selected = 2; // viewing "c"
        let update = s.apply_new_head(vec![
            entry("x", "x", false),
            entry("a", "a", false),
            entry("b", "b", false),
            entry("c", "c", false),
        ]);
        assert_eq!(update, HeadUpdate::Prepended(1));
        assert_eq!(s.list.items[0].post_id, "x");
        assert_eq!(s.list.items.len(), 4);
        // selection followed "c" down by the one prepended row.
        assert_eq!(s.list.selected, 3);
        assert_eq!(s.list.items[s.list.selected].post_id, "c");
    }

    #[test]
    fn apply_new_head_at_top_reveals_newest() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        assert_eq!(s.list.selected, 0);
        let update = s.apply_new_head(vec![
            entry("x", "x", false),
            entry("a", "a", false),
            entry("b", "b", false),
        ]);
        assert_eq!(update, HeadUpdate::Prepended(1));
        assert_eq!(s.list.selected, 0); // stays at top, now showing "x"
        assert_eq!(s.list.items[0].post_id, "x");
    }

    #[test]
    fn apply_new_head_no_overlap_is_a_gap() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        let update = s.apply_new_head(vec![entry("x", "x", false), entry("y", "y", false)]);
        assert_eq!(update, HeadUpdate::Gap);
        assert_eq!(s.list.items.len(), 2); // unchanged
        assert_eq!(s.list.items[0].post_id, "a");
    }

    #[test]
    fn apply_new_head_nothing_new_is_noop() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        let update = s.apply_new_head(vec![entry("a", "a", false), entry("b", "b", false)]);
        assert_eq!(update, HeadUpdate::None);
        assert_eq!(s.list.items.len(), 2);
    }

    #[test]
    fn apply_new_head_shifts_by_visible_new_only() {
        // A hidden NSFW entry among the new ones doesn't move the visible cursor.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        s.include_nsfw = false;
        s.list.selected = 1; // "b" in the visible view
        let update = s.apply_new_head(vec![
            entry("n", "n", true),  // NSFW → hidden
            entry("x", "x", false), // visible
            entry("a", "a", false),
            entry("b", "b", false),
        ]);
        assert_eq!(update, HeadUpdate::Prepended(1)); // only "x" is visible-new
        assert_eq!(s.list.items.len(), 4); // raw grew by 2...
        assert_eq!(s.list.selected, 2); // ...but the view cursor shifted by 1
        let visible: Vec<_> = s
            .visible_indices()
            .iter()
            .map(|i| s.list.items[*i].post_id.clone())
            .collect();
        assert_eq!(visible[s.list.selected], "b");
    }

    #[test]
    fn apply_new_head_on_empty_feed_is_noop() {
        let mut s = FeedScreen::new();
        let update = s.apply_new_head(vec![entry("x", "x", false)]);
        assert_eq!(update, HeadUpdate::None);
        assert!(s.list.items.is_empty());
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
        assert_eq!(s.list.selected, 1);
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.list.selected, 2);
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.list.selected, 2, "should not advance past last");
    }

    #[test]
    fn k_decrements_selection_bounded() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        s.list.selected = 1;
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.list.selected, 0);
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.list.selected, 0);
    }

    #[test]
    fn b_bookmarks_selected_entry() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("p1", "a", false), entry("p2", "b", false)],
            None,
        )));
        s.list.selected = 1;
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
        s.list.selected = 1;
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, FeedIntent::OpenSelected("p2".into()));
    }

    #[test]
    fn n_requests_load_more_only_when_cursor_present() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], Some("next".into()))));
        let intent = s.handle_key(key(KeyCode::Char('n')));
        assert_eq!(intent, FeedIntent::LoadMore);
        assert!(s.list.loading);

        s.list.loading = false;
        s.list.next_cursor = None;
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
        assert_eq!(s.list.selected, 1);
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, FeedIntent::LoadMore);
        assert!(s.list.loading);
    }

    #[test]
    fn j_at_bottom_without_cursor_does_nothing() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], None)));
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, FeedIntent::None);
        assert_eq!(s.list.selected, 0);
        assert!(!s.list.loading);
    }

    #[test]
    fn r_resets_and_requests_refresh() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], Some("cur".into()))));
        s.list.selected = 0;
        let intent = s.handle_key(key(KeyCode::Char('r')));
        assert_eq!(intent, FeedIntent::Refresh);
        assert!(s.list.loading);
        assert!(s.list.items.is_empty());
        assert!(s.list.next_cursor.is_none());
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

    fn render_feed_to_string(s: &FeedScreen) -> String {
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| s.render(f, f.area(), &theme)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn load_more_failure_keeps_the_list_visible() {
        // Regression: a failed next-page fetch used to replace the whole feed
        // with a single error line. The list must stay, with the error inline.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("p1", "alice", false), entry("p2", "bob", false)],
            Some("cur".into()),
        )));
        s.apply_more(Err("network blip".into()));
        let text = render_feed_to_string(&s);
        assert!(
            text.contains("@alice"),
            "list must remain after a load-more error: {text:?}"
        );
        assert!(
            text.contains("network blip"),
            "error should be surfaced inline"
        );
    }

    #[test]
    fn apply_more_appends_entries() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("a", "a", false)], Some("c1".into()))));
        s.apply_more(Ok((vec![entry("b", "b", false)], None)));
        assert_eq!(s.list.items.len(), 2);
        assert!(s.list.next_cursor.is_none());
    }
}

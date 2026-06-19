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
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use super::list::{self, TabState};
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
    /// Play (or toggle) the selected entry's jukebox track. `None` when it has
    /// none — the app then treats `p` as pause for whatever is already playing.
    PlayJukebox(Option<super::audio::JukeboxTrack>),
    /// Open the selected entry's jukebox link in the browser.
    OpenJukebox(String),
    /// Follow/unfollow this topic (PATCHes `followedTopics`).
    ToggleFollow {
        slug: String,
    },
    /// Mute/unmute this topic (PATCHes `mutedTopics`).
    ToggleMute {
        slug: String,
    },
    Quit,
    None,
}

#[derive(Debug)]
pub struct TopicFeedScreen {
    pub slug: String,
    pub list: TabState<Entry>,
    pub include_nsfw: bool,
    /// Whether the user follows / mutes this topic (from settings).
    pub followed: bool,
    pub muted: bool,
}

impl TopicFeedScreen {
    pub fn new(slug: String) -> Self {
        Self {
            slug,
            list: TabState::loading(),
            include_nsfw: crate::config::get().nsfw,
            followed: false,
            muted: false,
        }
    }

    /// Update the follow/mute state for this topic (from settings).
    pub fn set_topic_state(&mut self, followed: bool, muted: bool) {
        self.followed = followed;
        self.muted = muted;
    }

    /// Indices of entries currently visible after NSFW filtering.
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

    pub fn handle_key(&mut self, key: KeyEvent) -> TopicFeedIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return TopicFeedIntent::Quit;
        }
        if key.code == KeyCode::Backspace {
            return TopicFeedIntent::Back;
        }
        // Follow/mute the whole topic — available even while posts are loading.
        match key.code {
            KeyCode::Char('f') => {
                return TopicFeedIntent::ToggleFollow {
                    slug: self.slug.clone(),
                }
            }
            KeyCode::Char('m') => {
                return TopicFeedIntent::ToggleMute {
                    slug: self.slug.clone(),
                }
            }
            _ => {}
        }
        if self.list.loading {
            return TopicFeedIntent::None;
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
                return TopicFeedIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return TopicFeedIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        match key.code {
            KeyCode::Char('r') => {
                self.list.items.clear();
                self.list.next_cursor = None;
                self.list.selected = 0;
                self.list.loading = true;
                self.list.error = None;
                return TopicFeedIntent::Refresh;
            }
            KeyCode::Enter => {
                if let Some(idx) = visible.get(self.list.selected) {
                    if let Some(e) = self.list.items.get(*idx) {
                        return TopicFeedIntent::OpenSelected {
                            post_id: e.post_id.clone(),
                        };
                    }
                }
            }
            KeyCode::Char('p') => {
                let track = visible
                    .get(self.list.selected)
                    .and_then(|idx| self.list.items.get(*idx))
                    .and_then(|e| super::audio::jukebox_track(&e.attachments));
                return TopicFeedIntent::PlayJukebox(track);
            }
            KeyCode::Char('o') => {
                if let Some(url) = visible
                    .get(self.list.selected)
                    .and_then(|idx| self.list.items.get(*idx))
                    .and_then(|e| super::audio::jukebox_url(&e.attachments))
                {
                    return TopicFeedIntent::OpenJukebox(url);
                }
            }
            _ => {}
        }
        TopicFeedIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Entry>, Option<String>), String>) {
        self.list.apply_initial(result);
        if self.list.selected >= self.visible_indices().len() {
            self.list.selected = 0;
        }
    }

    pub fn apply_more(&mut self, result: Result<(Vec<Entry>, Option<String>), String>) {
        self.list.apply_more(result);
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let marks = match (self.followed, self.muted) {
            (true, true) => " ★ muted",
            (true, false) => " ★",
            (false, true) => " muted",
            (false, false) => "",
        };
        let title = format!(" cs-tui • #{}{marks} ", self.slug);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let visible = self.visible_indices();
        list::render_body(
            frame,
            layout[0],
            theme,
            &self.list,
            &visible,
            "no entries in this topic",
            |e| entry_item(e, theme),
        );

        let (status_text, status_style) = if let Some(msg) = list::load_more_error(&self.list) {
            (msg, theme.error_style())
        } else if self.list.loading {
            (
                "loading… · enter open · r refresh · esc back".to_string(),
                theme.muted_style(),
            )
        } else {
            let follow = if self.followed { "unfollow" } else { "follow" };
            let mute = if self.muted { "unmute" } else { "mute" };
            let more = if self.list.next_cursor.is_some() {
                "scroll for more · "
            } else {
                ""
            };
            // Surface the jukebox keys only when the highlighted post has a track.
            let media = if self.selected_entry().is_some_and(super::audio::has_audio) {
                " · p play · o open"
            } else {
                ""
            };
            (
                format!(
                    "{} entries · {more}enter open{media} · f {follow} · m {mute} · r refresh · esc back",
                    self.list.items.len()
                ),
                theme.muted_style(),
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status_text, status_style))),
            layout[1],
        );
    }
}

fn entry_item(entry: &Entry, theme: &Theme) -> ListItem<'static> {
    let when = entry
        .created_at
        .map(crate::config::format_list_timestamp)
        .unwrap_or_default();
    let counts = format!(
        " · {} replies · {} bookmarks",
        entry.replies_count, entry.bookmarks_count
    );
    let mut header_spans = vec![
        Span::styled(format!("@{}", entry.author_username), theme.accent_style()),
        Span::styled(format!(" · {when}{counts}"), theme.muted_style()),
    ];
    if super::images::has_image(entry) {
        header_spans.push(Span::styled(" · [image]", theme.accent_style()));
    }
    if super::audio::has_audio(entry) {
        header_spans.push(Span::styled(" · [jukebox]", theme.accent_style()));
    }
    let mut lines = vec![Line::from(header_spans)];
    let snippet =
        super::markdown::content_preview(&entry.content, crate::config::get().preview_length);
    if !snippet.is_empty() {
        lines.push(Line::from(Span::styled(snippet, theme.base())));
    }
    if !crate::config::get().compact {
        lines.push(Line::from(""));
    }
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
    fn f_and_m_toggle_the_topic_even_while_loading() {
        // new() starts loading; follow/mute must still work (they're topic-level).
        let mut s = TopicFeedScreen::new("music".into());
        assert!(s.list.loading);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('f'))),
            TopicFeedIntent::ToggleFollow {
                slug: "music".into()
            }
        );
        assert_eq!(
            s.handle_key(key(KeyCode::Char('m'))),
            TopicFeedIntent::ToggleMute {
                slug: "music".into()
            }
        );
    }

    #[test]
    fn p_plays_the_highlighted_entrys_jukebox() {
        let mut s = TopicFeedScreen::new("music".into());
        let mut e = entry("p1");
        e.attachments = vec![cs_api::Attachment::Audio {
            src: "https://youtu.be/abc".into(),
            origin: "youtube".into(),
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            genre: "electronic".into(),
        }];
        s.apply_initial(Ok((vec![e], None)));
        match s.handle_key(key(KeyCode::Char('p'))) {
            TopicFeedIntent::PlayJukebox(Some(t)) => {
                assert_eq!(t.url, "https://youtu.be/abc");
                assert_eq!(t.title, "Paranoimia");
            }
            other => panic!("expected PlayJukebox(Some), got {other:?}"),
        }
    }

    #[test]
    fn p_with_no_jukebox_yields_play_none() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('p'))),
            TopicFeedIntent::PlayJukebox(None)
        );
    }

    #[test]
    fn o_opens_the_highlighted_entrys_jukebox() {
        let mut s = TopicFeedScreen::new("music".into());
        let mut e = entry("p1");
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
            TopicFeedIntent::OpenJukebox("https://youtu.be/abc".into())
        );
    }

    #[test]
    fn followed_state_renders_a_star_in_the_header() {
        let mut s = TopicFeedScreen::new("music".into());
        s.set_topic_state(true, false);
        s.apply_initial(Ok((vec![], None)));
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(60, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| s.render(f, f.area(), &theme)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.contains('★'),
            "followed topic header should show a star"
        );
    }

    #[test]
    fn enter_opens_selected_post() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1"), entry("p2")], None)));
        s.list.selected = 1;
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
        assert_eq!(s.list.items.len(), 2);
        assert!(s.list.next_cursor.is_none());
    }

    #[test]
    fn j_at_bottom_auto_loads_next_page() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1"), entry("p2")], Some("next".into()))));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.list.selected, 1);
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, TopicFeedIntent::LoadMore);
        assert!(s.list.loading);
    }

    #[test]
    fn nsfw_entries_hidden_by_default() {
        let mut s = TopicFeedScreen::new("music".into());
        let mut nsfw = entry("p2");
        nsfw.is_nsfw = true;
        s.apply_initial(Ok((vec![entry("p1"), nsfw, entry("p3")], None)));
        // Default config nsfw=false → the NSFW entry is filtered out.
        assert_eq!(s.visible_indices(), vec![0, 2]);
    }

    #[test]
    fn enter_opens_visible_entry_skipping_nsfw() {
        let mut s = TopicFeedScreen::new("music".into());
        let mut nsfw = entry("p2");
        nsfw.is_nsfw = true;
        s.apply_initial(Ok((vec![nsfw, entry("p3")], None)));
        // selected=0 maps to the first VISIBLE entry (p3), not the hidden p2.
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            TopicFeedIntent::OpenSelected {
                post_id: "p3".into()
            }
        );
    }

    #[test]
    fn j_at_bottom_without_cursor_does_nothing() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, TopicFeedIntent::None);
        assert_eq!(s.list.selected, 0);
        assert!(!s.list.loading);
    }
}

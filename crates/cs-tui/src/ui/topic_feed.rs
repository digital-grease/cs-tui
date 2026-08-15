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

use super::flag::{render_flag_prompt, FlagPrompt, FlagPromptKey};
use super::list::{self, TabState};
use super::theme::Theme;

/// A flag-reason prompt on a topic-feed row: the reported entry's `post_id` is
/// all the target this screen needs.
type EntryFlagPrompt = FlagPrompt<String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicFeedIntent {
    /// Return to the topics index.
    Back,
    LoadMore,
    Refresh,
    OpenSelected {
        post_id: String,
    },
    /// Edit the selected entry (`e`). Carries the entry as the screen holds it
    /// so the shell can pre-fill an edit form without a re-fetch.
    ///
    /// Offered on every entry, not just the viewer's own: editing is
    /// supporter-only and expires 5 minutes after publishing (v0.8.4 § Edit
    /// Entry), neither of which the client can see, so the server's `403` is the
    /// only honest gate. Mirrors [`super::feed::FeedIntent::EditEntry`].
    EditEntry {
        post_id: String,
        content: String,
        title: Option<String>,
        topics: Vec<String>,
        is_public: bool,
        is_nsfw: bool,
    },
    /// Report the selected entry (`F`), with the reason typed into the inline
    /// prompt. `reason` is `None` on a blank submit, which v0.8.4 § Flag an
    /// Entry allows.
    FlagEntry {
        post_id: String,
        reason: Option<String>,
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
    /// The open flag-reason prompt (`F`), or `None` when nothing is being
    /// reported. While it's open it owns every key the screen sees, including
    /// the bare `f`/`m` topic toggles.
    pub flag_prompt: Option<EntryFlagPrompt>,
}

impl TopicFeedScreen {
    pub fn new(slug: String) -> Self {
        Self {
            slug,
            list: TabState::loading(),
            include_nsfw: crate::config::get().nsfw,
            followed: false,
            muted: false,
            flag_prompt: None,
        }
    }

    /// Update the follow/mute state for this topic (from settings).
    pub fn set_topic_state(&mut self, followed: bool, muted: bool) {
        self.followed = followed;
        self.muted = muted;
    }

    /// Whether a field is capturing text, so the shell's global single-letter
    /// shortcuts (section jumps, `i`, `S`, the player keys) must not swallow the
    /// keystroke. True only while the flag-reason prompt is open.
    #[must_use]
    pub fn is_text_input(&self) -> bool {
        self.flag_prompt.is_some()
    }

    /// Close an open flag-reason prompt without reporting anything, returning
    /// `true` when there was one. Lets the shell give Esc to the prompt before
    /// its usual "back" role, the same way the topics search box does.
    pub fn cancel_flag_prompt(&mut self) -> bool {
        self.flag_prompt.take().is_some()
    }

    /// Insert bracketed-paste text into the flag-reason prompt. A no-op when no
    /// prompt is open, since this screen captures no other text.
    pub fn paste_text(&mut self, text: &str) {
        if let Some(prompt) = self.flag_prompt.as_mut() {
            prompt.paste(text);
        }
    }

    /// Fold a freshly fetched copy of one entry into the loaded list, in place,
    /// after the user edited it. v0.8.4 § Edit Entry returns only the echoed
    /// `postId`, so the shell re-fetches the entry and hands it here rather than
    /// reloading the topic and losing the reader's place. Returns `true` when a
    /// matching entry was loaded.
    ///
    /// The selection is re-clamped afterwards because turning `is_nsfw` on can
    /// hide the row it was sitting on.
    pub fn apply_edited_entry(&mut self, fresh: &Entry) -> bool {
        let Some(item) = self
            .list
            .items
            .iter_mut()
            .find(|e| e.post_id == fresh.post_id)
        else {
            return false;
        };
        item.content = fresh.content.clone();
        item.title = fresh.title.clone();
        item.topics = fresh.topics.clone();
        item.attachments = fresh.attachments.clone();
        item.is_public = fresh.is_public;
        item.is_nsfw = fresh.is_nsfw;
        item.edited_at = fresh.edited_at;
        item.replies_count = fresh.replies_count;
        item.bookmarks_count = fresh.bookmarks_count;
        let view_len = self.visible_indices().len();
        if self.list.selected >= view_len {
            self.list.selected = view_len.saturating_sub(1);
        }
        true
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
        // An open flag prompt owns every other key, so a reason can contain the
        // letters this screen binds, including the bare `f`/`m` toggles below
        // which would otherwise follow or mute the topic mid-sentence.
        if let Some(outcome) = self.flag_prompt.as_mut().map(|p| p.handle_key(key)) {
            return match outcome {
                FlagPromptKey::Consumed => TopicFeedIntent::None,
                FlagPromptKey::Cancelled => {
                    self.flag_prompt = None;
                    TopicFeedIntent::None
                }
                FlagPromptKey::Submitted => match self.flag_prompt.take() {
                    Some(prompt) => {
                        let reason = prompt.reason_to_send();
                        TopicFeedIntent::FlagEntry {
                            post_id: prompt.target,
                            reason,
                        }
                    }
                    None => TopicFeedIntent::None,
                },
            };
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
            KeyCode::Char('e') => {
                if let Some(idx) = visible.get(self.list.selected) {
                    if let Some(e) = self.list.items.get(*idx) {
                        return TopicFeedIntent::EditEntry {
                            post_id: e.post_id.clone(),
                            content: e.content.clone(),
                            title: e.title.clone(),
                            topics: e.topics.clone(),
                            is_public: e.is_public,
                            is_nsfw: e.is_nsfw,
                        };
                    }
                }
            }
            KeyCode::Char('F') => {
                if let Some(idx) = visible.get(self.list.selected) {
                    if let Some(e) = self.list.items.get(*idx) {
                        self.flag_prompt = Some(FlagPrompt::new(e.post_id.clone()));
                    }
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

        // The flag prompt takes a second row at the bottom (hint + input) in
        // place of the status line, which it replaces while it's open.
        let bottom = if self.flag_prompt.is_some() { 2 } else { 1 };
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(bottom)])
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
                    "{} entries · {more}enter open{media} · e edit · F flag · f {follow} · m {mute} · r refresh · esc back",
                    self.list.items.len()
                ),
                theme.muted_style(),
            )
        };
        if let Some(prompt) = &self.flag_prompt {
            render_flag_prompt(frame, layout[1], theme, prompt);
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(status_text, status_style))),
                layout[1],
            );
        }
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
    // v0.8.4 § Edit Entry: an entry that has been edited carries `editedAt`.
    // Marked right after its timestamp, in the same muted metadata span, from
    // the shared helper every listing screen uses.
    let edited = super::text::edited_marker(entry.edited_at);
    let mut header_spans = vec![
        Span::styled(format!("@{}", entry.author_username), theme.accent_style()),
        Span::styled(format!(" · {when}{edited}{counts}"), theme.muted_style()),
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
            edited_at: None,
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

    /// A fixed `editedAt` stamp for the edit-marker tests.
    fn edited_stamp() -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid timestamp")
    }

    /// Flatten a rendered screen into one string of buffer symbols.
    fn render_to_string(s: &TopicFeedScreen, width: u16, height: u16) -> String {
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(width, height);
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
    fn an_edited_entry_is_marked_in_the_list() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        assert!(
            !render_to_string(&s, 80, 8).contains("(edited)"),
            "an untouched entry carries no marker"
        );

        let mut edited = entry("p1");
        edited.edited_at = Some(edited_stamp());
        s.apply_initial(Ok((vec![edited], None)));
        let text = render_to_string(&s, 80, 8);
        assert!(
            text.contains("(edited)"),
            "edited entry is marked: {text:?}"
        );
        assert!(text.contains("@alice"), "the rest of the header survives");
    }

    #[test]
    fn e_requests_an_edit_of_the_selected_entry() {
        let mut s = TopicFeedScreen::new("music".into());
        let mut second = entry("p2");
        second.title = Some("Headline".into());
        second.is_public = true;
        s.apply_initial(Ok((vec![entry("p1"), second], None)));
        s.list.selected = 1;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'))),
            TopicFeedIntent::EditEntry {
                post_id: "p2".into(),
                content: "entry p2".into(),
                title: Some("Headline".into()),
                topics: vec!["music".into()],
                is_public: true,
                is_nsfw: false,
            }
        );
    }

    #[test]
    fn capital_f_opens_the_flag_prompt_and_enter_files_the_report() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        assert_eq!(s.handle_key(key(KeyCode::Char('F'))), TopicFeedIntent::None);
        assert!(s.is_text_input(), "the open prompt captures text");
        for c in "spam".chars() {
            assert_eq!(s.handle_key(key(KeyCode::Char(c))), TopicFeedIntent::None);
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            TopicFeedIntent::FlagEntry {
                post_id: "p1".into(),
                reason: Some("spam".into()),
            }
        );
        assert!(!s.is_text_input(), "submitting closes the prompt");
    }

    #[test]
    fn an_empty_flag_reason_is_a_valid_report() {
        // v0.8.4 § Flag an Entry: `reason` is optional, so a blank submit files
        // the report with no reason rather than being rejected here.
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            TopicFeedIntent::FlagEntry {
                post_id: "p1".into(),
                reason: None,
            }
        );
    }

    #[test]
    fn the_flag_prompt_swallows_the_topic_toggles() {
        // `f` and `m` are topic-level toggles here, so a reason mentioning
        // "misinformation" must not follow or mute the topic as it is typed.
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        s.handle_key(key(KeyCode::Char('F')));
        for c in "fm".chars() {
            assert_eq!(s.handle_key(key(KeyCode::Char(c))), TopicFeedIntent::None);
        }
        assert_eq!(
            s.flag_prompt.as_ref().map(|p| p.reason.as_str()),
            Some("fm")
        );
    }

    #[test]
    fn the_flag_prompt_takes_backspace_before_the_back_key() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        s.handle_key(key(KeyCode::Char('F')));
        s.handle_key(key(KeyCode::Char('x')));
        assert_eq!(
            s.handle_key(key(KeyCode::Backspace)),
            TopicFeedIntent::None,
            "backspace edits the reason instead of leaving the topic"
        );
        assert_eq!(s.flag_prompt.as_ref().map(|p| p.reason.as_str()), Some(""));
    }

    #[test]
    fn esc_cancels_the_flag_prompt_and_the_shell_hook_reports_it() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(s.handle_key(key(KeyCode::Esc)), TopicFeedIntent::None);
        assert!(s.flag_prompt.is_none());

        assert!(!s.cancel_flag_prompt(), "nothing open, key not consumed");
        s.handle_key(key(KeyCode::Char('F')));
        assert!(s.cancel_flag_prompt(), "open prompt consumes the escape");
        assert!(!s.is_text_input());
    }

    #[test]
    fn paste_into_the_flag_prompt_collapses_newlines() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        s.paste_text("ignored"); // no prompt open yet
        assert!(s.flag_prompt.is_none());
        s.handle_key(key(KeyCode::Char('F')));
        s.paste_text("copy\npasted");
        assert_eq!(
            s.flag_prompt.as_ref().map(|p| p.reason.as_str()),
            Some("copy pasted")
        );
    }

    #[test]
    fn flag_prompt_replaces_the_status_line_while_open() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1")], None)));
        assert!(
            render_to_string(&s, 80, 8).contains("e edit"),
            "the new keys are advertised on the status line"
        );
        s.handle_key(key(KeyCode::Char('F')));
        let text = render_to_string(&s, 80, 8);
        assert!(text.contains("flag reason"), "prompt is drawn: {text:?}");
        assert!(text.contains("@alice"), "the list stays visible behind it");
    }

    #[test]
    fn apply_edited_entry_updates_in_place() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1"), entry("p2")], None)));
        let mut fresh = entry("p2");
        fresh.content = "rewritten".into();
        fresh.title = Some("Fixed".into());
        fresh.edited_at = Some(edited_stamp());

        assert!(s.apply_edited_entry(&fresh));
        assert_eq!(s.list.items[1].content, "rewritten");
        assert_eq!(s.list.items[1].title.as_deref(), Some("Fixed"));
        assert!(s.list.items[1].edited_at.is_some());
        assert_eq!(s.list.items[0].content, "entry p1", "others untouched");

        assert!(
            !s.apply_edited_entry(&entry("gone")),
            "an entry we never loaded reports a miss"
        );
    }

    #[test]
    fn apply_edited_entry_reclamps_a_selection_it_hides() {
        let mut s = TopicFeedScreen::new("music".into());
        s.apply_initial(Ok((vec![entry("p1"), entry("p2")], None)));
        s.include_nsfw = false;
        s.list.selected = 1; // sitting on "p2"
        let mut fresh = entry("p2");
        fresh.is_nsfw = true; // the row the cursor is on disappears
        assert!(s.apply_edited_entry(&fresh));
        assert_eq!(s.visible_indices(), vec![0]);
        assert_eq!(s.list.selected, 0, "selection clamps to the shrunken view");
    }
}

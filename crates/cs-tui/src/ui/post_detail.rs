//! Post detail screen — entry header + content + scrollable replies (oldest first).
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Entry, Reply};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;

use super::images::{entry_image_urls, reply_image_urls};
use super::markdown::{render_markdown, render_markdown_with, ImageUrls};
use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostDetailIntent {
    /// Return to the previous screen.
    Back,
    /// Exit the app.
    Quit,
    /// Load the next page of replies using the held cursor.
    LoadMoreReplies,
    /// Re-fetch the replies from scratch.
    RefreshReplies,
    /// Start composing a reply to this post (empty editor).
    Reply,
    /// Start a reply pre-filled with a quote of the post (`Q`).
    QuoteReply,
    /// Bookmark this post.
    Bookmark,
    /// Bookmark the selected reply.
    BookmarkReply {
        reply_id: String,
    },
    /// Open a URL (the jukebox link) in the user's default browser.
    OpenUrl(String),
    /// Play (or toggle) the focused jukebox track. `None` when there's none —
    /// the app then treats `p` as pause for whatever is already playing.
    PlayJukebox(Option<super::audio::JukeboxTrack>),
    /// User confirmed deletion of the entry.
    DeleteEntryConfirmed,
    None,
}

pub struct PostDetailScreen {
    pub entry: Entry,
    pub replies: Vec<Reply>,
    pub next_replies_cursor: Option<String>,
    pub loading_replies: bool,
    pub error: Option<String>,
    pub scroll: u16,
    /// Max scroll offset for the current content/viewport, recomputed each
    /// render (interior-mutable so `render(&self)` can record it). Scroll keys
    /// clamp to this so the body can't be scrolled off into empty space.
    pub max_scroll: Cell<u16>,
    /// Optional reply id to highlight (set when arriving from a reply notification).
    pub highlight_reply_id: Option<String>,
    /// Currently selected reply (index into `replies`), driven by `J`/`K`. `None`
    /// means the post itself is the focus. Selecting a reply lets `b` bookmark it.
    pub selected_reply: Option<usize>,
    /// Logical line index where each reply begins, recorded during `compose_body`.
    reply_starts: RefCell<Vec<usize>>,
    /// Wrapped-row scroll offset of each reply, derived each render so `J`/`K`
    /// can scroll the selected reply into view.
    reply_anchors: RefCell<Vec<u16>>,
    /// Two-step delete: first `d` arms confirmation; `y` confirms.
    pub confirming_delete: bool,
    /// The focused image, decoded into a terminal-graphics protocol once fetched
    /// (only on terminals that support images): the post's first image, or — when
    /// a reply with its own image is selected — that reply's first image.
    /// `RefCell` because the stateful image widget mutates while rendering, and
    /// render takes `&self`.
    pub image: RefCell<Option<StatefulProtocol>>,
    /// URL of the image currently in `image` (or the one being fetched for it).
    /// Lets the fetch result confirm the focus hasn't moved on before it displays,
    /// and lets selection changes detect when the strip needs a different image.
    image_url: RefCell<Option<String>>,
    /// Raw fetched image bytes by URL, so re-selecting a reply (or returning to
    /// the post) re-decodes from memory instead of re-fetching. Lives and dies
    /// with the screen, bounding it to one thread's images.
    image_bytes: RefCell<HashMap<String, Vec<u8>>>,
}

impl PostDetailScreen {
    pub fn new(entry: Entry) -> Self {
        Self {
            entry,
            replies: Vec::new(),
            next_replies_cursor: None,
            loading_replies: true,
            error: None,
            scroll: 0,
            max_scroll: Cell::new(0),
            highlight_reply_id: None,
            selected_reply: None,
            reply_starts: RefCell::new(Vec::new()),
            reply_anchors: RefCell::new(Vec::new()),
            confirming_delete: false,
            image: RefCell::new(None),
            image_url: RefCell::new(None),
            image_bytes: RefCell::new(HashMap::new()),
        }
    }

    /// Display a decoded image protocol for `url` in the top strip. Called on the
    /// UI thread after the image bytes are fetched and decoded.
    pub fn set_image(&self, url: String, protocol: StatefulProtocol) {
        *self.image_url.borrow_mut() = Some(url);
        *self.image.borrow_mut() = Some(protocol);
    }

    /// Mark `url` as the image we want shown, clearing any stale graphic so the
    /// strip blanks until the fetch/decode completes.
    pub fn set_pending_image(&self, url: String) {
        *self.image_url.borrow_mut() = Some(url);
        *self.image.borrow_mut() = None;
    }

    /// Drop the focused image entirely (the focus has no image).
    pub fn clear_image(&self) {
        *self.image_url.borrow_mut() = None;
        *self.image.borrow_mut() = None;
    }

    /// The URL currently displayed or awaited in the image strip.
    pub fn pending_url(&self) -> Option<String> {
        self.image_url.borrow().clone()
    }

    /// Remember fetched bytes for `url` so a later re-focus skips the network.
    pub fn cache_image_bytes(&self, url: String, bytes: Vec<u8>) {
        self.image_bytes.borrow_mut().insert(url, bytes);
    }

    /// Previously-fetched bytes for `url`, if any (cloned for decoding).
    pub fn cached_image_bytes(&self, url: &str) -> Option<Vec<u8>> {
        self.image_bytes.borrow().get(url).cloned()
    }

    /// The image that should occupy the top strip: the selected reply's first
    /// image when it has one, otherwise the post's first image (or, for a jukebox
    /// post, its cover art). Mirrors how `o`/`p` target the selected reply before
    /// falling back to the post.
    pub fn focused_image_url(&self) -> Option<String> {
        if let Some(reply) = self.selected_reply.and_then(|i| self.replies.get(i)) {
            if let Some(url) = reply_image_urls(reply).into_iter().next() {
                return Some(url);
            }
        }
        entry_image_urls(&self.entry)
            .into_iter()
            .next()
            .or_else(|| super::audio::entry_cover_art_url(&self.entry))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PostDetailIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return PostDetailIntent::Quit;
        }
        // While arming delete, only `y` confirms; anything else cancels the arming.
        if self.confirming_delete {
            self.confirming_delete = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                return PostDetailIntent::DeleteEntryConfirmed;
            }
            return PostDetailIntent::None;
        }
        match key.code {
            KeyCode::Backspace => PostDetailIntent::Back,
            KeyCode::Char('R') => PostDetailIntent::Reply,
            KeyCode::Char('Q') => PostDetailIntent::QuoteReply,
            // J/K move a reply selection (capitalized so j/k still scroll); the
            // selected reply scrolls into view via the recorded anchors.
            KeyCode::Char('J') if !self.replies.is_empty() => {
                let next = match self.selected_reply {
                    Some(i) => (i + 1).min(self.replies.len() - 1),
                    None => 0,
                };
                self.selected_reply = Some(next);
                self.scroll_to_reply(next);
                PostDetailIntent::None
            }
            KeyCode::Char('K') => {
                if let Some(i) = self.selected_reply {
                    let prev = i.saturating_sub(1);
                    self.selected_reply = Some(prev);
                    self.scroll_to_reply(prev);
                }
                PostDetailIntent::None
            }
            // `b` bookmarks the selected reply, or the post when none is selected.
            KeyCode::Char('b') => match self.selected_reply.and_then(|i| self.replies.get(i)) {
                Some(r) => PostDetailIntent::BookmarkReply {
                    reply_id: r.reply_id.clone(),
                },
                None => PostDetailIntent::Bookmark,
            },
            // `o` opens the jukebox link in the browser — the selected reply's
            // link when one is selected, otherwise the post's.
            KeyCode::Char('o') => match self.jukebox_url() {
                Some(url) => PostDetailIntent::OpenUrl(url),
                None => PostDetailIntent::None,
            },
            // `p` plays the focused jukebox track (selected reply's, else the
            // post's); the app toggles pause when it's already playing.
            KeyCode::Char('p') => PostDetailIntent::PlayJukebox(self.focused_track()),
            KeyCode::Char('d') => {
                if crate::config::get().confirm_deletes {
                    self.confirming_delete = true;
                    PostDetailIntent::None
                } else {
                    PostDetailIntent::DeleteEntryConfirmed
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                // At the bottom, scrolling down pulls the next page of replies
                // automatically rather than scrolling into empty space.
                if self.scroll >= self.max_scroll.get()
                    && self.next_replies_cursor.is_some()
                    && !self.loading_replies
                {
                    self.loading_replies = true;
                    return PostDetailIntent::LoadMoreReplies;
                }
                self.scroll = self.scroll.saturating_add(1).min(self.max_scroll.get());
                PostDetailIntent::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                PostDetailIntent::None
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.scroll = self.scroll.saturating_add(10).min(self.max_scroll.get());
                PostDetailIntent::None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                PostDetailIntent::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.scroll = 0;
                PostDetailIntent::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.scroll = self.max_scroll.get();
                PostDetailIntent::None
            }
            KeyCode::Char('n') if self.next_replies_cursor.is_some() => {
                self.loading_replies = true;
                PostDetailIntent::LoadMoreReplies
            }
            KeyCode::Char('r') => {
                self.replies.clear();
                self.next_replies_cursor = None;
                self.loading_replies = true;
                self.error = None;
                PostDetailIntent::RefreshReplies
            }
            _ => PostDetailIntent::None,
        }
    }

    pub fn apply_replies_initial(&mut self, result: Result<(Vec<Reply>, Option<String>), String>) {
        self.loading_replies = false;
        match result {
            Ok((replies, cursor)) => {
                self.replies = replies;
                self.next_replies_cursor = cursor;
                self.error = None;
                // The list changed out from under any selection.
                self.selected_reply = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    /// The jukebox link to open with `o`: the selected reply's when a reply is
    /// selected and carries one, otherwise the post's. Mirrors how `b` targets
    /// the selection before falling back to the post.
    fn jukebox_url(&self) -> Option<String> {
        if let Some(reply) = self.selected_reply.and_then(|i| self.replies.get(i)) {
            if let Some(url) = super::audio::jukebox_url(&reply.attachments) {
                return Some(url);
            }
        }
        super::audio::jukebox_url(&self.entry.attachments)
    }

    /// The jukebox track to play with `p`, same selection precedence as
    /// [`Self::jukebox_url`].
    fn focused_track(&self) -> Option<super::audio::JukeboxTrack> {
        if let Some(reply) = self.selected_reply.and_then(|i| self.replies.get(i)) {
            if let Some(track) = super::audio::jukebox_track(&reply.attachments) {
                return Some(track);
            }
        }
        super::audio::jukebox_track(&self.entry.attachments)
    }

    /// Scroll so reply `i` sits at the top of the viewport (best effort, using
    /// the anchors recorded at the last render).
    fn scroll_to_reply(&mut self, i: usize) {
        if let Some(&anchor) = self.reply_anchors.borrow().get(i) {
            self.scroll = anchor.min(self.max_scroll.get());
        }
    }

    pub fn apply_replies_more(&mut self, result: Result<(Vec<Reply>, Option<String>), String>) {
        self.loading_replies = false;
        match result {
            Ok((mut replies, cursor)) => {
                self.replies.append(&mut replies);
                self.next_replies_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, images_on: bool) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(
                format!(" post · @{} ", self.entry.author_username),
                theme.accent_style(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let body_area = layout[0];
        let status_area = layout[1];

        let lines = self.compose_body(theme);

        // When an image is loaded AND images are on, reserve a strip at the top
        // of the body for it and flow the text below. Terminals without graphics
        // never build a protocol, and the `i` toggle (`images_on`) forces
        // text-only too; either way the body uses the full area.
        let mut img = self.image.borrow_mut();
        let has_image = images_on && img.is_some() && body_area.height > 4;
        let img_h = if has_image {
            (body_area.height / 2).clamp(1, crate::config::get().image_height)
        } else {
            0
        };
        let text_area = Rect::new(
            body_area.x,
            body_area.y + img_h,
            body_area.width,
            body_area.height - img_h,
        );

        // Bound the scroll to the wrapped content height so the body can't be
        // scrolled off into empty space. Count wrapped rows per logical line
        // (ceil(line width / columns)); close enough to ratatui's word wrap to
        // keep `j`/`G` from running past the end.
        let cols = u32::from(text_area.width).max(1);
        // Single pass: total wrapped rows (for max_scroll) and, at each reply's
        // start line, the wrapped-row offset (so `J`/`K` can scroll it into view).
        let reply_starts = self.reply_starts.borrow();
        let mut anchors: Vec<u16> = Vec::with_capacity(reply_starts.len());
        let mut acc: u32 = 0;
        let mut si = 0;
        let row_count = |w: u32| if w <= cols { 1 } else { w.div_ceil(cols) + 1 };
        for (idx, l) in lines.iter().enumerate() {
            while si < reply_starts.len() && reply_starts[si] == idx {
                anchors.push(acc.min(u32::from(u16::MAX)) as u16);
                si += 1;
            }
            acc += row_count(l.width() as u32);
        }
        while si < reply_starts.len() {
            anchors.push(acc.min(u32::from(u16::MAX)) as u16);
            si += 1;
        }
        drop(reply_starts);
        *self.reply_anchors.borrow_mut() = anchors;
        let wrapped_rows = acc;
        let max_scroll = wrapped_rows
            .saturating_sub(u32::from(text_area.height))
            .min(u32::from(u16::MAX)) as u16;
        self.max_scroll.set(max_scroll);
        let scroll = self.scroll.min(max_scroll);

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        if has_image {
            if let Some(proto) = img.as_mut() {
                let img_area = Rect::new(body_area.x, body_area.y, body_area.width, img_h);
                frame.render_stateful_widget(
                    StatefulImage::<StatefulProtocol>::new(),
                    img_area,
                    proto,
                );
            }
        }
        frame.render_widget(para, text_area);

        // Surface the jukebox keys only when there's a track to act on.
        let open_hint = if self.jukebox_url().is_some() {
            " · p play · o open"
        } else {
            ""
        };
        let status_text = if self.confirming_delete {
            "really delete this post? y=yes, any other key=cancel".to_string()
        } else if self.loading_replies && self.replies.is_empty() {
            "loading replies… · esc back".to_string()
        } else if let Some(msg) = &self.error {
            format!("error: {msg} · esc back · r retry")
        } else if self.next_replies_cursor.is_some() {
            format!(
                "{} replies · scroll down for more · esc back · J/K select reply · R reply · Q quote · b bookmark{open_hint} · d delete · r refresh",
                self.replies.len()
            )
        } else {
            format!(
                "{} replies · end · esc back · J/K select reply · R reply · Q quote · b bookmark{open_hint} · d delete · r refresh",
                self.replies.len()
            )
        };
        let status = Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style())));
        frame.render_widget(status, status_area);
    }

    fn compose_body(&self, theme: &Theme) -> Vec<Line<'_>> {
        let mut lines = Vec::new();

        // Header
        let when = self
            .entry
            .created_at
            .map(crate::config::format_absolute)
            .unwrap_or_default();
        let topics = if self.entry.topics.is_empty() {
            String::new()
        } else {
            format!(" · #{}", self.entry.topics.join(" #"))
        };
        // v0.3.7: lead with the entry title (when set) as a headline above the
        // author/metadata line. Skipped for None/whitespace-only titles.
        if let Some(title) = self.entry.title.as_deref() {
            let title = title.trim();
            if !title.is_empty() {
                lines.push(Line::from(Span::styled(
                    title.to_string(),
                    theme.accent_style(),
                )));
            }
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("@{}", self.entry.author_username),
                theme.accent_style(),
            ),
            Span::styled(format!(" · {when}{topics}"), theme.muted_style()),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "{} replies · {} bookmarks{}",
                self.entry.replies_count,
                self.entry.bookmarks_count,
                if self.entry.is_nsfw { " · NSFW" } else { "" }
            ),
            theme.muted_style(),
        )));
        lines.push(Line::from(""));

        // Body — rendered with pulldown-cmark (markdown + @mention highlighting).
        // The post's image is drawn as graphics in the top strip, so its URL is
        // hidden here (links still surface their URL).
        for md_line in render_markdown_with(&self.entry.content, theme, ImageUrls::Hide) {
            lines.push(md_line);
        }

        // Jukebox (audio) attachment — usually a YouTube link. We can't stream it
        // inline, but keep the track card and link visible rather than dropping
        // the whole attachment with the rest of the non-text content.
        let audio = super::audio::audio_lines(&self.entry.attachments, theme);
        if !audio.is_empty() {
            lines.push(Line::from(""));
            lines.extend(audio);
        }

        // Replies separator
        if !self.replies.is_empty() || self.loading_replies {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── replies ───",
                theme.muted_style(),
            )));
            lines.push(Line::from(""));
        }

        // Replies
        let mut reply_starts = Vec::with_capacity(self.replies.len());
        for (i, reply) in self.replies.iter().enumerate() {
            reply_starts.push(lines.len());
            let highlight = self
                .highlight_reply_id
                .as_deref()
                .is_some_and(|id| id == reply.reply_id);
            let selected = self.selected_reply == Some(i);
            let style = if highlight {
                theme.accent_style()
            } else {
                theme.base()
            };
            let when = reply
                .created_at
                .map(crate::config::format_absolute)
                .unwrap_or_default();
            let parent = if reply.parent_reply_id.is_some() {
                " · ↪"
            } else {
                ""
            };
            // The selected reply's header is reverse-video so it stands out.
            let author_style = if selected {
                theme.accent_style().add_modifier(Modifier::REVERSED)
            } else {
                theme.accent_style()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("@{}", reply.author_username), author_style),
                Span::styled(format!(" · {when}{parent}"), theme.muted_style()),
            ]));
            // Reply body — markdown-rendered. Highlight overrides via the loop below.
            for md_line in render_markdown(&reply.content, theme) {
                if highlight {
                    let restyled: Vec<Span<'_>> = md_line
                        .spans
                        .iter()
                        .map(|s| Span::styled(s.content.to_string(), style))
                        .collect();
                    lines.push(Line::from(restyled));
                } else {
                    lines.push(md_line);
                }
            }
            // A jukebox link on the reply gets the same treatment as the post body.
            for audio_line in super::audio::audio_lines(&reply.attachments, theme) {
                lines.push(audio_line);
            }
            lines.push(Line::from(""));
        }

        if self.loading_replies && !self.replies.is_empty() {
            lines.push(Line::from(Span::styled(
                "loading more replies…",
                theme.accent_style(),
            )));
        }

        *self.reply_starts.borrow_mut() = reply_starts;
        lines
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
            content: "hello\nworld".into(),
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

    fn reply(reply_id: &str, post_id: &str) -> Reply {
        Reply {
            reply_id: reply_id.into(),
            post_id: post_id.into(),
            author_id: "b".into(),
            author_username: "bob".into(),
            content: format!("reply {reply_id}"),
            parent_reply_id: None,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    #[test]
    fn j_k_select_replies_and_b_bookmarks_the_selection() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1"), reply("r2", "p1")], None)));

        // No selection → b bookmarks the post.
        assert_eq!(
            s.handle_key(key(KeyCode::Char('b'))),
            PostDetailIntent::Bookmark
        );

        // J selects the first reply; b bookmarks it.
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(s.selected_reply, Some(0));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('b'))),
            PostDetailIntent::BookmarkReply {
                reply_id: "r1".into()
            }
        );

        // J advances, K retreats; selection clamps at the ends.
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(s.selected_reply, Some(1));
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(s.selected_reply, Some(1), "stays on the last reply");
        s.handle_key(key(KeyCode::Char('K')));
        assert_eq!(s.selected_reply, Some(0));

        // A fresh reply page clears the selection.
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], None)));
        assert_eq!(s.selected_reply, None);
    }

    fn body_text(s: &PostDetailScreen) -> Vec<String> {
        let theme = Theme::cyber();
        s.compose_body(&theme)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn image_attachment(src: &str) -> cs_api::Attachment {
        cs_api::Attachment::Image {
            src: src.into(),
            width: 0,
            height: 0,
        }
    }

    #[test]
    fn focused_image_url_tracks_reply_selection() {
        let mut e = entry("p1");
        e.attachments = vec![image_attachment("https://x/post.png")];
        let r0 = reply("r0", "p1"); // no image
        let mut r1 = reply("r1", "p1");
        r1.attachments = vec![image_attachment("https://x/reply.png")];
        let mut s = PostDetailScreen::new(e);
        s.apply_replies_initial(Ok((vec![r0, r1], None)));

        // No selection → the post's image.
        assert_eq!(s.focused_image_url().as_deref(), Some("https://x/post.png"));
        // A selected reply WITHOUT an image falls back to the post's image.
        s.selected_reply = Some(0);
        assert_eq!(s.focused_image_url().as_deref(), Some("https://x/post.png"));
        // A selected reply WITH an image focuses that image.
        s.selected_reply = Some(1);
        assert_eq!(
            s.focused_image_url().as_deref(),
            Some("https://x/reply.png")
        );
    }

    #[test]
    fn reply_body_surfaces_image_url_as_clickable_text() {
        let mut s = PostDetailScreen::new(entry("p1"));
        let mut r = reply("r1", "p1");
        r.content = "look ![a cat](https://x/cat.png)".into();
        s.apply_replies_initial(Ok((vec![r], None)));
        let body = body_text(&s).join("\n");
        assert!(body.contains("[image: a cat]"), "alt tag shown: {body:?}");
        assert!(
            body.contains("https://x/cat.png"),
            "reply image url surfaced as text: {body:?}"
        );
    }

    #[test]
    fn post_body_hides_image_url_since_it_is_drawn_as_graphics() {
        let mut e = entry("p1");
        e.content = "hero ![a cat](https://x/cat.png)".into();
        let body = body_text(&PostDetailScreen::new(e)).join("\n");
        assert!(body.contains("[image: a cat]"), "alt tag shown: {body:?}");
        assert!(
            !body.contains("https://x/cat.png"),
            "post image url hidden (the image is drawn): {body:?}"
        );
    }

    #[test]
    fn pending_image_url_tracks_latest_focus_so_stale_fetches_are_ignored() {
        // The ImageFetched race guard displays a fetched image only when its URL
        // still matches pending_url(). This verifies pending_url() reflects the
        // LATEST focus, so a late-arriving fetch for a superseded URL won't match.
        let s = PostDetailScreen::new(entry("p1"));
        assert_eq!(s.pending_url(), None, "nothing awaited initially");
        s.set_pending_image("https://x/a.png".into());
        assert_eq!(s.pending_url().as_deref(), Some("https://x/a.png"));
        // Focus moves on (e.g. user selects another reply) before A's fetch lands.
        s.set_pending_image("https://x/b.png".into());
        assert_eq!(s.pending_url().as_deref(), Some("https://x/b.png"));
        assert_ne!(
            s.pending_url().as_deref(),
            Some("https://x/a.png"),
            "a stale fetch for A no longer matches the awaited URL"
        );
        s.clear_image();
        assert_eq!(s.pending_url(), None, "clear drops the awaited URL");
    }

    #[test]
    fn image_bytes_cache_round_trips_and_misses_unknown_urls() {
        // Backs the reconcile cache-hit path: re-focusing a reply re-decodes from
        // memory instead of re-fetching.
        let s = PostDetailScreen::new(entry("p1"));
        assert!(s.cached_image_bytes("https://x/a.png").is_none());
        s.cache_image_bytes("https://x/a.png".into(), vec![1, 2, 3]);
        assert_eq!(
            s.cached_image_bytes("https://x/a.png").as_deref(),
            Some(&[1u8, 2, 3][..])
        );
        assert!(
            s.cached_image_bytes("https://x/other.png").is_none(),
            "unknown url misses the cache"
        );
    }

    #[test]
    fn compose_body_leads_with_title_when_present() {
        let mut e = entry("p1");
        e.title = Some("Headline Here".into());
        let lines = body_text(&PostDetailScreen::new(e));
        assert_eq!(lines[0], "Headline Here", "title should be the first line");
    }

    #[test]
    fn compose_body_renders_jukebox_link_and_metadata() {
        let mut e = entry("p1");
        e.attachments = vec![cs_api::Attachment::Audio {
            src: "https://www.youtube.com/watch?v=abc".into(),
            origin: "youtube".into(),
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            genre: "electronic".into(),
        }];
        let lines = body_text(&PostDetailScreen::new(e)).join("\n");
        assert!(lines.contains("♪ Paranoimia"), "track title: {lines:?}");
        assert!(lines.contains("Art of Noise"), "artist: {lines:?}");
        assert!(
            lines.contains("https://www.youtube.com/watch?v=abc"),
            "the jukebox link must be retained in the post body: {lines:?}"
        );
    }

    #[test]
    fn compose_body_omits_title_when_none() {
        let lines = body_text(&PostDetailScreen::new(entry("p1"))); // title: None
        assert!(
            lines[0].starts_with("@alice"),
            "without a title the first line is the author header, got {:?}",
            lines[0]
        );
    }

    #[test]
    fn new_starts_loading_replies() {
        let s = PostDetailScreen::new(entry("p1"));
        assert!(s.loading_replies);
        assert!(s.replies.is_empty());
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn backspace_emits_back() {
        let mut s = PostDetailScreen::new(entry("p1"));
        assert_eq!(
            s.handle_key(key(KeyCode::Backspace)),
            PostDetailIntent::Back
        );
    }

    #[test]
    fn r_plain_reply_and_q_quote_reply_are_distinct() {
        let mut s = PostDetailScreen::new(entry("p1"));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('R'))),
            PostDetailIntent::Reply
        );
        assert_eq!(
            s.handle_key(key(KeyCode::Char('Q'))),
            PostDetailIntent::QuoteReply
        );
    }

    fn jukebox(src: &str) -> cs_api::Attachment {
        cs_api::Attachment::Audio {
            src: src.into(),
            origin: "youtube".into(),
            artist: "Art of Noise".into(),
            title: "Paranoimia".into(),
            genre: "electronic".into(),
        }
    }

    #[test]
    fn o_opens_the_post_jukebox_link() {
        let mut e = entry("p1");
        e.attachments = vec![jukebox("https://youtu.be/abc")];
        let mut s = PostDetailScreen::new(e);
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            PostDetailIntent::OpenUrl("https://youtu.be/abc".into())
        );
    }

    #[test]
    fn o_is_a_noop_without_a_jukebox_link() {
        let mut s = PostDetailScreen::new(entry("p1")); // no attachments
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            PostDetailIntent::None
        );
    }

    #[test]
    fn o_prefers_the_selected_replys_jukebox_link() {
        let mut e = entry("p1");
        e.attachments = vec![jukebox("https://youtu.be/post")];
        let mut s = PostDetailScreen::new(e);
        let mut r = reply("r1", "p1");
        r.attachments = vec![jukebox("https://youtu.be/reply")];
        s.apply_replies_initial(Ok((vec![r], None)));

        // No selection → opens the post's link.
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            PostDetailIntent::OpenUrl("https://youtu.be/post".into())
        );
        // Select the reply → opens the reply's link instead.
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('o'))),
            PostDetailIntent::OpenUrl("https://youtu.be/reply".into())
        );
    }

    #[test]
    fn p_plays_the_post_jukebox_track() {
        let mut e = entry("p1");
        e.attachments = vec![jukebox("https://youtu.be/abc")];
        let mut s = PostDetailScreen::new(e);
        match s.handle_key(key(KeyCode::Char('p'))) {
            PostDetailIntent::PlayJukebox(Some(t)) => {
                assert_eq!(t.url, "https://youtu.be/abc");
                assert_eq!(t.title, "Paranoimia");
            }
            other => panic!("expected PlayJukebox(Some), got {other:?}"),
        }
    }

    #[test]
    fn p_without_a_jukebox_yields_play_none() {
        let mut s = PostDetailScreen::new(entry("p1")); // no attachments
        assert_eq!(
            s.handle_key(key(KeyCode::Char('p'))),
            PostDetailIntent::PlayJukebox(None)
        );
    }

    #[test]
    fn b_emits_bookmark() {
        let mut s = PostDetailScreen::new(entry("p1"));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('b'))),
            PostDetailIntent::Bookmark
        );
    }

    #[test]
    fn j_and_k_adjust_scroll_bounded() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.max_scroll.set(100); // normally set by render
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.scroll, 2);
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.scroll, 1);
        s.handle_key(key(KeyCode::Char('k')));
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn j_does_not_scroll_past_max() {
        // The infinite-downward-scroll bug: scroll must clamp to max_scroll.
        let mut s = PostDetailScreen::new(entry("p1"));
        s.max_scroll.set(3);
        for _ in 0..10 {
            s.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(s.scroll, 3, "scroll must not run past the content");
    }

    #[test]
    fn g_jumps_to_top_and_capital_g_to_bottom() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.max_scroll.set(42);
        s.scroll = 20;
        s.handle_key(key(KeyCode::Char('g')));
        assert_eq!(s.scroll, 0);
        s.handle_key(key(KeyCode::Char('G')));
        assert_eq!(s.scroll, 42, "G jumps to the bottom of the content");
    }

    #[test]
    fn j_at_bottom_with_more_replies_loads_them() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.loading_replies = false;
        s.next_replies_cursor = Some("c".into());
        s.max_scroll.set(0); // content fits; already at the bottom
        let intent = s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(intent, PostDetailIntent::LoadMoreReplies);
        assert!(s.loading_replies);
    }

    #[test]
    fn n_requests_more_only_with_cursor() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.loading_replies = false;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('n'))),
            PostDetailIntent::None
        );

        s.next_replies_cursor = Some("c".into());
        assert_eq!(
            s.handle_key(key(KeyCode::Char('n'))),
            PostDetailIntent::LoadMoreReplies
        );
        assert!(s.loading_replies);
    }

    #[test]
    fn r_resets_and_requests_refresh() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.replies = vec![reply("r1", "p1")];
        s.next_replies_cursor = Some("c".into());
        s.loading_replies = false;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('r'))),
            PostDetailIntent::RefreshReplies
        );
        assert!(s.replies.is_empty());
        assert!(s.next_replies_cursor.is_none());
        assert!(s.loading_replies);
    }

    #[test]
    fn apply_replies_initial_populates() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], Some("cur".into()))));
        assert!(!s.loading_replies);
        assert_eq!(s.replies.len(), 1);
        assert_eq!(s.next_replies_cursor.as_deref(), Some("cur"));
    }

    #[test]
    fn apply_replies_initial_error_sets_error() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Err("boom".into()));
        assert_eq!(s.error.as_deref(), Some("boom"));
        assert!(!s.loading_replies);
    }

    #[test]
    fn apply_replies_more_appends() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], Some("c".into()))));
        s.apply_replies_more(Ok((vec![reply("r2", "p1")], None)));
        assert_eq!(s.replies.len(), 2);
        assert!(s.next_replies_cursor.is_none());
    }

    #[test]
    fn compose_body_includes_separator_when_replies_present() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], None)));
        let lines = s.compose_body(&Theme::dark());
        // Look for the replies separator marker.
        let body_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.as_ref()))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(body_text.contains("replies"));
        assert!(body_text.contains("@bob"));
    }
}

//! Post detail screen — entry header + content + scrollable replies (oldest first).
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Entry, Reply};
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

use super::images::{entry_image_urls, reply_image_urls};
use super::markdown::{render_markdown_collect, ImageUrls};
use super::theme::Theme;

/// An inline image reserved in the post-detail body: its source URL and the
/// logical line index where its blank-row gap begins. `render` overlays the
/// graphic onto the gap, clipped (not resized) against the viewport edges so it
/// scrolls like the rest of the body.
struct ImageSlot {
    url: String,
    start_line: usize,
}

/// A surfaced link/image URL reserved in the post-detail body: its target `url`,
/// the logical `start_line` carrying the bare URL text, and the `col` that text
/// begins at. `render` overlays an OSC 8 hyperlink onto that row so the link is
/// clickable even when the URL is long enough to wrap, which defeats a terminal's
/// own URL detection. Recomputed every frame, like [`ImageSlot`].
struct LinkSlot {
    url: String,
    start_line: usize,
    col: u16,
}

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
    /// Fixed-size, render-ready image protocols by URL, paired with the
    /// (width, height) cell box they were encoded for so a terminal resize forces
    /// a rebuild. Built lazily from `image_bytes` the first time an image scrolls
    /// into view, then reused every frame (so scrolling doesn't re-encode). The
    /// fixed size lets the static `Image` widget clip — rather than resize — the
    /// image at the viewport edge. Only populated on graphics-capable terminals.
    protocols: RefCell<HashMap<String, (Protocol, Size)>>,
    /// Raw fetched image bytes by URL. Filled by the background fetch event;
    /// decoded into `protocols` on demand. Lives and dies with the screen.
    image_bytes: RefCell<HashMap<String, Vec<u8>>>,
    /// Image URLs already requested from the network, so the fetch driver doesn't
    /// re-spawn a fetch for one already in flight or cached.
    requested: RefCell<HashSet<String>>,
    /// Inline image placeholders recorded by `compose_body` (URL + the logical
    /// line where its reserved blank-row gap starts), consumed by `render` to
    /// overlay each image onto its gap. Recomputed every frame.
    image_slots: RefCell<Vec<ImageSlot>>,
    /// Surfaced link/image URLs recorded by `compose_body`, consumed by `render`
    /// to overlay an OSC 8 hyperlink onto each URL row. Recomputed every frame.
    link_slots: RefCell<Vec<LinkSlot>>,
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
            protocols: RefCell::new(HashMap::new()),
            image_bytes: RefCell::new(HashMap::new()),
            requested: RefCell::new(HashSet::new()),
            image_slots: RefCell::new(Vec::new()),
            link_slots: RefCell::new(Vec::new()),
        }
    }

    /// Remember fetched bytes for `url`. Drops any stale decoded protocol so the
    /// next render rebuilds it from the fresh bytes.
    pub fn cache_image_bytes(&self, url: String, bytes: Vec<u8>) {
        self.protocols.borrow_mut().remove(&url);
        self.image_bytes.borrow_mut().insert(url, bytes);
    }

    /// Whether `url`'s bytes are already cached (so no fetch is needed).
    pub fn has_image_bytes(&self, url: &str) -> bool {
        self.image_bytes.borrow().contains_key(url)
    }

    /// Record that `url` has been requested from the network; returns `true` only
    /// the first time, so the caller spawns exactly one fetch per URL.
    pub fn mark_requested(&self, url: String) -> bool {
        self.requested.borrow_mut().insert(url)
    }

    /// The post's own inline image: its first markdown/attachment image, or — for
    /// a jukebox post that carries no image — the track's cover-art thumbnail.
    fn post_image_url(&self) -> Option<String> {
        entry_image_urls(&self.entry)
            .into_iter()
            .next()
            .or_else(|| super::audio::entry_cover_art_url(&self.entry))
    }

    /// Every image URL the post detail can show — the post's, then each reply's
    /// first image — in body order. Drives the fetch loop in `App`.
    pub fn all_image_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        if let Some(u) = self.post_image_url() {
            urls.push(u);
        }
        for reply in &self.replies {
            if let Some(u) = reply_image_urls(reply).into_iter().next() {
                urls.push(u);
            }
        }
        urls
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

    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        images_on: bool,
        picker: Option<&Picker>,
    ) {
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

        // Images are drawn inline in the body flow, each into a reserved blank-row
        // gap, on graphics-capable terminals with images enabled and enough room.
        // Otherwise the body is plain text and `compose_body` surfaces the image
        // URL instead. Each gap is capped at half the pane so text stays visible.
        let inline_images = images_on && picker.is_some() && body_area.height > 4;
        let img_rows: u16 = if inline_images {
            crate::config::get()
                .image_height
                .min(body_area.height / 2)
                .max(1)
        } else {
            0
        };

        // OSC 8 hyperlinks make surfaced URLs clickable even when long enough to
        // wrap (which defeats the terminal's own URL detection); off falls back to
        // the bare URL text. Independent of graphics support.
        let hyperlinks_on = crate::config::get().hyperlinks;
        let lines = self.compose_body(theme, inline_images, img_rows, hyperlinks_on);

        // The whole body is the text area; images overlay the blank gaps within it.
        let text_area = body_area;

        // Bound the scroll to the wrapped content height so the body can't be
        // scrolled off into empty space. Count wrapped rows per logical line
        // (ceil(line width / columns)); close enough to ratatui's word wrap to
        // keep `j`/`G` from running past the end.
        let cols = u32::from(text_area.width).max(1);
        // Single pass: total wrapped rows (for max_scroll), each reply's start
        // offset (so `J`/`K` can scroll it into view), and each image gap's start
        // offset (so it can be overlaid at the right screen row).
        let reply_starts = self.reply_starts.borrow();
        let slots = self.image_slots.borrow();
        let link_slots = self.link_slots.borrow();
        let mut anchors: Vec<u16> = Vec::with_capacity(reply_starts.len());
        let mut slot_offsets: Vec<u32> = Vec::with_capacity(slots.len());
        // Wrapped-row offset of each link slot's URL row (its first row, where the
        // URL glyphs begin), so the overlay lands on the right screen row.
        let mut link_offsets: Vec<u32> = Vec::with_capacity(link_slots.len());
        let mut acc: u32 = 0;
        let mut si = 0;
        let mut sj = 0;
        let mut sk = 0;
        let row_count = |w: u32| if w <= cols { 1 } else { w.div_ceil(cols) + 1 };
        for (idx, l) in lines.iter().enumerate() {
            while si < reply_starts.len() && reply_starts[si] == idx {
                anchors.push(acc.min(u32::from(u16::MAX)) as u16);
                si += 1;
            }
            while sj < slots.len() && slots[sj].start_line == idx {
                slot_offsets.push(acc);
                sj += 1;
            }
            while sk < link_slots.len() && link_slots[sk].start_line == idx {
                link_offsets.push(acc);
                sk += 1;
            }
            acc += row_count(l.width() as u32);
        }
        while si < reply_starts.len() {
            anchors.push(acc.min(u32::from(u16::MAX)) as u16);
            si += 1;
        }
        while sj < slots.len() {
            slot_offsets.push(acc);
            sj += 1;
        }
        while sk < link_slots.len() {
            link_offsets.push(acc);
            sk += 1;
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
        frame.render_widget(para, text_area);

        // Overlay each inline image onto its reserved gap. The protocol is encoded
        // once at the full gap size, so the image is always drawn full-size; when
        // only part of its gap is on screen the static `Image` widget *clips* it
        // (not resizes it) against the viewport's bottom edge, so it scrolls in
        // smoothly like the surrounding text. Decode/encode lazily — the first
        // time an image scrolls into view — and rebuild only if the layout (and so
        // the target size) changed.
        if let Some(picker) = picker.filter(|_| inline_images) {
            let target = Size::new(text_area.width, img_rows);
            let mut protocols = self.protocols.borrow_mut();
            let bytes = self.image_bytes.borrow();
            for (slot, &offset) in slots.iter().zip(slot_offsets.iter()) {
                let rel = offset as i64 - i64::from(scroll);
                // Skip if the gap top is above the viewport or at/below its bottom.
                if rel < 0 || rel >= i64::from(text_area.height) {
                    continue;
                }
                let rel = rel as u16;
                let visible_rows = img_rows.min(text_area.height - rel);
                let stale = match protocols.get(&slot.url) {
                    Some((_, built)) => *built != target,
                    None => true,
                };
                if stale {
                    let Some(raw) = bytes.get(&slot.url) else {
                        continue;
                    };
                    let proto = image::load_from_memory(raw)
                        .map_err(|e| e.to_string())
                        .and_then(|img| {
                            picker
                                .new_protocol(img, target, Resize::Fit(None))
                                .map_err(|e| e.to_string())
                        });
                    match proto {
                        Ok(proto) => {
                            protocols.insert(slot.url.clone(), (proto, target));
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, url = %slot.url, "image encode failed");
                            continue;
                        }
                    }
                }
                if let Some((proto, _)) = protocols.get(&slot.url) {
                    let img_area = Rect::new(
                        text_area.x,
                        text_area.y + rel,
                        text_area.width,
                        visible_rows,
                    );
                    frame.render_widget(Image::new(proto).allow_clipping(true), img_area);
                }
            }
        }
        drop(slots);

        // Overlay an OSC 8 hyperlink onto each surfaced URL row that's in view.
        // The paragraph already painted the bare URL glyphs; `linkify_run` wraps
        // exactly those cells so the URL is clickable, including when it's long
        // enough to wrap (the link covers the first row's glyphs, with the full
        // URL as the target). Independent of graphics — works on any terminal.
        if hyperlinks_on {
            let buf = frame.buffer_mut();
            for (slot, &offset) in link_slots.iter().zip(link_offsets.iter()) {
                let rel = offset as i64 - i64::from(scroll);
                if rel < 0 || rel >= i64::from(text_area.height) {
                    continue;
                }
                if slot.col >= text_area.width {
                    continue;
                }
                let x = text_area.x + slot.col;
                let max_cols = text_area.width - slot.col;
                super::hyperlink::linkify_run(
                    buf,
                    x,
                    text_area.y + rel as u16,
                    &slot.url,
                    max_cols,
                );
            }
        }
        drop(link_slots);

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

    /// Build the scrollable body. When `inline_images` is set, image URLs are
    /// suppressed (the image is drawn as graphics) and a blank-row gap of
    /// `img_rows` is reserved at each image's position for `render` to overlay;
    /// otherwise image URLs are surfaced as text and no gaps are reserved. When
    /// `hyperlinks` is set, each surfaced URL is recorded as a [`LinkSlot`] so
    /// `render` can overlay an OSC 8 hyperlink onto its row.
    fn compose_body(
        &self,
        theme: &Theme,
        inline_images: bool,
        img_rows: u16,
        hyperlinks: bool,
    ) -> Vec<Line<'_>> {
        let mut lines = Vec::new();
        let mut slots: Vec<ImageSlot> = Vec::new();
        let mut link_slots: Vec<LinkSlot> = Vec::new();
        let image_urls = if inline_images {
            ImageUrls::Hide
        } else {
            ImageUrls::Show
        };
        // Fold a markdown block's links (positions relative to that block) into
        // absolute `LinkSlot`s anchored at `base`, the block's first line index.
        let mut push_links = |base: usize, refs: Vec<super::markdown::LinkRef>| {
            if hyperlinks {
                for r in refs {
                    link_slots.push(LinkSlot {
                        url: r.url,
                        start_line: base + r.line,
                        col: r.col,
                    });
                }
            }
        };

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
        // When images are drawn inline, the image URL is hidden here (the graphic
        // appears in the reserved gap below); otherwise it's surfaced as text.
        // Links always surface their URL.
        let base = lines.len();
        let (md_lines, md_links) = render_markdown_collect(&self.entry.content, theme, image_urls);
        lines.extend(md_lines);
        push_links(base, md_links);

        // The post's own image (or, for a jukebox post, its cover art) drawn
        // inline right after the body it belongs to.
        if inline_images {
            if let Some(url) = self.post_image_url() {
                slots.push(ImageSlot {
                    url,
                    start_line: lines.len(),
                });
                for _ in 0..img_rows {
                    lines.push(Line::from(""));
                }
            }
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
            let base = lines.len();
            let (md_lines, md_links) = render_markdown_collect(&reply.content, theme, image_urls);
            for md_line in md_lines {
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
            push_links(base, md_links);
            // The reply's own image, drawn inline right after its text.
            if inline_images {
                if let Some(url) = reply_image_urls(reply).into_iter().next() {
                    slots.push(ImageSlot {
                        url,
                        start_line: lines.len(),
                    });
                    for _ in 0..img_rows {
                        lines.push(Line::from(""));
                    }
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
        *self.image_slots.borrow_mut() = slots;
        *self.link_slots.borrow_mut() = link_slots;
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

    /// Flatten the body into per-line strings. `inline_images` mirrors the render
    /// path: when set, image URLs are suppressed (the graphic is drawn in a gap);
    /// when clear, image URLs are surfaced as text.
    fn body_text_mode(s: &PostDetailScreen, inline_images: bool) -> Vec<String> {
        let theme = Theme::cyber();
        let img_rows = if inline_images { 6 } else { 0 };
        s.compose_body(&theme, inline_images, img_rows, false)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// Text-mode body (no inline graphics) — the common case for the assertions
    /// here that only inspect text.
    fn body_text(s: &PostDetailScreen) -> Vec<String> {
        body_text_mode(s, false)
    }

    fn image_attachment(src: &str) -> cs_api::Attachment {
        cs_api::Attachment::Image {
            src: src.into(),
            width: 0,
            height: 0,
        }
    }

    #[test]
    fn all_image_urls_lists_post_then_each_replys_first_image() {
        let mut e = entry("p1");
        e.attachments = vec![image_attachment("https://x/post.png")];
        let r0 = reply("r0", "p1"); // no image
        let mut r1 = reply("r1", "p1");
        r1.attachments = vec![image_attachment("https://x/reply.png")];
        let mut s = PostDetailScreen::new(e);
        s.apply_replies_initial(Ok((vec![r0, r1], None)));

        // Post image first, then each reply that has one — in body order. The
        // image-less reply contributes nothing.
        assert_eq!(
            s.all_image_urls(),
            vec!["https://x/post.png", "https://x/reply.png"]
        );
    }

    #[test]
    fn reply_image_renders_inline_so_its_url_is_hidden() {
        // With graphics on, the reply's image is drawn in a reserved gap, so its
        // URL is suppressed; a blank-row gap is reserved for the overlay.
        let mut s = PostDetailScreen::new(entry("p1"));
        let mut r = reply("r1", "p1");
        r.content = "look ![a cat](https://x/cat.png)".into();
        s.apply_replies_initial(Ok((vec![r], None)));
        let body = body_text_mode(&s, true).join("\n");
        assert!(body.contains("[image: a cat]"), "alt tag shown: {body:?}");
        assert!(
            !body.contains("https://x/cat.png"),
            "image drawn inline, url hidden: {body:?}"
        );
        // A slot was recorded for the inline image overlay.
        assert_eq!(s.image_slots.borrow().len(), 1, "one inline image reserved");
    }

    #[test]
    fn reply_image_url_is_surfaced_when_graphics_are_off() {
        // No graphics (terminal can't, or `i` toggled off): the image isn't drawn,
        // so its URL is surfaced as text instead and no gap is reserved.
        let mut s = PostDetailScreen::new(entry("p1"));
        let mut r = reply("r1", "p1");
        r.content = "look ![a cat](https://x/cat.png)".into();
        s.apply_replies_initial(Ok((vec![r], None)));
        let body = body_text_mode(&s, false).join("\n");
        assert!(body.contains("[image: a cat]"), "alt tag shown: {body:?}");
        assert!(
            body.contains("https://x/cat.png"),
            "image url surfaced as text: {body:?}"
        );
        assert!(s.image_slots.borrow().is_empty(), "no gap reserved");
    }

    #[test]
    fn post_image_url_is_hidden_when_drawn_inline() {
        let mut e = entry("p1");
        e.content = "hero ![a cat](https://x/cat.png)".into();
        let s = PostDetailScreen::new(e);
        let body = body_text_mode(&s, true).join("\n");
        assert!(body.contains("[image: a cat]"), "alt tag shown: {body:?}");
        assert!(
            !body.contains("https://x/cat.png"),
            "post image url hidden (the image is drawn): {body:?}"
        );
    }

    #[test]
    fn image_bytes_cache_and_request_dedup() {
        let s = PostDetailScreen::new(entry("p1"));
        assert!(!s.has_image_bytes("https://x/a.png"));
        s.cache_image_bytes("https://x/a.png".into(), vec![1, 2, 3]);
        assert!(s.has_image_bytes("https://x/a.png"));
        assert!(!s.has_image_bytes("https://x/other.png"));

        // mark_requested returns true only the first time, so exactly one fetch
        // is spawned per URL.
        assert!(s.mark_requested("https://x/a.png".into()), "first request");
        assert!(
            !s.mark_requested("https://x/a.png".into()),
            "second request is a no-op"
        );
    }

    /// A tiny valid PNG so `render`'s lazy decode succeeds.
    fn tiny_png() -> Vec<u8> {
        let buf = image::ImageBuffer::from_pixel(2, 2, image::Rgba([10u8, 20, 30, 255]));
        let mut cur = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut cur, image::ImageFormat::Png)
            .expect("encode png");
        cur.into_inner()
    }

    #[test]
    fn inline_image_is_decoded_and_overlaid_once_in_view() {
        let mut s = PostDetailScreen::new(entry("p1"));
        let mut r = reply("r1", "p1");
        r.attachments = vec![image_attachment("https://x/cat.png")];
        s.apply_replies_initial(Ok((vec![r], None)));
        s.cache_image_bytes("https://x/cat.png".into(), tiny_png());

        let picker = Picker::halfblocks();
        let backend = ratatui::backend::TestBackend::new(40, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber(), true, Some(&picker)))
            .expect("draw");

        // The reply's gap is near the top and fully in view, so render decoded the
        // cached bytes and cached a ready protocol — the heart of the inline path.
        assert!(
            s.protocols.borrow().contains_key("https://x/cat.png"),
            "inline image decoded and cached for overlay"
        );
    }

    #[test]
    fn inline_image_is_clipped_not_shrunk_when_partly_off_screen() {
        // A tall image, so its fitted height exceeds the room left at the bottom
        // of the viewport and it must be clipped rather than resized.
        let buf = image::ImageBuffer::from_pixel(120, 600, image::Rgba([80u8, 160, 240, 255]));
        let mut cur = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut cur, image::ImageFormat::Png)
            .expect("encode png");
        let png = cur.into_inner();

        let mut s = PostDetailScreen::new(entry("p1"));
        let mut r = reply("r1", "p1");
        r.content = "photo:".into();
        r.attachments = vec![image_attachment("https://x/cat.png")];
        s.apply_replies_initial(Ok((vec![r], None)));
        s.cache_image_bytes("https://x/cat.png".into(), png);
        let picker = Picker::halfblocks();

        // The drawn image's (width, height) in cells — image cells are the only
        // ones with a non-default background (halfblocks paint).
        let measure = |s: &mut PostDetailScreen, scroll: u16| -> (u16, u16) {
            s.scroll = scroll;
            let backend = ratatui::backend::TestBackend::new(40, 20);
            let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| s.render(f, f.area(), &Theme::cyber(), true, Some(&picker)))
                .expect("draw");
            let buf = terminal.backend().buffer();
            let (mut width, mut height) = (0u16, 0u16);
            for y in 0..buf.area.height {
                let row_w = (0..buf.area.width)
                    .filter(|&x| buf[(x, y)].bg != ratatui::style::Color::Reset)
                    .count() as u16;
                if row_w > 0 {
                    height += 1;
                    width = width.max(row_w);
                }
            }
            (width, height)
        };

        let (w_clipped, h_clipped) = measure(&mut s, 0); // gap near the viewport bottom
        let (w_full, h_full) = measure(&mut s, 4); // scrolled so the gap fully fits
        assert!(w_full > 0 && h_full > 0, "image renders when fully in view");
        assert_eq!(
            w_clipped, w_full,
            "width is identical at both scrolls — the image is clipped, not resized"
        );
        assert!(
            h_clipped < h_full,
            "the partly-off-screen image shows fewer rows (clipped): {h_clipped} vs {h_full}"
        );
    }

    #[test]
    fn off_screen_inline_image_is_not_decoded_until_scrolled_into_view() {
        // Many image-less replies push the only image-bearing reply far down.
        let mut replies: Vec<Reply> = (0..30).map(|i| reply(&format!("r{i}"), "p1")).collect();
        let mut last = reply("rlast", "p1");
        last.attachments = vec![image_attachment("https://x/cat.png")];
        replies.push(last);
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((replies, None)));
        s.cache_image_bytes("https://x/cat.png".into(), tiny_png());
        // Scroll stays at 0 — the trailing image is well below the viewport.

        let picker = Picker::halfblocks();
        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber(), true, Some(&picker)))
            .expect("draw");

        assert!(
            !s.protocols.borrow().contains_key("https://x/cat.png"),
            "an off-screen image must not be decoded until it scrolls into view"
        );
    }

    #[test]
    fn images_disabled_reserves_no_gap_and_decodes_nothing() {
        // With `images_on = false`, the body is plain text: no gap, no decode.
        let mut s = PostDetailScreen::new(entry("p1"));
        let mut r = reply("r1", "p1");
        r.attachments = vec![image_attachment("https://x/cat.png")];
        s.apply_replies_initial(Ok((vec![r], None)));
        s.cache_image_bytes("https://x/cat.png".into(), tiny_png());

        let picker = Picker::halfblocks();
        let backend = ratatui::backend::TestBackend::new(40, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber(), false, Some(&picker)))
            .expect("draw");

        assert!(
            s.image_slots.borrow().is_empty(),
            "no gap reserved when off"
        );
        assert!(
            s.protocols.borrow().is_empty(),
            "nothing decoded when images are off"
        );
    }

    #[test]
    fn render_overlays_an_osc8_hyperlink_on_a_surfaced_url() {
        // A post body with a markdown link surfaces its URL on its own row; the
        // render overlay turns that row into a clickable OSC 8 hyperlink (the
        // first cell carries the open+text+close sequence). Width is wide enough
        // that the URL doesn't wrap. Hyperlinks default on in the test config.
        let mut e = entry("p1");
        e.content = "see [the site](https://x.example/page)".into();
        let s = PostDetailScreen::new(e);

        let backend = ratatui::backend::TestBackend::new(60, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber(), false, None))
            .expect("draw");

        // One link slot was recorded for the surfaced URL.
        assert_eq!(s.link_slots.borrow().len(), 1, "one link slot recorded");

        // A buffer cell carries the OSC 8 open sequence targeting the full URL.
        let buf = terminal.backend().buffer();
        let mut linked = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)]
                    .symbol()
                    .contains("\u{1b}]8;;https://x.example/page\u{1b}\\")
                {
                    linked = true;
                }
            }
        }
        assert!(linked, "the URL row is wrapped in an OSC 8 hyperlink");
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
        let lines = s.compose_body(&Theme::dark(), false, 0, false);
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

//! Feed screen — paginated list of entries with cursor-driven scroll.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::Entry;
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph};
use ratatui::Frame;

use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

use super::flag::{render_flag_prompt, FlagPrompt, FlagPromptKey};
use super::list::{self, TabState};
use super::theme::Theme;

/// A flag-reason prompt on a feed row: the reported entry's `post_id` is all the
/// target this screen needs.
type EntryFlagPrompt = FlagPrompt<String>;

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
    /// Edit the selected entry (`e`). Carries the entry as the feed holds it so
    /// the shell can pre-fill an edit form without a re-fetch.
    ///
    /// The action is offered on every entry, not just the viewer's own: editing
    /// is supporter-only and expires 5 minutes after publishing (v0.8.4 § Edit
    /// Entry), and the client knows neither its own identity nor its supporter
    /// status, so the server's `403` is the only honest gate. Same precedent as
    /// the delete key on post detail.
    EditEntry {
        post_id: String,
        content: String,
        title: Option<String>,
        topics: Vec<String>,
        is_public: bool,
        is_nsfw: bool,
    },
    /// Report the selected entry (`F`), with the reason the user typed into the
    /// inline prompt. `reason` is `None` when they submitted it blank, which
    /// v0.8.4 § Flag an Entry allows.
    FlagEntry {
        post_id: String,
        reason: Option<String>,
    },
    /// Exit the app.
    Quit,
    None,
}

pub struct FeedScreen {
    pub list: TabState<Entry>,
    pub include_nsfw: bool,
    /// The open flag-reason prompt (`F`), or `None` when nothing is being
    /// reported. While it's open it owns every key the screen sees.
    pub flag_prompt: Option<EntryFlagPrompt>,
    /// Raw bytes of images already fetched, keyed by URL. Same lazy model as
    /// the chat screens: decoding waits until the picture is on screen.
    image_bytes: RefCell<HashMap<String, Vec<u8>>>,
    /// URLs already asked for, so a failed fetch is not retried in a loop.
    image_requested: RefCell<HashSet<String>>,
    /// URLs whose fetch failed; they fall back to a chip instead of a band.
    image_failed: RefCell<HashSet<String>>,
    /// Pixel dimensions per URL, so a band can shrink to what the picture needs.
    image_dims: RefCell<HashMap<String, (u32, u32)>>,
    /// Encoded protocols with the size they were built for.
    image_protocols: RefCell<HashMap<String, (Protocol, Size)>>,
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

impl std::fmt::Debug for FeedScreen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Protocol` has no `Debug`, and a dump of encoded pixels helps nobody.
        f.debug_struct("FeedScreen")
            .field("list", &self.list)
            .field("include_nsfw", &self.include_nsfw)
            .field("images_cached", &self.image_bytes.borrow().len())
            .finish_non_exhaustive()
    }
}

impl FeedScreen {
    pub fn new() -> Self {
        Self {
            list: TabState::loading(),
            include_nsfw: crate::config::get().nsfw,
            flag_prompt: None,
            image_bytes: RefCell::new(HashMap::new()),
            image_requested: RefCell::new(HashSet::new()),
            image_failed: RefCell::new(HashSet::new()),
            image_dims: RefCell::new(HashMap::new()),
            image_protocols: RefCell::new(HashMap::new()),
        }
    }

    /// Whether a field is capturing text, so the shell's global single-letter
    /// shortcuts (`i`, `S`, the player keys) must not swallow the keystroke.
    /// True only while the flag-reason prompt is open.
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
    /// prompt is open, since the feed captures no other text.
    pub fn paste_text(&mut self, text: &str) {
        if let Some(prompt) = self.flag_prompt.as_mut() {
            prompt.paste(text);
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
        // An open flag prompt owns every other key, so a reason can contain the
        // letters the list otherwise binds (`r`, `b`, `j`…).
        if let Some(outcome) = self.flag_prompt.as_mut().map(|p| p.handle_key(key)) {
            return match outcome {
                FlagPromptKey::Consumed => FeedIntent::None,
                FlagPromptKey::Cancelled => {
                    self.flag_prompt = None;
                    FeedIntent::None
                }
                FlagPromptKey::Submitted => match self.flag_prompt.take() {
                    Some(prompt) => {
                        let reason = prompt.reason_to_send();
                        FeedIntent::FlagEntry {
                            post_id: prompt.target,
                            reason,
                        }
                    }
                    None => FeedIntent::None,
                },
            };
        }
        if self.list.loading {
            return FeedIntent::None;
        }
        let visible = self.visible_indices();
        let page = self.list.page_items();
        match super::list_nav::navigate_paged(
            key.code,
            &mut self.list.selected,
            visible.len(),
            self.list.next_cursor.is_some(),
            page,
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
            KeyCode::Char('e') => {
                if let Some(idx) = visible.get(self.list.selected) {
                    if let Some(entry) = self.list.items.get(*idx) {
                        return FeedIntent::EditEntry {
                            post_id: entry.post_id.clone(),
                            content: entry.content.clone(),
                            title: entry.title.clone(),
                            topics: entry.topics.clone(),
                            is_public: entry.is_public,
                            is_nsfw: entry.is_nsfw,
                        };
                    }
                }
            }
            KeyCode::Char('F') => {
                if let Some(idx) = visible.get(self.list.selected) {
                    if let Some(entry) = self.list.items.get(*idx) {
                        self.flag_prompt = Some(FlagPrompt::new(entry.post_id.clone()));
                    }
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

    /// True when the user's cursor sits on the newest entry — the top of the
    /// feed. The background head-poll uses this to decide whether it may reveal
    /// freshly arrived posts in place (safe at the top) or must defer to a toast
    /// hint so a scrolled-down reader doesn't lose their place.
    pub fn is_at_top(&self) -> bool {
        self.list.selected == 0
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
            // No strictly-new entries, but the head page may carry fresher
            // counts and edited text for posts we already show, so fold those
            // in and let them converge without a manual refresh.
            Some(0) => {
                self.merge_updates(&head);
                return HeadUpdate::None;
            }
            Some(k) => k,                   // head[0..k] are strictly newer
            None => return HeadUpdate::Gap, // a full page of new entries: gap
        };
        // head[new_count..] overlaps posts we already have; refresh those before
        // splicing the strictly-new prefix in at the top.
        self.merge_updates(&head);
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

    /// Refresh already-loaded entries from a freshly fetched head page, matched
    /// by `post_id`: the mutable engagement counts (`replies_count`,
    /// `bookmarks_count`) and the fields an author's edit can rewrite
    /// (`content`, `title`, `topics`, and the `edited_at` stamp that drives the
    /// `(edited)` marker, v0.8.4 § Edit Entry). The background head-poll fetches
    /// all of it for free, so folding it in lets a post already on screen
    /// converge without the user pressing `r`. Previously an entry someone else
    /// edited kept its stale text and never gained its marker.
    ///
    /// Deliberately left alone, because a background poll must not move the
    /// ground under the reader: `is_nsfw` (it re-filters the visible view, so
    /// folding it in would shift the row under the cursor mid-scroll),
    /// `is_public`, `attachments` (they retarget the `p`/`o` jukebox keys and
    /// the `[image]`/`[jukebox]` markers), `deleted`, and everything
    /// identifying (`post_id`, author, `created_at`, `slug`). A manual refresh
    /// picks those up. Strictly-new entries are handled separately by
    /// [`apply_new_head`](Self::apply_new_head).
    fn merge_updates(&mut self, head: &[Entry]) {
        use std::collections::HashMap;
        let fresh: HashMap<&str, &Entry> = head.iter().map(|e| (e.post_id.as_str(), e)).collect();
        for item in &mut self.list.items {
            if let Some(f) = fresh.get(item.post_id.as_str()) {
                item.replies_count = f.replies_count;
                item.bookmarks_count = f.bookmarks_count;
                item.content = f.content.clone();
                item.title = f.title.clone();
                item.topics = f.topics.clone();
                item.edited_at = f.edited_at;
            }
        }
    }

    /// Fold a freshly fetched copy of one entry into the loaded list, in place.
    /// v0.8.4 § Edit Entry returns only the echoed `postId`, so after an edit
    /// the shell re-fetches the entry and hands it here rather than reloading
    /// the whole feed and losing the reader's place. Returns `true` when a
    /// matching entry was loaded.
    ///
    /// Unlike the background head-poll merge this is user-initiated and
    /// authoritative, so it also applies the visibility flags and attachments;
    /// the selection is re-clamped afterwards because turning `is_nsfw` on can
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

    /// Record that `url` could not be fetched, so no band is held open for it.
    ///
    /// Without this the layout reserves rows for a picture that will never
    /// arrive *and* suppresses the `[image]` chip, because as far as the layout
    /// knows one is being drawn. The reader gets a blank hole with nothing to
    /// explain it. Marking the URL failed puts the chip back.
    pub fn note_image_failed(&self, url: &str) {
        self.image_failed.borrow_mut().insert(url.to_string());
    }

    /// The bytes already fetched for `url`, for the fullscreen modal.
    #[must_use]
    pub fn held_image_bytes(&self, url: &str) -> Option<Vec<u8>> {
        self.image_bytes.borrow().get(url).cloned()
    }

    /// Hand the screen the bytes of an image it asked for.
    pub fn cache_image_bytes(&self, url: String, bytes: Vec<u8>) {
        if let Some(dims) = super::images::probe_dimensions(&bytes) {
            self.image_dims.borrow_mut().insert(url.clone(), dims);
        }
        self.image_bytes.borrow_mut().insert(url, bytes);
    }

    /// The first image URL of each loaded entry that has not been asked for yet.
    ///
    /// Only the first per entry: a card shows one picture, and fetching the
    /// rest would spend the reader's bandwidth on images no screen will draw.
    pub fn image_urls_to_fetch(&self, limit: usize) -> Vec<String> {
        let mut requested = self.image_requested.borrow_mut();
        let bytes = self.image_bytes.borrow();
        self.list
            .items
            .iter()
            .filter_map(|e| super::images::entry_image_urls(e).into_iter().next())
            // Three steps, and the order of all three matters. Skip what is
            // already held or already asked for, *then* cap the batch, and only
            // then mark. Capping before the skip would re-take the same URLs
            // every call and hand back nothing; marking before the cap would
            // spend URLs that were never fetched, and a spent URL is never
            // offered again.
            .filter(|url| !bytes.contains_key(url) && !requested.contains(url))
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .inspect(|url| {
                requested.insert(url.clone());
            })
            .collect()
    }

    /// Paint each card's reserved band, after the list has settled its offset.
    ///
    /// Positions are summed from `entry_height` using the same `rows_for` the
    /// cards were built with, so the two cannot disagree. The list tiles
    /// top-down from `list_offset`, unlike the chat panes which are anchored to
    /// the bottom, so `y` simply accumulates from the top of the pane.
    #[allow(clippy::too_many_arguments)]
    fn paint_images(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        visible: &[usize],
        cols: u16,
        font: (u16, u16),
        rows_for: &dyn Fn(&Entry) -> u16,
        picker: Option<&Picker>,
    ) {
        let Some(picker) = picker else { return };
        let _ = font;
        let mut protocols = self.image_protocols.borrow_mut();
        let bytes = self.image_bytes.borrow();
        let bottom = area.y.saturating_add(area.height);
        let mut y = area.y;
        for &i in visible.iter().skip(self.list.list_offset()) {
            let Some(entry) = self.list.items.get(i) else {
                break;
            };
            if y >= bottom {
                break;
            }
            let rows = rows_for(entry);
            let height = entry_height(entry, rows);
            if rows > 0 {
                if let Some(url) = super::images::entry_image_urls(entry).into_iter().next() {
                    // The band sits below the header, title and snippet, which
                    // is what `entry_height` counts before it.
                    let above = entry_height(entry, 0).saturating_sub(
                        u16::from(!crate::config::get().compact), // the rule
                    );
                    let top = y.saturating_add(above);
                    let visible_rows = rows.min(bottom.saturating_sub(top));
                    if top < bottom && visible_rows > 0 {
                        let target = Size::new(cols, rows);
                        let stale = protocols
                            .get(&url)
                            .map_or(true, |(_, built)| *built != target);
                        if stale {
                            if let Some(raw) = bytes.get(&url) {
                                match super::images::decode_bounded(raw).and_then(|img| {
                                    picker
                                        .new_protocol(
                                            img,
                                            target,
                                            Resize::Fit(super::images::filter()),
                                        )
                                        .map_err(|e| e.to_string())
                                }) {
                                    Ok(proto) => {
                                        protocols.insert(url.clone(), (proto, target));
                                    }
                                    Err(e) => {
                                        tracing::debug!(error = %e, url = %url, "feed image encode failed");
                                    }
                                }
                            }
                        }
                        if let Some((proto, _)) = protocols.get(&url) {
                            let img_area =
                                Rect::new(area.x.saturating_add(2), top, cols, visible_rows);
                            frame.render_widget(Image::new(proto).allow_clipping(true), img_area);
                        }
                    }
                }
            }
            y = y.saturating_add(height);
        }
    }

    pub fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        picker: Option<&Picker>,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • feed ", theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // The flag prompt takes a second row at the bottom (hint + input) in
        // place of the status line, which it replaces while it's open.
        let bottom = if self.flag_prompt.is_some() { 2 } else { 1 };
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(bottom)])
            .split(inner);
        let list_area = layout[0];
        let status_area = layout[1];

        let visible = self.visible_indices();
        let width = list_area.width;

        // A feed is an index you scan, not a page you read, so a picture here
        // is held to a *third* of the pane rather than the half a conversation
        // allows. Without that, a run of image posts turns the feed into one
        // post per screen and stops being a feed at all. The configured
        // `image_height` still caps it, and `fitted_image_rows` usually lands
        // well under both.
        let band_cap: Option<u16> = picker.filter(|_| list_area.height > 8).map(|_| {
            crate::config::get()
                .image_height
                .min(list_area.height / 3)
                .max(1)
        });
        let font = picker.map_or((8, 16), |p| {
            let fs = p.font_size();
            (fs.width, fs.height)
        });
        // The text column: the list reserves a two-column highlight gutter.
        let cols = width.saturating_sub(2).max(1);
        let rows_for = |e: &Entry| -> u16 {
            let Some(cap) = band_cap else { return 0 };
            let Some(url) = super::images::entry_image_urls(e).into_iter().next() else {
                return 0;
            };
            // A failed fetch reserves nothing, so a card does not carry a blank
            // band for a picture that will never arrive.
            if self.image_failed.borrow().contains(&url) {
                return 0;
            }
            match self.image_dims.borrow().get(&url) {
                Some(&dims) => super::chat::fitted_image_rows(dims, cols, font, cap),
                // Not fetched yet: reserve the cap so the picture is never
                // clipped, and let the band shrink when the header is known.
                None => cap,
            }
        };

        list::render_body(
            frame,
            list_area,
            theme,
            &self.list,
            &visible,
            "no entries to show",
            |e| entry_item(e, width, theme, rows_for(e)),
        );
        self.paint_images(frame, list_area, &visible, cols, font, &rows_for, picker);

        if let Some(prompt) = &self.flag_prompt {
            render_flag_prompt(frame, status_area, theme, prompt);
        } else {
            let status = status_line(self, theme);
            frame.render_widget(status, status_area);
        }
    }
}

impl Default for FeedScreen {
    fn default() -> Self {
        Self::new()
    }
}

/// Rows an entry card occupies, including any reserved image band.
///
/// **Must agree with [`entry_item`] exactly.** The overlay places pictures by
/// summing these, so a disagreement puts every image below the first one in the
/// wrong place. `entry_height_matches_entry_item` pins it.
fn entry_height(entry: &Entry, image_rows: u16) -> u16 {
    let mut rows = 1u16; // header
    if entry.title.as_deref().is_some_and(|t| !t.trim().is_empty()) {
        rows += 1;
    }
    if !super::markdown::content_preview(&entry.content, crate::config::get().preview_length)
        .is_empty()
    {
        rows += 1;
    }
    rows += image_rows;
    if !crate::config::get().compact {
        rows += 1; // rule
    }
    rows
}

fn entry_item(entry: &Entry, width: u16, theme: &Theme, image_rows: u16) -> ListItem<'static> {
    let when = entry
        .created_at
        .map(crate::config::format_list_timestamp)
        .unwrap_or_default();
    let topics = if entry.topics.is_empty() {
        String::new()
    } else {
        format!(" · #{}", entry.topics.join(" #"))
    };
    // v0.8.10 § List Entries puts guild forum threads in the main feed, for the
    // guilds you belong to. Without a marker a thread is indistinguishable from
    // a personal post, and the reader has no way to see which guild it came
    // from — the one piece of context that makes a thread make sense.
    let guild = entry
        .guild_thread_of()
        .map(|slug| format!(" · guild:{slug}"))
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
        Span::styled(
            format!(" · {when}{edited}{guild}{topics}{counts}"),
            theme.muted_style(),
        ),
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

    // Blank rows the screen paints the picture onto after the list is drawn.
    // Below the snippet so the text stays the thing you scan; a card still
    // reads as a card with the image as its illustration.
    for _ in 0..image_rows {
        lines.push(Line::from(String::new()));
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
    let media = if s.selected_entry().is_some_and(super::audio::has_audio) {
        " · p play · o open"
    } else {
        ""
    };
    let text = if s.list.loading {
        "loading… · c new post · enter open · b bookmark · r refresh · esc menu".to_string()
    } else if s.list.next_cursor.is_some() {
        format!(
            "{} entries · scroll down for more · c new post · enter open · b bookmark{media} · e edit · F flag · r refresh · esc menu",
            s.list.items.len()
        )
    } else {
        format!(
            "{} entries · end of feed · c new post · enter open · b bookmark{media} · e edit · F flag · r refresh · esc menu",
            s.list.items.len()
        )
    };
    Paragraph::new(Line::from(Span::styled(text, theme.muted_style())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::flag::MAX_FLAG_REASON;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn a_short_feed_page_is_not_the_end_of_the_feed() {
        // v0.8.10 § List Entries: guild forum threads are filtered out of the
        // feed *after* the page is taken, so a page can come back shorter than
        // `limit`, or empty, with more entries behind it. The end of the feed is
        // a null cursor and nothing else — reading it off the page size would
        // truncate the feed at the first filtered thread.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("p1", "trinity", false)],
            Some("cursor-1".into()),
        )));
        assert_eq!(s.list.items.len(), 1, "a page far shorter than the limit");
        assert!(s.list.next_cursor.is_some());

        // Even an empty page keeps the feed going.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((Vec::new(), Some("cursor-1".into()))));
        assert!(s.list.items.is_empty());
        assert!(
            s.list.next_cursor.is_some(),
            "the cursor is what says whether there is more"
        );
        // And scrolling off the (empty) end asks for the next page rather than
        // stopping dead.
        assert!(matches!(
            s.handle_key(key(KeyCode::Char('n'))),
            FeedIntent::LoadMore
        ));
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
            edited_at: None,
            deleted: false,
            ..Default::default()
        }
    }

    fn render_entry_item(entry: &Entry) -> String {
        use ratatui::widgets::List;
        let theme = Theme::cyber();
        let item = entry_item(entry, 80, &theme, 0);
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
    fn pgdn_and_pgup_move_the_feed_by_a_screenful() {
        let entries: Vec<Entry> = (0..40)
            .map(|i| entry(&format!("e{i}"), "text", false))
            .collect();
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((entries, None)));
        assert_eq!(s.list.selected, 0);

        // Render once so the screen knows how many cards fit; before that
        // `page_items` is 1 and paging would behave like a single step.
        let _ = render_feed_to_string(&s);
        let page = s.list.page_items();
        assert!(page > 1, "a rendered feed fits more than one card: {page}");

        s.handle_key(key(KeyCode::PageDown));
        assert_eq!(s.list.selected, page, "PgDn moves exactly one page");

        s.handle_key(key(KeyCode::PageUp));
        assert_eq!(s.list.selected, 0, "and PgUp comes back");
    }

    #[test]
    fn paging_up_from_the_top_of_the_feed_stays_put() {
        let entries: Vec<Entry> = (0..10)
            .map(|i| entry(&format!("e{i}"), "text", false))
            .collect();
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((entries, None)));
        let _ = render_feed_to_string(&s);
        s.handle_key(key(KeyCode::PageUp));
        assert_eq!(s.list.selected, 0);
    }

    #[test]
    fn entry_height_matches_entry_item() {
        // The invariant the image overlay rests on: it sums `entry_height` to
        // find where each card starts, so any disagreement puts every picture
        // below the first one in the wrong place. This is the same class of bug
        // that `body_height` had for image gaps.
        let theme = Theme::cyber();
        let mut plain = entry("e1", "just text", false);
        plain.title = None;
        let mut titled = entry("e2", "with a title", false);
        titled.title = Some("A Title".into());
        let empty = entry("e3", "", false);

        for e in [&plain, &titled, &empty] {
            for rows in [0u16, 1, 7] {
                let item = entry_item(e, 80, &theme, rows);
                assert_eq!(
                    u16::try_from(item.height()).unwrap_or(u16::MAX),
                    entry_height(e, rows),
                    "height disagreed for {:?} at {rows} image rows",
                    e.post_id,
                );
            }
        }
    }

    #[test]
    fn a_card_reserves_no_rows_when_there_is_no_picture() {
        let theme = Theme::cyber();
        let e = entry("e1", "no image here", false);
        assert_eq!(
            entry_item(&e, 80, &theme, 0).height(),
            entry_height(&e, 0) as usize,
        );
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
    fn apply_new_head_refreshes_counts_when_nothing_new() {
        // The reported bug: a post you already see gains replies, but the
        // background head-poll left its count stale because no *new* posts
        // arrived. The head page now folds fresher counts into existing posts.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        assert_eq!(s.list.items[0].replies_count, 0);

        let mut fresh_a = entry("a", "a", false);
        fresh_a.replies_count = 3;
        fresh_a.bookmarks_count = 2;
        let update = s.apply_new_head(vec![fresh_a, entry("b", "b", false)]);

        assert_eq!(update, HeadUpdate::None); // nothing prepended
        assert_eq!(s.list.items.len(), 2); // no entries added
        assert_eq!(s.list.items[0].replies_count, 3); // ...but counts updated
        assert_eq!(s.list.items[0].bookmarks_count, 2);
    }

    #[test]
    fn apply_new_head_refreshes_counts_while_prepending() {
        // A new post arrives AND an existing post's reply count changed in the
        // same poll: prepend the new one and refresh the overlap's counts.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        let mut fresh_a = entry("a", "a", false);
        fresh_a.replies_count = 5;
        let update = s.apply_new_head(vec![
            entry("x", "x", false), // strictly new
            fresh_a,                // existing, now with replies
            entry("b", "b", false),
        ]);
        assert_eq!(update, HeadUpdate::Prepended(1));
        assert_eq!(s.list.items[0].post_id, "x");
        // "a" is now at index 1 and carries the refreshed count.
        assert_eq!(s.list.items[1].post_id, "a");
        assert_eq!(s.list.items[1].replies_count, 5);
    }

    #[test]
    fn is_at_top_tracks_selection() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        assert!(s.is_at_top(), "selection starts on the newest entry");
        s.list.selected = 1;
        assert!(!s.is_at_top(), "scrolled down is not at top");
        s.list.selected = 0;
        assert!(s.is_at_top(), "back at the newest entry");
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
        terminal
            .draw(|f| s.render(f, f.area(), &theme, None))
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

    /// A fixed `editedAt` stamp for the edit-marker tests.
    fn edited_stamp() -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("valid timestamp")
    }

    #[test]
    fn entry_item_marks_an_edited_entry_only_when_stamped() {
        let mut e = entry("a", "alice", false);
        assert!(
            !render_entry_item(&e).contains("(edited)"),
            "an untouched entry carries no marker"
        );
        e.edited_at = Some(edited_stamp());
        let text = render_entry_item(&e);
        assert!(
            text.contains("(edited)"),
            "edited entry is marked: {text:?}"
        );
        assert!(text.contains("@alice"), "the rest of the header survives");
    }

    #[test]
    fn e_requests_an_edit_of_the_selected_entry() {
        let mut s = FeedScreen::new();
        let mut second = entry("p2", "bob", false);
        second.title = Some("Headline".into());
        second.topics = vec!["music".into()];
        second.is_public = true;
        s.apply_initial(Ok((vec![entry("p1", "alice", false), second], None)));
        s.list.selected = 1;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'))),
            FeedIntent::EditEntry {
                post_id: "p2".into(),
                content: "content of p2".into(),
                title: Some("Headline".into()),
                topics: vec!["music".into()],
                is_public: true,
                is_nsfw: false,
            }
        );
    }

    #[test]
    fn e_on_an_empty_feed_is_a_noop() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![], None)));
        assert_eq!(s.handle_key(key(KeyCode::Char('e'))), FeedIntent::None);
    }

    #[test]
    fn capital_f_opens_the_flag_prompt_and_enter_files_the_report() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("p1", "alice", false)], None)));
        assert_eq!(s.handle_key(key(KeyCode::Char('F'))), FeedIntent::None);
        assert!(s.is_text_input(), "the open prompt captures text");
        for c in "spam".chars() {
            assert_eq!(s.handle_key(key(KeyCode::Char(c))), FeedIntent::None);
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            FeedIntent::FlagEntry {
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
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("p1", "alice", false)], None)));
        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            FeedIntent::FlagEntry {
                post_id: "p1".into(),
                reason: None,
            }
        );
    }

    #[test]
    fn whitespace_only_flag_reason_is_sent_as_none() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("p1", "alice", false)], None)));
        s.handle_key(key(KeyCode::Char('F')));
        s.handle_key(key(KeyCode::Char(' ')));
        s.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            FeedIntent::FlagEntry {
                post_id: "p1".into(),
                reason: None,
            }
        );
    }

    #[test]
    fn esc_cancels_the_flag_prompt_without_reporting() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("p1", "alice", false)], None)));
        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(s.handle_key(key(KeyCode::Esc)), FeedIntent::None);
        assert!(s.flag_prompt.is_none());
    }

    #[test]
    fn cancel_flag_prompt_reports_whether_it_consumed_the_key() {
        // The shell asks first, so Esc dismisses the prompt instead of popping
        // the screen; with no prompt open it must decline the key.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("p1", "alice", false)], None)));
        assert!(!s.cancel_flag_prompt());
        s.handle_key(key(KeyCode::Char('F')));
        assert!(s.cancel_flag_prompt());
        assert!(s.flag_prompt.is_none());
        assert!(!s.is_text_input());
    }

    #[test]
    fn the_flag_prompt_swallows_list_shortcuts() {
        // Every bare letter must reach the reason field, so a reason can say
        // "reposts junk" without refreshing the feed or moving the cursor.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("p1", "alice", false), entry("p2", "bob", false)],
            None,
        )));
        s.handle_key(key(KeyCode::Char('F')));
        for c in "jrb".chars() {
            assert_eq!(s.handle_key(key(KeyCode::Char(c))), FeedIntent::None);
        }
        assert_eq!(s.list.selected, 0, "j typed a letter, it did not navigate");
        assert_eq!(
            s.list.items.len(),
            2,
            "r typed a letter, it did not refresh"
        );
        assert_eq!(
            s.flag_prompt.as_ref().map(|p| p.reason.as_str()),
            Some("jrb")
        );
    }

    #[test]
    fn flag_reason_stops_at_the_spec_cap() {
        let mut prompt = EntryFlagPrompt::new("p1".into());
        assert!(prompt.is_empty());
        for _ in 0..(MAX_FLAG_REASON + 10) {
            prompt.handle_key(key(KeyCode::Char('x')));
        }
        assert_eq!(prompt.len(), MAX_FLAG_REASON);
        assert_eq!(prompt.cursor, MAX_FLAG_REASON);
    }

    #[test]
    fn flag_prompt_edits_at_the_caret() {
        let mut prompt = EntryFlagPrompt::new("p1".into());
        for c in "spm".chars() {
            prompt.handle_key(key(KeyCode::Char(c)));
        }
        prompt.handle_key(key(KeyCode::Left));
        prompt.handle_key(key(KeyCode::Char('a')));
        assert_eq!(prompt.reason, "spam");
        assert_eq!(prompt.cursor, 3);

        prompt.handle_key(key(KeyCode::Home));
        prompt.handle_key(key(KeyCode::Delete));
        assert_eq!(prompt.reason, "pam");

        prompt.handle_key(key(KeyCode::End));
        prompt.handle_key(key(KeyCode::Backspace));
        assert_eq!(prompt.reason, "pa");
        assert_eq!(prompt.reason_to_send().as_deref(), Some("pa"));
    }

    #[test]
    fn flag_prompt_keeps_multibyte_reasons_intact() {
        // Caret positions are char indices, so editing must not split a glyph.
        let mut prompt = EntryFlagPrompt::new("p1".into());
        for c in "spæm".chars() {
            prompt.handle_key(key(KeyCode::Char(c)));
        }
        prompt.handle_key(key(KeyCode::Left));
        prompt.handle_key(key(KeyCode::Backspace));
        assert_eq!(prompt.reason, "spm");
    }

    #[test]
    fn paste_into_the_flag_prompt_collapses_newlines() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("p1", "alice", false)], None)));
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
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((vec![entry("p1", "alice", false)], None)));
        assert!(
            render_feed_to_string(&s).contains("e edit"),
            "the new keys are advertised on the status line"
        );
        s.handle_key(key(KeyCode::Char('F')));
        let text = render_feed_to_string(&s);
        assert!(text.contains("flag reason"), "prompt is drawn: {text:?}");
        assert!(text.contains("@alice"), "the list stays visible behind it");
    }

    #[test]
    fn apply_new_head_folds_an_edit_into_a_loaded_entry() {
        // The reported bug: the head-poll merged only counts, so an entry
        // edited by someone else kept its stale text and never gained its
        // marker until a manual refresh.
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        let mut fresh = entry("a", "a", false);
        fresh.content = "corrected".into();
        fresh.title = Some("New Headline".into());
        fresh.topics = vec!["linux".into()];
        fresh.edited_at = Some(edited_stamp());
        fresh.is_nsfw = true; // must NOT be folded in: it would hide the row

        let update = s.apply_new_head(vec![fresh, entry("b", "b", false)]);

        assert_eq!(update, HeadUpdate::None);
        assert_eq!(s.list.items[0].content, "corrected");
        assert_eq!(s.list.items[0].title.as_deref(), Some("New Headline"));
        assert_eq!(s.list.items[0].topics, vec!["linux".to_string()]);
        assert!(
            s.list.items[0].edited_at.is_some(),
            "marker stamp folded in"
        );
        assert!(
            !s.list.items[0].is_nsfw,
            "a background poll must not re-filter the view under the cursor"
        );
    }

    #[test]
    fn apply_new_head_folds_an_edit_while_prepending() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        let mut fresh = entry("a", "a", false);
        fresh.content = "corrected".into();
        fresh.edited_at = Some(edited_stamp());
        let update = s.apply_new_head(vec![
            entry("x", "x", false), // strictly new
            fresh,                  // existing, now edited
            entry("b", "b", false),
        ]);
        assert_eq!(update, HeadUpdate::Prepended(1));
        assert_eq!(s.list.items[1].post_id, "a");
        assert_eq!(s.list.items[1].content, "corrected");
        assert!(s.list.items[1].edited_at.is_some());
    }

    #[test]
    fn apply_edited_entry_updates_in_place() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        let mut fresh = entry("b", "b", false);
        fresh.content = "rewritten".into();
        fresh.title = Some("Fixed".into());
        fresh.is_public = true;
        fresh.edited_at = Some(edited_stamp());

        assert!(s.apply_edited_entry(&fresh));
        assert_eq!(s.list.items[1].content, "rewritten");
        assert_eq!(s.list.items[1].title.as_deref(), Some("Fixed"));
        assert!(
            s.list.items[1].is_public,
            "an explicit edit is authoritative"
        );
        assert!(s.list.items[1].edited_at.is_some());
        assert_eq!(s.list.items[0].content, "content of a", "others untouched");

        assert!(
            !s.apply_edited_entry(&entry("gone", "z", false)),
            "an entry we never loaded reports a miss"
        );
    }

    #[test]
    fn apply_edited_entry_reclamps_a_selection_it_hides() {
        let mut s = FeedScreen::new();
        s.apply_initial(Ok((
            vec![entry("a", "a", false), entry("b", "b", false)],
            None,
        )));
        s.include_nsfw = false;
        s.list.selected = 1; // sitting on "b"
        let mut fresh = entry("b", "b", false);
        fresh.is_nsfw = true; // the row the cursor is on disappears
        assert!(s.apply_edited_entry(&fresh));
        assert_eq!(s.visible_indices(), vec![0]);
        assert_eq!(s.list.selected, 0, "selection clamps to the shrunken view");
    }
}

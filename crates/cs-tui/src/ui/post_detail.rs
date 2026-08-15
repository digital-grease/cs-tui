//! Post detail screen — entry header + content + scrollable replies (oldest first).
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Entry, EntryEdit, Reply};
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};
use time::OffsetDateTime;

use super::flag::{FlagPrompt, FlagPromptKey, MAX_FLAG_REASON};
use super::images::{entry_image_urls, reply_image_urls};
use super::markdown::{render_markdown_with, ImageUrls};
use super::text::edited_marker;
use super::theme::Theme;

/// How long after publishing the server still accepts an edit, in seconds
/// (v0.8.4 § Edit Entry: "within **5 minutes** of publishing"). Used for a
/// status-line hint only, never to withhold the key.
const EDIT_WINDOW_SECS: i64 = 5 * 60;

/// Label in front of the flag-reason field, so the prompt row and the width the
/// field is windowed into agree on how much room the text has.
const FLAG_PROMPT_LABEL: &str = "reason (optional): ";

/// An inline image reserved in the post-detail body: its source URL and the
/// logical line index where its blank-row gap begins. `render` overlays the
/// graphic onto the gap, clipped (not resized) against the viewport edges so it
/// scrolls like the rest of the body.
struct ImageSlot {
    url: String,
    start_line: usize,
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
    /// Toggle watching this thread (subscribe to / unsubscribe from
    /// `thread_reply` notifications).
    ToggleWatch,
    /// Open a URL (the jukebox link) in the user's default browser.
    OpenUrl(String),
    /// Play (or toggle) the focused jukebox track. `None` when there's none —
    /// the app then treats `p` as pause for whatever is already playing.
    PlayJukebox(Option<super::audio::JukeboxTrack>),
    /// User confirmed deletion of the entry.
    DeleteEntryConfirmed,
    /// Edit this entry (`e` with the post focused). Carries the current values
    /// of every field `PATCH /v1/posts/:id` accepts (v0.8.4 § Edit Entry) so the
    /// app can prefill an edit flow without re-fetching. The slug is absent on
    /// purpose: it is frozen once published and sending one is a `400`.
    EditEntry {
        post_id: String,
        content: String,
        title: Option<String>,
        topics: Vec<String>,
        is_public: bool,
        is_nsfw: bool,
    },
    /// Edit the selected reply (`e` with a reply selected). `content` is the
    /// only editable field (v0.8.4 § Edit Reply), so it is all that travels.
    EditReply {
        reply_id: String,
        content: String,
    },
    /// Report this entry (`F` with the post focused, after the reason prompt).
    /// `reason` is `None` when the prompt was submitted empty, which the spec
    /// allows (v0.8.4 § Flag an Entry).
    FlagEntry {
        post_id: String,
        reason: Option<String>,
    },
    /// Report the selected reply (`F` with a reply selected, after the prompt).
    FlagReply {
        reply_id: String,
        reason: Option<String>,
    },
    None,
}

/// What an open flag-reason prompt reports. Captured when `F` is pressed, so
/// the report can never drift onto another target while the reason is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FlagTarget {
    /// The entry itself, by post id (no reply was selected).
    Entry(String),
    /// One reply, by reply id.
    Reply(String),
}

impl FlagTarget {
    /// How the status line names this target while the prompt is open.
    fn label(&self) -> &'static str {
        match self {
            Self::Entry(_) => "this post",
            Self::Reply(_) => "this reply",
        }
    }
}

/// An open flag-reason prompt on this screen: the shared single-line field
/// (identical typing, caret keys, paste and cap everywhere `F` is bound) over
/// this screen's post-or-reply target.
type DetailFlagPrompt = FlagPrompt<FlagTarget>;

/// The intent that files `prompt`'s report. A blank (or all-whitespace) reason
/// travels as `None` rather than as `""`: the field is optional, and an empty
/// string only spends bytes to say nothing.
fn flag_intent(prompt: DetailFlagPrompt) -> PostDetailIntent {
    let reason = prompt.reason_to_send();
    match prompt.target {
        FlagTarget::Entry(post_id) => PostDetailIntent::FlagEntry { post_id, reason },
        FlagTarget::Reply(reply_id) => PostDetailIntent::FlagReply { reply_id, reason },
    }
}

/// Whether `created_at` could still be inside the server's edit window.
///
/// Fails OPEN: `created_at` is optional on both `Entry` and `Reply`, and a
/// timestamp this client never received answers `true`. A clock we do not have
/// must never be the reason a user is talked out of trying. This only picks the
/// wording of a status-line hint; the key itself is never gated, and the server
/// has the last word (v0.8.4 § Edit Entry answers `403` outside the window).
fn within_edit_window(created_at: Option<OffsetDateTime>) -> bool {
    match created_at {
        Some(t) => (OffsetDateTime::now_utc() - t).whole_seconds() <= EDIT_WINDOW_SECS,
        None => true,
    }
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
    /// Whether the viewer currently watches this thread (subscribed to its
    /// `thread_reply` notifications). `None` until the background status fetch
    /// resolves; set optimistically on `w` and reconciled by the toggle result.
    pub watching: Option<bool>,
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
    /// The flag-reason prompt opened by `F`, if any. While it is open it owns
    /// every printable key, so the app must route text-input decisions through
    /// [`PostDetailScreen::is_text_input`] and Esc through
    /// [`PostDetailScreen::cancel_flag_prompt`].
    flag_prompt: Option<DetailFlagPrompt>,
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
            watching: None,
            selected_reply: None,
            reply_starts: RefCell::new(Vec::new()),
            reply_anchors: RefCell::new(Vec::new()),
            confirming_delete: false,
            flag_prompt: None,
            protocols: RefCell::new(HashMap::new()),
            image_bytes: RefCell::new(HashMap::new()),
            requested: RefCell::new(HashSet::new()),
            image_slots: RefCell::new(Vec::new()),
        }
    }

    /// Record the latest known watch state for this thread (from the status
    /// fetch on open or a watch/unwatch toggle result).
    pub fn set_watching(&mut self, watching: bool) {
        self.watching = Some(watching);
    }

    /// Whether a field on this screen currently owns the keyboard, i.e. the
    /// flag-reason prompt is open. The app consults this before its global
    /// shortcuts so a typed `?`, `i`, `S` or digit lands in the reason instead
    /// of opening help, toggling images, shuffling or jumping sections.
    pub fn is_text_input(&self) -> bool {
        self.flag_prompt.is_some()
    }

    /// Close the flag-reason prompt without filing anything, answering `true`
    /// only when there was one to close.
    ///
    /// The app intercepts Esc before the screen sees it, so it has to offer the
    /// prompt the first Esc (the same shape as the topics filter box); a `false`
    /// answer means Esc keeps its usual "go back" meaning.
    pub fn cancel_flag_prompt(&mut self) -> bool {
        self.flag_prompt.take().is_some()
    }

    /// Fold a successful entry edit into the entry on screen: apply exactly the
    /// fields `edit` carried and stamp `edited_at`, so the "(edited)" marker
    /// appears without a round trip.
    ///
    /// v0.8.4 § Edit Entry returns only the echoed `postId`, so there is nothing
    /// to merge from the response, and `created_at` is deliberately left alone
    /// ("`createdAt` never changes"). The stamp is this machine's clock, an
    /// approximation of the server's `editedAt` that drives nothing but the
    /// marker; a later re-fetch replaces it with the real value.
    pub fn apply_entry_edit(&mut self, edit: &EntryEdit) {
        if let Some(content) = &edit.content {
            self.entry.content = content.clone();
        }
        if let Some(title) = &edit.title {
            // Removing a title and setting one are the same field on the wire;
            // `is_remove` is what tells them apart (v0.8.4 § Edit Entry: "Send
            // `\"\"` to remove a title").
            self.entry.title = if title.is_remove() {
                None
            } else {
                Some(title.as_str().to_string())
            };
        }
        if let Some(topics) = &edit.topics {
            self.entry.topics = topics.clone();
        }
        if let Some(is_public) = edit.is_public {
            self.entry.is_public = is_public;
        }
        if let Some(is_nsfw) = edit.is_nsfw {
            self.entry.is_nsfw = is_nsfw;
        }
        if let Some(attachments) = &edit.attachments {
            self.entry.attachments = attachments.clone();
        }
        self.entry.edited_at = Some(OffsetDateTime::now_utc());
    }

    /// Fold a successful reply edit into the reply with `reply_id`, marking it
    /// edited the same way [`Self::apply_entry_edit`] marks the entry. Answers
    /// `false` when that reply is not on this page, which is the caller's cue to
    /// refresh instead.
    ///
    /// `content` is the only editable field (v0.8.4 § Edit Reply), and editing
    /// does not bump the thread, so no counter is touched here either.
    pub fn apply_reply_edit(&mut self, reply_id: &str, content: String) -> bool {
        match self.replies.iter_mut().find(|r| r.reply_id == reply_id) {
            Some(reply) => {
                reply.content = content;
                reply.edited_at = Some(OffsetDateTime::now_utc());
                true
            }
            None => false,
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
        // An open reason prompt is a text field, so it swallows every key before
        // any of the screen's own bindings can read it as a command.
        if self.flag_prompt.is_some() {
            return self.handle_flag_prompt_key(key);
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
            // `e` edits whatever is focused: the selected reply, else the post.
            // It is never gated on authorship, supporter status or the 5-minute
            // window, none of which the client knows for certain, so the
            // server's 403 surfaces like any other error, exactly as `d` already
            // behaves here (v0.8.4 § Edit Entry, § Edit Reply).
            KeyCode::Char('e') => self.edit_intent(),
            // `F` reports whatever is focused, after an optional reason.
            KeyCode::Char('F') => {
                self.flag_prompt = Some(FlagPrompt::new(self.flag_target()));
                PostDetailIntent::None
            }
            // `w` watches / unwatches the thread (post-level, ignores reply selection).
            KeyCode::Char('w') => PostDetailIntent::ToggleWatch,
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

    /// Keys while the flag-reason prompt is open, delegated to the shared field
    /// so reporting types the same here as it does on the feeds: Enter files the
    /// report, Esc abandons it, and everything else is text or caret movement.
    fn handle_flag_prompt_key(&mut self, key: KeyEvent) -> PostDetailIntent {
        let Some(outcome) = self.flag_prompt.as_mut().map(|p| p.handle_key(key)) else {
            return PostDetailIntent::None;
        };
        match outcome {
            FlagPromptKey::Consumed => PostDetailIntent::None,
            // The local mirror of `cancel_flag_prompt`, for whenever Esc does
            // reach the screen.
            FlagPromptKey::Cancelled => {
                self.flag_prompt = None;
                PostDetailIntent::None
            }
            FlagPromptKey::Submitted => match self.flag_prompt.take() {
                Some(prompt) => flag_intent(prompt),
                None => PostDetailIntent::None,
            },
        }
    }

    /// Insert bracketed-paste text into the flag-reason prompt. A no-op when no
    /// prompt is open, since this screen captures no other text.
    pub fn paste_text(&mut self, text: &str) {
        if let Some(prompt) = self.flag_prompt.as_mut() {
            prompt.paste(text);
        }
    }

    /// What `e` edits: the selected reply, or the post when none is selected.
    /// Mirrors how `b` targets the selection before falling back to the post.
    fn edit_intent(&self) -> PostDetailIntent {
        match self.selected_reply.and_then(|i| self.replies.get(i)) {
            Some(reply) => PostDetailIntent::EditReply {
                reply_id: reply.reply_id.clone(),
                content: reply.content.clone(),
            },
            None => PostDetailIntent::EditEntry {
                post_id: self.entry.post_id.clone(),
                content: self.entry.content.clone(),
                title: self.entry.title.clone(),
                topics: self.entry.topics.clone(),
                is_public: self.entry.is_public,
                is_nsfw: self.entry.is_nsfw,
            },
        }
    }

    /// What `F` reports, resolved once at the keypress with the same selection
    /// precedence as [`Self::edit_intent`].
    fn flag_target(&self) -> FlagTarget {
        match self.selected_reply.and_then(|i| self.replies.get(i)) {
            Some(reply) => FlagTarget::Reply(reply.reply_id.clone()),
            None => FlagTarget::Entry(self.entry.post_id.clone()),
        }
    }

    /// The publish time of whatever `e` would edit, which is `None` whenever the
    /// server did not send one. Only [`within_edit_window`] reads it.
    fn focused_created_at(&self) -> Option<OffsetDateTime> {
        match self.selected_reply.and_then(|i| self.replies.get(i)) {
            Some(reply) => reply.created_at,
            None => self.entry.created_at,
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
                theme.heading_style(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // The flag-reason field takes a row of its own between the body and the
        // status line, and gives it back the moment the prompt closes.
        let prompt_open = self.flag_prompt.is_some();
        let constraints: Vec<Constraint> = if prompt_open {
            vec![
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ]
        } else {
            vec![Constraint::Min(1), Constraint::Length(1)]
        };
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);
        let (body_area, prompt_area, status_area) = if prompt_open {
            (layout[0], Some(layout[1]), layout[2])
        } else {
            (layout[0], None, layout[1])
        };

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

        // OSC 8 hyperlinks make URLs in the body clickable even when long enough
        // to wrap (which defeats the terminal's own URL detection); off falls back
        // to plain text. Independent of graphics support.
        let hyperlinks_on = crate::config::get().hyperlinks;
        let lines = self.compose_body(theme, inline_images, img_rows);

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
        let mut anchors: Vec<u16> = Vec::with_capacity(reply_starts.len());
        let mut slot_offsets: Vec<u32> = Vec::with_capacity(slots.len());
        let mut acc: u32 = 0;
        let mut si = 0;
        let mut sj = 0;
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
        drop(reply_starts);
        *self.reply_anchors.borrow_mut() = anchors;
        let wrapped_rows = acc;
        let max_scroll = wrapped_rows
            .saturating_sub(u32::from(text_area.height))
            .min(u32::from(u16::MAX)) as u16;
        self.max_scroll.set(max_scroll);
        let scroll = self.scroll.min(max_scroll);

        // Find every URL in the body before the paragraph consumes the lines, so
        // `render` can overlay OSC 8 hyperlinks once the glyphs are on screen.
        let link_targets = if hyperlinks_on {
            super::hyperlink::find_link_targets(&lines, text_area.width)
        } else {
            Vec::new()
        };

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

        // Overlay an OSC 8 hyperlink onto every URL the paragraph painted that's
        // in view — markdown links, image/jukebox URLs, raw URLs alike. The link
        // covers the visible glyphs with the full URL as the target, so it works
        // even when a long URL wraps. Independent of graphics — any terminal.
        if hyperlinks_on {
            super::hyperlink::apply_link_targets(
                frame.buffer_mut(),
                text_area,
                scroll,
                &link_targets,
            );
        }

        // The reason field itself, windowed so a long reason keeps its caret in
        // view rather than running off the row.
        if let (Some(prompt), Some(prompt_area)) = (self.flag_prompt.as_ref(), prompt_area) {
            let field_width =
                (prompt_area.width as usize).saturating_sub(FLAG_PROMPT_LABEL.chars().count());
            let mut spans = vec![Span::styled(FLAG_PROMPT_LABEL, theme.muted_style())];
            spans.extend(
                super::input::windowed_line(&prompt.reason, prompt.cursor, field_width, theme)
                    .spans,
            );
            frame.render_widget(Paragraph::new(Line::from(spans)), prompt_area);
        }

        // Surface the jukebox keys only when there's a track to act on.
        let open_hint = if self.jukebox_url().is_some() {
            " · p play · o open"
        } else {
            ""
        };
        let watch_hint = if self.watching == Some(true) {
            " · w unwatch"
        } else {
            " · w watch"
        };
        // A soft hint, never a gate: it says when the focused item is certainly
        // past the server's 5-minute edit window, but the key stays on offer and
        // a missing `created_at` reads as "still open".
        let edit_hint = if within_edit_window(self.focused_created_at()) {
            " · e edit"
        } else {
            " · e edit (5m passed)"
        };
        let status_text = if let Some(prompt) = &self.flag_prompt {
            format!(
                "report {} · enter send · esc cancel · reason optional · {}/{MAX_FLAG_REASON}",
                prompt.target.label(),
                prompt.reason.chars().count()
            )
        } else if self.confirming_delete {
            "really delete this post? y=yes, any other key=cancel".to_string()
        } else if self.loading_replies && self.replies.is_empty() {
            "loading replies… · esc back".to_string()
        } else if let Some(msg) = &self.error {
            format!("error: {msg} · esc back · r retry")
        } else if self.next_replies_cursor.is_some() {
            format!(
                "{} replies · scroll down for more · esc back · J/K select reply · R reply · Q quote · b bookmark{open_hint}{watch_hint}{edit_hint} · F flag · d delete · r refresh",
                self.replies.len()
            )
        } else {
            format!(
                "{} replies · end · esc back · J/K select reply · R reply · Q quote · b bookmark{open_hint}{watch_hint}{edit_hint} · F flag · d delete · r refresh",
                self.replies.len()
            )
        };
        let status = Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style())));
        frame.render_widget(status, status_area);
    }

    /// Build the scrollable body. When `inline_images` is set, image URLs are
    /// suppressed (the image is drawn as graphics) and a blank-row gap of
    /// `img_rows` is reserved at each image's position for `render` to overlay;
    /// otherwise image URLs are surfaced as text and no gaps are reserved. Any
    /// URL in the result is made clickable by `render` via
    /// [`super::hyperlink::find_link_targets`].
    fn compose_body(&self, theme: &Theme, inline_images: bool, img_rows: u16) -> Vec<Line<'_>> {
        let mut lines = Vec::new();
        let mut slots: Vec<ImageSlot> = Vec::new();
        let image_urls = if inline_images {
            ImageUrls::Hide
        } else {
            ImageUrls::Show
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
        // v0.8.4: an entry the author has since corrected carries `editedAt`, and
        // says so next to its timestamp.
        let edited = edited_marker(self.entry.edited_at);
        lines.push(Line::from(vec![
            Span::styled(
                format!("@{}", self.entry.author_username),
                theme.accent_style(),
            ),
            Span::styled(format!(" · {when}{topics}{edited}"), theme.muted_style()),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "{} replies · {} bookmarks{}{}",
                self.entry.replies_count,
                self.entry.bookmarks_count,
                if self.entry.is_nsfw { " · NSFW" } else { "" },
                if self.watching == Some(true) {
                    " · ★ watching"
                } else {
                    ""
                }
            ),
            theme.muted_style(),
        )));
        lines.push(Line::from(""));

        // Body — rendered with pulldown-cmark (markdown + @mention highlighting).
        // When images are drawn inline, the image URL is hidden here (the graphic
        // appears in the reserved gap below); otherwise it's surfaced as text.
        // Links always surface their URL.
        for md_line in render_markdown_with(&self.entry.content, theme, image_urls) {
            lines.push(md_line);
        }

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
            let edited = edited_marker(reply.edited_at);
            // The selected reply's header is marked to match the list style:
            // `fill` tints it with the selection background, `bar` reverse-videos
            // it (the older look).
            let author_style = if selected {
                match crate::config::get().selection {
                    crate::config::SelectionStyle::Fill => theme.selection_style(),
                    crate::config::SelectionStyle::Bar => {
                        theme.accent_style().add_modifier(Modifier::REVERSED)
                    }
                }
            } else {
                theme.accent_style()
            };
            lines.push(Line::from(vec![
                Span::styled(format!("@{}", reply.author_username), author_style),
                Span::styled(format!(" · {when}{parent}{edited}"), theme.muted_style()),
            ]));
            // Reply body — markdown-rendered. Highlight overrides via the loop below.
            for md_line in render_markdown_with(&reply.content, theme, image_urls) {
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
            edited_at: None,
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
            edited_at: None,
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

    #[test]
    fn w_toggles_watch_at_thread_level_even_with_a_reply_selected() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], None)));

        // No selection → w is a thread-level toggle.
        assert_eq!(
            s.handle_key(key(KeyCode::Char('w'))),
            PostDetailIntent::ToggleWatch
        );

        // Selecting a reply doesn't retarget w (unlike b, which targets the reply).
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(s.selected_reply, Some(0));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('w'))),
            PostDetailIntent::ToggleWatch
        );
    }

    #[test]
    fn watching_indicator_shows_only_when_watching() {
        let mut s = PostDetailScreen::new(entry("p1"));
        // Unknown state: no indicator.
        assert!(!body_text(&s).iter().any(|l| l.contains("watching")));
        // Watching: the meta line gains the indicator.
        s.set_watching(true);
        assert!(body_text(&s).iter().any(|l| l.contains("★ watching")));
        // Not watching: indicator hidden again.
        s.set_watching(false);
        assert!(!body_text(&s).iter().any(|l| l.contains("watching")));
    }

    /// Flatten the body into per-line strings. `inline_images` mirrors the render
    /// path: when set, image URLs are suppressed (the graphic is drawn in a gap);
    /// when clear, image URLs are surfaced as text.
    fn body_text_mode(s: &PostDetailScreen, inline_images: bool) -> Vec<String> {
        let theme = Theme::cyber();
        let img_rows = if inline_images { 6 } else { 0 };
        s.compose_body(&theme, inline_images, img_rows)
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
        let lines = s.compose_body(&Theme::dark(), false, 0);
        // Look for the replies separator marker.
        let body_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.as_ref()))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(body_text.contains("replies"));
        assert!(body_text.contains("@bob"));
    }

    /// A screen whose first reply fetch has settled, so the status line shows
    /// the browse hints instead of "loading replies…".
    fn settled(e: Entry) -> PostDetailScreen {
        let mut s = PostDetailScreen::new(e);
        s.apply_replies_initial(Ok((Vec::new(), None)));
        s
    }

    /// The screen as one string per terminal row, for asserting on the reason
    /// prompt and the status line, neither of which `compose_body` produces.
    fn rendered_rows(s: &PostDetailScreen, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber(), false, None))
            .expect("draw");
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn e_edits_the_post_when_nothing_is_selected_and_the_reply_when_one_is() {
        let mut e = entry("p1");
        e.title = Some("Headline".into());
        e.is_public = true;
        let mut s = PostDetailScreen::new(e);
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], None)));

        // No selection → the post, carrying every editable field.
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'))),
            PostDetailIntent::EditEntry {
                post_id: "p1".into(),
                content: "hello\nworld".into(),
                title: Some("Headline".into()),
                topics: vec!["music".into()],
                is_public: true,
                is_nsfw: false,
            }
        );

        // Selecting a reply retargets `e`, exactly as it retargets `b`.
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'))),
            PostDetailIntent::EditReply {
                reply_id: "r1".into(),
                content: "reply r1".into(),
            }
        );
    }

    #[test]
    fn e_is_offered_on_a_post_far_past_the_edit_window() {
        // The 5-minute window is the server's to enforce; the client offers the
        // key regardless and lets the 403 speak, the way `d` already does.
        let mut e = entry("p1");
        e.created_at = Some(OffsetDateTime::now_utc() - time::Duration::hours(3));
        let mut s = PostDetailScreen::new(e);
        assert!(matches!(
            s.handle_key(key(KeyCode::Char('e'))),
            PostDetailIntent::EditEntry { .. }
        ));
    }

    #[test]
    fn f_prompts_for_a_reason_and_enter_files_the_report() {
        let mut s = PostDetailScreen::new(entry("p1"));
        // `F` files nothing on its own, it opens the prompt.
        assert_eq!(
            s.handle_key(key(KeyCode::Char('F'))),
            PostDetailIntent::None
        );
        assert!(s.is_text_input(), "the prompt owns the keyboard");
        for c in "spam".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        s.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            PostDetailIntent::FlagEntry {
                post_id: "p1".into(),
                reason: Some("spa".into()),
            }
        );
        assert!(!s.is_text_input(), "submitting closes the prompt");
    }

    #[test]
    fn f_reports_the_selected_reply() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1"), reply("r2", "p1")], None)));
        s.handle_key(key(KeyCode::Char('J')));
        s.handle_key(key(KeyCode::Char('J')));
        assert_eq!(s.selected_reply, Some(1));

        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            PostDetailIntent::FlagReply {
                reply_id: "r2".into(),
                reason: None,
            }
        );
    }

    #[test]
    fn a_blank_reason_files_the_report_without_one() {
        // The reason is optional, so an empty submit is valid and travels as
        // `None` rather than as an empty string.
        let mut s = PostDetailScreen::new(entry("p1"));
        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            PostDetailIntent::FlagEntry {
                post_id: "p1".into(),
                reason: None,
            }
        );

        // So does a reason that is nothing but spaces.
        s.handle_key(key(KeyCode::Char('F')));
        s.handle_key(key(KeyCode::Char(' ')));
        s.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            PostDetailIntent::FlagEntry {
                post_id: "p1".into(),
                reason: None,
            }
        );
    }

    #[test]
    fn the_reason_stops_at_the_servers_character_cap() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.handle_key(key(KeyCode::Char('F')));
        for _ in 0..(MAX_FLAG_REASON + 50) {
            s.handle_key(key(KeyCode::Char('x')));
        }
        match s.handle_key(key(KeyCode::Enter)) {
            PostDetailIntent::FlagEntry {
                reason: Some(reason),
                ..
            } => assert_eq!(
                reason.chars().count(),
                MAX_FLAG_REASON,
                "typing stops at the cap instead of building a body the API rejects"
            ),
            other => panic!("expected a reasoned FlagEntry, got {other:?}"),
        }
    }

    #[test]
    fn a_reason_can_be_pasted_and_edited_at_the_caret() {
        // The shared field gives this screen the same editing the feeds have:
        // bracketed paste (newlines collapsed so it cannot submit) and caret
        // keys, not just append-and-backspace.
        let mut s = PostDetailScreen::new(entry("p1"));
        s.paste_text("dropped"); // no prompt open yet
        assert!(!s.is_text_input());

        s.handle_key(key(KeyCode::Char('F')));
        s.paste_text("copy\npasted");
        s.handle_key(key(KeyCode::Home));
        for c in "why: ".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            PostDetailIntent::FlagEntry {
                post_id: "p1".into(),
                reason: Some("why: copy pasted".into()),
            }
        );
    }

    #[test]
    fn esc_and_cancel_flag_prompt_both_abandon_the_report() {
        let mut s = PostDetailScreen::new(entry("p1"));

        // Esc reaching the screen closes the prompt and files nothing.
        s.handle_key(key(KeyCode::Char('F')));
        assert_eq!(s.handle_key(key(KeyCode::Esc)), PostDetailIntent::None);
        assert!(!s.is_text_input());

        // The app's Esc hook does the same, and says whether it acted so a
        // second Esc can keep its usual "go back" meaning.
        s.handle_key(key(KeyCode::Char('F')));
        assert!(s.cancel_flag_prompt(), "closed the open prompt");
        assert!(!s.cancel_flag_prompt(), "nothing left to close");
        assert!(!s.is_text_input());
    }

    #[test]
    fn keys_typed_into_a_reason_never_reach_the_screens_bindings() {
        // Every bare letter is text while the prompt is open, so selecting,
        // deleting and bookmarking all stay out of the way.
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], None)));
        s.handle_key(key(KeyCode::Char('F')));
        for c in "Jdyb".chars() {
            assert_eq!(s.handle_key(key(KeyCode::Char(c))), PostDetailIntent::None);
        }
        assert_eq!(s.selected_reply, None, "J did not move the selection");
        assert!(!s.confirming_delete, "d did not arm a delete");
        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            PostDetailIntent::FlagEntry {
                post_id: "p1".into(),
                reason: Some("Jdyb".into()),
            }
        );
    }

    #[test]
    fn is_text_input_is_false_until_the_prompt_opens() {
        let mut s = PostDetailScreen::new(entry("p1"));
        assert!(!s.is_text_input(), "browsing captures no text");
        s.handle_key(key(KeyCode::Char('F')));
        assert!(s.is_text_input());
    }

    #[test]
    fn the_edited_marker_shows_only_on_edited_items() {
        let mut e = entry("p1");
        let mut edited_reply = reply("r1", "p1");
        edited_reply.edited_at = Some(OffsetDateTime::now_utc());

        // Nothing edited: no marker anywhere.
        let mut s = PostDetailScreen::new(e.clone());
        s.apply_replies_initial(Ok((vec![reply("r0", "p1")], None)));
        assert!(
            !body_text(&s).iter().any(|l| l.contains("(edited)")),
            "an untouched post and reply say nothing"
        );

        // The entry's own `editedAt` marks its author line.
        e.edited_at = Some(OffsetDateTime::now_utc());
        let mut s = PostDetailScreen::new(e);
        s.apply_replies_initial(Ok((vec![reply("r0", "p1"), edited_reply], None)));
        let body = body_text(&s);
        assert!(
            body.iter()
                .any(|l| l.starts_with("@alice") && l.contains("(edited)")),
            "the entry header carries the marker: {body:?}"
        );
        // Exactly one of the two replies is marked.
        assert_eq!(
            body.iter()
                .filter(|l| l.starts_with("@bob") && l.contains("(edited)"))
                .count(),
            1,
            "only the edited reply is marked: {body:?}"
        );
    }

    #[test]
    fn apply_entry_edit_applies_only_the_fields_it_carries() {
        let mut e = entry("p1");
        e.title = Some("Keep Me".into());
        e.created_at = Some(OffsetDateTime::now_utc());
        let created = e.created_at;
        let mut s = PostDetailScreen::new(e);

        s.apply_entry_edit(&EntryEdit {
            content: Some("corrected".into()),
            topics: Some(vec!["rust".into()]),
            is_nsfw: Some(true),
            ..EntryEdit::default()
        });

        assert_eq!(s.entry.content, "corrected");
        assert_eq!(s.entry.topics, vec!["rust".to_string()]);
        assert!(s.entry.is_nsfw);
        assert_eq!(
            s.entry.title.as_deref(),
            Some("Keep Me"),
            "an omitted field is left alone"
        );
        assert_eq!(s.entry.created_at, created, "createdAt never changes");
        assert!(s.entry.edited_at.is_some(), "the marker appears at once");
        assert!(body_text(&s).iter().any(|l| l.contains("(edited)")));
    }

    #[test]
    fn apply_entry_edit_removes_a_title_and_sets_a_new_one() {
        let mut e = entry("p1");
        e.title = Some("Old".into());
        let mut s = PostDetailScreen::new(e);

        s.apply_entry_edit(&EntryEdit {
            title: Some(cs_api::TitleEdit::Set("New".into())),
            ..EntryEdit::default()
        });
        assert_eq!(s.entry.title.as_deref(), Some("New"));

        s.apply_entry_edit(&EntryEdit {
            title: Some(cs_api::TitleEdit::Remove),
            ..EntryEdit::default()
        });
        assert_eq!(s.entry.title, None, "an empty title is a removal");
    }

    #[test]
    fn apply_reply_edit_touches_only_the_matching_reply() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1"), reply("r2", "p1")], None)));

        assert!(s.apply_reply_edit("r2", "corrected".into()));
        assert_eq!(s.replies[1].content, "corrected");
        assert!(s.replies[1].edited_at.is_some());
        assert_eq!(
            s.replies[0].content, "reply r1",
            "the neighbour is untouched"
        );
        assert!(s.replies[0].edited_at.is_none());

        assert!(
            !s.apply_reply_edit("nope", "x".into()),
            "a reply that isn't on this page reports back as a miss"
        );
    }

    #[test]
    fn the_reason_prompt_renders_its_label_the_typed_text_and_its_keys() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.handle_key(key(KeyCode::Char('F')));
        for c in "bot".chars() {
            s.handle_key(key(KeyCode::Char(c)));
        }
        let rows = rendered_rows(&s, 80, 12);
        assert!(
            rows.iter().any(|r| r.contains("reason (optional): bot")),
            "the field shows the label and what was typed: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("report this post") && r.contains("enter send")),
            "the status line names the target and the keys: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("3/500")),
            "and how much of the reason budget is spent: {rows:?}"
        );
    }

    #[test]
    fn the_status_line_notes_a_closed_edit_window_but_still_offers_the_key() {
        // Fresh post: the plain hint.
        let mut e = entry("p1");
        e.created_at = Some(OffsetDateTime::now_utc());
        let rows = rendered_rows(&settled(e), 200, 10);
        let status = rows.join("\n");
        assert!(status.contains("e edit"), "the key is offered: {status:?}");
        assert!(!status.contains("5m passed"), "no note yet: {status:?}");
        assert!(status.contains("F flag"), "so is flagging: {status:?}");

        // Old post: the hint says the window has passed, and still offers `e`.
        let mut e = entry("p1");
        e.created_at = Some(OffsetDateTime::now_utc() - time::Duration::hours(1));
        let rows = rendered_rows(&settled(e), 200, 10);
        let status = rows.join("\n");
        assert!(
            status.contains("e edit (5m passed)"),
            "a soft hint, not a removal: {status:?}"
        );

        // Unknown publish time: fail open, no discouraging note.
        let rows = rendered_rows(&settled(entry("p1")), 200, 10);
        let status = rows.join("\n");
        assert!(status.contains("e edit"));
        assert!(
            !status.contains("5m passed"),
            "a timestamp we never received can't close the window: {status:?}"
        );
    }

    #[test]
    fn within_edit_window_fails_open_and_expires_on_a_known_timestamp() {
        assert!(
            within_edit_window(None),
            "no timestamp means the server decides"
        );
        assert!(within_edit_window(Some(OffsetDateTime::now_utc())));
        assert!(within_edit_window(Some(
            OffsetDateTime::now_utc() - time::Duration::seconds(299)
        )));
        assert!(!within_edit_window(Some(
            OffsetDateTime::now_utc() - time::Duration::seconds(301)
        )));
    }
}

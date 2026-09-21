//! Compose screen — entry or reply, with $EDITOR for body content and an
//! inline confirmation step.
//!
//! Entries and replies may carry one jukebox track (v0.8.10 § Create Entry,
//! § Create Reply). It is typed as one `url | artist | title | genre` line
//! rather than four fields, which is the shape the `/song` command already uses
//! in chat, so there is one syntax to learn instead of two. Images are not
//! offered at all: v0.8.10 makes them website-uploaded and inline in the body,
//! so there is nothing for this screen to attach.
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use cs_api::{
    Attachment, Entry, EntryEdit, Reply, TitleEdit, MAX_AUDIO_ARTIST_LEN, MAX_AUDIO_GENRE_LEN,
    MAX_AUDIO_TITLE_LEN,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::theme::Theme;

/// What an entry looked like when an edit began (v0.8.4 § Edit Entry).
///
/// The patch is a diff against this snapshot, because "every field is optional;
/// only what you send changes": a field the user never touched is left out of
/// the request entirely and the server keeps it. Without the snapshot there is
/// no way to tell "the title was already blank" from "the user cleared a title
/// that was set", and those are different operations on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntrySnapshot {
    /// Body as published, so a body the user did not re-edit is not re-sent.
    pub content: String,
    /// Title as published. `None` means the entry has no title.
    pub title: Option<String>,
    /// Topic list as published. An edit replaces this list wholesale.
    pub topics: Vec<String>,
    /// Whether the entry is readable without logging in.
    pub is_public: bool,
    /// Content-warning flag.
    pub is_nsfw: bool,
    /// The entry's URL slug. Frozen once published so share links keep working,
    /// so this is shown read-only and never sent.
    pub slug: Option<String>,
    /// Attachments as published. An edit replaces the list wholesale
    /// (§ Edit Entry), so the patch needs to know what was there to tell
    /// "unchanged" from "cleared".
    pub attachments: Vec<Attachment>,
}

impl EntrySnapshot {
    /// Take the snapshot from the entry about to be edited.
    pub fn from_entry(entry: &Entry) -> Self {
        Self {
            content: entry.content.clone(),
            title: entry.title.clone(),
            topics: entry.topics.clone(),
            is_public: entry.is_public,
            is_nsfw: entry.is_nsfw,
            slug: entry.slug.clone(),
            attachments: entry.attachments.clone(),
        }
    }

    /// The published track as a `url | artist | title | genre` line, for
    /// prefilling the field.
    fn track_line(&self) -> String {
        match self.attachments.first() {
            Some(Attachment::Audio {
                src,
                artist,
                title,
                genre,
                ..
            }) => format!("{src} | {artist} | {title} | {genre}"),
            _ => String::new(),
        }
    }

    /// Whether this screen may rewrite the entry's attachments.
    ///
    /// No, once the entry carries something this screen cannot express. Two
    /// cases, and the second is the one that is easy to miss:
    ///
    /// - A shape with no field at all: a legacy image, or something added to
    ///   the API after this build.
    /// - An audio attachment whose metadata does not survive the one-line
    ///   `url | artist | title | genre` form. A stored `artist` of `"AC|DC"`
    ///   re-splits into five parts; an uppercase `genre` fails the lowercase
    ///   rule; a `src` the host check does not know fails that. Testing the
    ///   *variant* alone let those through, and the unparseable line then
    ///   refused every submit — § Edit Entry sends one patch for the whole
    ///   entry, so a track it could not read locked out the title, the body and
    ///   the flags too, on an entry the user had five minutes to fix.
    ///
    /// § Edit Entry replaces the whole attachment list, so offering the field
    /// in either case would mean sending a list that silently dropped what is
    /// there — and sending an image back is a `400` anyway. Leaving
    /// `attachments` out of the patch is what keeps it.
    fn track_is_editable(&self) -> bool {
        match self.attachments.as_slice() {
            [] => true,
            [Attachment::Audio { .. }] => {
                matches!(parse_track_line(&self.track_line()), Ok(Some(_)))
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeKind {
    NewEntry,
    Reply {
        post_id: String,
        parent_reply_id: Option<String>,
    },
    NewNote,
    UpdateNote {
        note_id: String,
    },
    /// A new thread in a guild's forum.
    GuildThread {
        guild_slug: String,
    },
    /// Editing an entry that is already published (v0.8.4 § Edit Entry).
    ///
    /// `original` is what the entry looked like when the edit began; it rides
    /// along in the kind so that any construction path, including a plain
    /// [`ComposeScreen::new`], prefills the fields and diffs against them.
    EditEntry {
        post_id: String,
        original: EntrySnapshot,
    },
    /// Editing a reply that is already posted (v0.8.4 § Edit Reply). Content is
    /// the only editable field there, and it is required.
    EditReply {
        reply_id: String,
        original_content: String,
    },
}

impl ComposeKind {
    /// The kind for editing `entry` (v0.8.4 § Edit Entry), snapshot included.
    /// Build it this way rather than by hand: the snapshot is what every field
    /// prefills from and what the patch is diffed against.
    pub fn edit_entry(entry: &Entry) -> Self {
        Self::EditEntry {
            post_id: entry.post_id.clone(),
            original: EntrySnapshot::from_entry(entry),
        }
    }

    /// The kind for editing `reply` (v0.8.4 § Edit Reply), carrying the posted
    /// content so an edit that changes nothing can be refused.
    pub fn edit_reply(reply: &Reply) -> Self {
        Self::EditReply {
            reply_id: reply.reply_id.clone(),
            original_content: reply.content.clone(),
        }
    }

    fn has_topics(&self) -> bool {
        matches!(
            self,
            Self::NewEntry
                | Self::NewNote
                | Self::UpdateNote { .. }
                | Self::GuildThread { .. }
                | Self::EditEntry { .. }
        )
    }

    /// Public/NSFW flags apply to top-level entries only — not guild threads,
    /// replies, or notes.
    fn has_visibility_toggles(&self) -> bool {
        matches!(self, Self::NewEntry | Self::EditEntry { .. })
    }

    /// Titles are valid on top-level entries, guild threads, and entry edits.
    fn has_title(&self) -> bool {
        matches!(
            self,
            Self::NewEntry | Self::GuildThread { .. } | Self::EditEntry { .. }
        )
    }

    /// Whether this kind may carry a jukebox track.
    ///
    /// Entries, replies and entry edits (§ Create Entry, § Create Reply, § Edit
    /// Entry). Not notes, which are not posts; not guild threads, whose
    /// documented body has no `attachments` field; and not a reply edit, where
    /// content is the only thing § Edit Reply accepts.
    fn has_attachment(&self) -> bool {
        match self {
            Self::NewEntry | Self::Reply { .. } => true,
            Self::EditEntry { original, .. } => original.track_is_editable(),
            Self::NewNote
            | Self::UpdateNote { .. }
            | Self::GuildThread { .. }
            | Self::EditReply { .. } => false,
        }
    }

    /// The fields Tab cycles through, in order. Empty for a kind with nothing
    /// to focus.
    ///
    /// One list, read by both [`ComposeScreen::new`] (which starts on its first
    /// entry) and `cycle_focus`. Keeping them apart is how a screen ends up
    /// opening focused on a field its own Tab order does not contain, which is
    /// exactly what a reply became the moment it gained a track field.
    fn focus_order(&self) -> &'static [ConfirmField] {
        match self {
            Self::NewEntry => &[
                ConfirmField::Title,
                ConfirmField::Slug,
                ConfirmField::Topics,
                ConfirmField::Track,
                ConfirmField::Public,
                ConfirmField::Nsfw,
            ],
            Self::NewNote | Self::UpdateNote { .. } => &[ConfirmField::Topics],
            Self::GuildThread { .. } => &[
                ConfirmField::Title,
                ConfirmField::Slug,
                ConfirmField::Topics,
            ],
            // No Slug: an edit cannot change it (§ Edit Entry). The track is
            // offered only while the entry carries nothing this screen cannot
            // express; `has_attachment` is what decides.
            Self::EditEntry { .. } if self.has_attachment() => &[
                ConfirmField::Title,
                ConfirmField::Topics,
                ConfirmField::Track,
                ConfirmField::Public,
                ConfirmField::Nsfw,
            ],
            Self::EditEntry { .. } => &[
                ConfirmField::Title,
                ConfirmField::Topics,
                ConfirmField::Public,
                ConfirmField::Nsfw,
            ],
            // A reply has no topics or flags, but since v0.8.10 it may carry a
            // track, so it is no longer a screen with nothing to focus.
            Self::Reply { .. } => &[ConfirmField::Track],
            Self::EditReply { .. } => &[],
        }
    }

    /// Why the track field is missing on an entry edit that has an attachment
    /// this screen cannot express, so the absence reads as deliberate.
    fn locked_attachment_note(&self) -> Option<&'static str> {
        match self {
            Self::EditEntry { original, .. } if !original.track_is_editable() => {
                Some("this post's attachment can't be edited here, and is kept as it is")
            }
            _ => None,
        }
    }

    /// A custom per-author URL slug is accepted on entries and guild threads.
    /// Never on an edit: § Edit Entry freezes the slug at publish time and
    /// answers `400` to one that is sent anyway.
    fn has_slug(&self) -> bool {
        matches!(self, Self::NewEntry | Self::GuildThread { .. })
    }

    /// The published slug to show read-only, so the user can see why it is not
    /// editable. `None` for every kind that can still choose one.
    fn frozen_slug(&self) -> Option<&str> {
        match self {
            Self::EditEntry { original, .. } => original.slug.as_deref(),
            _ => None,
        }
    }

    /// The pre-edit entry values, for the one kind that carries them.
    fn entry_original(&self) -> Option<&EntrySnapshot> {
        match self {
            Self::EditEntry { original, .. } => Some(original),
            _ => None,
        }
    }

    /// Whether this kind changes something that already exists on the server,
    /// so the screen can say "delete it" instead of "post it" when the body is
    /// blank, and refuse a submit that would change nothing.
    pub fn is_edit(&self) -> bool {
        matches!(self, Self::EditEntry { .. } | Self::EditReply { .. })
    }

    /// The bordered-box title for this compose kind, shared by the confirm
    /// screen and the built-in editor.
    pub fn title(&self) -> String {
        match self {
            Self::NewEntry => " cs-tui • new post ".to_string(),
            Self::Reply { post_id, .. } => format!(" cs-tui • reply to {post_id} "),
            Self::NewNote => " cs-tui • new note ".to_string(),
            Self::UpdateNote { note_id } => format!(" cs-tui • edit note {note_id} "),
            Self::GuildThread { guild_slug } => {
                format!(" cs-tui • new thread in {guild_slug} ")
            }
            Self::EditEntry { post_id, .. } => format!(" cs-tui • edit post {post_id} "),
            Self::EditReply { reply_id, .. } => format!(" cs-tui • edit reply {reply_id} "),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmField {
    Title,
    Slug,
    Topics,
    /// The one jukebox track an entry or reply may carry (§ Create Entry).
    Track,
    Public,
    Nsfw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeIntent {
    Quit,
    /// User confirmed: send to the API.
    Submit,
    /// Re-open `$EDITOR` on the current body (Ctrl+E).
    Edit,
    None,
}

#[derive(Debug)]
pub struct ComposeScreen {
    pub kind: ComposeKind,
    pub content: String,
    pub title_input: String,
    pub slug_input: String,
    pub topics_input: String,
    /// The jukebox track as one `url | artist | title | genre` line. Blank means
    /// no track; on an edit, blank where there was one removes it.
    pub track_input: String,
    pub is_public: bool,
    pub is_nsfw: bool,
    pub focused: ConfirmField,
    pub submitting: bool,
    pub error: Option<String>,
}

impl ComposeScreen {
    /// Build the confirm screen for `kind` over an already-composed body.
    ///
    /// A create kind starts blank. An edit kind prefills every editable field
    /// from the snapshot its kind carries, and it does so here rather than in a
    /// separate constructor so no caller can produce a half-filled edit screen:
    /// the patch is a diff against those values, so a blank title field on a
    /// titled entry would read as "remove the title" and a false `is_public` on
    /// a public entry would read as "make it private".
    pub fn new(kind: ComposeKind, content: String) -> Self {
        let focused = kind
            .focus_order()
            .first()
            .copied()
            .unwrap_or(ConfirmField::Topics);
        let (title_input, slug_input, topics_input, track_input, is_public, is_nsfw) =
            match kind.entry_original() {
                Some(original) => (
                    original.title.clone().unwrap_or_default(),
                    original.slug.clone().unwrap_or_default(),
                    original.topics.join(", "),
                    original.track_line(),
                    original.is_public,
                    original.is_nsfw,
                ),
                None => (
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    false,
                    false,
                ),
            };
        Self {
            kind,
            content,
            title_input,
            slug_input,
            topics_input,
            track_input,
            is_public,
            is_nsfw,
            focused,
            submitting: false,
            error: None,
        }
    }

    /// Build the confirm screen for editing a published entry (v0.8.4 § Edit
    /// Entry), with `content` as the body the editor produced.
    ///
    /// Every editable field round-trips from `entry`, and the snapshot the
    /// screen keeps is the entry as published, not `content`, so a body the
    /// user left alone is not re-sent.
    pub fn from_entry(entry: &Entry, content: String) -> Self {
        Self::new(ComposeKind::edit_entry(entry), content)
    }

    /// Build the confirm screen for editing a posted reply (v0.8.4 § Edit
    /// Reply), with `content` as the body the editor produced. Content is the
    /// only field a reply edit can change, so there is nothing else to carry.
    pub fn from_reply(reply: &Reply, content: String) -> Self {
        Self::new(ComposeKind::edit_reply(reply), content)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ComposeIntent {
        if self.submitting {
            return ComposeIntent::None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ComposeIntent::Quit;
        }
        if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.try_submit();
        }
        // Ctrl+E re-opens the editor on the body (plain `e` is a typed char in
        // the title/slug/topics fields, so it must be modified).
        if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ComposeIntent::Edit;
        }
        match key.code {
            KeyCode::Tab => {
                self.cycle_focus(false);
            }
            KeyCode::BackTab => {
                self.cycle_focus(true);
            }
            KeyCode::Enter => {
                return self.try_submit();
            }
            KeyCode::Char(' ') if !self.focused_is_text() => {
                self.toggle_current();
            }
            KeyCode::Backspace => {
                if let Some(field) = self.focused_text_mut() {
                    field.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(field) = self.focused_text_mut() {
                    field.push(c);
                }
            }
            _ => {}
        }
        ComposeIntent::None
    }

    /// Insert bracketed-paste text into the focused single-line field, with
    /// newlines collapsed to spaces (these fields stay single-line, and a stray
    /// pasted newline must not trigger the Enter-to-submit path).
    pub fn paste_into_focused(&mut self, text: &str) {
        if self.submitting {
            return;
        }
        let cleaned = super::input::collapse_newlines(text);
        if let Some(field) = self.focused_text_mut() {
            field.push_str(&cleaned);
        }
    }

    /// Whether the focused field accepts typed characters.
    fn focused_is_text(&self) -> bool {
        matches!(
            self.focused,
            ConfirmField::Title | ConfirmField::Slug | ConfirmField::Topics | ConfirmField::Track
        )
    }

    /// The text buffer for the focused field, if it's a text field.
    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.focused {
            ConfirmField::Title => Some(&mut self.title_input),
            ConfirmField::Slug => Some(&mut self.slug_input),
            ConfirmField::Topics => Some(&mut self.topics_input),
            ConfirmField::Track => Some(&mut self.track_input),
            ConfirmField::Public | ConfirmField::Nsfw => None,
        }
    }

    fn cycle_focus(&mut self, backward: bool) {
        let order = self.kind.focus_order();
        if order.is_empty() {
            return;
        }
        let i = order.iter().position(|f| *f == self.focused).unwrap_or(0);
        let len = order.len();
        let new_i = if backward {
            (i + len - 1) % len
        } else {
            (i + 1) % len
        };
        self.focused = order[new_i];
    }

    fn toggle_current(&mut self) {
        match self.focused {
            ConfirmField::Public => self.is_public = !self.is_public,
            ConfirmField::Nsfw => self.is_nsfw = !self.is_nsfw,
            ConfirmField::Title
            | ConfirmField::Slug
            | ConfirmField::Topics
            | ConfirmField::Track => {}
        }
    }

    /// The slug to send (trimmed; `None` when blank — server auto-generates).
    ///
    /// Always `None` for a kind that cannot choose one, which is what keeps an
    /// entry edit from sending the frozen slug it displays: § Edit Entry answers
    /// `400` to a slug.
    pub fn slug_to_send(&self) -> Option<String> {
        if !self.kind.has_slug() {
            return None;
        }
        let s = self.slug_input.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    /// The `PATCH /v1/posts/:id` body this screen would send (v0.8.4 § Edit
    /// Entry), or `None` when the kind is not an entry edit.
    ///
    /// Only fields the user actually changed are set, since "every field is
    /// optional; only what you send changes". Clearing a title that was set is
    /// the one case where an empty field still sends something: it becomes
    /// [`TitleEdit::Remove`], which puts `""` on the wire and removes the title,
    /// where omitting the field would have kept it.
    ///
    /// `slug` is never sent: it is frozen at publish time. `attachments` is sent
    /// only when the user changed the track, and never at all once the entry
    /// carries an attachment this screen cannot express, so leaving the field
    /// out keeps what is there.
    pub fn entry_edit(&self) -> Option<EntryEdit> {
        let original = self.kind.entry_original()?;
        let mut edit = EntryEdit::default();
        if self.content != original.content {
            edit.content = Some(self.content.clone());
        }
        let title = self.title_input.trim();
        match (title.is_empty(), original.title.as_deref()) {
            // Cleared a title that was set: a removal, not an omission.
            (true, Some(_)) => edit.title = Some(TitleEdit::Remove),
            (true, None) => {}
            (false, previous) => {
                if previous != Some(title) {
                    edit.title = Some(TitleEdit::Set(title.to_string()));
                }
            }
        }
        let topics = self.parse_topics();
        if topics != original.topics {
            edit.topics = Some(topics);
        }
        if self.is_public != original.is_public {
            edit.is_public = Some(self.is_public);
        }
        if self.is_nsfw != original.is_nsfw {
            edit.is_nsfw = Some(self.is_nsfw);
        }
        // Attachments are sent only when this screen can express what the entry
        // carries *and* the user changed it. Leaving the field out is what keeps
        // the current list (§ Edit Entry), which is the right answer both for an
        // untouched track and for the image an older entry may still hold.
        // An untouched field is not a change, whatever it happens to parse to.
        // Comparing the parsed attachment against the stored one instead was
        // wrong in both directions the round-trip is lossy: the parser trims
        // each part, and it fills in the `origin` the server may have omitted,
        // so an edit that changed only the title would re-send `attachments`,
        // rewrite the stored values and stamp `editedAt` for everyone.
        if self.kind.has_attachment() && self.track_input.trim() != original.track_line().trim() {
            let desired: Vec<Attachment> = match self.parse_track() {
                Ok(Some(track)) => vec![track],
                Ok(None) => Vec::new(),
                // Unparseable input is not a removal. `try_submit` refuses the
                // submit outright, and until then the patch must not claim the
                // user asked for the track to go.
                Err(_) => original.attachments.clone(),
            };
            if desired != original.attachments {
                edit.attachments = Some(desired);
            }
        }
        Some(edit)
    }

    /// Whether an edit would actually change anything. A patch with no fields is
    /// a `400` (§ Edit Entry: "Send at least one, or you get a 400") and a reply
    /// edit that re-sends identical content only spends a rate-limit token and
    /// stamps `editedAt`, so both are refused before they leave the client.
    /// Always true for a create kind, which has nothing to compare against.
    fn edit_changes_something(&self) -> bool {
        match &self.kind {
            ComposeKind::EditEntry { .. } => self.entry_edit().is_some_and(|edit| !edit.is_empty()),
            ComposeKind::EditReply {
                original_content, ..
            } => self.content != *original_content,
            _ => true,
        }
    }

    fn try_submit(&mut self) -> ComposeIntent {
        // Blanking the body is not an edit: content is required on a reply edit,
        // and an entry edit that clears it is rejected client-side rather than
        // turning a post into an empty one. Removing what you published is what
        // delete is for, so say so instead of offering to re-edit forever.
        if self.kind.is_edit() && self.content.trim().is_empty() {
            self.error = Some("content cannot be empty · delete it instead".into());
            return ComposeIntent::None;
        }
        if self.content.trim().is_empty() {
            self.error = Some("content is empty — ctrl+e to re-edit · esc to cancel".into());
            return ComposeIntent::None;
        }
        if self.kind.has_title() {
            let t = self.title_input.trim();
            if t.chars().count() > 100 {
                self.error = Some("title must be ≤100 characters".into());
                return ComposeIntent::None;
            }
        }
        if self.kind.has_slug() {
            let s = self.slug_input.trim();
            if !s.is_empty() {
                // § Content Limits: "Entry slug | 60 chars". The form used to
                // pre-check 100 and say so, so a 75-character slug passed the
                // confirm screen and was then refused a step later by cs-api,
                // which has always had it right.
                if s.chars().count() > 60 {
                    self.error = Some("slug must be ≤60 characters".into());
                    return ComposeIntent::None;
                }
                if s.chars()
                    .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '-')
                {
                    self.error = Some("slug must be lowercase a-z, 0-9, or -".into());
                    return ComposeIntent::None;
                }
            }
        }
        if self.kind.has_topics() {
            let parsed = self.parse_topics();
            if parsed.len() > 3 {
                self.error = Some("at most 3 topics allowed".into());
                return ComposeIntent::None;
            }
            for t in &parsed {
                if t.chars()
                    .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_')
                {
                    self.error = Some(format!("topic {t:?} must be lowercase a-z 0-9 _"));
                    return ComposeIntent::None;
                }
            }
        }
        if let Err(msg) = self.parse_track() {
            self.error = Some(msg);
            return ComposeIntent::None;
        }
        if !self.edit_changes_something() {
            self.error = Some("nothing changed · esc to cancel".into());
            return ComposeIntent::None;
        }
        self.submitting = true;
        self.error = None;
        ComposeIntent::Submit
    }

    /// Trimmed title to send, or `None` when empty / not applicable.
    pub fn title_to_send(&self) -> Option<String> {
        if !self.kind.has_title() {
            return None;
        }
        let t = self.title_input.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }

    /// The jukebox track this screen would post, or `None` for no track.
    ///
    /// `Err` carries the message to show the user. Parsing is deliberately here
    /// rather than at submit time alone so the render can show the same error
    /// while they are still typing.
    pub fn parse_track(&self) -> Result<Option<Attachment>, String> {
        if !self.kind.has_attachment() {
            return Ok(None);
        }
        parse_track_line(&self.track_input)
    }

    /// The attachments to post with a new entry or reply.
    ///
    /// Empty for a screen with no track, which is what keeps `attachments` off
    /// the request entirely (§ Create Entry calls the field optional). Invalid
    /// input answers empty too; `try_submit` has already refused to get here.
    pub fn attachments_to_send(&self) -> Vec<Attachment> {
        match self.parse_track() {
            Ok(Some(track)) => vec![track],
            _ => Vec::new(),
        }
    }

    pub fn parse_topics(&self) -> Vec<String> {
        self.topics_input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn finish_submit(&mut self, result: Result<(), String>) {
        self.submitting = false;
        if let Err(msg) = result {
            self.error = Some(msg);
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let title = self.kind.title();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Layout: optional [title label, title input] at top, then body preview,
        // then topics label, topics input, toggles, status.
        let mut constraints: Vec<Constraint> = Vec::new();
        let title_idx = if self.kind.has_title() {
            let idx = constraints.len();
            constraints.push(Constraint::Length(1)); // title label
            constraints.push(Constraint::Length(1)); // title input
            Some(idx)
        } else {
            None
        };
        // An edit shows its slug too, read-only, so the frozen slug is visible
        // rather than mysteriously missing.
        let slug_idx = if self.kind.has_slug() || self.kind.frozen_slug().is_some() {
            let idx = constraints.len();
            constraints.push(Constraint::Length(1)); // slug label
            constraints.push(Constraint::Length(1)); // slug input
            Some(idx)
        } else {
            None
        };
        let body_idx = constraints.len();
        constraints.push(Constraint::Min(3)); // body preview
        let topics_label_idx = constraints.len();
        constraints.push(Constraint::Length(1)); // topics label / placeholder
        constraints.push(Constraint::Length(1)); // topics input / spacer
        let track_idx = if self.kind.has_attachment() {
            let idx = constraints.len();
            constraints.push(Constraint::Length(1)); // track label
            constraints.push(Constraint::Length(1)); // track input
            Some(idx)
        } else {
            None
        };
        // An entry whose attachment this screen cannot express says so on one
        // line, in place of the field it is not offering.
        let locked_note_idx = if track_idx.is_none() && self.kind.locked_attachment_note().is_some()
        {
            let idx = constraints.len();
            constraints.push(Constraint::Length(1));
            Some(idx)
        } else {
            None
        };
        let toggles_idx = constraints.len();
        constraints.push(Constraint::Length(1)); // toggles
        let status_idx = constraints.len();
        constraints.push(Constraint::Length(1)); // status / error

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(constraints)
            .split(inner);

        // Title (entries only)
        if let Some(idx) = title_idx {
            let style = if self.focused == ConfirmField::Title {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            // On an entry that has a title, say what clearing the field does:
            // § Edit Entry reads an empty title as a removal.
            let label = if self
                .kind
                .entry_original()
                .is_some_and(|original| original.title.is_some())
            {
                "title (max 100 chars · clearing it removes the title)"
            } else {
                "title (optional, max 100 chars)"
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(label, style))),
                layout[idx],
            );
            let title_area = layout[idx + 1];
            let title_line = if self.focused == ConfirmField::Title {
                super::input::windowed_line(
                    &self.title_input,
                    self.title_input.chars().count(),
                    title_area.width as usize,
                    theme,
                )
            } else {
                Line::from(Span::styled(self.title_input.clone(), theme.base()))
            };
            frame.render_widget(Paragraph::new(title_line), title_area);
        }

        // Slug (entries / guild threads), or the frozen one an edit displays.
        if let Some(idx) = slug_idx {
            if let Some(frozen) = self.kind.frozen_slug() {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "slug (fixed once published, so share links keep working)",
                        theme.muted_style(),
                    ))),
                    layout[idx],
                );
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        frozen.to_string(),
                        theme.muted_style(),
                    ))),
                    layout[idx + 1],
                );
            } else {
                let style = if self.focused == ConfirmField::Slug {
                    theme.accent_style()
                } else {
                    theme.muted_style()
                };
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "slug (optional · a-z 0-9 - · blank = auto)",
                        style,
                    ))),
                    layout[idx],
                );
                let slug_area = layout[idx + 1];
                let slug_line = if self.focused == ConfirmField::Slug {
                    super::input::windowed_line(
                        &self.slug_input,
                        self.slug_input.chars().count(),
                        slug_area.width as usize,
                        theme,
                    )
                } else {
                    Line::from(Span::styled(self.slug_input.clone(), theme.base()))
                };
                frame.render_widget(Paragraph::new(slug_line), slug_area);
            }
        }

        // Body preview
        let preview = Paragraph::new(self.content.clone())
            .wrap(Wrap { trim: false })
            .style(theme.base());
        frame.render_widget(preview, layout[body_idx]);

        // Topics
        if self.kind.has_topics() {
            let topics_style = if self.focused == ConfirmField::Topics {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "topics (comma-separated, max 3, lowercase)",
                    topics_style,
                ))),
                layout[topics_label_idx],
            );
            let topics_area = layout[topics_label_idx + 1];
            let topics_line = if self.focused == ConfirmField::Topics {
                super::input::windowed_line(
                    &self.topics_input,
                    self.topics_input.chars().count(),
                    topics_area.width as usize,
                    theme,
                )
            } else {
                Line::from(Span::styled(self.topics_input.clone(), theme.base()))
            };
            frame.render_widget(Paragraph::new(topics_line), topics_area);
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "replies cannot have topics or visibility flags",
                    theme.muted_style(),
                ))),
                layout[topics_label_idx],
            );
        }

        // Track (entries, replies, and edits of entries that carry no
        // attachment this screen cannot express).
        if let Some(idx) = track_idx {
            let focused = self.focused == ConfirmField::Track;
            let style = if focused {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "track (optional · youtube url | artist | title | genre)",
                    style,
                ))),
                layout[idx],
            );
            let track_area = layout[idx + 1];
            let track_line = if focused {
                super::input::windowed_line(
                    &self.track_input,
                    self.track_input.chars().count(),
                    track_area.width as usize,
                    theme,
                )
            } else {
                Line::from(Span::styled(self.track_input.clone(), theme.base()))
            };
            frame.render_widget(Paragraph::new(track_line), track_area);
        }
        if let (Some(idx), Some(note)) = (locked_note_idx, self.kind.locked_attachment_note()) {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(note, theme.muted_style()))),
                layout[idx],
            );
        }

        // Toggles (entries only)
        if self.kind.has_visibility_toggles() {
            let public_marker = if self.is_public { "[x]" } else { "[ ]" };
            let nsfw_marker = if self.is_nsfw { "[x]" } else { "[ ]" };
            let public_style = if self.focused == ConfirmField::Public {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            let nsfw_style = if self.focused == ConfirmField::Nsfw {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{public_marker} public"), public_style),
                    Span::raw("   "),
                    Span::styled(format!("{nsfw_marker} NSFW"), nsfw_style),
                ])),
                layout[toggles_idx],
            );
        }

        // Status / error
        let status: Line<'_> = if self.submitting {
            Line::from(Span::styled("submitting…", theme.accent_style()))
        } else if let Some(msg) = &self.error {
            Line::from(Span::styled(msg.clone(), theme.error_style()))
        } else {
            Line::from(Span::styled(
                "tab focus · space toggle · ctrl+e edit body · enter/ctrl+d submit · esc cancel",
                theme.muted_style(),
            ))
        };
        frame.render_widget(Paragraph::new(status), layout[status_idx]);
    }
}

/// Parse a `url | artist | title | genre` line into the one attachment an entry
/// or reply may carry (v0.8.10 § Create Entry).
///
/// The pipe-separated shape mirrors the `/song` command (§ Commands), which is
/// the syntax a cs-tui user already has for this. It differs in one place: on a
/// post all four parts are required, where `/song` lets the genre be omitted.
/// Saying so is the whole reason this refuses a three-part line rather than
/// quietly sending a blank genre for the server to reject.
///
/// `Ok(None)` for a blank line — no track, which is not an error.
fn parse_track_line(input: &str) -> Result<Option<Attachment>, String> {
    let line = input.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let parts: Vec<&str> = line.split('|').map(str::trim).collect();
    let [src, artist, title, genre] = parts.as_slice() else {
        return Err(format!(
            "track needs url | artist | title | genre ({} part{} given)",
            parts.len(),
            if parts.len() == 1 { "" } else { "s" }
        ));
    };
    for (label, value) in [
        ("url", src),
        ("artist", artist),
        ("title", title),
        ("genre", genre),
    ] {
        if value.is_empty() {
            return Err(format!("track {label} cannot be empty"));
        }
    }
    for (label, value, max) in [
        ("artist", artist, MAX_AUDIO_ARTIST_LEN),
        ("title", title, MAX_AUDIO_TITLE_LEN),
        ("genre", genre, MAX_AUDIO_GENRE_LEN),
    ] {
        if value.chars().count() > max {
            return Err(format!("track {label} must be ≤{max} characters"));
        }
    }
    if genre.chars().any(char::is_uppercase) {
        return Err("track genre must be lowercase".into());
    }
    let track = Attachment::audio(*src, *artist, *title, *genre);
    // The crate owns the rule about what a valid src is; going through it here
    // means the composer and the API layer cannot disagree about it.
    cs_api::validate_write_attachments(std::slice::from_ref(&track))
        .map_err(|e| e.user_message())?;
    Ok(Some(track))
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("editor exited with failure")]
    EditorFailed,
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

/// Suspend the ratatui terminal, run the configured external editor on a
/// tempfile pre-filled with `initial`, restore the terminal, and return the
/// final file contents.
///
/// Only reached when the user set `editor` in their config (the built-in editor
/// is the default). The implicit `$VISUAL`/`$EDITOR` fallback was removed: an
/// environment editor that GUI-forks or is missing silently aborted the compose
/// flow, so shelling out is now opt-in. `nano` remains a last-resort default if
/// the config value is somehow empty.
///
/// This must run on a blocking thread (use `tokio::task::spawn_blocking`) so
/// the tokio runtime stays responsive — but in practice the editor owns the TTY
/// while it's open, so no other terminal I/O happens.
pub fn launch_editor(initial: &str, suffix: &str) -> Result<String, ComposeError> {
    let editor = crate::config::get()
        .editor
        .clone()
        .unwrap_or_else(|| "nano".to_string());
    let path = tmp_compose_path(suffix);
    fs::write(&path, initial)?;

    suspend_terminal()?;
    let status = Command::new(&editor).arg(&path).status();
    let restore_result = resume_terminal();

    let status = status?;
    restore_result?;
    if !status.success() {
        let _ = fs::remove_file(&path);
        return Err(ComposeError::EditorFailed);
    }

    let content = fs::read_to_string(&path)?;
    let _ = fs::remove_file(&path);
    Ok(content)
}

fn tmp_compose_path(suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("cs-tui-compose-{pid}-{nanos}{suffix}"))
}

fn suspend_terminal() -> Result<(), io::Error> {
    let mut out = io::stdout();
    // Hand the external editor a clean terminal: it manages its own bracketed
    // paste, and would otherwise leave ours in an unknown state on exit.
    execute!(out, crossterm::event::DisableBracketedPaste)?;
    execute!(out, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

fn resume_terminal() -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    // Re-assert bracketed paste: most editors reset it on exit, which would
    // otherwise silently break the app's multi-line paste handling for the rest
    // of the session.
    execute!(out, crossterm::event::EnableBracketedPaste)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn parse_topics_splits_and_trims() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.topics_input = "  music ,linux, ,2026 ".into();
        let topics = s.parse_topics();
        assert_eq!(topics, vec!["music", "linux", "2026"]);
    }

    #[test]
    fn empty_content_blocks_submit() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "   ".into());
        let intent = s.handle_key(key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(intent, ComposeIntent::None);
        assert!(s.error.is_some());
        assert!(!s.submitting);
    }

    #[test]
    fn submit_with_valid_content_sets_submitting() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hello".into());
        let intent = s.handle_key(key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(intent, ComposeIntent::Submit);
        assert!(s.submitting);
    }

    #[test]
    fn invalid_topic_rejected() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.topics_input = "Music".into();
        let intent = s.handle_key(key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(intent, ComposeIntent::None);
        assert!(s.error.as_deref().unwrap().contains("Music"));
    }

    #[test]
    fn too_many_topics_rejected() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.topics_input = "a,b,c,d".into();
        let intent = s.handle_key(key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(intent, ComposeIntent::None);
        assert!(s.error.as_deref().unwrap().contains("3"));
    }

    #[test]
    fn ctrl_c_quits() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        let intent = s.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(intent, ComposeIntent::Quit);
    }

    #[test]
    fn space_toggles_public_when_focused() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.focused = ConfirmField::Public;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert!(s.is_public);
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert!(!s.is_public);
    }

    #[test]
    fn typing_appends_to_topics_when_focused() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.focused = ConfirmField::Topics;
        s.handle_key(key(KeyCode::Char('m'), KeyModifiers::empty()));
        s.handle_key(key(KeyCode::Char('u'), KeyModifiers::empty()));
        assert_eq!(s.topics_input, "mu");
    }

    #[test]
    fn tab_cycles_focus_for_entry() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        // New default for entries is Title (v0.3.7+).
        assert_eq!(s.focused, ConfirmField::Title);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Slug);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Topics);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Track, "v0.8.10 § Create Entry");
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Public);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Nsfw);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Title);
    }

    #[test]
    fn slug_input_accepts_typing_and_validates() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.focused = ConfirmField::Slug;
        for c in "my-post".chars() {
            s.handle_key(key(KeyCode::Char(c), KeyModifiers::empty()));
        }
        assert_eq!(s.slug_to_send().as_deref(), Some("my-post"));

        // An invalid slug is rejected at submit with a clear message.
        s.slug_input = "Bad Slug!".into();
        let intent = s.handle_key(key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(intent, ComposeIntent::None);
        assert!(s.error.as_deref().unwrap_or_default().contains("slug"));
    }

    #[test]
    fn ctrl_e_requests_re_edit() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "body".into());
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            ComposeIntent::Edit
        );
    }

    #[test]
    fn plain_e_types_into_a_text_field_not_re_edit() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "body".into());
        s.focused = ConfirmField::Title;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'), KeyModifiers::empty())),
            ComposeIntent::None
        );
        assert_eq!(s.title_input, "e");
    }

    #[test]
    fn title_input_accepts_typing() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        assert_eq!(s.focused, ConfirmField::Title);
        s.handle_key(key(KeyCode::Char('H'), KeyModifiers::empty()));
        s.handle_key(key(KeyCode::Char('i'), KeyModifiers::empty()));
        assert_eq!(s.title_input, "Hi");
        assert_eq!(s.title_to_send().as_deref(), Some("Hi"));
    }

    #[test]
    fn empty_title_sends_none() {
        let s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        assert!(s.title_to_send().is_none());
    }

    #[test]
    fn title_to_send_is_none_for_reply() {
        let mut s = ComposeScreen::new(
            ComposeKind::Reply {
                post_id: "p".into(),
                parent_reply_id: None,
            },
            "hi".into(),
        );
        s.title_input = "Ignored".into();
        assert!(s.title_to_send().is_none());
    }

    #[test]
    fn title_over_100_chars_rejected() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "body".into());
        s.title_input = "x".repeat(101);
        let intent = s.handle_key(key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(intent, ComposeIntent::None);
        assert!(s.error.is_some());
    }

    #[test]
    fn a_reply_has_a_track_field_and_nothing_else() {
        // A reply has no topics or visibility flags, but v0.8.10 § Create Reply
        // gave it the same one attachment an entry takes — so the screen opens
        // on the field it actually has, not on a topics box it will never show.
        let mut s = ComposeScreen::new(
            ComposeKind::Reply {
                post_id: "p1".into(),
                parent_reply_id: None,
            },
            "hi".into(),
        );
        assert_eq!(s.focused, ConfirmField::Track);
        assert!(!s.kind.has_topics());
        // One field, so Tab stays put.
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Track);
    }

    // ---- editing a published entry / reply (v0.8.4 § Edit Entry, § Edit Reply)

    /// A published entry with every editable field set, so a prefill that drops
    /// one is visible.
    fn published_entry(title: Option<&str>) -> Entry {
        Entry {
            post_id: "p1".into(),
            content: "original body".into(),
            title: title.map(str::to_string),
            slug: Some("original-slug".into()),
            topics: vec!["music".into(), "linux".into()],
            is_public: true,
            is_nsfw: true,
            ..Default::default()
        }
    }

    fn published_reply() -> Reply {
        Reply {
            reply_id: "r1".into(),
            post_id: "p1".into(),
            content: "original reply".into(),
            ..Default::default()
        }
    }

    // ---- the jukebox track (v0.8.10 § Create Entry, § Create Reply) --------

    fn track(src: &str) -> Attachment {
        Attachment::audio(src, "Artist", "Title", "synthwave")
    }

    #[test]
    fn a_blank_track_field_is_no_track_rather_than_an_error() {
        let s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        assert_eq!(s.parse_track(), Ok(None));
        assert!(s.attachments_to_send().is_empty());
    }

    #[test]
    fn a_track_line_parses_into_the_one_attachment_the_api_takes() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.track_input = " https://youtu.be/abc | Artist | Title | synthwave ".into();
        assert_eq!(s.parse_track(), Ok(Some(track("https://youtu.be/abc"))));
        assert_eq!(s.attachments_to_send(), vec![track("https://youtu.be/abc")]);
    }

    #[test]
    fn a_track_line_reports_what_is_wrong_with_it() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());

        // Unlike `/song`, a post needs all four parts (§ Create Entry: artist,
        // title and genre "are all required").
        s.track_input = "https://youtu.be/abc | Artist | Title".into();
        assert!(s.parse_track().unwrap_err().contains("url | artist"));

        s.track_input = "https://youtu.be/abc |  | Title | synthwave".into();
        assert!(s.parse_track().unwrap_err().contains("artist"));

        s.track_input = "https://youtu.be/abc | Artist | Title | Synthwave".into();
        assert!(s.parse_track().unwrap_err().contains("lowercase"));

        s.track_input = "https://vimeo.com/1 | Artist | Title | synthwave".into();
        assert!(s
            .parse_track()
            .unwrap_err()
            .to_lowercase()
            .contains("youtube"));

        s.track_input = format!("https://youtu.be/abc | {} | T | g", "a".repeat(101));
        assert!(s.parse_track().unwrap_err().contains("≤100"));
    }

    #[test]
    fn an_unparseable_track_blocks_the_submit() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.track_input = "not a track line".into();
        assert_eq!(
            s.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::None
        );
        assert!(s.error.is_some(), "the reason has to be on screen");
        assert!(!s.submitting);
    }

    #[test]
    fn a_kind_with_no_attachment_field_never_sends_one() {
        // Notes are not posts, and § Create Guild Thread documents no
        // `attachments` field, so a stray value in the buffer must not travel.
        for kind in [
            ComposeKind::NewNote,
            ComposeKind::GuildThread {
                guild_slug: "night-owls".into(),
            },
        ] {
            let mut s = ComposeScreen::new(kind, "hi".into());
            s.track_input = "https://youtu.be/abc | Artist | Title | synthwave".into();
            assert_eq!(s.parse_track(), Ok(None));
            assert!(s.attachments_to_send().is_empty());
        }
    }

    #[test]
    fn an_edit_prefills_the_published_track_and_leaves_it_alone() {
        let mut entry = published_entry(Some("Old Title"));
        entry.attachments = vec![track("https://youtu.be/abc")];
        let content = entry.content.clone();
        let s = ComposeScreen::from_entry(&entry, content);

        assert_eq!(
            s.track_input,
            "https://youtu.be/abc | Artist | Title | synthwave"
        );
        let edit = s.entry_edit().expect("an entry edit");
        assert!(
            edit.attachments.is_none(),
            "an untouched track must not be re-sent: § Edit Entry keeps what it is not told about"
        );
        assert!(edit.is_empty(), "nothing at all changed");
    }

    #[test]
    fn clearing_the_track_field_removes_the_attachment() {
        let mut entry = published_entry(Some("Old Title"));
        entry.attachments = vec![track("https://youtu.be/abc")];
        let content = entry.content.clone();
        let mut s = ComposeScreen::from_entry(&entry, content);

        s.track_input.clear();
        let edit = s.entry_edit().expect("an entry edit");
        assert_eq!(
            edit.attachments,
            Some(Vec::new()),
            "§ Edit Entry: an empty list is how attachments are removed"
        );
    }

    #[test]
    fn changing_the_track_replaces_the_attachment() {
        let mut entry = published_entry(Some("Old Title"));
        entry.attachments = vec![track("https://youtu.be/abc")];
        let content = entry.content.clone();
        let mut s = ComposeScreen::from_entry(&entry, content);

        s.track_input = "https://youtu.be/xyz | Artist | Title | synthwave".into();
        let edit = s.entry_edit().expect("an entry edit");
        assert_eq!(edit.attachments, Some(vec![track("https://youtu.be/xyz")]));
    }

    #[test]
    fn a_broken_track_line_is_not_read_as_a_removal() {
        // Half-typed input must not look like "the user cleared the track": the
        // submit is refused anyway, and a patch built from it would send `[]`.
        let mut entry = published_entry(Some("Old Title"));
        entry.attachments = vec![track("https://youtu.be/abc")];
        let content = entry.content.clone();
        let mut s = ComposeScreen::from_entry(&entry, content);

        s.track_input = "https://youtu.be/xyz | Artist".into();
        let edit = s.entry_edit().expect("an entry edit");
        assert!(edit.attachments.is_none());
    }

    #[test]
    fn metadata_the_one_line_form_cannot_express_makes_the_track_read_only() {
        // The pipe is a separator, so an artist of "AC|DC" re-splits into five
        // parts. Gating the field on the attachment's *variant* let that
        // through, and the unparseable line then refused the whole submit: the
        // title, the body and the flags were all locked out of an entry the
        // user had five minutes to fix.
        for (artist, title, genre) in [
            ("AC|DC", "Thunderstruck", "rock"),
            ("Artist", "Song | Reprise", "rock"),
            ("Artist", "Song", "Synthwave"),
        ] {
            let mut entry = published_entry(Some("Old Title"));
            entry.attachments = vec![Attachment::audio(
                "https://youtu.be/abc",
                artist,
                title,
                genre,
            )];
            let content = entry.content.clone();
            let mut s = ComposeScreen::from_entry(&entry, content);

            assert!(
                !s.kind.has_attachment(),
                "{artist}/{title}/{genre} does not round-trip, so the field must not be offered"
            );
            assert!(s.kind.locked_attachment_note().is_some());

            // And the rest of the entry is still editable.
            s.title_input = "Fixed Title".into();
            assert_eq!(
                s.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
                ComposeIntent::Submit,
                "the title edit must go through"
            );
            let edit = s.entry_edit().expect("an entry edit");
            assert_eq!(edit.title, Some(TitleEdit::Set("Fixed Title".into())));
            assert!(
                edit.attachments.is_none(),
                "and the track it could not express is left exactly where it was"
            );
        }
    }

    #[test]
    fn an_untouched_track_field_is_never_a_change() {
        // The round-trip is lossy in the successful direction too: the parser
        // trims each part and fills in the `origin` the server may omit. Diffing
        // the parsed attachment against the stored one therefore called an
        // untouched field a change, sent `attachments`, rewrote the stored
        // values and stamped `editedAt` for everyone.
        let stored: Vec<Attachment> = serde_json::from_str(
            r#"[{"type":"audio","src":"https://youtu.be/abc","artist":"Bjork ","title":"Army of Me","genre":"trip hop"}]"#,
        )
        .expect("must decode");
        assert!(
            matches!(stored.first(), Some(Attachment::Audio { origin, .. }) if origin.is_empty()),
            "the fixture is an attachment with no origin and a trailing space"
        );

        let mut entry = published_entry(Some("Old Title"));
        entry.attachments = stored;
        let content = entry.content.clone();
        let s = ComposeScreen::from_entry(&entry, content);

        let edit = s.entry_edit().expect("an entry edit");
        assert!(edit.attachments.is_none());
        assert!(edit.is_empty(), "nothing changed, so nothing is sent");
    }

    #[test]
    fn a_reply_posts_the_track_that_was_typed_on_it() {
        // v0.8.10 § Create Reply newly allows this, and nothing covered the
        // path from the reply composer to the request body.
        let mut s = ComposeScreen::new(
            ComposeKind::Reply {
                post_id: "p1".into(),
                parent_reply_id: None,
            },
            "this one".into(),
        );
        s.track_input = "https://youtu.be/abc | Artist | Title | ambient".into();
        assert_eq!(
            s.attachments_to_send(),
            vec![Attachment::audio(
                "https://youtu.be/abc",
                "Artist",
                "Title",
                "ambient"
            )]
        );
    }

    #[test]
    fn the_slug_field_states_the_limit_the_server_actually_enforces() {
        let mut s = ComposeScreen::new(ComposeKind::NewEntry, "hi".into());
        s.slug_input = "a".repeat(61);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::None
        );
        assert!(
            s.error.as_deref().is_some_and(|m| m.contains("60")),
            "{:?}",
            s.error
        );

        s.slug_input = "a".repeat(60);
        s.error = None;
        assert_eq!(
            s.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::Submit
        );
    }

    #[test]
    fn an_entry_carrying_an_image_keeps_it_and_hides_the_field() {
        // v0.8.10 § Create Entry: sending an image back is a 400, and § Edit
        // Entry replaces the whole list — so the only safe patch is one with no
        // `attachments` at all, and the screen has to say why it is not asking.
        let mut entry = published_entry(Some("Old Title"));
        entry.attachments = vec![Attachment::Image {
            src: "https://bunker.cyberspace.online/x.png".into(),
            width: 8,
            height: 8,
        }];
        let content = entry.content.clone();
        let mut s = ComposeScreen::from_entry(&entry, content);

        assert!(!s.kind.has_attachment());
        assert!(s.kind.locked_attachment_note().is_some());
        assert!(
            !s.kind.focus_order().contains(&ConfirmField::Track),
            "a field that cannot be sent must not be focusable"
        );

        // Even with something in the buffer, the patch leaves attachments alone.
        s.track_input = "https://youtu.be/abc | Artist | Title | synthwave".into();
        let edit = s.entry_edit().expect("an entry edit");
        assert!(edit.attachments.is_none());
    }

    /// An edit screen over `published_entry`, with the body unchanged.
    fn entry_edit_screen(title: Option<&str>) -> ComposeScreen {
        let entry = published_entry(title);
        let content = entry.content.clone();
        ComposeScreen::from_entry(&entry, content)
    }

    #[test]
    fn from_entry_round_trips_every_editable_field() {
        let s = entry_edit_screen(Some("Old Title"));
        assert_eq!(s.title_input, "Old Title");
        assert_eq!(s.topics_input, "music, linux");
        assert!(s.is_public);
        assert!(s.is_nsfw);
        assert_eq!(s.slug_input, "original-slug");
        assert_eq!(s.focused, ConfirmField::Title);
        assert_eq!(s.kind.title(), " cs-tui • edit post p1 ");
    }

    #[test]
    fn plain_new_also_prefills_an_edit_kind() {
        // The prefill lives in the constructor every path goes through, so an
        // edit screen built the ordinary way cannot silently wipe a title or
        // flip a public entry private.
        let entry = published_entry(Some("Old Title"));
        let s = ComposeScreen::new(ComposeKind::edit_entry(&entry), "rewritten body".into());
        assert_eq!(s.title_input, "Old Title");
        assert!(s.is_public);
        let edit = s.entry_edit().unwrap();
        assert_eq!(edit.content.as_deref(), Some("rewritten body"));
        assert!(edit.title.is_none(), "an untouched title is left alone");
        assert!(edit.is_public.is_none(), "an untouched flag is left alone");
    }

    #[test]
    fn entry_edit_sends_only_what_changed() {
        let mut s = entry_edit_screen(Some("Old Title"));
        s.content = "corrected body".into();
        let edit = s.entry_edit().unwrap();
        assert_eq!(edit.content.as_deref(), Some("corrected body"));
        assert!(edit.title.is_none());
        assert!(edit.topics.is_none());
        assert!(edit.is_public.is_none());
        assert!(edit.is_nsfw.is_none());
        assert_eq!(
            serde_json::to_string(&edit).unwrap(),
            r#"{"content":"corrected body"}"#
        );
    }

    #[test]
    fn clearing_a_title_removes_it_rather_than_omitting_it() {
        let mut s = entry_edit_screen(Some("Old Title"));
        s.title_input.clear();
        let edit = s.entry_edit().unwrap();
        assert_eq!(edit.title, Some(TitleEdit::Remove));
        // The removal is the empty string, which is what the server reads.
        assert_eq!(serde_json::to_string(&edit).unwrap(), r#"{"title":""}"#);
    }

    #[test]
    fn an_absent_title_left_blank_is_omitted_not_removed() {
        let mut s = entry_edit_screen(None);
        s.content = "corrected body".into();
        let edit = s.entry_edit().unwrap();
        assert!(edit.title.is_none());
        let v: serde_json::Value = serde_json::to_value(&edit).unwrap();
        assert!(!v.as_object().unwrap().contains_key("title"));
    }

    #[test]
    fn retyping_the_same_title_sends_nothing() {
        let mut s = entry_edit_screen(Some("Old Title"));
        s.title_input = "  Old Title  ".into();
        s.content = "corrected body".into();
        assert!(s.entry_edit().unwrap().title.is_none());
    }

    #[test]
    fn entry_edit_carries_new_topics_and_flipped_flags() {
        let mut s = entry_edit_screen(Some("Old Title"));
        s.topics_input = "music".into();
        s.is_public = false;
        s.is_nsfw = false;
        let edit = s.entry_edit().unwrap();
        assert_eq!(edit.topics, Some(vec!["music".to_string()]));
        assert_eq!(edit.is_public, Some(false));
        assert_eq!(edit.is_nsfw, Some(false));
        assert!(edit.content.is_none(), "the body was never re-edited");
    }

    #[test]
    fn entry_edit_never_sends_the_frozen_slug() {
        // The slug is fixed once published and sending one is a 400, so neither
        // the patch nor the create path can carry it.
        let mut s = entry_edit_screen(Some("Old Title"));
        s.content = "corrected body".into();
        assert!(s.slug_to_send().is_none());
        let v: serde_json::Value = serde_json::to_value(s.entry_edit().unwrap()).unwrap();
        assert!(!v.as_object().unwrap().contains_key("slug"));
    }

    #[test]
    fn entry_edit_leaves_attachments_alone() {
        // This screen has no attachment editor, and an omitted list keeps what
        // the entry already carries, where an empty list would clear it.
        let mut s = entry_edit_screen(Some("Old Title"));
        s.content = "corrected body".into();
        assert!(s.entry_edit().unwrap().attachments.is_none());
    }

    #[test]
    fn entry_edit_is_none_for_every_other_kind() {
        for kind in [
            ComposeKind::NewEntry,
            ComposeKind::NewNote,
            ComposeKind::EditReply {
                reply_id: "r1".into(),
                original_content: "x".into(),
            },
        ] {
            assert!(ComposeScreen::new(kind, "body".into())
                .entry_edit()
                .is_none());
        }
    }

    #[test]
    fn an_unchanged_entry_edit_is_refused_before_it_is_sent() {
        let mut s = entry_edit_screen(Some("Old Title"));
        let intent = s.handle_key(key(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(intent, ComposeIntent::None);
        assert!(s.error.as_deref().unwrap().contains("nothing changed"));
        assert!(!s.submitting);
    }

    #[test]
    fn a_changed_entry_edit_submits() {
        let mut s = entry_edit_screen(Some("Old Title"));
        s.content = "corrected body".into();
        assert_eq!(
            s.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::Submit
        );
        assert!(s.submitting);
    }

    #[test]
    fn reply_edit_submits_only_when_the_content_moved() {
        let reply = published_reply();
        let mut same = ComposeScreen::from_reply(&reply, reply.content.clone());
        assert_eq!(
            same.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::None
        );
        assert!(same.error.as_deref().unwrap().contains("nothing changed"));

        let mut changed = ComposeScreen::from_reply(&reply, "corrected reply".into());
        assert_eq!(
            changed.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::Submit
        );
        assert!(changed.entry_edit().is_none(), "a reply edit sends content");
    }

    #[test]
    fn a_blank_edit_body_points_at_delete_instead() {
        let mut s = entry_edit_screen(Some("Old Title"));
        s.content = "   ".into();
        assert_eq!(
            s.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::None
        );
        let msg = s.error.as_deref().unwrap();
        assert!(msg.contains("empty"));
        assert!(msg.contains("delete"));
        assert!(!s.submitting);
    }

    #[test]
    fn an_overlong_edited_title_is_rejected() {
        let mut s = entry_edit_screen(Some("Old Title"));
        s.title_input = "x".repeat(101);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter, KeyModifiers::empty())),
            ComposeIntent::None
        );
        assert!(s.error.is_some());
        assert!(!s.submitting);
    }

    #[test]
    fn tab_cycles_an_entry_edit_and_skips_the_frozen_slug() {
        let mut s = entry_edit_screen(Some("Old Title"));
        assert_eq!(s.focused, ConfirmField::Title);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Topics);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Track);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Public);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Nsfw);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Title);
    }

    #[test]
    fn a_reply_edit_has_no_focusable_fields() {
        let mut s = ComposeScreen::from_reply(&published_reply(), "corrected".into());
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Topics);
        // Typing still cannot reach the topics box: replies have no topics.
        assert!(!s.kind.has_topics());
    }

    #[test]
    fn ctrl_e_re_edits_an_edit_body_too() {
        let mut s = entry_edit_screen(Some("Old Title"));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            ComposeIntent::Edit
        );
    }

    fn render_to_string(s: &ComposeScreen) -> String {
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(70, 16);
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
    fn an_edit_shows_its_slug_read_only_and_explains_the_title_field() {
        let out = render_to_string(&entry_edit_screen(Some("Old Title")));
        assert!(out.contains("original-slug"), "the frozen slug is visible");
        assert!(out.contains("fixed once published"), "and says why: {out}");
        assert!(out.contains("clearing it removes the title"));
        assert!(out.contains("Old Title"), "the title prefill is visible");
        assert!(out.contains("music, linux"), "the topic prefill is visible");
        assert!(out.contains("[x] public"), "the flags round-trip");
        assert!(out.contains("[x] NSFW"));
    }

    #[test]
    fn a_new_entry_still_offers_an_editable_slug() {
        let out = render_to_string(&ComposeScreen::new(ComposeKind::NewEntry, "hi".into()));
        assert!(out.contains("blank = auto"));
        assert!(!out.contains("fixed once published"));
    }
}

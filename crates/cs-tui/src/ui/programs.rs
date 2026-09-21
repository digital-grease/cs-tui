//! Programs — the terminal program registry (API v0.8.10 § Programs).
//!
//! A tab-bar section of its own, reached with the section arrows like every
//! other root. It has sub-modes (a source view, a publish form, a save prompt,
//! a delete confirm), so Esc is decided in [`ProgramsScreen::handle_escape`]
//! rather than in the key handler: the shell consults it before Esc takes on
//! its usual "back, or the menu at a root" meaning, which is what lets Esc
//! close a form without also leaving the section.
//!
//! cs-tui cannot *run* a program — `web` programs belong to the website's
//! terminal and `term`/`wasm` to the terminal machine — so what it offers is the
//! three things a client can honestly do: browse the gallery, read a release's
//! source (which § Read the Source asks you to do before running anything,
//! "publishing is open to every member"), and manage your own programs. Saving
//! a source to a file and publishing one from a file are what connect this to
//! the machine that does the running.
use std::cell::Cell;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{
    Program, ProgramQuery, ProgramSource, Runtime, MAX_PROGRAM_DESCRIPTION_LEN,
    MAX_PROGRAM_NAME_LEN,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::list::{self, TabState};
use super::list_nav::{self, ListNav};
use super::theme::Theme;

/// Which listing the gallery is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The public gallery: every published program, newest first.
    Gallery,
    /// `?mine=1` — your own, drafts and recalled ones included.
    Mine,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::Gallery => "gallery",
            Self::Mine => "mine",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::Gallery => Self::Mine,
            Self::Mine => Self::Gallery,
        }
    }
}

/// How many pages the screen will pull on its own when a filtered page comes
/// back empty (§ Browse the Gallery: "A filtered page can come back shorter
/// than `limit`, or empty, and still have a `cursor`. Follow the cursor until
/// it is null rather than stopping at the first short page").
///
/// Bounded rather than a loop: `?runtime=wasm` over a gallery of `web`
/// programs could otherwise walk the whole registry before drawing anything,
/// and the reader has `n` for the rest.
const MAX_AUTO_PAGES: u8 = 4;

/// The runtime filter, cycled with `t`. `None` asks for every kind.
///
/// § Browse the Gallery suggests keeping only the kinds you can run. cs-tui runs
/// none of them, so the default is every kind and the filter is there for a
/// reader who only cares about one machine.
const RUNTIME_CYCLE: &[Option<Runtime>] = &[
    None,
    Some(Runtime::Web),
    Some(Runtime::Term),
    Some(Runtime::Wasm),
];

fn runtime_label(runtime: Option<Runtime>) -> &'static str {
    match runtime {
        None => "all",
        Some(Runtime::Web) => "web",
        Some(Runtime::Term) => "term",
        Some(Runtime::Wasm) => "wasm",
        Some(Runtime::Unknown) => "?",
    }
}

/// A field of the publish form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishField {
    Name,
    Description,
    Runtime,
    SourcePath,
    Note,
}

impl PublishField {
    const ALL: [PublishField; 5] = [
        Self::Name,
        Self::Description,
        Self::Runtime,
        Self::SourcePath,
        Self::Note,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Name => "name (letters, digits, . _ - · max 32)",
            Self::Description => "description (required, max 256)",
            Self::Runtime => "runtime (space cycles · fixed at the first release)",
            Self::SourcePath => "source file",
            Self::Note => "release note (optional)",
        }
    }
}

/// The publish form (§ Publish).
///
/// Source comes from a file rather than from a text box on purpose: a program is
/// something you wrote in an editor, the ceilings run to megabytes, and a `wasm`
/// binary cannot be typed at all.
#[derive(Debug, Default)]
pub struct PublishForm {
    pub name: String,
    pub description: String,
    pub runtime: Runtime,
    pub source_path: String,
    pub note: String,
    focused: usize,
    pub submitting: bool,
    pub error: Option<String>,
}

impl PublishForm {
    fn focused_field(&self) -> PublishField {
        PublishField::ALL[self.focused.min(PublishField::ALL.len() - 1)]
    }

    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.focused_field() {
            PublishField::Name => Some(&mut self.name),
            PublishField::Description => Some(&mut self.description),
            PublishField::SourcePath => Some(&mut self.source_path),
            PublishField::Note => Some(&mut self.note),
            PublishField::Runtime => None,
        }
    }

    fn value_of(&self, field: PublishField) -> String {
        match field {
            PublishField::Name => self.name.clone(),
            PublishField::Description => self.description.clone(),
            PublishField::Runtime => runtime_label(Some(self.runtime)).to_string(),
            PublishField::SourcePath => self.source_path.clone(),
            PublishField::Note => self.note.clone(),
        }
    }

    fn cycle_runtime(&mut self) {
        self.runtime = match self.runtime {
            Runtime::Web => Runtime::Term,
            Runtime::Term => Runtime::Wasm,
            // `Unknown` is never something to publish under, so the cycle simply
            // does not visit it.
            Runtime::Wasm | Runtime::Unknown => Runtime::Web,
        };
    }

    /// What the form would submit, or the first thing wrong with it.
    ///
    /// Only the checks this screen can make on its own. Everything about the
    /// source itself — that it exists, that a `wasm` one starts with `\0asm`,
    /// that it fits the tier's ceiling — waits until the bytes are read, which
    /// happens in the background.
    fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("a program needs a name".into());
        }
        if name.chars().count() > MAX_PROGRAM_NAME_LEN {
            return Err(format!("name must be ≤{MAX_PROGRAM_NAME_LEN} characters"));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err("name may hold only letters, digits, '.', '_' and '-'".into());
        }
        if self.description.trim().is_empty() {
            return Err("a program needs a description".into());
        }
        if self.description.trim().chars().count() > MAX_PROGRAM_DESCRIPTION_LEN {
            return Err(format!(
                "description must be ≤{MAX_PROGRAM_DESCRIPTION_LEN} characters"
            ));
        }
        if self.source_path.trim().is_empty() {
            return Err("point the form at the file to publish".into());
        }
        Ok(())
    }
}

/// What the screen is showing.
#[derive(Debug)]
pub enum ProgramsMode {
    /// The listing, in whichever scope is selected.
    Gallery,
    /// One program's source at one release.
    Source {
        program: Program,
        /// The release being read. `None` until the first fetch answers, then
        /// the number that came back, so `,`/`.` can walk the history.
        release: Option<u32>,
        source: Option<ProgramSource>,
        loading: bool,
        error: Option<String>,
        scroll: u16,
        /// Max scroll offset for the last rendered size, recomputed each render.
        /// Scroll keys clamp to it, so the body cannot be pushed off into empty
        /// space with no way back but `g` — the same contract post detail and
        /// the help overlay keep.
        max_scroll: Cell<u16>,
    },
    /// The publish form.
    Publish(PublishForm),
    /// "Really recall this?", which is also where a purge is confirmed.
    ConfirmRecall {
        program_id: String,
        name: String,
        /// `true` deletes the record rather than pulling it from the gallery.
        /// Irreversible (§ Recall), which is the whole reason for this step.
        purge: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramsIntent {
    /// Say something to the reader without changing the screen.
    Warn(String),
    /// (Re)load the listing from the top.
    Reload {
        query: Box<ProgramQuery>,
    },
    /// Fetch the next page.
    LoadMore {
        query: Box<ProgramQuery>,
        before: String,
    },
    LoadSource {
        program_id: String,
        release: Option<u32>,
    },
    /// Read `source_path` and publish it. The file is read in the background,
    /// so the rest of the program travels alongside the path rather than as
    /// bytes this screen would have had to block on.
    Publish {
        name: String,
        description: String,
        runtime: Runtime,
        note: Option<String>,
        source_path: String,
    },
    Recall {
        program_id: String,
        purge: bool,
    },
    SaveSource {
        path: String,
        bytes: Vec<u8>,
    },
    Quit,
    None,
}

#[derive(Debug)]
pub struct ProgramsScreen {
    pub list: TabState<Program>,
    pub mode: ProgramsMode,
    pub scope: Scope,
    /// The scope the rows currently on screen were fetched under.
    ///
    /// Not the same thing as `scope`, which flips the instant `m` is pressed
    /// while the rows stay put until the reload answers — and stay put forever
    /// if it fails, since a failed reload keeps the old items and only sets an
    /// error. Gating the recall keys on `scope` therefore offered to delete
    /// another member's program; gating them on this cannot.
    loaded_scope: Scope,
    /// Index into [`RUNTIME_CYCLE`].
    runtime_filter: usize,
    /// Automatic follow-up pages still allowed; see [`MAX_AUTO_PAGES`].
    auto_pages_left: u8,
    /// The save-path prompt, when one is open over the source view.
    ///
    /// An overlay rather than a mode of its own: cancelling has to put the
    /// reader back on the source they were reading, and the bytes to write live
    /// in that mode. A separate mode would have had to carry a copy of both.
    save_prompt: Option<String>,
}

impl Default for ProgramsScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgramsScreen {
    #[must_use]
    pub fn new() -> Self {
        Self {
            list: TabState::loading(),
            mode: ProgramsMode::Gallery,
            scope: Scope::Gallery,
            loaded_scope: Scope::Gallery,
            runtime_filter: 0,
            auto_pages_left: MAX_AUTO_PAGES,
            save_prompt: None,
        }
    }

    /// What Esc means here, before the shell gives it its usual meaning.
    ///
    /// `true` when this screen consumed it by closing a sub-mode. `false` from
    /// the gallery, which is the section's top level, so Esc there opens the
    /// overlay menu exactly as it does on every other root.
    ///
    /// Esc closes a submitting publish form too. It used to be swallowed, on
    /// the reasoning that the response needed the form to land on — but the
    /// outcome is announced as a toast whether or not the form is still there,
    /// and swallowing it meant a form whose response never arrived (the reader
    /// stepped away and back, rebuilding the screen) had no key that got out of
    /// it at all.
    pub fn handle_escape(&mut self) -> bool {
        if self.save_prompt.take().is_some() {
            return true;
        }
        match &self.mode {
            ProgramsMode::Gallery => false,
            _ => {
                self.mode = ProgramsMode::Gallery;
                true
            }
        }
    }

    /// Whether a printable key belongs to a field rather than to a global
    /// shortcut. The publish form and the save prompt both capture text.
    #[must_use]
    pub fn is_text_input(&self) -> bool {
        self.save_prompt.is_some() || matches!(self.mode, ProgramsMode::Publish(_))
    }

    pub fn paste_text(&mut self, text: &str) {
        let cleaned = super::input::collapse_newlines(text);
        if let Some(path) = self.save_prompt.as_mut() {
            path.push_str(&cleaned);
            return;
        }
        if let ProgramsMode::Publish(form) = &mut self.mode {
            if let Some(field) = form.focused_text_mut() {
                field.push_str(&cleaned);
            }
        }
    }

    /// The query the current scope and filter describe.
    #[must_use]
    pub fn query(&self) -> ProgramQuery {
        let mut query = match self.scope {
            Scope::Gallery => ProgramQuery::default(),
            Scope::Mine => ProgramQuery::mine(),
        };
        if let Some(runtime) = RUNTIME_CYCLE[self.runtime_filter] {
            query = query.with_runtimes(&[runtime]);
        }
        query
    }

    /// Drop the rows belonging to the listing being navigated away from.
    ///
    /// `r` deliberately does not do this — a refresh of the same query should
    /// not blink the list away — but `m` and `t` do, because the rows on screen
    /// would otherwise sit under a header describing a different listing until
    /// the reload lands, or indefinitely if it fails.
    fn clear_for_new_query(&mut self) {
        self.list.items.clear();
        self.list.next_cursor = None;
        self.list.selected = 0;
    }

    /// The intent that refills the listing for the current scope and filter.
    fn reload(&mut self) -> ProgramsIntent {
        self.list.loading = true;
        self.list.error = None;
        self.auto_pages_left = MAX_AUTO_PAGES;
        ProgramsIntent::Reload {
            query: Box::new(self.query()),
        }
    }

    /// Apply the first page, returning the cursor to follow when the page came
    /// back empty with more behind it.
    pub fn apply_initial(
        &mut self,
        result: Result<(Vec<Program>, Option<String>), String>,
    ) -> Option<String> {
        let ok = result.is_ok();
        self.list.apply_initial(result);
        // Only a page that actually arrived changes what the rows are.
        if ok {
            self.loaded_scope = self.scope;
        }
        self.next_auto_page()
    }

    /// Apply a follow-up page, returning the cursor to follow when that page
    /// was empty too.
    pub fn apply_more(
        &mut self,
        result: Result<(Vec<Program>, Option<String>), String>,
    ) -> Option<String> {
        self.list.apply_more(result);
        self.next_auto_page()
    }

    /// The cursor to chase when the listing is still empty and the server says
    /// there is more, within the [`MAX_AUTO_PAGES`] budget.
    ///
    /// The status line already reports "0 programs · n more", but a pane
    /// reading "no programs published yet" over a non-null cursor is simply
    /// wrong, and a reader has no reason to press `j` on an empty list to find
    /// out.
    fn next_auto_page(&mut self) -> Option<String> {
        if !self.list.items.is_empty() || self.auto_pages_left == 0 {
            return None;
        }
        let cursor = self.list.next_cursor.clone()?;
        self.auto_pages_left -= 1;
        self.list.loading = true;
        Some(cursor)
    }

    /// Fold a fetched source into the open program.
    ///
    /// `program_id` is the program the fetch was *for*. Without it a slow
    /// response from a program the reader has since backed out of gets painted
    /// into whichever program is open now — under the new program's name, and
    /// with the old one's release number, so `,` then walks a history the open
    /// program does not have.
    pub fn apply_source(&mut self, program_id: &str, result: Result<ProgramSource, String>) {
        let ProgramsMode::Source {
            program,
            release,
            source,
            loading,
            error,
            scroll,
            max_scroll,
        } = &mut self.mode
        else {
            return;
        };
        if program.id != program_id {
            return;
        }
        let _ = max_scroll;
        *loading = false;
        match result {
            Ok(fetched) => {
                // Take the release number from the response rather than from
                // what was asked for: a plain read asks for nothing and gets
                // the current one, and `,` needs to know which that was.
                *release = Some(fetched.release);
                *source = Some(fetched);
                *error = None;
                *scroll = 0;
                max_scroll.set(0);
            }
            Err(msg) => *error = Some(msg),
        }
    }

    /// Fold a finished publish back into the screen, returning the line to show
    /// the reader.
    pub fn apply_published(&mut self, result: Result<String, String>) -> Option<String> {
        let ProgramsMode::Publish(form) = &mut self.mode else {
            return None;
        };
        form.submitting = false;
        match result {
            Ok(message) => {
                self.mode = ProgramsMode::Gallery;
                Some(message)
            }
            Err(msg) => {
                form.error = Some(msg);
                None
            }
        }
    }

    /// The program under the cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&Program> {
        self.list.items.get(self.list.selected)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ProgramsIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ProgramsIntent::Quit;
        }
        // The save prompt sits over whatever is behind it, so it takes the key
        // before the mode does.
        if self.save_prompt.is_some() {
            return self.handle_save_prompt_key(key);
        }
        match &self.mode {
            ProgramsMode::Gallery => self.handle_gallery_key(key),
            ProgramsMode::Source { .. } => self.handle_source_key(key),
            ProgramsMode::Publish(_) => self.handle_publish_key(key),
            ProgramsMode::ConfirmRecall { .. } => self.handle_confirm_key(key),
        }
    }

    fn handle_gallery_key(&mut self, key: KeyEvent) -> ProgramsIntent {
        let has_more = self.list.next_cursor.is_some();
        let view_len = self.list.items.len();
        let page = self.list.page_items();
        match list_nav::navigate_paged(key.code, &mut self.list.selected, view_len, has_more, page)
        {
            ListNav::Moved => return ProgramsIntent::None,
            ListNav::LoadMore => {
                let Some(before) = self.list.next_cursor.clone() else {
                    return ProgramsIntent::None;
                };
                self.list.loading = true;
                return ProgramsIntent::LoadMore {
                    query: Box::new(self.query()),
                    before,
                };
            }
            ListNav::Ignored => {}
        }

        match key.code {
            KeyCode::Enter => {
                let Some(program) = self.selected().cloned() else {
                    return ProgramsIntent::None;
                };
                let program_id = program.id.clone();
                self.mode = ProgramsMode::Source {
                    program,
                    release: None,
                    source: None,
                    loading: true,
                    error: None,
                    scroll: 0,
                    max_scroll: Cell::new(0),
                };
                ProgramsIntent::LoadSource {
                    program_id,
                    release: None,
                }
            }
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('m') => {
                self.scope = self.scope.toggled();
                self.clear_for_new_query();
                self.reload()
            }
            KeyCode::Char('t') => {
                self.runtime_filter = (self.runtime_filter + 1) % RUNTIME_CYCLE.len();
                self.clear_for_new_query();
                self.reload()
            }
            KeyCode::Char('P') => {
                self.mode = ProgramsMode::Publish(PublishForm::default());
                ProgramsIntent::None
            }
            KeyCode::Char('d') | KeyCode::Char('D') => {
                let purge = key.code == KeyCode::Char('D');
                self.start_recall(purge)
            }
            _ => ProgramsIntent::None,
        }
    }

    /// Open the confirm step for the selected program, or explain why not.
    ///
    /// Both refusals are things the server would answer anyway; saying them here
    /// means the reader learns the rule instead of reading a `403`.
    fn start_recall(&mut self, purge: bool) -> ProgramsIntent {
        // The scope the rows CAME FROM, not the flag `m` flips at once: a
        // reload that has not answered (or failed outright) leaves the previous
        // listing's rows on screen, and those belong to other members.
        if self.loaded_scope != Scope::Mine {
            return ProgramsIntent::Warn("m first — you can only recall your own programs".into());
        }
        let Some(program) = self.selected() else {
            return ProgramsIntent::None;
        };
        // Taken-down first: such a program is also `is_draft()`, and answering
        // that one first sent the reader to `D`, which then refuses too.
        if program.is_taken_down() {
            return ProgramsIntent::Warn(
                "a moderator took this down — it cannot be recalled or deleted".into(),
            );
        }
        if !purge && program.is_draft() {
            return ProgramsIntent::Warn(
                "already out of the gallery · D deletes the record".into(),
            );
        }
        self.mode = ProgramsMode::ConfirmRecall {
            program_id: program.id.clone(),
            name: program.name.clone(),
            purge,
        };
        ProgramsIntent::None
    }

    fn handle_source_key(&mut self, key: KeyEvent) -> ProgramsIntent {
        let ProgramsMode::Source {
            program,
            release,
            source,
            scroll,
            max_scroll,
            ..
        } = &mut self.mode
        else {
            return ProgramsIntent::None;
        };
        let max = max_scroll.get();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                *scroll = scroll.saturating_add(1).min(max);
                ProgramsIntent::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *scroll = scroll.saturating_sub(1);
                ProgramsIntent::None
            }
            KeyCode::PageDown => {
                *scroll = scroll.saturating_add(10).min(max);
                ProgramsIntent::None
            }
            KeyCode::PageUp => {
                *scroll = scroll.saturating_sub(10);
                ProgramsIntent::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                *scroll = 0;
                ProgramsIntent::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                *scroll = max;
                ProgramsIntent::None
            }
            // `,` and `.` walk the release history. Release objects are
            // immutable (§ Read the Source), so an older one is exactly what
            // went out — which is the point of being able to look.
            //
            // Not `[` and `]`, the obvious pair: the shell gives those to the
            // jukebox volume whenever something is playing, and it does so
            // before the screen ever sees the key. Reading source is exactly
            // the sort of sitting still that happens with music on, so taking
            // the volume keys there would be the worse trade.
            KeyCode::Char(',') => {
                let Some(current) = *release else {
                    return ProgramsIntent::None;
                };
                if current <= 1 {
                    return ProgramsIntent::Warn("that is the first release".into());
                }
                let program_id = program.id.clone();
                let wanted = current - 1;
                if let ProgramsMode::Source { loading, .. } = &mut self.mode {
                    *loading = true;
                }
                ProgramsIntent::LoadSource {
                    program_id,
                    release: Some(wanted),
                }
            }
            KeyCode::Char('.') => {
                let Some(current) = *release else {
                    return ProgramsIntent::None;
                };
                if current >= program.release {
                    return ProgramsIntent::Warn("that is the current release".into());
                }
                let program_id = program.id.clone();
                let wanted = current + 1;
                if let ProgramsMode::Source { loading, .. } = &mut self.mode {
                    *loading = true;
                }
                ProgramsIntent::LoadSource {
                    program_id,
                    release: Some(wanted),
                }
            }
            KeyCode::Char('w') => {
                let Some(fetched) = source.as_ref() else {
                    return ProgramsIntent::None;
                };
                self.save_prompt = Some(default_save_path(fetched));
                ProgramsIntent::None
            }
            _ => ProgramsIntent::None,
        }
    }

    fn handle_save_prompt_key(&mut self, key: KeyEvent) -> ProgramsIntent {
        match key.code {
            KeyCode::Enter => {
                let path = self
                    .save_prompt
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if path.is_empty() {
                    return ProgramsIntent::Warn("give the file a path".into());
                }
                let ProgramsMode::Source { source, .. } = &self.mode else {
                    self.save_prompt = None;
                    return ProgramsIntent::None;
                };
                let Some(fetched) = source.as_ref() else {
                    self.save_prompt = None;
                    return ProgramsIntent::None;
                };
                // Decoded bytes, not the wire form: a wasm program arrives
                // base64 and writing that verbatim produces a file that is not
                // a wasm module (§ Read the Source).
                match fetched.bytes() {
                    Ok(bytes) => {
                        self.save_prompt = None;
                        ProgramsIntent::SaveSource { path, bytes }
                    }
                    // Deterministic on the same payload, so leaving the prompt
                    // open just invites the reader to press Enter forever.
                    Err(e) => {
                        self.save_prompt = None;
                        ProgramsIntent::Warn(e.user_message())
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(path) = self.save_prompt.as_mut() {
                    path.pop();
                }
                ProgramsIntent::None
            }
            // A bare character only. `Ctrl+W` and `Ctrl+U` are reflexes for
            // "delete word" and "clear line"; without this they append `w` and
            // `u` to a path that is about to be written to disk.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(path) = self.save_prompt.as_mut() {
                    path.push(c);
                }
                ProgramsIntent::None
            }
            _ => ProgramsIntent::None,
        }
    }

    fn handle_publish_key(&mut self, key: KeyEvent) -> ProgramsIntent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('d') {
            return self.submit_publish();
        }
        let ProgramsMode::Publish(form) = &mut self.mode else {
            return ProgramsIntent::None;
        };
        if form.submitting {
            return ProgramsIntent::None;
        }
        match key.code {
            KeyCode::Tab => {
                form.focused = (form.focused + 1) % PublishField::ALL.len();
                ProgramsIntent::None
            }
            KeyCode::BackTab => {
                form.focused =
                    (form.focused + PublishField::ALL.len() - 1) % PublishField::ALL.len();
                ProgramsIntent::None
            }
            KeyCode::Enter => self.submit_publish(),
            KeyCode::Char(' ') if form.focused_field() == PublishField::Runtime => {
                form.cycle_runtime();
                ProgramsIntent::None
            }
            KeyCode::Backspace => {
                if let Some(field) = form.focused_text_mut() {
                    field.pop();
                }
                ProgramsIntent::None
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(field) = form.focused_text_mut() {
                    field.push(c);
                }
                ProgramsIntent::None
            }
            _ => ProgramsIntent::None,
        }
    }

    fn submit_publish(&mut self) -> ProgramsIntent {
        let ProgramsMode::Publish(form) = &mut self.mode else {
            return ProgramsIntent::None;
        };
        if form.submitting {
            return ProgramsIntent::None;
        }
        if let Err(msg) = form.validate() {
            form.error = Some(msg);
            return ProgramsIntent::None;
        }
        form.submitting = true;
        form.error = None;
        let note = form.note.trim();
        ProgramsIntent::Publish {
            name: form.name.trim().to_string(),
            description: form.description.trim().to_string(),
            runtime: form.runtime,
            note: (!note.is_empty()).then(|| note.to_string()),
            source_path: form.source_path.trim().to_string(),
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> ProgramsIntent {
        let ProgramsMode::ConfirmRecall {
            program_id, purge, ..
        } = &self.mode
        else {
            return ProgramsIntent::None;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let intent = ProgramsIntent::Recall {
                    program_id: program_id.clone(),
                    purge: *purge,
                };
                self.mode = ProgramsMode::Gallery;
                self.list.loading = true;
                intent
            }
            // Anything else is "no". A destructive step should not have a second
            // key that also means yes.
            _ => {
                self.mode = ProgramsMode::Gallery;
                ProgramsIntent::None
            }
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if let Some(path) = &self.save_prompt {
            render_save_prompt(frame, area, theme, path);
            return;
        }
        match &self.mode {
            ProgramsMode::Gallery => self.render_gallery(frame, area, theme),
            ProgramsMode::Source {
                program,
                release,
                source,
                loading,
                error,
                scroll,
                max_scroll,
            } => render_source(
                frame,
                area,
                theme,
                program,
                *release,
                source.as_ref(),
                *loading,
                error,
                *scroll,
                max_scroll,
            ),
            ProgramsMode::Publish(form) => render_publish(frame, area, theme, form),
            ProgramsMode::ConfirmRecall { name, purge, .. } => {
                render_confirm(frame, area, theme, name, *purge);
            }
        }
    }

    fn render_gallery(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(
                format!(
                    " cs-tui • programs • {} • runtime: {} ",
                    self.scope.label(),
                    runtime_label(RUNTIME_CYCLE[self.runtime_filter]),
                ),
                theme.heading_style(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let visible: Vec<usize> = (0..self.list.items.len()).collect();
        let empty = match self.scope {
            Scope::Gallery => "no programs published yet",
            Scope::Mine => "you have not published a program · P to publish one",
        };
        let theme_for_row = theme.clone();
        list::render_body(
            frame,
            layout[0],
            theme,
            &self.list,
            &visible,
            empty,
            move |p| program_row(p, &theme_for_row),
        );

        let status = if let Some(msg) = list::load_more_error(&self.list) {
            Line::from(Span::styled(msg, theme.error_style()))
        } else {
            Line::from(Span::styled(gallery_status(self), theme.muted_style()))
        };
        frame.render_widget(Paragraph::new(status), layout[1]);
    }
}

/// One gallery row: the name and owner, then what a reader needs to judge it.
fn program_row(p: &Program, theme: &Theme) -> ListItem<'static> {
    let mut header = vec![
        Span::styled(p.name.clone(), theme.base()),
        Span::styled(format!(" by @{}", p.owner_username), theme.muted_style()),
        Span::styled(
            format!(" · {} · v{}", runtime_label(Some(p.runtime)), p.release),
            theme.muted_style(),
        ),
    ];
    // Only a `?mine=1` row can say these, and each one changes what the keys
    // below will do, so they are worth a marker rather than a separate column.
    if p.is_taken_down() {
        header.push(Span::styled(" · taken down", theme.error_style()));
    } else if p.is_draft() {
        header.push(Span::styled(" · not in the gallery", theme.warning_style()));
    }
    let description = if p.description.trim().is_empty() {
        "no description".to_string()
    } else {
        p.description.clone()
    };
    ListItem::new(vec![
        Line::from(header),
        Line::from(Span::styled(
            format!("  {description}"),
            theme.muted_style(),
        )),
    ])
}

fn gallery_status(s: &ProgramsScreen) -> String {
    let keys =
        "enter read · m scope · t runtime · P publish · d recall · D delete · r refresh · esc back";
    if s.list.loading && s.list.items.is_empty() {
        return format!("loading… · {keys}");
    }
    let end = if s.list.next_cursor.is_some() {
        "n more"
    } else {
        "end"
    };
    format!("{} programs · {end} · {keys}", s.list.items.len())
}

#[allow(clippy::too_many_arguments)]
fn render_source(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    program: &Program,
    release: Option<u32>,
    source: Option<&ProgramSource>,
    loading: bool,
    error: &Option<String>,
    scroll: u16,
    max_scroll: &Cell<u16>,
) {
    let shown = release.unwrap_or(program.release);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .title(Span::styled(
            format!(
                " cs-tui • {} by @{} • v{shown} ",
                program.name, program.owner_username
            ),
            theme.heading_style(),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // Rows the body needs at this width, so the scroll keys have a floor to
    // stop at. Only the text branch scrolls; the rest are a couple of lines.
    let mut content_rows = 1usize;
    let body: Paragraph<'_> = if loading {
        Paragraph::new(Line::from(Span::styled("loading…", theme.accent_style())))
    } else if let Some(msg) = error {
        Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style())))
    } else if let Some(fetched) = source {
        match fetched.text() {
            Some(text) => {
                content_rows = wrapped_rows(&text, layout[0].width);
                Paragraph::new(text).style(theme.base()).scroll((scroll, 0))
            }
            // Nothing this client can render as text. Two different reasons,
            // and they read differently: a wasm module has no text form at all,
            // while a text runtime whose payload will not decode is a server
            // problem the reader should see as one rather than as "wasm".
            None => {
                content_rows = 2;
                Paragraph::new(undisplayable_source_lines(fetched, theme))
            }
        }
    } else {
        Paragraph::new(Line::from(Span::styled("no source", theme.muted_style())))
    };
    let pane_rows = layout[0].height as usize;
    max_scroll.set(u16::try_from(content_rows.saturating_sub(pane_rows)).unwrap_or(u16::MAX));
    frame.render_widget(body.wrap(Wrap { trim: false }), layout[0]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k/PgUp/PgDn/g/G scroll · , . releases · w save to a file · esc back",
            theme.muted_style(),
        ))),
        layout[1],
    );
}

/// How many rows `text` occupies once wrapped to `width`.
///
/// An approximation on purpose: it counts characters rather than display width,
/// which is right for program source (ASCII in practice) and only ever
/// over-estimates for wider glyphs, so the scroll floor stays reachable.
fn wrapped_rows(text: &str, width: u16) -> usize {
    let width = (width as usize).max(1);
    text.lines()
        .map(|line| line.chars().count().div_ceil(width).max(1))
        .sum::<usize>()
        .max(1)
}

/// The two-line stand-in for a source with no text form, saying which of the
/// two reasons applies.
fn undisplayable_source_lines(source: &ProgramSource, theme: &Theme) -> Vec<Line<'static>> {
    match source.bytes() {
        Ok(bytes) if source.runtime.is_binary() => vec![
            Line::from(Span::styled(
                "a wasm binary — nothing to read here",
                theme.muted_style(),
            )),
            Line::from(Span::styled(
                format!("{} bytes · w saves it to a file", bytes.len()),
                theme.muted_style(),
            )),
        ],
        // A `web`/`term` program whose decoded bytes are not text. Calling that
        // a wasm binary would send the reader looking for the wrong thing.
        Ok(bytes) => vec![
            Line::from(Span::styled(
                "this program's source is not readable text",
                theme.warning_style(),
            )),
            Line::from(Span::styled(
                format!("{} bytes · w saves it to a file", bytes.len()),
                theme.muted_style(),
            )),
        ],
        Err(e) => vec![
            Line::from(Span::styled(e.user_message(), theme.error_style())),
            Line::from(Span::styled(
                "the server sent a source this client cannot decode",
                theme.muted_style(),
            )),
        ],
    }
}

fn render_save_prompt(frame: &mut Frame<'_>, area: Rect, theme: &Theme, path: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .title(Span::styled(
            " cs-tui • save program source ",
            theme.heading_style(),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("write to", theme.accent_style()))),
        layout[0],
    );
    frame.render_widget(
        Paragraph::new(super::input::windowed_line(
            path,
            path.chars().count(),
            layout[1].width as usize,
            theme,
        )),
        layout[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "enter save · esc cancel",
            theme.muted_style(),
        ))),
        layout[3],
    );
}

fn render_publish(frame: &mut Frame<'_>, area: Rect, theme: &Theme, form: &PublishForm) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .title(Span::styled(
            " cs-tui • publish a program ",
            theme.heading_style(),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut constraints: Vec<Constraint> = PublishField::ALL
        .iter()
        .flat_map(|_| [Constraint::Length(1), Constraint::Length(1)])
        .collect();
    constraints.push(Constraint::Min(0));
    constraints.push(Constraint::Length(1));

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(inner);

    for (i, field) in PublishField::ALL.iter().enumerate() {
        let focused = form.focused_field() == *field;
        let style = if focused {
            theme.accent_style()
        } else {
            theme.muted_style()
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(field.label(), style))),
            layout[i * 2],
        );
        let value_area = layout[i * 2 + 1];
        let value = form.value_of(*field);
        let line = if focused && *field != PublishField::Runtime {
            super::input::windowed_line(
                &value,
                value.chars().count(),
                value_area.width as usize,
                theme,
            )
        } else {
            Line::from(Span::styled(value, theme.base()))
        };
        frame.render_widget(Paragraph::new(line), value_area);
    }

    let status_idx = layout.len() - 1;
    let status: Line<'_> = if form.submitting {
        Line::from(Span::styled("publishing…", theme.accent_style()))
    } else if let Some(msg) = &form.error {
        Line::from(Span::styled(msg.clone(), theme.error_style()))
    } else {
        Line::from(Span::styled(
            "tab focus · space cycles runtime · enter/ctrl+d publish · esc cancel",
            theme.muted_style(),
        ))
    };
    frame.render_widget(Paragraph::new(status), layout[status_idx]);
}

fn render_confirm(frame: &mut Frame<'_>, area: Rect, theme: &Theme, name: &str, purge: bool) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style())
        .title(Span::styled(" cs-tui • programs ", theme.heading_style()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // The two are different enough to be worth different words: a recall is
    // reversible and a purge is not (§ Recall).
    let lines = if purge {
        vec![
            Line::from(Span::styled(
                format!("Delete the record for {name}?"),
                theme.error_style(),
            )),
            Line::from(Span::styled(
                "Irreversible. The release history goes with it, and the slot it holds \
                 against your program count is freed. Copies other members installed are \
                 unaffected.",
                theme.muted_style(),
            )),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                format!("Take {name} out of the gallery?"),
                theme.warning_style(),
            )),
            Line::from(Span::styled(
                "The release history stays and publishing again resumes from the next \
                 version, so this can be undone. It does not free the slot the program \
                 holds against your count — D does that.",
                theme.muted_style(),
            )),
        ]
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), layout[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            // `y` and Esc are the two that are guaranteed to arrive: the shell
            // claims `i`, `S`, `?`, Backspace and the jukebox keys before the
            // screen is asked, so "any other key cancels" was not true.
            "y confirms · esc cancels",
            theme.muted_style(),
        ))),
        layout[1],
    );
}

/// A sensible filename to offer when saving a source.
///
/// The extension follows the runtime, because that is what decides the file's
/// shape: `wasm` is a binary and the other two are JavaScript modules.
fn default_save_path(source: &ProgramSource) -> String {
    let stem = if source.name.trim().is_empty() {
        "program"
    } else {
        source.name.trim()
    };
    let extension = if source.runtime.is_binary() {
        "wasm"
    } else {
        "js"
    };
    PathBuf::from(format!("{stem}.{extension}"))
        .to_string_lossy()
        .into_owned()
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

    fn shifted(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn program(id: &str, name: &str) -> Program {
        Program {
            id: id.into(),
            name: name.into(),
            owner_username: "trinity".into(),
            description: "does a thing".into(),
            runtime: Runtime::Term,
            release: 3,
            ..Default::default()
        }
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn source_at(release: u32) -> ProgramSource {
        ProgramSource {
            id: "p1".into(),
            name: "hello".into(),
            runtime: Runtime::Term,
            release,
            encoding: "utf8".into(),
            source: "line\n".repeat(50),
            ..Default::default()
        }
    }

    fn loaded(items: Vec<Program>) -> ProgramsScreen {
        let mut s = ProgramsScreen::new();
        s.apply_initial(Ok((items, None)));
        s
    }

    #[test]
    fn the_gallery_starts_public_and_unfiltered() {
        let s = ProgramsScreen::new();
        assert_eq!(s.scope, Scope::Gallery);
        let q = s.query();
        assert!(!q.mine);
        assert!(
            q.runtimes.is_empty(),
            "cs-tui runs none of them, so it asks for every kind"
        );
    }

    #[test]
    fn m_toggles_the_scope_and_reloads() {
        let mut s = loaded(vec![program("p1", "hello")]);
        let intent = s.handle_key(key(KeyCode::Char('m')));
        assert_eq!(s.scope, Scope::Mine);
        match intent {
            ProgramsIntent::Reload { query } => assert!(query.mine),
            other => panic!("expected a reload, got {other:?}"),
        }
        assert!(s.list.loading);

        s.handle_key(key(KeyCode::Char('m')));
        assert_eq!(s.scope, Scope::Gallery);
    }

    #[test]
    fn t_cycles_the_runtime_filter_through_every_kind_and_back() {
        let mut s = loaded(vec![program("p1", "hello")]);
        for expected in [
            Some(Runtime::Web),
            Some(Runtime::Term),
            Some(Runtime::Wasm),
            None,
        ] {
            s.handle_key(key(KeyCode::Char('t')));
            let wanted: Vec<Runtime> = expected.into_iter().collect();
            assert_eq!(s.query().runtimes, wanted);
        }
    }

    #[test]
    fn enter_opens_the_source_and_asks_for_the_current_release() {
        let mut s = loaded(vec![program("p1", "hello")]);
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(
            intent,
            ProgramsIntent::LoadSource {
                program_id: "p1".into(),
                release: None,
            },
            "a plain read asks for nothing and gets the current release"
        );
        assert!(matches!(s.mode, ProgramsMode::Source { loading: true, .. }));
    }

    #[test]
    fn the_release_that_came_back_is_the_one_the_screen_walks_from() {
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source(
            "p1",
            Ok(ProgramSource {
                id: "p1".into(),
                name: "hello".into(),
                runtime: Runtime::Term,
                release: 3,
                encoding: "utf8".into(),
                source: "export default 1".into(),
                ..Default::default()
            }),
        );

        // `,` steps back through the history…
        assert_eq!(
            s.handle_key(key(KeyCode::Char(','))),
            ProgramsIntent::LoadSource {
                program_id: "p1".into(),
                release: Some(2),
            }
        );
        // …and stops at the first one rather than asking for release 0.
        s.apply_source(
            "p1",
            Ok(ProgramSource {
                release: 1,
                encoding: "utf8".into(),
                source: "v1".into(),
                ..Default::default()
            }),
        );
        assert!(matches!(
            s.handle_key(key(KeyCode::Char(','))),
            ProgramsIntent::Warn(_)
        ));
    }

    #[test]
    fn the_source_view_avoids_the_keys_the_shell_takes_first() {
        // The shell hands `[` and `]` to the jukebox volume, `i` to the image
        // toggle, `S` to shuffle and `s` to the player stop, all before the
        // screen is asked. A binding on any of them would simply never fire
        // while music was playing.
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source(
            "p1",
            Ok(ProgramSource {
                release: 3,
                encoding: "utf8".into(),
                source: "x".into(),
                ..Default::default()
            }),
        );
        for stolen in ['[', ']', 'i', 'S', 's', 'p', '<', '>'] {
            assert_eq!(
                s.handle_key(key(KeyCode::Char(stolen))),
                ProgramsIntent::None,
                "{stolen} must not be load-bearing here"
            );
        }
        // The keys it does use still work.
        assert_eq!(
            s.handle_key(key(KeyCode::Char(','))),
            ProgramsIntent::LoadSource {
                program_id: "p1".into(),
                release: Some(2),
            }
        );
    }

    #[test]
    fn a_recall_is_refused_outside_your_own_listing() {
        // The gallery shows everybody's programs; `d` there would be a 403.
        let mut s = loaded(vec![program("p1", "hello")]);
        assert!(matches!(
            s.handle_key(key(KeyCode::Char('d'))),
            ProgramsIntent::Warn(_)
        ));
        assert!(matches!(s.mode, ProgramsMode::Gallery));
    }

    #[test]
    fn recalling_and_deleting_each_take_a_confirm() {
        let mut p = program("p1", "hello");
        p.is_published = Some(true);
        let mut s = loaded(vec![p]);
        s.scope = Scope::Mine;
        s.loaded_scope = Scope::Mine;

        s.handle_key(key(KeyCode::Char('d')));
        match &s.mode {
            ProgramsMode::ConfirmRecall {
                program_id, purge, ..
            } => {
                assert_eq!(program_id, "p1");
                assert!(!purge);
            }
            other => panic!("expected a confirm, got {other:?}"),
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Char('y'))),
            ProgramsIntent::Recall {
                program_id: "p1".into(),
                purge: false,
            }
        );

        s.handle_key(shifted(KeyCode::Char('D')));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('y'))),
            ProgramsIntent::Recall {
                program_id: "p1".into(),
                purge: true,
            }
        );
    }

    #[test]
    fn anything_but_y_cancels_a_destructive_confirm() {
        let mut p = program("p1", "hello");
        p.is_published = Some(true);
        let mut s = loaded(vec![p]);
        s.scope = Scope::Mine;
        s.loaded_scope = Scope::Mine;

        for cancel in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('n')] {
            s.handle_key(shifted(KeyCode::Char('D')));
            assert!(matches!(s.mode, ProgramsMode::ConfirmRecall { .. }));
            assert_eq!(s.handle_key(key(cancel)), ProgramsIntent::None);
            assert!(matches!(s.mode, ProgramsMode::Gallery), "{cancel:?}");
        }
    }

    #[test]
    fn a_taken_down_program_cannot_be_deleted() {
        // § Recall: a purge is "refused on a program a moderator has taken
        // down". Saying so beats spending the call to be told.
        let mut p = program("p1", "hello");
        p.is_published = Some(false);
        p.taken_down = Some(true);
        let mut s = loaded(vec![p]);
        s.scope = Scope::Mine;
        s.loaded_scope = Scope::Mine;
        assert!(matches!(
            s.handle_key(shifted(KeyCode::Char('D'))),
            ProgramsIntent::Warn(_)
        ));
    }

    #[test]
    fn recalling_a_program_that_is_already_out_says_which_key_to_use() {
        let mut p = program("p1", "hello");
        p.is_published = Some(false);
        let mut s = loaded(vec![p]);
        s.scope = Scope::Mine;
        s.loaded_scope = Scope::Mine;
        match s.handle_key(key(KeyCode::Char('d'))) {
            ProgramsIntent::Warn(msg) => assert!(msg.contains('D'), "{msg}"),
            other => panic!("expected a warning, got {other:?}"),
        }
    }

    #[test]
    fn the_publish_form_refuses_what_the_spec_states_flatly() {
        let mut s = loaded(Vec::new());
        s.handle_key(shifted(KeyCode::Char('P')));
        assert!(matches!(s.mode, ProgramsMode::Publish(_)));

        // Empty: nothing is sent and the reason is on screen.
        assert_eq!(s.handle_key(key(KeyCode::Enter)), ProgramsIntent::None);
        let ProgramsMode::Publish(form) = &s.mode else {
            panic!("still the form");
        };
        assert!(form.error.is_some());
        assert!(!form.submitting);
    }

    #[test]
    fn a_filled_publish_form_sends_what_the_endpoint_takes() {
        let mut s = loaded(Vec::new());
        s.handle_key(shifted(KeyCode::Char('P')));
        let ProgramsMode::Publish(form) = &mut s.mode else {
            panic!("the form");
        };
        form.name = " hello ".into();
        form.description = " says hi ".into();
        form.source_path = " ./hello.js ".into();
        form.runtime = Runtime::Term;

        assert_eq!(
            s.handle_key(key(KeyCode::Enter)),
            ProgramsIntent::Publish {
                name: "hello".into(),
                description: "says hi".into(),
                runtime: Runtime::Term,
                note: None,
                source_path: "./hello.js".into(),
            },
            "every field is trimmed, and a blank note is omitted"
        );
    }

    #[test]
    fn the_runtime_field_cycles_without_ever_landing_on_unknown() {
        let mut s = loaded(Vec::new());
        s.handle_key(shifted(KeyCode::Char('P')));
        let ProgramsMode::Publish(form) = &mut s.mode else {
            panic!("the form");
        };
        form.focused = 2; // the runtime field
        assert_eq!(form.runtime, Runtime::Web, "the documented default");
        for expected in [Runtime::Term, Runtime::Wasm, Runtime::Web] {
            s.handle_key(key(KeyCode::Char(' ')));
            let ProgramsMode::Publish(form) = &s.mode else {
                panic!("the form");
            };
            assert_eq!(form.runtime, expected);
        }
    }

    #[test]
    fn the_form_and_the_save_prompt_capture_text_but_the_gallery_does_not() {
        // Otherwise a `t` typed into a program name would cycle the runtime
        // filter behind the form.
        let mut s = loaded(Vec::new());
        assert!(!s.is_text_input());
        s.handle_key(shifted(KeyCode::Char('P')));
        assert!(s.is_text_input());
    }

    #[test]
    fn saving_offers_a_filename_that_matches_the_runtime() {
        let js = ProgramSource {
            name: "hello".into(),
            runtime: Runtime::Term,
            ..Default::default()
        };
        assert_eq!(default_save_path(&js), "hello.js");

        let wasm = ProgramSource {
            name: "hello".into(),
            runtime: Runtime::Wasm,
            ..Default::default()
        };
        assert_eq!(default_save_path(&wasm), "hello.wasm");

        // A nameless program still gets a usable path rather than ".js".
        assert_eq!(default_save_path(&ProgramSource::default()), "program.js");
    }

    #[test]
    fn scrolling_the_source_never_underflows() {
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source(
            "p1",
            Ok(ProgramSource {
                release: 1,
                encoding: "utf8".into(),
                source: "line\n".repeat(50),
                ..Default::default()
            }),
        );
        s.handle_key(key(KeyCode::Char('k')));
        s.handle_key(key(KeyCode::PageUp));
        let ProgramsMode::Source { scroll, .. } = &s.mode else {
            panic!("the source view");
        };
        assert_eq!(*scroll, 0);
    }

    #[test]
    fn a_failed_source_load_is_shown_rather_than_leaving_it_spinning() {
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source("p1", Err("403 forbidden".into()));
        match &s.mode {
            ProgramsMode::Source { loading, error, .. } => {
                assert!(!loading);
                assert_eq!(error.as_deref(), Some("403 forbidden"));
            }
            other => panic!("expected the source view, got {other:?}"),
        }
    }

    #[test]
    fn walking_forward_through_the_release_history_works_and_stops_at_the_top() {
        let mut s = loaded(vec![program("p1", "hello")]); // release 3
        s.handle_key(key(KeyCode::Enter));
        s.apply_source("p1", Ok(source_at(1)));

        assert_eq!(
            s.handle_key(key(KeyCode::Char('.'))),
            ProgramsIntent::LoadSource {
                program_id: "p1".into(),
                release: Some(2),
            }
        );
        s.apply_source("p1", Ok(source_at(3)));
        assert!(
            matches!(
                s.handle_key(key(KeyCode::Char('.'))),
                ProgramsIntent::Warn(_)
            ),
            "the listing's release is the top of the history"
        );
    }

    #[test]
    fn a_source_response_for_another_program_is_ignored() {
        // Open A, back out, open B: A's slower response must not be painted
        // into B's view, which would show A's code under B's name and leave `,`
        // walking a history B does not have.
        let mut s = loaded(vec![program("p1", "alpha"), program("p2", "beta")]);
        s.list.selected = 1;
        s.handle_key(key(KeyCode::Enter)); // opens p2

        s.apply_source(
            "p1",
            Ok(ProgramSource {
                id: "p1".into(),
                name: "alpha".into(),
                release: 5,
                encoding: "utf8".into(),
                source: "alpha source".into(),
                ..Default::default()
            }),
        );
        match &s.mode {
            ProgramsMode::Source {
                program,
                source,
                loading,
                ..
            } => {
                assert_eq!(program.id, "p2");
                assert!(source.is_none(), "beta's view is untouched");
                assert!(loading, "and is still waiting for its own response");
            }
            other => panic!("expected the source view, got {other:?}"),
        }
    }

    #[test]
    fn scrolling_is_clamped_to_the_content() {
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source("p1", Ok(source_at(1)));
        // Before a render there is no measured content, so the floor is 0 and
        // scrolling cannot push the body off into blank space.
        for _ in 0..30 {
            s.handle_key(key(KeyCode::PageDown));
        }
        let ProgramsMode::Source { scroll, .. } = &s.mode else {
            panic!("the source view");
        };
        assert_eq!(*scroll, 0);
    }

    #[test]
    fn the_recall_keys_follow_the_rows_on_screen_not_the_scope_flag() {
        // `m` flips the scope at once, but a failed reload leaves the previous
        // listing's rows in place — and those belong to other members.
        let mut s = loaded(vec![program("p1", "someone-elses")]);
        assert_eq!(s.loaded_scope, Scope::Gallery);

        s.handle_key(key(KeyCode::Char('m')));
        assert_eq!(s.scope, Scope::Mine);
        // Switching listings drops the rows that belonged to the old one.
        assert!(s.list.items.is_empty());

        // A failed reload leaves nothing to act on, and the scope the rows were
        // fetched under has not moved.
        s.apply_initial(Err("503".into()));
        assert_eq!(s.loaded_scope, Scope::Gallery);
        assert!(matches!(
            s.handle_key(shifted(KeyCode::Char('D'))),
            ProgramsIntent::Warn(_)
        ));

        // Once a `mine` page actually lands, the keys arm.
        let mut mine = program("p9", "mine");
        mine.is_published = Some(true);
        s.apply_initial(Ok((vec![mine], None)));
        assert_eq!(s.loaded_scope, Scope::Mine);
        s.handle_key(shifted(KeyCode::Char('D')));
        assert!(matches!(s.mode, ProgramsMode::ConfirmRecall { .. }));
    }

    #[test]
    fn a_taken_down_program_says_so_on_both_keys() {
        // `d` used to answer "already out of the gallery · D deletes the
        // record", and `D` then refused — the client's own advice was a dead
        // end.
        let mut p = program("p1", "hello");
        p.is_published = Some(false);
        p.taken_down = Some(true);
        let mut s = loaded(vec![p]);
        s.loaded_scope = Scope::Mine;
        s.scope = Scope::Mine;
        for k in [key(KeyCode::Char('d')), shifted(KeyCode::Char('D'))] {
            match s.handle_key(k) {
                ProgramsIntent::Warn(msg) => assert!(msg.contains("moderator"), "{msg}"),
                other => panic!("expected a warning, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_control_chord_does_not_type_itself_into_a_path_or_a_field() {
        // Ctrl+W and Ctrl+U are "delete word" and "clear line" reflexes; they
        // used to append `w` and `u` to a path about to be written to disk.
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source("p1", Ok(source_at(1)));
        s.handle_key(key(KeyCode::Char('w')));
        let before = s.save_prompt.clone().expect("the prompt is open");
        s.handle_key(ctrl(KeyCode::Char('w')));
        s.handle_key(ctrl(KeyCode::Char('u')));
        assert_eq!(s.save_prompt.as_deref(), Some(before.as_str()));

        let mut s = loaded(Vec::new());
        s.handle_key(shifted(KeyCode::Char('P')));
        s.handle_key(ctrl(KeyCode::Char('w')));
        let ProgramsMode::Publish(form) = &s.mode else {
            panic!("the form");
        };
        assert!(form.name.is_empty());
    }

    #[test]
    fn saving_hands_back_the_decoded_bytes_and_closes_the_prompt() {
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source(
            "p1",
            Ok(ProgramSource {
                id: "p1".into(),
                name: "hello".into(),
                runtime: Runtime::Wasm,
                release: 1,
                encoding: "base64".into(),
                source: "AGFzbQEAAAA=".into(),
                ..Default::default()
            }),
        );
        s.handle_key(key(KeyCode::Char('w')));
        assert_eq!(s.save_prompt.as_deref(), Some("hello.wasm"));
        match s.handle_key(key(KeyCode::Enter)) {
            ProgramsIntent::SaveSource { path, bytes } => {
                assert_eq!(path, "hello.wasm");
                assert_eq!(
                    bytes, b"\0asm\x01\0\0\0",
                    "the decoded module, not the base64 text"
                );
            }
            other => panic!("expected a save, got {other:?}"),
        }
        assert!(s.save_prompt.is_none());
    }

    #[test]
    fn an_undecodable_source_closes_the_prompt_rather_than_looping() {
        let mut s = loaded(vec![program("p1", "hello")]);
        s.handle_key(key(KeyCode::Enter));
        s.apply_source(
            "p1",
            Ok(ProgramSource {
                runtime: Runtime::Wasm,
                encoding: "base64".into(),
                source: "!!!!".into(),
                ..Default::default()
            }),
        );
        s.handle_key(key(KeyCode::Char('w')));
        assert!(matches!(
            s.handle_key(key(KeyCode::Enter)),
            ProgramsIntent::Warn(_)
        ));
        assert!(
            s.save_prompt.is_none(),
            "retrying can never succeed, so the prompt must not stay open"
        );
    }

    #[test]
    fn a_publish_result_closes_the_form_only_when_it_succeeded() {
        let mut s = loaded(Vec::new());
        s.handle_key(shifted(KeyCode::Char('P')));
        let ProgramsMode::Publish(form) = &mut s.mode else {
            panic!("the form");
        };
        form.submitting = true;
        assert_eq!(
            s.apply_published(Err("name already taken".into())),
            None,
            "a failure has nothing to announce from here"
        );
        match &s.mode {
            ProgramsMode::Publish(form) => {
                assert!(!form.submitting);
                assert_eq!(form.error.as_deref(), Some("name already taken"));
            }
            other => panic!("expected the form, got {other:?}"),
        }

        let ProgramsMode::Publish(form) = &mut s.mode else {
            panic!("the form");
        };
        form.submitting = true;
        assert_eq!(
            s.apply_published(Ok("published hello v1".into())),
            Some("published hello v1".to_string())
        );
        assert!(matches!(s.mode, ProgramsMode::Gallery));
    }

    #[test]
    fn esc_gets_out_of_a_submitting_form() {
        // It used to be swallowed, so a form whose response never landed had no
        // key that left it at all.
        let mut s = loaded(Vec::new());
        s.handle_key(shifted(KeyCode::Char('P')));
        let ProgramsMode::Publish(form) = &mut s.mode else {
            panic!("the form");
        };
        form.submitting = true;
        assert!(s.handle_escape());
        assert!(matches!(s.mode, ProgramsMode::Gallery));
        assert!(!s.handle_escape(), "and the gallery hands Esc to the shell");
    }

    #[test]
    fn an_empty_filtered_page_follows_its_cursor_instead_of_claiming_the_gallery_is_empty() {
        // § Browse the Gallery: "A filtered page can come back shorter than
        // `limit`, or empty, and still have a `cursor`."
        let mut s = ProgramsScreen::new();
        assert_eq!(
            s.apply_initial(Ok((Vec::new(), Some("g30".into())))),
            Some("g30".to_string())
        );
        assert_eq!(
            s.apply_more(Ok((Vec::new(), Some("g60".into())))),
            Some("g60".to_string())
        );
        // A page with rows on it stops the chase.
        assert_eq!(
            s.apply_more(Ok((vec![program("p1", "hello")], Some("g90".into())))),
            None
        );

        // And the budget is bounded rather than walking the whole registry.
        let mut s = ProgramsScreen::new();
        let mut chases = 0;
        let mut next = s.apply_initial(Ok((Vec::new(), Some("c".into()))));
        while next.is_some() {
            chases += 1;
            next = s.apply_more(Ok((Vec::new(), Some("c".into()))));
        }
        assert_eq!(chases, usize::from(MAX_AUTO_PAGES));
    }

    #[test]
    fn a_page_of_programs_loads_the_next_one_by_its_cursor() {
        let mut s = ProgramsScreen::new();
        s.apply_initial(Ok((vec![program("p1", "a")], Some("cursor-1".into()))));
        // `n` is the explicit next-page key on every list screen.
        match s.handle_key(key(KeyCode::Char('n'))) {
            ProgramsIntent::LoadMore { before, query } => {
                assert_eq!(before, "cursor-1");
                assert!(!query.mine);
            }
            other => panic!("expected a load-more, got {other:?}"),
        }
    }
}

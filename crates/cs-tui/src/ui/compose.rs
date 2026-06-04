//! Compose screen — entry or reply, with $EDITOR for body content and an
//! inline confirmation step.
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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::theme::Theme;

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
}

impl ComposeKind {
    fn has_topics(&self) -> bool {
        matches!(
            self,
            Self::NewEntry | Self::NewNote | Self::UpdateNote { .. } | Self::GuildThread { .. }
        )
    }

    /// Public/NSFW flags apply to top-level entries only — not guild threads,
    /// replies, or notes.
    fn has_visibility_toggles(&self) -> bool {
        matches!(self, Self::NewEntry)
    }

    /// Titles are valid on top-level entries and guild threads.
    fn has_title(&self) -> bool {
        matches!(self, Self::NewEntry | Self::GuildThread { .. })
    }

    /// A custom per-author URL slug is accepted on entries and guild threads.
    fn has_slug(&self) -> bool {
        matches!(self, Self::NewEntry | Self::GuildThread { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmField {
    Title,
    Slug,
    Topics,
    Public,
    Nsfw,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeIntent {
    Quit,
    /// User confirmed: send to the API.
    Submit,
    None,
}

#[derive(Debug)]
pub struct ComposeScreen {
    pub kind: ComposeKind,
    pub content: String,
    pub title_input: String,
    pub slug_input: String,
    pub topics_input: String,
    pub is_public: bool,
    pub is_nsfw: bool,
    pub focused: ConfirmField,
    pub submitting: bool,
    pub error: Option<String>,
}

impl ComposeScreen {
    pub fn new(kind: ComposeKind, content: String) -> Self {
        let focused = if kind.has_title() {
            ConfirmField::Title
        } else {
            ConfirmField::Topics
        };
        Self {
            kind,
            content,
            title_input: String::new(),
            slug_input: String::new(),
            topics_input: String::new(),
            is_public: false,
            is_nsfw: false,
            focused,
            submitting: false,
            error: None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ComposeIntent {
        if self.submitting {
            return ComposeIntent::None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ComposeIntent::Quit;
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.try_submit();
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

    /// Whether the focused field accepts typed characters.
    fn focused_is_text(&self) -> bool {
        matches!(
            self.focused,
            ConfirmField::Title | ConfirmField::Slug | ConfirmField::Topics
        )
    }

    /// The text buffer for the focused field, if it's a text field.
    fn focused_text_mut(&mut self) -> Option<&mut String> {
        match self.focused {
            ConfirmField::Title => Some(&mut self.title_input),
            ConfirmField::Slug => Some(&mut self.slug_input),
            ConfirmField::Topics => Some(&mut self.topics_input),
            ConfirmField::Public | ConfirmField::Nsfw => None,
        }
    }

    fn cycle_focus(&mut self, backward: bool) {
        let order: &[ConfirmField] = match self.kind {
            ComposeKind::NewEntry => &[
                ConfirmField::Title,
                ConfirmField::Slug,
                ConfirmField::Topics,
                ConfirmField::Public,
                ConfirmField::Nsfw,
            ],
            ComposeKind::NewNote | ComposeKind::UpdateNote { .. } => &[ConfirmField::Topics],
            ComposeKind::GuildThread { .. } => &[
                ConfirmField::Title,
                ConfirmField::Slug,
                ConfirmField::Topics,
            ],
            ComposeKind::Reply { .. } => &[],
        };
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
            ConfirmField::Title | ConfirmField::Slug | ConfirmField::Topics => {}
        }
    }

    /// The slug to send (trimmed; `None` when blank — server auto-generates).
    pub fn slug_to_send(&self) -> Option<String> {
        let s = self.slug_input.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    fn try_submit(&mut self) -> ComposeIntent {
        if self.content.trim().is_empty() {
            self.error = Some("content is empty — press e to re-edit".into());
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
                if s.chars().count() > 100 {
                    self.error = Some("slug must be ≤100 characters".into());
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
        let title = match &self.kind {
            ComposeKind::NewEntry => " cs-tui • new post ".to_string(),
            ComposeKind::Reply { post_id, .. } => format!(" cs-tui • reply to {post_id} "),
            ComposeKind::NewNote => " cs-tui • new note ".to_string(),
            ComposeKind::UpdateNote { note_id } => format!(" cs-tui • edit note {note_id} "),
            ComposeKind::GuildThread { guild_slug } => {
                format!(" cs-tui • new thread in {guild_slug} ")
            }
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(title, theme.accent_style()));
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
        let slug_idx = if self.kind.has_slug() {
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
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "title (optional, max 100 chars)",
                    style,
                ))),
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

        // Slug (entries / guild threads)
        if let Some(idx) = slug_idx {
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
                "tab/shift+tab focus · space toggle · enter or ctrl+s submit · esc cancel",
                theme.muted_style(),
            ))
        };
        frame.render_widget(Paragraph::new(status), layout[status_idx]);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("editor exited with failure")]
    EditorFailed,
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

/// Suspend the ratatui terminal, run `$EDITOR` (falling back to `nano`) on a
/// tempfile pre-filled with `initial`, restore the terminal, and return the
/// final file contents.
///
/// This must run on a blocking thread (use `tokio::task::spawn_blocking`) so
/// the tokio runtime stays responsive — but in practice the editor owns the TTY
/// while it's open, so no other terminal I/O happens.
pub fn launch_editor(initial: &str, suffix: &str) -> Result<String, ComposeError> {
    // Config `editor` wins, then $VISUAL, then $EDITOR, then nano.
    let editor = crate::config::get()
        .editor
        .clone()
        .or_else(|| std::env::var("VISUAL").ok())
        .or_else(|| std::env::var("EDITOR").ok())
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
    execute!(out, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

fn resume_terminal() -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
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
    fn reply_skips_topics_input() {
        let mut s = ComposeScreen::new(
            ComposeKind::Reply {
                post_id: "p1".into(),
                parent_reply_id: None,
            },
            "hi".into(),
        );
        // Tab is a no-op for replies.
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, ConfirmField::Topics);
    }
}

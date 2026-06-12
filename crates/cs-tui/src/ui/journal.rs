//! Journal screen — private notes browser with inline view.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Note, NoteRevision};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use time::OffsetDateTime;

use super::markdown::render_markdown;
use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalIntent {
    LoadMore,
    Refresh,
    /// Open the compose flow to write a new note.
    Compose,
    /// Open the compose flow to edit the selected note.
    EditSelected {
        note_id: String,
        content: String,
        topics: Vec<String>,
    },
    /// Delete the selected note (after y-confirmation).
    DeleteSelected {
        note_id: String,
    },
    /// Load the selected note's revisions and switch to revisions view.
    ShowRevisions {
        note_id: String,
    },
    /// Switch back from revisions view to current content view.
    HideRevisions,
    Quit,
    None,
}

/// Inline detail panel mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailMode {
    /// Show the latest content of the selected note.
    Current,
    /// Show the list of revisions for the selected note.
    Revisions,
}

#[derive(Debug)]
pub struct JournalScreen {
    pub notes: Vec<Note>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub mode: DetailMode,
    pub revisions: Vec<NoteRevision>,
    pub revisions_for: Option<String>,
    pub revision_selected: usize,
    pub loading_revisions: bool,
    pub confirming_delete: bool,
}

impl JournalScreen {
    pub fn new() -> Self {
        Self {
            notes: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: true,
            error: None,
            mode: DetailMode::Current,
            revisions: Vec::new(),
            revisions_for: None,
            revision_selected: 0,
            loading_revisions: false,
            confirming_delete: false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> JournalIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return JournalIntent::Quit;
        }

        // Two-step delete: arming d, then y to confirm.
        if self.confirming_delete {
            self.confirming_delete = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                if let Some(n) = self.notes.get(self.selected) {
                    return JournalIntent::DeleteSelected {
                        note_id: n.note_id.clone(),
                    };
                }
            }
            return JournalIntent::None;
        }

        // Revisions mode has its own key map.
        if self.mode == DetailMode::Revisions {
            return self.handle_key_revisions(key);
        }

        match super::list_nav::navigate(
            key.code,
            &mut self.selected,
            self.notes.len(),
            self.next_cursor.is_some(),
        ) {
            super::list_nav::ListNav::LoadMore => {
                self.loading = true;
                return JournalIntent::LoadMore;
            }
            super::list_nav::ListNav::Moved => return JournalIntent::None,
            super::list_nav::ListNav::Ignored => {}
        }
        match key.code {
            KeyCode::Char('r') => {
                self.notes.clear();
                self.next_cursor = None;
                self.selected = 0;
                self.loading = true;
                self.error = None;
                return JournalIntent::Refresh;
            }
            KeyCode::Char('c') => return JournalIntent::Compose,
            KeyCode::Char('e') => {
                if let Some(n) = self.notes.get(self.selected) {
                    return JournalIntent::EditSelected {
                        note_id: n.note_id.clone(),
                        content: n.content.clone(),
                        topics: n.topics.clone(),
                    };
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if self.notes.get(self.selected).is_some() => {
                if crate::config::get().confirm_deletes {
                    self.confirming_delete = true;
                } else if let Some(n) = self.notes.get(self.selected) {
                    return JournalIntent::DeleteSelected {
                        note_id: n.note_id.clone(),
                    };
                }
            }
            KeyCode::Char('v') => {
                if let Some(n) = self.notes.get(self.selected) {
                    return JournalIntent::ShowRevisions {
                        note_id: n.note_id.clone(),
                    };
                }
            }
            _ => {}
        }
        JournalIntent::None
    }

    fn handle_key_revisions(&mut self, key: KeyEvent) -> JournalIntent {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down
                if !self.revisions.is_empty()
                    && self.revision_selected < self.revisions.len() - 1 =>
            {
                self.revision_selected += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.revision_selected = self.revision_selected.saturating_sub(1);
            }
            KeyCode::Char('v') | KeyCode::Backspace => return JournalIntent::HideRevisions,
            _ => {}
        }
        JournalIntent::None
    }

    pub fn apply_initial(&mut self, result: Result<(Vec<Note>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((notes, cursor)) => {
                self.notes = notes;
                self.next_cursor = cursor;
                if self.selected >= self.notes.len() {
                    self.selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_more(&mut self, result: Result<(Vec<Note>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((mut notes, cursor)) => {
                self.notes.append(&mut notes);
                self.next_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_revisions(&mut self, note_id: String, result: Result<Vec<NoteRevision>, String>) {
        self.loading_revisions = false;
        match result {
            Ok(revs) => {
                self.revisions = revs;
                self.revisions_for = Some(note_id);
                self.revision_selected = 0;
                self.mode = DetailMode::Revisions;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn remove_local(&mut self, note_id: &str) {
        if let Some(idx) = self.notes.iter().position(|n| n.note_id == note_id) {
            self.notes.remove(idx);
            if self.selected >= self.notes.len() {
                self.selected = self.notes.len().saturating_sub(1);
            }
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • journal ", theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        // Split the main area: left list / right detail panel.
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(layout[0]);
        let list_area = split[0];
        let detail_area = split[1];

        // Notes list
        if self.loading && self.notes.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading notes…",
                    theme.accent_style(),
                ))),
                list_area,
            );
        } else if let Some(msg) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style()))),
                list_area,
            );
        } else if self.notes.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "no notes — press c to compose",
                    theme.muted_style(),
                ))),
                list_area,
            );
        } else {
            let items: Vec<ListItem<'_>> = self.notes.iter().map(|n| note_item(n, theme)).collect();
            let list = List::new(items)
                .highlight_style(theme.accent_style())
                .highlight_symbol("▌ ");
            let mut state = ListState::default();
            state.select(Some(self.selected.min(self.notes.len().saturating_sub(1))));
            frame.render_stateful_widget(list, list_area, &mut state);
        }

        // Detail panel
        match self.mode {
            DetailMode::Current => self.render_current_detail(frame, detail_area, theme),
            DetailMode::Revisions => self.render_revisions_detail(frame, detail_area, theme),
        }

        // Status bar
        let status_text = if self.confirming_delete {
            "really delete this note? y=yes, any other key=cancel".to_string()
        } else if self.mode == DetailMode::Revisions {
            format!(
                "viewing revisions ({}) · v or backspace to return",
                self.revisions.len()
            )
        } else if self.next_cursor.is_some() {
            format!(
                "{} notes · scroll down for more · c compose · e edit · d delete · v revisions · r refresh · esc menu",
                self.notes.len()
            )
        } else {
            format!(
                "{} notes · end · c compose · e edit · d delete · v revisions · r refresh · esc menu",
                self.notes.len()
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style()))),
            layout[1],
        );
    }

    fn render_current_detail(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let Some(n) = self.notes.get(self.selected) else {
            return;
        };
        let mut lines: Vec<Line<'_>> = Vec::new();
        let when = n.created_at.map(format_full).unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!(
                "note {} · revision {} · {when}",
                n.note_id, n.revision_number
            ),
            theme.muted_style(),
        )));
        if !n.topics.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("topics: #{}", n.topics.join(" #")),
                theme.muted_style(),
            )));
        }
        lines.push(Line::from(""));
        for md_line in render_markdown(&n.content, theme) {
            lines.push(md_line);
        }
        super::hyperlink::render_linked_paragraph(
            frame,
            area,
            lines,
            0,
            crate::config::get().hyperlinks,
        );
    }

    fn render_revisions_detail(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if self.loading_revisions {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "loading revisions…",
                    theme.accent_style(),
                ))),
                area,
            );
            return;
        }
        if self.revisions.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "no revisions",
                    theme.muted_style(),
                ))),
                area,
            );
            return;
        }

        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length((self.revisions.len() as u16).min(8) + 1),
                Constraint::Min(1),
            ])
            .split(area);

        let items: Vec<ListItem<'_>> = self
            .revisions
            .iter()
            .map(|r| {
                let when = r.created_at.map(format_full).unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!("r{}", r.revision_number), theme.accent_style()),
                    Span::styled(format!(" · {when}"), theme.muted_style()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(theme.accent_style())
            .highlight_symbol("▌ ");
        let mut state = ListState::default();
        state.select(Some(
            self.revision_selected
                .min(self.revisions.len().saturating_sub(1)),
        ));
        frame.render_stateful_widget(list, split[0], &mut state);

        if let Some(rev) = self.revisions.get(self.revision_selected) {
            let mut lines: Vec<Line<'_>> = Vec::new();
            if !rev.topics.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("topics: #{}", rev.topics.join(" #")),
                    theme.muted_style(),
                )));
                lines.push(Line::from(""));
            }
            for md_line in render_markdown(&rev.content, theme) {
                lines.push(md_line);
            }
            super::hyperlink::render_linked_paragraph(
                frame,
                split[1],
                lines,
                0,
                crate::config::get().hyperlinks,
            );
        }
    }
}

impl Default for JournalScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn note_item<'a>(n: &'a Note, theme: &Theme) -> ListItem<'a> {
    let when = n.created_at.map(format_relative).unwrap_or_default();
    let preview = first_line_truncated(&n.content, 60);
    let topics = if n.topics.is_empty() {
        String::new()
    } else {
        format!("  #{}", n.topics.join(" #"))
    };
    let header = Line::from(vec![
        Span::styled(format!("[r{}]", n.revision_number), theme.muted_style()),
        Span::styled(format!(" {when}{topics}"), theme.muted_style()),
    ]);
    let body = Line::from(Span::styled(preview, theme.base()));
    ListItem::new(vec![header, body, Line::from("")])
}

fn first_line_truncated(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or("").trim();
    if first.chars().count() <= max {
        first.to_string()
    } else {
        let truncated: String = first.chars().take(max - 1).collect();
        format!("{truncated}…")
    }
}

fn format_relative(t: OffsetDateTime) -> String {
    let now = OffsetDateTime::now_utc();
    let secs = (now - t).whole_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else if secs < 30 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else {
        let d = t.date();
        format!("{}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
    }
}

fn format_full(t: OffsetDateTime) -> String {
    let d = t.date();
    let time = t.time();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        d.year(),
        u8::from(d.month()),
        d.day(),
        time.hour(),
        time.minute()
    )
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

    fn note(id: &str, content: &str) -> Note {
        Note {
            note_id: id.into(),
            author_id: "u".into(),
            content: content.into(),
            topics: vec!["journal".into()],
            revision_number: 1,
            deleted: false,
            created_at: None,
        }
    }

    #[test]
    fn c_emits_compose() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "x")], None)));
        assert_eq!(
            s.handle_key(key(KeyCode::Char('c'))),
            JournalIntent::Compose
        );
    }

    #[test]
    fn render_makes_a_note_link_clickable() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((
            vec![note("n1", "see [site](https://x.example/page)")],
            None,
        )));

        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| s.render(f, f.area(), &Theme::cyber()))
            .expect("draw");

        let buf = terminal.backend().buffer();
        let linked = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| {
                buf[(x, y)]
                    .symbol()
                    .contains("\u{1b}]8;;https://x.example/page\u{1b}\\")
            })
        });
        assert!(
            linked,
            "a markdown link in a journal note is an OSC 8 hyperlink"
        );
    }

    #[test]
    fn e_emits_edit_selected() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "x")], None)));
        let intent = s.handle_key(key(KeyCode::Char('e')));
        assert_eq!(
            intent,
            JournalIntent::EditSelected {
                note_id: "n1".into(),
                content: "x".into(),
                topics: vec!["journal".into()],
            }
        );
    }

    #[test]
    fn d_then_y_deletes() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "x")], None)));
        s.handle_key(key(KeyCode::Char('d')));
        assert!(s.confirming_delete);
        let intent = s.handle_key(key(KeyCode::Char('y')));
        assert_eq!(
            intent,
            JournalIntent::DeleteSelected {
                note_id: "n1".into()
            }
        );
    }

    #[test]
    fn d_then_other_key_cancels() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "x")], None)));
        s.handle_key(key(KeyCode::Char('d')));
        let intent = s.handle_key(key(KeyCode::Char('x')));
        assert_eq!(intent, JournalIntent::None);
        assert!(!s.confirming_delete);
    }

    #[test]
    fn v_emits_show_revisions() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "x")], None)));
        let intent = s.handle_key(key(KeyCode::Char('v')));
        assert_eq!(
            intent,
            JournalIntent::ShowRevisions {
                note_id: "n1".into()
            }
        );
    }

    #[test]
    fn apply_revisions_switches_mode() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "x")], None)));
        s.apply_revisions(
            "n1".into(),
            Ok(vec![NoteRevision {
                revision_number: 1,
                content: "x".into(),
                topics: vec![],
                created_at: None,
            }]),
        );
        assert_eq!(s.mode, DetailMode::Revisions);
    }

    #[test]
    fn v_in_revisions_mode_returns_to_current() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "x")], None)));
        s.apply_revisions("n1".into(), Ok(vec![]));
        let intent = s.handle_key(key(KeyCode::Char('v')));
        assert_eq!(intent, JournalIntent::HideRevisions);
    }

    #[test]
    fn remove_local_drops_note() {
        let mut s = JournalScreen::new();
        s.apply_initial(Ok((vec![note("n1", "a"), note("n2", "b")], None)));
        s.remove_local("n1");
        assert_eq!(s.notes.len(), 1);
        assert_eq!(s.notes[0].note_id, "n2");
    }
}

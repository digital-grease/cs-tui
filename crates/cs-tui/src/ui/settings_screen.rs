//! Settings screen — boolean toggles + string preferences. Opaque fields are
//! left untouched on PATCH.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{NotificationPrefs, Settings, SettingsUpdate};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Bool,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldSpec {
    key: &'static str,
    label: &'static str,
    kind: FieldKind,
}

const FIELDS: &[FieldSpec] = &[
    FieldSpec {
        key: "filterNSFW",
        label: "filter NSFW posts",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "showFollowerCount",
        label: "show follower count on profile",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "hideImagesInFeed",
        label: "hide images in feed",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "hideAudioInFeed",
        label: "hide audio in feed",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "autoWatchOnReply",
        label: "auto-watch threads I reply to",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "useLegacyMenuOrder",
        label: "use legacy menu order",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "defaultPublicPost",
        label: "default new posts to public",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "notif.bookmark",
        label: "notify on bookmark",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "notif.reply",
        label: "notify on reply",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "notif.poke",
        label: "notify on poke",
        kind: FieldKind::Bool,
    },
    FieldSpec {
        key: "iconTheme",
        label: "icon theme",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "imagePixelSize",
        label: "image pixel size",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "timeDisplayFormat",
        label: "time display format",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "keyboardPreset",
        label: "keyboard preset",
        kind: FieldKind::Text,
    },
];

#[derive(Debug, Clone, PartialEq)]
pub enum SettingsIntent {
    Cancel,
    Quit,
    Submit { update: Box<SettingsUpdate> },
    None,
}

#[derive(Debug)]
pub struct SettingsScreen {
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub submitting: bool,
    pub focused: usize,
    /// Values keyed by FIELDS index.
    pub bools: [Option<bool>; 14],
    pub texts: [String; 14],
    pub dirty: [bool; 14],
}

impl SettingsScreen {
    pub fn new() -> Self {
        Self {
            loaded: false,
            loading: true,
            error: None,
            submitting: false,
            focused: 0,
            bools: [None; 14],
            texts: std::array::from_fn(|_| String::new()),
            dirty: [false; 14],
        }
    }

    pub fn apply_loaded(&mut self, result: Result<Settings, String>) {
        self.loading = false;
        self.loaded = true;
        match result {
            Ok(s) => {
                self.bools[0] = s.filter_nsfw;
                self.bools[1] = s.show_follower_count;
                self.bools[2] = s.hide_images_in_feed;
                self.bools[3] = s.hide_audio_in_feed;
                self.bools[4] = s.auto_watch_on_reply;
                self.bools[5] = s.use_legacy_menu_order;
                self.bools[6] = s.default_public_post;
                self.bools[7] = s.notifications.bookmark;
                self.bools[8] = s.notifications.reply;
                self.bools[9] = s.notifications.poke;
                self.texts[10] = s.icon_theme.unwrap_or_default();
                self.texts[11] = s.image_pixel_size.unwrap_or_default();
                self.texts[12] = s.time_display_format.unwrap_or_default();
                self.texts[13] = s.keyboard_preset.unwrap_or_default();
                self.dirty = [false; 14];
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    /// Whether a free-text field is currently focused. Only then must printable
    /// keys and arrows reach the field instead of triggering global section
    /// navigation — so on a toggle field the tab bar (1-8, Tab, ←/→) still
    /// works, while editing a text field is uninterrupted.
    #[must_use]
    pub fn is_editing_text(&self) -> bool {
        self.loaded
            && FIELDS
                .get(self.focused)
                .is_some_and(|f| f.kind == FieldKind::Text)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SettingsIntent {
        if self.submitting {
            return SettingsIntent::None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return SettingsIntent::Quit;
        }
        if !self.loaded {
            return SettingsIntent::None;
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.try_submit();
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => {
                self.focused = (self.focused + 1) % FIELDS.len();
                self.error = None;
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                self.focused = (self.focused + FIELDS.len() - 1) % FIELDS.len();
                self.error = None;
            }
            KeyCode::Enter => return self.try_submit(),
            KeyCode::Char(' ') if FIELDS[self.focused].kind == FieldKind::Bool => {
                let prev = self.bools[self.focused].unwrap_or(false);
                self.bools[self.focused] = Some(!prev);
                self.dirty[self.focused] = true;
            }
            KeyCode::Backspace if FIELDS[self.focused].kind == FieldKind::Text => {
                self.texts[self.focused].pop();
                self.dirty[self.focused] = true;
            }
            KeyCode::Char(c) if FIELDS[self.focused].kind == FieldKind::Text => {
                self.texts[self.focused].push(c);
                self.dirty[self.focused] = true;
            }
            _ => {}
        }
        SettingsIntent::None
    }

    fn try_submit(&mut self) -> SettingsIntent {
        let update = self.build_update();
        if update.is_empty() {
            return SettingsIntent::Cancel;
        }
        self.submitting = true;
        SettingsIntent::Submit {
            update: Box::new(update),
        }
    }

    pub fn finish_submit(&mut self, result: Result<(), String>) {
        self.submitting = false;
        if let Err(msg) = result {
            self.error = Some(msg);
        } else {
            self.dirty = [false; 14];
        }
    }

    fn build_update(&self) -> SettingsUpdate {
        let mut u = SettingsUpdate::default();
        let mut notif = NotificationPrefs::default();
        let mut notif_dirty = false;
        for (i, field) in FIELDS.iter().enumerate() {
            if !self.dirty[i] {
                continue;
            }
            match field.key {
                "filterNSFW" => u.filter_nsfw = self.bools[i],
                "showFollowerCount" => u.show_follower_count = self.bools[i],
                "hideImagesInFeed" => u.hide_images_in_feed = self.bools[i],
                "hideAudioInFeed" => u.hide_audio_in_feed = self.bools[i],
                "autoWatchOnReply" => u.auto_watch_on_reply = self.bools[i],
                "useLegacyMenuOrder" => u.use_legacy_menu_order = self.bools[i],
                "defaultPublicPost" => u.default_public_post = self.bools[i],
                "notif.bookmark" => {
                    notif.bookmark = self.bools[i];
                    notif_dirty = true;
                }
                "notif.reply" => {
                    notif.reply = self.bools[i];
                    notif_dirty = true;
                }
                "notif.poke" => {
                    notif.poke = self.bools[i];
                    notif_dirty = true;
                }
                "iconTheme" => u.icon_theme = Some(self.texts[i].clone()),
                "imagePixelSize" => u.image_pixel_size = Some(self.texts[i].clone()),
                "timeDisplayFormat" => u.time_display_format = Some(self.texts[i].clone()),
                "keyboardPreset" => u.keyboard_preset = Some(self.texts[i].clone()),
                _ => {}
            }
        }
        if notif_dirty {
            u.notifications = Some(notif);
        }
        u
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • settings ", theme.accent_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        if !self.loaded {
            let msg = if let Some(e) = &self.error {
                e.clone()
            } else {
                "loading settings…".to_string()
            };
            let style = if self.error.is_some() {
                theme.error_style()
            } else {
                theme.accent_style()
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(msg, style))),
                layout[0],
            );
            return;
        }

        // Build list items.
        let items: Vec<ListItem<'_>> = FIELDS
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let dirty_marker = if self.dirty[i] { "*" } else { " " };
                let value = match f.kind {
                    FieldKind::Bool => {
                        let v = self.bools[i].unwrap_or(false);
                        if v {
                            "[x]".to_string()
                        } else {
                            "[ ]".to_string()
                        }
                    }
                    FieldKind::Text => format!("\"{}\"", self.texts[i]),
                };
                let style = if i == self.focused {
                    theme.accent_style()
                } else {
                    theme.muted_style()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{dirty_marker} "), style),
                    Span::styled(format!("{:<32}", f.label), style),
                    Span::styled(value, theme.base()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(theme.accent_style())
            .highlight_symbol("▌ ");
        let mut state = ListState::default();
        state.select(Some(self.focused));
        frame.render_stateful_widget(list, layout[0], &mut state);

        // Status line
        let dirty_count = self.dirty.iter().filter(|d| **d).count();
        let status_text = if self.submitting {
            "saving…".to_string()
        } else if let Some(msg) = &self.error {
            format!("error: {msg} · esc to cancel")
        } else {
            format!(
                "{dirty_count} unsaved · space toggle · type to edit text · 1-8/←→ switch section · ctrl+s save · esc menu"
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style()))),
            layout[1],
        );
    }
}

impl Default for SettingsScreen {
    fn default() -> Self {
        Self::new()
    }
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
    fn keys_ignored_before_load() {
        let mut s = SettingsScreen::new();
        let i = s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert_eq!(i, SettingsIntent::None);
        assert_eq!(s.focused, 0);
    }

    #[test]
    fn apply_loaded_populates_values() {
        let mut s = SettingsScreen::new();
        let settings = Settings {
            filter_nsfw: Some(true),
            icon_theme: Some("cyber".into()),
            notifications: NotificationPrefs {
                bookmark: Some(false),
                ..Default::default()
            },
            ..Default::default()
        };
        s.apply_loaded(Ok(settings));
        assert!(s.loaded);
        assert_eq!(s.bools[0], Some(true)); // filterNSFW
        assert_eq!(s.bools[7], Some(false)); // notif.bookmark
        assert_eq!(s.texts[10], "cyber");
    }

    #[test]
    fn space_toggles_bool() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings {
            filter_nsfw: Some(false),
            ..Default::default()
        }));
        s.focused = 0;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert_eq!(s.bools[0], Some(true));
        assert!(s.dirty[0]);
    }

    #[test]
    fn typing_edits_text_field() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        s.focused = 10; // iconTheme
        s.handle_key(key(KeyCode::Char('c'), KeyModifiers::empty()));
        s.handle_key(key(KeyCode::Char('6'), KeyModifiers::empty()));
        s.handle_key(key(KeyCode::Char('4'), KeyModifiers::empty()));
        assert_eq!(s.texts[10], "c64");
        assert!(s.dirty[10]);
    }

    #[test]
    fn submit_with_no_dirty_cancels() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        let i = s.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(i, SettingsIntent::Cancel);
    }

    #[test]
    fn submit_with_dirty_emits_update() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        s.focused = 0;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        let i = s.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        match i {
            SettingsIntent::Submit { update } => {
                assert_eq!(update.filter_nsfw, Some(true));
            }
            other => panic!("expected Submit, got {other:?}"),
        }
    }

    #[test]
    fn notification_toggles_grouped_into_subobject() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        s.focused = 7; // notif.bookmark
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        let update = s.build_update();
        assert!(update.notifications.is_some());
        assert_eq!(update.notifications.unwrap().bookmark, Some(true));
    }

    #[test]
    fn tab_cycles_focus() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        assert_eq!(s.focused, 0);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, 1);
    }
}

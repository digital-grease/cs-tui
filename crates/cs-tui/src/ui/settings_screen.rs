//! Settings screen — boolean toggles + string preferences. Opaque fields are
//! left untouched on PATCH.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{NotificationPrefs, Settings, SettingsUpdate, IMAGE_PIXEL_SIZES};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    /// A toggle, carrying what the server does when it has never been set.
    ///
    /// `GET /v1/settings` omits a field the account has never changed, and
    /// cs-api keeps that absence distinct from `false` on purpose. Rendering
    /// the absence as `[ ]` was a lie for the two fields the spec documents as
    /// "default on": the row said guild threads were out of the feed while they
    /// were in it.
    Bool { default_on: bool },
    /// A fixed set of allowed values, cycled in place with space.
    Choice(&'static [&'static str]),
    /// A server-managed value shown for reference but not editable here: the API
    /// documents these as settable yet never enumerates their allowed values,
    /// and the reference client defers them too, so we never PATCH them.
    ReadOnly,
}

/// Allowed values for `timeDisplayFormat`. Order defines the space-cycle
/// sequence. `imagePixelSize` is the other string field with a known value set
/// (v0.9.0), cycled from [`IMAGE_PIXEL_SIZES`].
const TIME_FORMATS: &[&str] = &["datetime", "relative", "unix", "swatch"];

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
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "showFollowerCount",
        label: "show follower count on profile",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "hideImagesInFeed",
        label: "hide images in feed",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "hideAudioInFeed",
        label: "hide audio in feed",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "autoWatchOnReply",
        label: "auto-watch threads I reply to",
        // § Settings: "autoWatchOnReply (default on)".
        kind: FieldKind::Bool { default_on: true },
    },
    FieldSpec {
        key: "showGuildPostsInFeed",
        label: "show guild threads in the feed",
        // § Settings: "showGuildPostsInFeed (default on)".
        kind: FieldKind::Bool { default_on: true },
    },
    FieldSpec {
        key: "useLegacyMenuOrder",
        label: "use legacy menu order",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "defaultPublicPost",
        label: "default new posts to public",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "colorNicknames",
        label: "colour nicknames in cIRC",
        // § Settings (v0.9.0): "colorNicknames (default off)". Off, cs-tui
        // keeps its own palette; on, it uses the website's hue per name.
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "notif.bookmark",
        label: "notify on bookmark",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "notif.reply",
        label: "notify on reply",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "notif.poke",
        label: "notify on poke",
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "iconTheme",
        label: "icon theme",
        kind: FieldKind::ReadOnly,
    },
    FieldSpec {
        key: "imagePixelSize",
        label: "image pixel size (website)",
        // v0.9.0 enumerates the values, so this is no longer read-only.
        kind: FieldKind::Choice(IMAGE_PIXEL_SIZES),
    },
    FieldSpec {
        key: "imageColor",
        label: "images in colour (website)",
        // § Settings (v0.9.0): "imageColor (default off)". A website rendering
        // preference; cs-tui always draws images in full colour.
        kind: FieldKind::Bool { default_on: false },
    },
    FieldSpec {
        key: "timeDisplayFormat",
        label: "time display format",
        kind: FieldKind::Choice(TIME_FORMATS),
    },
    FieldSpec {
        key: "keyboardPreset",
        label: "keyboard preset",
        kind: FieldKind::ReadOnly,
    },
];

/// The three parallel arrays are sized from `FIELDS` itself, so adding a row
/// can never leave them short.
const FIELD_COUNT: usize = FIELDS.len();

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
    pub bools: [Option<bool>; FIELD_COUNT],
    pub texts: [String; FIELD_COUNT],
    pub dirty: [bool; FIELD_COUNT],
}

impl SettingsScreen {
    pub fn new() -> Self {
        Self {
            loaded: false,
            loading: true,
            error: None,
            submitting: false,
            focused: 0,
            bools: [None; FIELD_COUNT],
            texts: std::array::from_fn(|_| String::new()),
            dirty: [false; FIELD_COUNT],
        }
    }

    pub fn apply_loaded(&mut self, result: Result<Settings, String>) {
        self.loading = false;
        self.loaded = true;
        match result {
            Ok(s) => {
                // Keyed rather than positional, so a row added to `FIELDS` can
                // never shift a value onto its neighbour.
                for (i, field) in FIELDS.iter().enumerate() {
                    match field.key {
                        "filterNSFW" => self.bools[i] = s.filter_nsfw,
                        "showFollowerCount" => self.bools[i] = s.show_follower_count,
                        "hideImagesInFeed" => self.bools[i] = s.hide_images_in_feed,
                        "hideAudioInFeed" => self.bools[i] = s.hide_audio_in_feed,
                        "autoWatchOnReply" => self.bools[i] = s.auto_watch_on_reply,
                        "showGuildPostsInFeed" => self.bools[i] = s.show_guild_posts_in_feed,
                        "useLegacyMenuOrder" => self.bools[i] = s.use_legacy_menu_order,
                        "defaultPublicPost" => self.bools[i] = s.default_public_post,
                        "colorNicknames" => self.bools[i] = s.color_nicknames,
                        "imageColor" => self.bools[i] = s.image_color,
                        "notif.bookmark" => self.bools[i] = s.notifications.bookmark,
                        "notif.reply" => self.bools[i] = s.notifications.reply,
                        "notif.poke" => self.bools[i] = s.notifications.poke,
                        "iconTheme" => self.texts[i] = s.icon_theme.clone().unwrap_or_default(),
                        "imagePixelSize" => {
                            self.texts[i] = s.image_pixel_size.clone().unwrap_or_default();
                        }
                        "timeDisplayFormat" => {
                            self.texts[i] = s.time_display_format.clone().unwrap_or_default();
                        }
                        "keyboardPreset" => {
                            self.texts[i] = s.keyboard_preset.clone().unwrap_or_default();
                        }
                        // Every key above is in `FIELDS`;
                        // `every_field_loads_from_the_server` pins that.
                        _ => {}
                    }
                }
                self.dirty = [false; FIELD_COUNT];
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
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
        if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
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
            KeyCode::Char(' ') => match FIELDS[self.focused].kind {
                FieldKind::Bool { default_on } => {
                    // Toggle against what is actually in effect, which for an
                    // unset field is the documented default, not `false`.
                    let prev = self.bools[self.focused].unwrap_or(default_on);
                    self.bools[self.focused] = Some(!prev);
                    self.dirty[self.focused] = true;
                }
                FieldKind::Choice(opts) => {
                    let cur = self.texts[self.focused].as_str();
                    let next = opts
                        .iter()
                        .position(|o| *o == cur)
                        .map_or(0, |i| (i + 1) % opts.len());
                    self.texts[self.focused] = opts[next].to_string();
                    self.dirty[self.focused] = true;
                }
                FieldKind::ReadOnly => {}
            },
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
            self.dirty = [false; FIELD_COUNT];
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
                "showGuildPostsInFeed" => u.show_guild_posts_in_feed = self.bools[i],
                "useLegacyMenuOrder" => u.use_legacy_menu_order = self.bools[i],
                "defaultPublicPost" => u.default_public_post = self.bools[i],
                "colorNicknames" => u.color_nicknames = self.bools[i],
                "imageColor" => u.image_color = self.bools[i],
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
                "timeDisplayFormat" => u.time_display_format = Some(self.texts[i].clone()),
                "imagePixelSize" => u.image_pixel_size = Some(self.texts[i].clone()),
                // iconTheme / keyboardPreset are read-only here (never dirty),
                // so they're intentionally never PATCHed.
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
            .title(Span::styled(" cs-tui • settings ", theme.heading_style()));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

        // Local config-file path — shown regardless of the server-settings load
        // state so users can always find (and edit) their config.toml.
        let cfg_path = crate::config::config_path().map_or_else(
            || "(default location)".to_string(),
            |p| p.display().to_string(),
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("config file: ", theme.muted_style()),
                Span::styled(cfg_path, theme.base()),
            ])),
            layout[1],
        );

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
                    FieldKind::Bool { default_on } => {
                        if self.bools[i].unwrap_or(default_on) {
                            "[x]".to_string()
                        } else {
                            "[ ]".to_string()
                        }
                    }
                    FieldKind::Choice(_) => format!("‹ {} ›", self.texts[i]),
                    FieldKind::ReadOnly => {
                        let v = self.texts[i].as_str();
                        let shown = if v.is_empty() { "—" } else { v };
                        format!("{shown}  (read-only)")
                    }
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
                "{dirty_count} unsaved · space toggle/cycle · ←→ section · enter/ctrl+d save · esc menu"
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style()))),
            layout[2],
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

    /// The row index for a settings key.
    ///
    /// Tests used to hard-code these, which meant adding one field in the
    /// middle of `FIELDS` quietly repointed five assertions at their
    /// neighbours. Looking the index up keeps a test pinned to the field it
    /// names.
    fn idx(key: &str) -> usize {
        FIELDS
            .iter()
            .position(|f| f.key == key)
            .unwrap_or_else(|| panic!("no settings field {key:?}"))
    }

    #[test]
    fn every_field_loads_from_the_server() {
        // A key missing from `apply_loaded` would silently show its default.
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings {
            filter_nsfw: Some(true),
            show_follower_count: Some(true),
            hide_images_in_feed: Some(true),
            hide_audio_in_feed: Some(true),
            auto_watch_on_reply: Some(true),
            show_guild_posts_in_feed: Some(true),
            use_legacy_menu_order: Some(true),
            default_public_post: Some(true),
            color_nicknames: Some(true),
            image_color: Some(true),
            notifications: NotificationPrefs {
                bookmark: Some(true),
                reply: Some(true),
                poke: Some(true),
                ..Default::default()
            },
            icon_theme: Some("x".into()),
            image_pixel_size: Some("x".into()),
            time_display_format: Some("x".into()),
            keyboard_preset: Some("x".into()),
            ..Default::default()
        }));
        for (i, f) in FIELDS.iter().enumerate() {
            match f.kind {
                FieldKind::Bool { .. } => assert_eq!(s.bools[i], Some(true), "{}", f.key),
                _ => assert_eq!(s.texts[i], "x", "{}", f.key),
            }
        }
    }

    #[test]
    fn the_v090_toggles_round_trip() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        for key_name in ["colorNicknames", "imageColor"] {
            let i = idx(key_name);
            assert_eq!(s.bools[i], None);
            s.focused = i;
            s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
            assert_eq!(s.bools[i], Some(true), "default off, so on: {key_name}");
        }
        let u = s.build_update();
        assert_eq!(u.color_nicknames, Some(true));
        assert_eq!(u.image_color, Some(true));
    }

    #[test]
    fn image_pixel_size_cycles_the_v090_values() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings {
            image_pixel_size: Some("sharp".into()),
            ..Default::default()
        }));
        let i = idx("imagePixelSize");
        s.focused = i;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert_eq!(s.texts[i], "crisp");
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert_eq!(s.texts[i], "rough", "wraps to the coarsest");
        assert_eq!(s.build_update().image_pixel_size.as_deref(), Some("rough"));
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
        assert_eq!(s.bools[idx("filterNSFW")], Some(true));
        assert_eq!(s.bools[idx("notif.bookmark")], Some(false));
        assert_eq!(s.texts[idx("iconTheme")], "cyber");
    }

    #[test]
    fn the_v0810_guild_feed_toggle_round_trips() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings {
            show_guild_posts_in_feed: Some(true),
            ..Default::default()
        }));
        let i = idx("showGuildPostsInFeed");
        assert_eq!(s.bools[i], Some(true));

        s.focused = i;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert_eq!(s.bools[i], Some(false));
        assert!(s.dirty[i]);
        assert_eq!(s.build_update().show_guild_posts_in_feed, Some(false));
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
    fn space_cycles_choice_field() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings {
            time_display_format: Some("datetime".into()),
            ..Default::default()
        }));
        let i = idx("timeDisplayFormat"); // a Choice field
        s.focused = i;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert_eq!(s.texts[i], "relative"); // datetime → relative
        assert!(s.dirty[i]);
        let update = s.build_update();
        assert_eq!(update.time_display_format.as_deref(), Some("relative"));
    }

    #[test]
    fn space_cycle_wraps_and_seeds_unknown_value() {
        let mut s = SettingsScreen::new();
        // An unrecognized server value starts the cycle at the first option.
        s.apply_loaded(Ok(Settings {
            time_display_format: Some("weird".into()),
            ..Default::default()
        }));
        let i = idx("timeDisplayFormat");
        s.focused = i;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        assert_eq!(s.texts[i], "datetime");
    }

    #[test]
    fn readonly_field_is_inert_and_never_patched() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings {
            icon_theme: Some("pixel".into()),
            ..Default::default()
        }));
        let i = idx("iconTheme"); // read-only
        s.focused = i;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        s.handle_key(key(KeyCode::Char('x'), KeyModifiers::empty()));
        assert_eq!(s.texts[i], "pixel"); // unchanged, still shown
        assert!(!s.dirty[i]);
        assert!(s.build_update().is_empty());
    }

    #[test]
    fn submit_with_no_dirty_cancels() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        let i = s.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(i, SettingsIntent::Cancel);
    }

    #[test]
    fn submit_with_dirty_emits_update() {
        let mut s = SettingsScreen::new();
        s.apply_loaded(Ok(Settings::default()));
        s.focused = 0;
        s.handle_key(key(KeyCode::Char(' '), KeyModifiers::empty()));
        let i = s.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
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
        s.focused = idx("notif.bookmark");
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

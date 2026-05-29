//! Edit-profile form — fields for bio, displayName, website*, location*,
//! pinnedPostId. Tab cycles focus; Enter or Ctrl+S submits; Esc cancels.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Patch, ProfileUpdate, User};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    DisplayName,
    Bio,
    WebsiteUrl,
    WebsiteName,
    WebsiteImageUrl,
    LocationName,
    LocationLatitude,
    LocationLongitude,
    PinnedPostId,
}

impl Field {
    const ALL: [Field; 9] = [
        Self::DisplayName,
        Self::Bio,
        Self::WebsiteUrl,
        Self::WebsiteName,
        Self::WebsiteImageUrl,
        Self::LocationName,
        Self::LocationLatitude,
        Self::LocationLongitude,
        Self::PinnedPostId,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::DisplayName => "displayName",
            Self::Bio => "bio",
            Self::WebsiteUrl => "websiteUrl",
            Self::WebsiteName => "websiteName",
            Self::WebsiteImageUrl => "websiteImageUrl",
            Self::LocationName => "locationName",
            Self::LocationLatitude => "locationLatitude",
            Self::LocationLongitude => "locationLongitude",
            Self::PinnedPostId => "pinnedPostId",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditProfileIntent {
    Cancel,
    Submit { update: Box<ProfileUpdate> },
    Quit,
    None,
}

#[derive(Debug)]
pub struct EditProfileScreen {
    pub fields: [String; 9],
    /// `true` for fields the user explicitly cleared (will be sent as `null`).
    pub cleared: [bool; 9],
    /// Initial values to detect unchanged fields (sent as `Skip`).
    pub initial: [Option<String>; 9],
    pub focused: usize,
    pub error: Option<String>,
    pub submitting: bool,
}

impl EditProfileScreen {
    pub fn from_user(u: &User) -> Self {
        let initial: [Option<String>; 9] = [
            u.display_name.clone(),
            u.bio.clone(),
            u.website_url.clone(),
            u.website_name.clone(),
            u.website_image_url.clone(),
            u.location_name.clone(),
            u.location_latitude.map(|v| v.to_string()),
            u.location_longitude.map(|v| v.to_string()),
            u.pinned_post_id.clone(),
        ];
        let fields = std::array::from_fn(|i| initial[i].clone().unwrap_or_default());
        Self {
            fields,
            cleared: [false; 9],
            initial,
            focused: 0,
            error: None,
            submitting: false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditProfileIntent {
        if self.submitting {
            return EditProfileIntent::None;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return EditProfileIntent::Quit;
        }
        // Ctrl+S submits.
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.submit();
        }

        match key.code {
            KeyCode::Tab => {
                self.focused = (self.focused + 1) % Field::ALL.len();
                self.error = None;
            }
            KeyCode::BackTab => {
                self.focused = (self.focused + Field::ALL.len() - 1) % Field::ALL.len();
                self.error = None;
            }
            KeyCode::Enter => {
                return self.submit();
            }
            KeyCode::Backspace => {
                self.fields[self.focused].pop();
                self.cleared[self.focused] = false;
                self.error = None;
            }
            KeyCode::Delete => {
                self.fields[self.focused].clear();
                self.cleared[self.focused] = true;
                self.error = None;
            }
            KeyCode::Char(c) => {
                self.fields[self.focused].push(c);
                self.cleared[self.focused] = false;
                self.error = None;
            }
            _ => {}
        }
        EditProfileIntent::None
    }

    fn submit(&mut self) -> EditProfileIntent {
        match self.build_update() {
            Ok(update) => {
                if update.is_empty() {
                    return EditProfileIntent::Cancel;
                }
                self.submitting = true;
                EditProfileIntent::Submit {
                    update: Box::new(update),
                }
            }
            Err(msg) => {
                self.error = Some(msg);
                EditProfileIntent::None
            }
        }
    }

    pub fn finish_submit(&mut self, result: Result<(), String>) {
        self.submitting = false;
        if let Err(msg) = result {
            self.error = Some(msg);
        }
    }

    fn build_update(&self) -> Result<ProfileUpdate, String> {
        let mut u = ProfileUpdate::default();
        for (i, field) in Field::ALL.iter().enumerate() {
            let v = self.fields[i].trim().to_string();
            let cleared = self.cleared[i];
            let initial = self.initial[i].as_deref().unwrap_or("");

            // Skip if unchanged from initial value and not explicitly cleared.
            let unchanged = !cleared && v == initial;
            if unchanged {
                continue;
            }

            // Cleared with empty value → Patch::Clear.
            // Empty value (no clear flag) but initial was empty → still Skip.
            // Empty value with cleared flag → Patch::Clear.
            let patch_str = if cleared || (v.is_empty() && !initial.is_empty()) {
                Patch::Clear
            } else {
                Patch::Set(v.clone())
            };

            match field {
                Field::DisplayName => u.display_name = patch_str,
                Field::Bio => u.bio = patch_str,
                Field::WebsiteUrl => u.website_url = patch_str,
                Field::WebsiteName => u.website_name = patch_str,
                Field::WebsiteImageUrl => u.website_image_url = patch_str,
                Field::LocationName => u.location_name = patch_str,
                Field::PinnedPostId => u.pinned_post_id = patch_str,
                Field::LocationLatitude | Field::LocationLongitude => {
                    let f64_patch = if cleared || v.is_empty() {
                        Patch::Clear
                    } else {
                        let parsed: f64 = v
                            .parse()
                            .map_err(|_| format!("{} must be a number", field.label()))?;
                        Patch::Set(parsed)
                    };
                    if matches!(field, Field::LocationLatitude) {
                        u.location_latitude = f64_patch;
                    } else {
                        u.location_longitude = f64_patch;
                    }
                }
            }
        }
        u.validate()?;
        Ok(u)
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(
                " cs-tui • edit profile ",
                theme.accent_style(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut constraints: Vec<Constraint> = Field::ALL
            .iter()
            .flat_map(|_| [Constraint::Length(1), Constraint::Length(1)])
            .collect();
        constraints.push(Constraint::Length(1)); // spacer
        constraints.push(Constraint::Length(1)); // status / error

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(constraints)
            .split(inner);

        for (i, field) in Field::ALL.iter().enumerate() {
            let label_idx = i * 2;
            let input_idx = label_idx + 1;
            let style = if i == self.focused {
                theme.accent_style()
            } else {
                theme.muted_style()
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(field.label(), style))),
                layout[label_idx],
            );
            let display = if self.cleared[i] && self.fields[i].is_empty() {
                "<cleared>".to_string()
            } else {
                self.fields[i].clone()
            };
            let caret = if i == self.focused { "█" } else { "" };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(display, theme.base()),
                    Span::styled(caret, theme.accent_style()),
                ])),
                layout[input_idx],
            );
        }

        let status_idx = layout.len() - 1;
        let status: Line<'_> = if self.submitting {
            Line::from(Span::styled("saving…", theme.accent_style()))
        } else if let Some(msg) = &self.error {
            Line::from(Span::styled(msg.clone(), theme.error_style()))
        } else {
            Line::from(Span::styled(
                "tab/shift+tab focus · enter or ctrl+s save · del clear · esc cancel",
                theme.muted_style(),
            ))
        };
        frame.render_widget(Paragraph::new(status), layout[status_idx]);
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

    fn user_with(display_name: Option<&str>, bio: Option<&str>) -> User {
        User {
            id: "u".into(),
            username: "me".into(),
            display_name: display_name.map(String::from),
            email: None,
            bio: bio.map(String::from),
            pinned_post_id: None,
            website_url: None,
            website_name: None,
            website_image_url: None,
            location_latitude: None,
            location_longitude: None,
            location_name: None,
            followers_count: None,
            following_count: None,
            posts_count: None,
            is_following: None,
            follow_id: None,
            created_at: None,
        }
    }

    #[test]
    fn unchanged_fields_skip() {
        let u = user_with(Some("Alice"), Some("hello"));
        let s = EditProfileScreen::from_user(&u);
        let update = s.build_update().unwrap();
        assert!(update.is_empty());
    }

    #[test]
    fn editing_a_field_sets_patch() {
        let u = user_with(Some("Alice"), Some("hello"));
        let mut s = EditProfileScreen::from_user(&u);
        s.focused = 0; // displayName
        s.fields[0] = "Alice B".into();
        let update = s.build_update().unwrap();
        match update.display_name {
            Patch::Set(v) => assert_eq!(v, "Alice B"),
            other => panic!("expected Set, got {other:?}"),
        }
        assert!(matches!(update.bio, Patch::Skip));
    }

    #[test]
    fn clearing_a_field_sends_null() {
        let u = user_with(Some("Alice"), Some("hello"));
        let mut s = EditProfileScreen::from_user(&u);
        s.focused = 1; // bio
        s.fields[1].clear();
        s.cleared[1] = true;
        let update = s.build_update().unwrap();
        assert!(matches!(update.bio, Patch::Clear));
    }

    #[test]
    fn validation_rejects_lone_latitude() {
        let u = user_with(None, None);
        let mut s = EditProfileScreen::from_user(&u);
        s.focused = 6; // location_latitude
        s.fields[6] = "45".into();
        // longitude left blank → ProfileUpdate::validate should reject
        let result = s.build_update();
        assert!(result.is_err());
    }

    #[test]
    fn ctrl_c_quits() {
        let u = user_with(None, None);
        let mut s = EditProfileScreen::from_user(&u);
        let i = s.handle_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(i, EditProfileIntent::Quit);
    }

    #[test]
    fn ctrl_s_with_no_changes_cancels() {
        let u = user_with(Some("Alice"), Some("hi"));
        let mut s = EditProfileScreen::from_user(&u);
        let i = s.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(i, EditProfileIntent::Cancel);
    }

    #[test]
    fn tab_cycles_focus() {
        let u = user_with(None, None);
        let mut s = EditProfileScreen::from_user(&u);
        assert_eq!(s.focused, 0);
        s.handle_key(key(KeyCode::Tab, KeyModifiers::empty()));
        assert_eq!(s.focused, 1);
        s.handle_key(key(KeyCode::BackTab, KeyModifiers::empty()));
        assert_eq!(s.focused, 0);
    }
}

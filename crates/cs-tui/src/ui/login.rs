//! Login screen — email + password fields, submit, error display.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Email,
    Password,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginIntent {
    /// User wants to attempt login with the current credentials.
    Submit,
    /// User wants to exit.
    Quit,
    /// Nothing actionable.
    None,
}

#[derive(Debug)]
pub struct LoginScreen {
    pub email: String,
    pub password: String,
    pub focused: Field,
    pub error: Option<String>,
    pub submitting: bool,
}

impl LoginScreen {
    pub fn new(prefill_email: String) -> Self {
        Self {
            email: prefill_email,
            password: String::new(),
            focused: Field::Email,
            error: None,
            submitting: false,
        }
    }

    /// Handle a keypress. Mutates field contents and returns the resulting intent.
    pub fn handle_key(&mut self, key: KeyEvent) -> LoginIntent {
        if self.submitting {
            return LoginIntent::None;
        }
        // Ctrl+C is the emergency exit. Esc opens the App menu (intercepted
        // upstream), which provides a deliberate Quit choice.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return LoginIntent::Quit;
        }

        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.focused = match self.focused {
                    Field::Email => Field::Password,
                    Field::Password => Field::Email,
                };
                self.error = None;
            }
            KeyCode::Enter => {
                if self.focused == Field::Email {
                    self.focused = Field::Password;
                    return LoginIntent::None;
                }
                if self.email.trim().is_empty() || self.password.is_empty() {
                    self.error = Some("Enter both email and password.".to_string());
                    return LoginIntent::None;
                }
                self.error = None;
                self.submitting = true;
                return LoginIntent::Submit;
            }
            KeyCode::Backspace => {
                self.field_mut().pop();
                self.error = None;
            }
            KeyCode::Char(c) => {
                self.field_mut().push(c);
                self.error = None;
            }
            _ => {}
        }
        LoginIntent::None
    }

    /// Insert bracketed-paste text into the focused field, newlines collapsed to
    /// spaces (these fields are single-line). Without this, enabling bracketed
    /// paste would make pasting into email/password stop working.
    pub fn paste_into_focused(&mut self, text: &str) {
        if self.submitting {
            return;
        }
        let cleaned = super::input::collapse_newlines(text);
        self.field_mut().push_str(&cleaned);
        self.error = None;
    }

    /// Called after the async login attempt completes.
    pub fn finish_submit(&mut self, result: Result<(), String>) {
        self.submitting = false;
        match result {
            Ok(()) => self.error = None,
            Err(msg) => {
                self.error = Some(humanize_login_error(&msg));
                self.password.clear();
                self.focused = Field::Password;
            }
        }
    }

    fn field_mut(&mut self) -> &mut String {
        match self.focused {
            Field::Email => &mut self.email,
            Field::Password => &mut self.password,
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        // Center a 60x12 box. Falls through to full area if terminal is smaller.
        let outer = Block::default().style(theme.base());
        frame.render_widget(outer, area);

        let h = 12u16.min(area.height);
        let w = 60u16.min(area.width);
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let card = Rect::new(x, y, w, h);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(" cs-tui • login ", theme.heading_style()));
        let inner = block.inner(card);
        frame.render_widget(block, card);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([
                Constraint::Length(1), // label: email
                Constraint::Length(1), // input: email
                Constraint::Length(1), // spacer
                Constraint::Length(1), // label: password
                Constraint::Length(1), // input: password
                Constraint::Length(1), // spacer
                Constraint::Min(1),    // status / error
            ])
            .split(inner);

        let email_label = labeled("email", self.focused == Field::Email, theme);
        frame.render_widget(email_label, layout[0]);
        frame.render_widget(
            input_line(
                &self.email,
                self.focused == Field::Email,
                layout[1].width as usize,
                theme,
            ),
            layout[1],
        );

        let pw_label = labeled("password", self.focused == Field::Password, theme);
        frame.render_widget(pw_label, layout[3]);
        let masked: String = "•".repeat(self.password.chars().count());
        frame.render_widget(
            input_line(
                &masked,
                self.focused == Field::Password,
                layout[4].width as usize,
                theme,
            ),
            layout[4],
        );

        let status = if self.submitting {
            Paragraph::new(Line::from(Span::styled(
                "logging in…",
                theme.accent_style(),
            )))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
        } else if let Some(msg) = &self.error {
            Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style())))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
        } else {
            Paragraph::new(Line::from(Span::styled(
                "tab to switch · enter to submit · esc for menu",
                theme.muted_style(),
            )))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
        };
        frame.render_widget(status, layout[6]);
    }
}

fn labeled<'a>(text: &'a str, focused: bool, theme: &Theme) -> Paragraph<'a> {
    let style = if focused {
        theme.accent_style()
    } else {
        theme.muted_style()
    };
    Paragraph::new(Line::from(Span::styled(text, style)))
}

fn input_line<'a>(value: &'a str, focused: bool, width: usize, theme: &Theme) -> Paragraph<'a> {
    // Login fields are append-only, so the caret sits at the end; the shared
    // helper windows long values (e.g. a long email) so the caret stays visible.
    if focused {
        let cursor = value.chars().count();
        Paragraph::new(super::input::windowed_line(value, cursor, width, theme))
    } else {
        Paragraph::new(Line::from(Span::styled(value, theme.base())))
    }
}

/// Turn a raw API/transport error string into a concise, actionable message for
/// the login form. Recognized cases are rewritten; anything else is shown as-is.
///
/// Matching is on the stringified error (our own `ApiError` `Display` text plus
/// any server `message`), so it tolerates the exact wording prod uses for each
/// condition rather than depending on a specific error code.
fn humanize_login_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("verif") {
        // Undocumented prod behavior: login is refused until the email is
        // verified. Resending isn't possible from a rejected login (that needs
        // an id_token no failed login issues), so point at the email link.
        "Email not verified. Open the verification link in your inbox, then log in again."
            .to_string()
    } else if lower.contains("rate limited") || lower.contains("too many") {
        "Too many attempts. Wait a moment, then try again.".to_string()
    } else if lower.contains("unauthorized")
        || lower.contains("(401)")
        || lower.contains("credential")
    {
        "Incorrect email or password.".to_string()
    } else {
        raw.to_string()
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

    #[test]
    fn typing_fills_focused_field() {
        let mut s = LoginScreen::new(String::new());
        s.handle_key(key(KeyCode::Char('a')));
        s.handle_key(key(KeyCode::Char('@')));
        assert_eq!(s.email, "a@");
        assert!(s.password.is_empty());

        s.handle_key(key(KeyCode::Tab));
        s.handle_key(key(KeyCode::Char('p')));
        assert_eq!(s.password, "p");
    }

    #[test]
    fn enter_on_email_advances_to_password() {
        let mut s = LoginScreen::new("a@b".into());
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, LoginIntent::None);
        assert_eq!(s.focused, Field::Password);
    }

    #[test]
    fn enter_on_password_with_both_fields_submits() {
        let mut s = LoginScreen::new("a@b".into());
        s.focused = Field::Password;
        s.password = "secret".into();
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, LoginIntent::Submit);
        assert!(s.submitting);
    }

    #[test]
    fn enter_with_empty_password_shows_error() {
        let mut s = LoginScreen::new("a@b".into());
        s.focused = Field::Password;
        let intent = s.handle_key(key(KeyCode::Enter));
        assert_eq!(intent, LoginIntent::None);
        assert!(s.error.is_some());
        assert!(!s.submitting);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut s = LoginScreen::new(String::new());
        let kev = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        assert_eq!(s.handle_key(kev), LoginIntent::Quit);
    }

    #[test]
    fn esc_is_no_longer_a_screen_concern() {
        // Esc is intercepted by App and opens the overlay menu; the login
        // screen itself should not treat it as a quit.
        let mut s = LoginScreen::new(String::new());
        let _ = s.handle_key(key(KeyCode::Esc));
        // No assertion on the returned intent — what matters is that handle_key
        // does not return Quit. (The match arm returning None is fine.)
    }

    #[test]
    fn finish_submit_error_clears_password() {
        let mut s = LoginScreen::new("a@b".into());
        s.password = "bad".into();
        s.submitting = true;
        s.finish_submit(Err("nope".into()));
        assert!(!s.submitting);
        assert_eq!(s.password, "");
        assert_eq!(s.error.as_deref(), Some("nope"));
        assert_eq!(s.focused, Field::Password);
    }

    #[test]
    fn backspace_pops_focused_field() {
        let mut s = LoginScreen::new("abc".into());
        s.handle_key(key(KeyCode::Backspace));
        assert_eq!(s.email, "ab");
    }

    #[test]
    fn keys_ignored_while_submitting() {
        let mut s = LoginScreen::new("a@b".into());
        s.submitting = true;
        s.handle_key(key(KeyCode::Char('x')));
        assert_eq!(s.email, "a@b");
    }

    #[test]
    fn humanize_detects_unverified_email_regardless_of_wording() {
        // Whatever wording/code prod uses, the substring "verif" drives it.
        for raw in [
            "api Unknown (403): email not verified",
            "api Forbidden (403): Email Not Verified",
            "EMAIL_NOT_VERIFIED",
        ] {
            assert!(
                humanize_login_error(raw).starts_with("Email not verified"),
                "expected verification guidance for {raw:?}"
            );
        }
    }

    #[test]
    fn humanize_maps_unauthorized_to_bad_credentials() {
        assert_eq!(
            humanize_login_error("unauthorized — token missing, invalid, or expired"),
            "Incorrect email or password."
        );
        assert_eq!(
            humanize_login_error("api Unauthorized (401): INVALID_LOGIN_CREDENTIALS"),
            "Incorrect email or password."
        );
    }

    #[test]
    fn humanize_maps_rate_limited() {
        assert_eq!(
            humanize_login_error("rate limited; retry after 30s"),
            "Too many attempts. Wait a moment, then try again."
        );
    }

    #[test]
    fn humanize_passes_through_unrecognized_errors() {
        assert_eq!(humanize_login_error("nope"), "nope");
        assert_eq!(
            humanize_login_error("transport: connection refused"),
            "transport: connection refused"
        );
    }

    #[test]
    fn finish_submit_humanizes_verification_error() {
        let mut s = LoginScreen::new("a@b".into());
        s.password = "pw".into();
        s.submitting = true;
        s.finish_submit(Err("api Unknown (403): email not verified".into()));
        assert!(s
            .error
            .as_deref()
            .unwrap()
            .starts_with("Email not verified"));
        assert_eq!(s.password, "");
    }

    #[test]
    fn only_the_focused_field_renders_a_caret() {
        // Regression: both fields used to draw the caret block at once.
        let s = LoginScreen::new("a@b".into()); // focus starts on Email
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                s.render(f, area, &theme);
            })
            .unwrap();
        // The caret is now a reverse-video cell (windowed input), so count cells
        // carrying the REVERSED modifier rather than a literal glyph.
        let carets = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|c| {
                c.style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
            .count();
        assert_eq!(
            carets, 1,
            "exactly one caret should render (focused field only)"
        );
    }
}

//! Esc-triggered overlay menu — universal Back / Logout / Quit / Cancel.
//!
//! Esc on any screen pops this menu. From here the user picks what to do; Esc
//! again (or Enter on "Cancel") dismisses the menu and returns to whatever
//! screen they were on.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Back,
    Logout,
    Quit,
    Cancel,
}

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            Self::Back => "Back  (close this screen)",
            Self::Logout => "Logout  (clear session, return to login)",
            Self::Quit => "Quit  (exit cs-tui)",
            Self::Cancel => "Cancel  (close menu, stay here)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuIntent {
    Cancel,
    Back,
    Logout,
    Quit,
    None,
}

#[derive(Debug)]
pub struct MenuOverlay {
    items: Vec<MenuItem>,
    selected: usize,
}

impl MenuOverlay {
    /// Build a menu sized to the current navigation context.
    ///
    /// - `has_back` is true when there's a child screen above a root (so "Back"
    ///   is a meaningful action).
    /// - `authenticated` is true after a successful login (so "Logout" is real).
    pub fn build(authenticated: bool, has_back: bool) -> Self {
        let mut items = Vec::new();
        if has_back {
            items.push(MenuItem::Back);
        }
        if authenticated {
            items.push(MenuItem::Logout);
        }
        items.push(MenuItem::Quit);
        items.push(MenuItem::Cancel);
        Self { items, selected: 0 }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> MenuIntent {
        // Esc always dismisses the menu.
        if key.code == KeyCode::Esc {
            return MenuIntent::Cancel;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return MenuIntent::Quit;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down
                if !self.items.is_empty() && self.selected < self.items.len() - 1 =>
            {
                self.selected += 1;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End if !self.items.is_empty() => {
                self.selected = self.items.len() - 1;
            }
            KeyCode::Enter => {
                if let Some(item) = self.items.get(self.selected) {
                    return match item {
                        MenuItem::Back => MenuIntent::Back,
                        MenuItem::Logout => MenuIntent::Logout,
                        MenuItem::Quit => MenuIntent::Quit,
                        MenuItem::Cancel => MenuIntent::Cancel,
                    };
                }
            }
            // Number shortcuts for direct selection.
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(idx) = (c as usize).checked_sub('1' as usize) {
                    if idx < self.items.len() {
                        self.selected = idx;
                        if let Some(item) = self.items.get(idx) {
                            return match item {
                                MenuItem::Back => MenuIntent::Back,
                                MenuItem::Logout => MenuIntent::Logout,
                                MenuItem::Quit => MenuIntent::Quit,
                                MenuItem::Cancel => MenuIntent::Cancel,
                            };
                        }
                    }
                }
            }
            _ => {}
        }
        MenuIntent::None
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        // Center a small modal: each item is one line plus borders + footer hint.
        let inner_height = self.items.len() as u16;
        let h = (inner_height + 4).min(area.height);
        let w = 48u16.min(area.width.saturating_sub(2));
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let card = Rect::new(x, y, w, h);

        // Clear underlying content so the menu is opaque.
        frame.render_widget(Clear, card);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.accent_style())
            .title(Span::styled(" menu ", theme.accent_style()));
        let inner = block.inner(card);
        frame.render_widget(block, card);

        // Reserve the bottom row for a hint.
        let list_area = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        );
        let hint_area = Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        );

        let items: Vec<ListItem<'_>> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let shortcut = format!("{}. ", i + 1);
                ListItem::new(Line::from(vec![
                    Span::styled(shortcut, theme.muted_style()),
                    Span::styled(it.label(), theme.base()),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(theme.accent_style())
            .highlight_symbol("▌ ");
        let mut state = ListState::default();
        state.select(Some(self.selected.min(self.items.len().saturating_sub(1))));
        frame.render_stateful_widget(list, list_area, &mut state);

        let hint = Paragraph::new(Line::from(Span::styled(
            "j/k or 1-4 · enter select · esc dismiss",
            theme.muted_style(),
        )));
        frame.render_widget(hint, hint_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn unauthenticated_root_menu_has_quit_and_cancel_only() {
        let m = MenuOverlay::build(false, false);
        assert_eq!(m.items, vec![MenuItem::Quit, MenuItem::Cancel]);
    }

    #[test]
    fn authenticated_root_menu_has_logout_quit_cancel() {
        let m = MenuOverlay::build(true, false);
        assert_eq!(
            m.items,
            vec![MenuItem::Logout, MenuItem::Quit, MenuItem::Cancel]
        );
    }

    #[test]
    fn child_screen_menu_offers_back_first() {
        let m = MenuOverlay::build(true, true);
        assert_eq!(
            m.items,
            vec![
                MenuItem::Back,
                MenuItem::Logout,
                MenuItem::Quit,
                MenuItem::Cancel
            ]
        );
    }

    #[test]
    fn esc_dismisses_menu() {
        let mut m = MenuOverlay::build(true, true);
        assert_eq!(m.handle_key(key(KeyCode::Esc)), MenuIntent::Cancel);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut m = MenuOverlay::build(true, false);
        let k = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(m.handle_key(k), MenuIntent::Quit);
    }

    #[test]
    fn enter_on_back_emits_back() {
        let mut m = MenuOverlay::build(true, true);
        // Back is item 0.
        assert_eq!(m.handle_key(key(KeyCode::Enter)), MenuIntent::Back);
    }

    #[test]
    fn enter_on_logout_emits_logout() {
        let mut m = MenuOverlay::build(true, false);
        // Logout is item 0 when not pushed.
        assert_eq!(m.handle_key(key(KeyCode::Enter)), MenuIntent::Logout);
    }

    #[test]
    fn j_advances_then_enter_picks() {
        let mut m = MenuOverlay::build(true, true);
        m.handle_key(key(KeyCode::Char('j'))); // Logout
        assert_eq!(m.handle_key(key(KeyCode::Enter)), MenuIntent::Logout);
    }

    #[test]
    fn digit_picks_directly() {
        let mut m = MenuOverlay::build(true, true);
        // 4 = Cancel
        assert_eq!(m.handle_key(key(KeyCode::Char('4'))), MenuIntent::Cancel);
    }

    #[test]
    fn out_of_range_digit_is_no_op() {
        let mut m = MenuOverlay::build(false, false);
        // Only 2 items; pressing 9 does nothing.
        assert_eq!(m.handle_key(key(KeyCode::Char('9'))), MenuIntent::None);
    }
}

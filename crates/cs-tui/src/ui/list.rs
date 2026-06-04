//! Shared cursor-paginated list state and rendering.
//!
//! Every list screen (feed, topic feed, bookmarks, notifications, guilds, and
//! each profile tab) holds the same paged state — items, a selection cursor, a
//! next-page cursor, loading/error flags — and renders the same body branches.
//! That was copy-pasted per screen, so fixes (e.g. "a load-more failure must not
//! blank an already-loaded list") had to be ported by hand to each one and were
//! easy to miss. This centralizes both.
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

/// Paged list state, generic over the item type.
#[derive(Debug)]
pub struct TabState<T> {
    pub items: Vec<T>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Whether an initial load has completed (used by lazily-loaded profile tabs).
    pub loaded: bool,
}

// Manual `Default` — `#[derive(Default)]` would add a `T: Default` bound that the
// payload types (Entry, Reply, Follow, …) don't satisfy.
impl<T> Default for TabState<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            next_cursor: None,
            loading: false,
            error: None,
            loaded: false,
        }
    }
}

impl<T> TabState<T> {
    /// A state that starts out loading (for screens that fetch on creation).
    #[must_use]
    pub fn loading() -> Self {
        Self {
            loading: true,
            ..Self::default()
        }
    }

    /// Apply an initial load / refresh. `view_len` is the count after any
    /// screen-specific filtering (e.g. NSFW) — the selection clamps to it.
    pub fn apply_initial_filtered(
        &mut self,
        result: Result<(Vec<T>, Option<String>), String>,
        view_len: impl FnOnce(&Self) -> usize,
    ) {
        self.loading = false;
        self.loaded = true;
        match result {
            Ok((items, cursor)) => {
                self.items = items;
                self.next_cursor = cursor;
                if self.selected >= view_len(self) {
                    self.selected = 0;
                }
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    /// Apply an initial load, clamping selection to the raw item count.
    pub fn apply_initial(&mut self, result: Result<(Vec<T>, Option<String>), String>) {
        self.apply_initial_filtered(result, |s| s.items.len());
    }

    /// Append a load-more page (selection is unaffected).
    pub fn apply_more(&mut self, result: Result<(Vec<T>, Option<String>), String>) {
        self.loading = false;
        match result {
            Ok((mut items, cursor)) => {
                self.items.append(&mut items);
                self.next_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }
}

/// Inline status text for a load-more failure: shown when the list is already
/// populated, so the list stays put and the error rides the status line instead
/// of replacing the whole view. `None` when there's nothing to surface there.
#[must_use]
pub fn load_more_error<T>(state: &TabState<T>) -> Option<String> {
    if !state.loading && !state.items.is_empty() {
        state
            .error
            .as_ref()
            .map(|msg| format!("⚠ {msg} · scroll or r to retry"))
    } else {
        None
    }
}

/// Render the list body into `area` over `visible` (indices into `state.items`;
/// pass every index for unfiltered screens). Branch order keeps a non-empty list
/// visible even when a load-more error is set — the error rides the status line
/// (see [`load_more_error`]); the full-area error is reserved for a failed
/// *initial* load.
pub fn render_body<T, F>(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    state: &TabState<T>,
    visible: &[usize],
    empty_label: &str,
    item: F,
) where
    F: Fn(&T) -> ListItem<'static>,
{
    if state.loading && state.items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("loading…", theme.accent_style()))),
            area,
        );
        return;
    }
    if !visible.is_empty() {
        let items: Vec<ListItem<'static>> =
            visible.iter().map(|&i| item(&state.items[i])).collect();
        let list = List::new(items)
            .highlight_style(theme.accent_style())
            .highlight_symbol("▌ ");
        let mut list_state = ListState::default();
        list_state.select(Some(state.selected.min(visible.len().saturating_sub(1))));
        frame.render_stateful_widget(list, area, &mut list_state);
        return;
    }
    if let Some(msg) = &state.error {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg.clone(), theme.error_style()))),
            area,
        );
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(empty_label, theme.muted_style()))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_initial_populates_and_clears_loading() {
        let mut s: TabState<i32> = TabState::loading();
        s.apply_initial(Ok((vec![1, 2, 3], Some("c".into()))));
        assert_eq!(s.items, vec![1, 2, 3]);
        assert!(!s.loading);
        assert!(s.loaded);
        assert_eq!(s.next_cursor.as_deref(), Some("c"));
    }

    #[test]
    fn apply_more_appends_and_keeps_cursor() {
        let mut s: TabState<i32> = TabState::default();
        s.apply_initial(Ok((vec![1], Some("c".into()))));
        s.apply_more(Ok((vec![2, 3], None)));
        assert_eq!(s.items, vec![1, 2, 3]);
        assert!(s.next_cursor.is_none());
    }

    #[test]
    fn load_more_error_only_surfaces_with_existing_items() {
        let mut s: TabState<i32> = TabState::default();
        // Empty + error = initial-load failure → not an inline status error.
        s.apply_initial(Err("boom".into()));
        assert!(load_more_error(&s).is_none());

        // Populated, then a load-more fails → inline status error.
        s.apply_initial(Ok((vec![1], Some("c".into()))));
        s.apply_more(Err("blip".into()));
        assert!(load_more_error(&s).unwrap().contains("blip"));
    }

    #[test]
    fn filtered_initial_clamps_selection_to_view() {
        let mut s: TabState<i32> = TabState {
            selected: 5,
            ..Default::default()
        };
        // Only 2 items are "visible" → selection resets.
        s.apply_initial_filtered(Ok((vec![1, 2, 3], None)), |_| 2);
        assert_eq!(s.selected, 0);
    }
}

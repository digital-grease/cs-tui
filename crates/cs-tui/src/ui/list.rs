//! Shared cursor-paginated list state and rendering.
//!
//! Every list screen (feed, topic feed, bookmarks, notifications, guilds, and
//! each profile tab) holds the same paged state — items, a selection cursor, a
//! next-page cursor, loading/error flags — and renders the same body branches.
//! That was copy-pasted per screen, so fixes (e.g. "a load-more failure must not
//! blank an already-loaded list") had to be ported by hand to each one and were
//! easy to miss. This centralizes both.
use std::cell::Cell;

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::config::SelectionStyle;

use super::theme::Theme;

/// Paged list state, generic over the item type.
#[derive(Debug, Clone)]
pub struct TabState<T> {
    pub items: Vec<T>,
    pub selected: usize,
    pub next_cursor: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Whether an initial load has completed (used by lazily-loaded profile tabs).
    pub loaded: bool,
    /// Persisted vertical scroll offset (index of the first visible row), kept
    /// across renders so the viewport scrolls naturally instead of re-deriving
    /// from 0 each frame (which pins the selection to the bottom row when
    /// scrolling back up). Updated by [`render_body`].
    list_offset: Cell<usize>,
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
            list_offset: Cell::new(0),
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

    /// Shift the persisted scroll offset (used when items are prepended at the
    /// top, e.g. background feed refresh, so the viewport keeps the same rows in
    /// view rather than jumping).
    pub fn shift_offset(&self, delta: usize) {
        self.list_offset.set(self.list_offset.get() + delta);
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
        // `fill` paints the whole selected row (bg only, so each span keeps its
        // color); `bar` keeps the older bold-accent recolor. Both repeat the `▌`
        // bar down every line of a multi-line item and reserve the gutter always,
        // so selection doesn't shift the row sideways.
        let highlight = match crate::config::get().selection {
            SelectionStyle::Fill => theme.selection_style(),
            SelectionStyle::Bar => theme.accent_style(),
        };
        let list = List::new(items)
            .highlight_style(highlight)
            .highlight_symbol("▌ ")
            .repeat_highlight_symbol(true)
            .highlight_spacing(HighlightSpacing::Always);
        let sel = state.selected.min(visible.len().saturating_sub(1));
        let mut list_state = ListState::default()
            .with_offset(state.list_offset.get())
            .with_selected(Some(sel));
        frame.render_stateful_widget(list, area, &mut list_state);
        // Persist the offset ratatui settled on so next frame scrolls naturally
        // (only when the selection leaves the window) instead of snapping the
        // selection to the bottom row on every upward move.
        state.list_offset.set(list_state.offset());
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

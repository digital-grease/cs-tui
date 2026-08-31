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
    /// How many of the first drawn item's top rows are cut off, so a pane can
    /// scroll by *rows* rather than by whole items.
    ///
    /// A `List` tiles whole items and cannot draw a partial one at the top, so
    /// wherever the next message up doesn't fit it leaves those rows blank.
    /// In a conversation that reads as a permanent gap under the title, and it
    /// never closes on its own: its size is fixed by the pane height and the
    /// heights of the messages at the tail. Screens that care (cIRC) compute a
    /// row-accurate window and set it with [`TabState::set_window`]; everything
    /// else leaves this at 0 and tiles as before.
    top_clip: Cell<u16>,
    /// How many whole items the last render fitted on screen, so PgUp and PgDn
    /// move a real page rather than a guessed number of rows.
    ///
    /// A fixed jump was the obvious alternative and is wrong here: items are
    /// not a uniform height. A feed card is three or four rows of text, but one
    /// carrying an inline picture is a dozen, so "ten items" can mean half a
    /// screen or three screens depending on what happens to be in the list.
    /// Recorded by [`render_body`], which is the only place that knows both the
    /// heights and the area.
    page_items: Cell<usize>,
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
            top_clip: Cell::new(0),
            page_items: Cell::new(0),
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
                // The items are replaced wholesale, so the persisted scroll offset
                // (an index into the *previous* list) is now meaningless. Reset it
                // to 0 and let the next render re-derive the viewport from the
                // selection — otherwise a refresh that returns fewer items than
                // were on screen (e.g. cIRC's post-send reload after live messages
                // have accumulated) leaves the offset clamped past the new end,
                // blanking the pane and stranding the selected row at the top.
                self.list_offset.set(0);
                self.top_clip.set(0);
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

    /// Shift the persisted scroll offset back, for items dropped off the *top*.
    ///
    /// The mirror of [`Self::shift_offset`], for a bounded history that trims
    /// its oldest entries: without it the offset still points at rows that are
    /// no longer there, and the pane jumps.
    pub fn shift_offset_back(&self, delta: usize) {
        self.list_offset
            .set(self.list_offset.get().saturating_sub(delta));
    }

    /// Place the viewport by row: `offset` names the first drawn item and `clip`
    /// how many of its top rows are cut off.
    ///
    /// The caller owns the scroll policy — it is the only thing that knows every
    /// item's height — and [`render_body_indexed`] draws exactly this window.
    pub fn set_window(&self, offset: usize, clip: u16) {
        self.list_offset.set(offset);
        self.top_clip.set(clip);
    }

    /// How many of the first drawn item's top rows the last render cut off.
    ///
    /// Only meaningful after a render, and 0 on every screen that tiles whole
    /// items. A caller overlaying anything onto the pane (pictures, chip links)
    /// needs it to know where the first item's rows actually start.
    #[must_use]
    pub fn top_clip(&self) -> u16 {
        self.top_clip.get()
    }

    /// How many whole items the last render fitted on screen.
    ///
    /// One before the first render, so paging before anything is drawn behaves
    /// like a single step rather than jumping nowhere.
    #[must_use]
    pub fn page_items(&self) -> usize {
        self.page_items.get().max(1)
    }

    /// The scroll offset the last [`render_body`] settled on: an index into the
    /// `visible` slice naming the first row actually drawn.
    ///
    /// Only meaningful after a render. A caller that has to reason about which
    /// items are on screen (overlaying links onto them, say) needs this, since
    /// the list widget owns the scroll and the screen otherwise cannot tell a
    /// scrolled-up pane from one sitting at the bottom.
    #[must_use]
    pub fn list_offset(&self) -> usize {
        self.list_offset.get()
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

/// A conversation pane's viewport, in rows rather than in whole items.
///
/// `first`/`last` index the same slice the heights came from; `top` and
/// `bottom` are how many of that item's own rows the pane edge cuts off.
///
/// Fed to [`TabState::set_window`] and drawn by [`render_body_indexed`], which
/// trims the edge items' lines to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowWindow {
    pub first: usize,
    pub top: u16,
    pub last: usize,
    pub bottom: u16,
}

/// Where the viewport sits over `heights`, given where the reader's cursor is
/// and where the last render left it.
///
/// Only the caller knows how tall each item draws, so the scroll policy lives
/// here rather than in the widget.
///
/// The whole point is that it never leaves a row unused. A pane that scrolls by
/// whole items has to blank however many rows the message at the edge
/// doesn't fit in, and that count is a function of the pane height and the
/// item heights alone — not of anything the reader can change — so the band
/// stays until the terminal is resized again. Scrolling by row instead means
/// the item at the edge is simply drawn short.
///
/// `prev` is the window the previous render settled on, so a cursor that is
/// already on screen leaves the view exactly where the reader left it instead
/// of re-deriving a viewport every frame.
pub fn row_window(heights: &[u16], selected: usize, prev: (usize, u16), pane: u16) -> RowWindow {
    let last_item = heights.len().saturating_sub(1);
    let empty = RowWindow {
        first: 0,
        top: 0,
        last: last_item,
        bottom: 0,
    };
    if heights.is_empty() || pane == 0 {
        return empty;
    }
    // Row at which each item starts, plus the total as a final entry, so a
    // row can be located with one binary search.
    let mut starts: Vec<u32> = Vec::with_capacity(heights.len() + 1);
    let mut acc = 0u32;
    for &h in heights {
        starts.push(acc);
        acc = acc.saturating_add(u32::from(h));
    }
    starts.push(acc);
    let total = acc;
    let pane_rows = u32::from(pane);
    if total <= pane_rows {
        return empty;
    }
    let max_row = total - pane_rows;
    let selected = selected.min(last_item);
    let row = if selected == last_item {
        // Anchored on the last item: a live conversation must keep its
        // tail flush against the composer, whatever is above it.
        max_row
    } else {
        let sel_top = starts[selected];
        let sel_bottom = starts[selected + 1];
        let mut row = starts
            .get(prev.0)
            .copied()
            .unwrap_or(0)
            .saturating_add(u32::from(prev.1))
            .min(max_row);
        if sel_top < row || u32::from(heights[selected]) >= pane_rows {
            // Above the window, or too tall to ever fit inside it: show the
            // item's head, which is where its speaker row is.
            row = sel_top;
        } else if sel_bottom > row + pane_rows {
            row = sel_bottom - pane_rows;
        }
        row.min(max_row)
    };
    // `partition_point` gives the first start *past* `row`; the item that
    // owns the row is the one before it. `starts[0]` is 0 and `row <= max_row <
    // total`, so this is always in range.
    let first = starts.partition_point(|&s| s <= row) - 1;
    let end = row + pane_rows;
    let last = starts.partition_point(|&s| s < end) - 1;
    RowWindow {
        first,
        top: u16::try_from(row - starts[first]).unwrap_or(u16::MAX),
        last,
        bottom: u16::try_from(starts[last + 1].saturating_sub(end)).unwrap_or(u16::MAX),
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
    render_body_indexed(frame, area, theme, state, visible, empty_label, |_, t| {
        item(t)
    });
}

/// [`render_body`], with the item's position in `visible` handed to the builder.
///
/// A screen that scrolls by row (see [`TabState::set_window`]) needs this: only
/// the caller can trim the top item's own lines, since only it knows how that
/// item's rows were laid out. The position is into `visible`, the same index
/// space as [`TabState::list_offset`].
pub fn render_body_indexed<T, F>(
    frame: &mut Frame<'_>,
    area: Rect,
    theme: &Theme,
    state: &TabState<T>,
    visible: &[usize],
    empty_label: &str,
    item: F,
) where
    F: Fn(usize, &T) -> ListItem<'static>,
{
    if state.loading && state.items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("loading…", theme.accent_style()))),
            area,
        );
        return;
    }
    if !visible.is_empty() {
        let items: Vec<ListItem<'static>> = visible
            .iter()
            .enumerate()
            .map(|(pos, &i)| item(pos, &state.items[i]))
            .collect();
        // `fill` paints the whole selected row (bg only, so each span keeps its
        // color); `bar` keeps the older bold-accent recolor. Both repeat the `▌`
        // bar down every line of a multi-line item and reserve the gutter always,
        // so selection doesn't shift the row sideways.
        let highlight = match crate::config::get().selection {
            SelectionStyle::Fill => theme.selection_style(),
            SelectionStyle::Bar => theme.accent_style(),
        };
        // Heights read before `List::new` takes the items, and from those very
        // items, so the page size matches what is tiled rather than an estimate.
        let heights: Vec<u16> = items
            .iter()
            .map(|i| u16::try_from(i.height()).unwrap_or(u16::MAX))
            .collect();
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
        let mut used = 0u16;
        let fitted = heights
            .iter()
            .skip(list_state.offset())
            .take_while(|&&h| {
                used = used.saturating_add(h);
                used <= area.height
            })
            .count();
        // At least one: an item taller than the pane still counts as a page, or
        // PgDn would never move past it.
        state.page_items.set(fitted.max(1));
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
    fn the_window_anchors_on_the_newest_message() {
        // 5 items of 2 rows in a 7-row pane: the tail is flush at the bottom
        // and the second item is cut to its last row.
        let heights = [2u16; 5];
        assert_eq!(
            row_window(&heights, 4, (0, 0), 7),
            RowWindow {
                first: 1,
                top: 1,
                last: 4,
                bottom: 0
            },
        );
    }

    #[test]
    fn a_cursor_already_on_screen_leaves_the_window_alone() {
        let heights = [2u16; 5];
        // Row 3 of 10, cursor on the item spanning rows 4-5: on screen
        // already, so the view does not move.
        let prev = (1, 1);
        assert_eq!(
            row_window(&heights, 2, prev, 4),
            RowWindow {
                first: 1,
                top: 1,
                last: 3,
                bottom: 1
            },
        );
        // Stepping the cursor above the window pulls it up to that item's
        // head rather than scrolling by a whole screen.
        assert_eq!(
            row_window(&heights, 0, prev, 4),
            RowWindow {
                first: 0,
                top: 0,
                last: 1,
                bottom: 0
            },
        );
    }

    #[test]
    fn an_item_taller_than_the_pane_shows_its_head() {
        // Its speaker row is the part worth seeing, and anchoring on its bottom
        // would hide who is talking.
        let heights = [2u16, 9, 2];
        assert_eq!(
            row_window(&heights, 1, (0, 0), 4),
            RowWindow {
                first: 1,
                top: 0,
                last: 1,
                bottom: 5
            },
        );
    }

    #[test]
    fn a_list_shorter_than_the_pane_is_not_scrolled() {
        let heights = [2u16, 2, 2];
        assert_eq!(
            row_window(&heights, 2, (0, 0), 20),
            RowWindow {
                first: 0,
                top: 0,
                last: 2,
                bottom: 0
            },
        );
    }

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
    fn apply_initial_resets_stale_scroll_offset() {
        // Regression (cIRC blank-pane-on-post): a persisted scroll offset from a
        // previously-longer list must not survive a refresh that replaces the
        // items with a shorter page, or ratatui clamps the offset past the new
        // end and renders only the selected row at the top of a blank pane.
        let mut s: TabState<i32> = TabState::default();
        s.apply_initial(Ok(((0..100).collect(), None)));
        s.shift_offset(94); // emulate the viewport having scrolled to the tail
        assert_eq!(s.list_offset.get(), 94);
        s.apply_initial(Ok(((0..50).collect(), None)));
        assert_eq!(s.list_offset.get(), 0);
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

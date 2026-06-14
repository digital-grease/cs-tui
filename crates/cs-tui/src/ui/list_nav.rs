//! Shared keyboard navigation for cursor-paginated list screens.
//!
//! Feed, topic feed, bookmarks, notifications (and any future list) all move a
//! selection cursor with `j/k`, `g/G`, arrows, `Home/End`, and pull the next
//! page when the user scrolls past the bottom (`j`/`Down`/`n`/`PageDown`).
//! That block used to be copy-pasted per screen, so a fix had to be ported by
//! hand to each one. This centralizes it: `selected` always indexes the *visible*
//! view of length `view_len`, so the model is identical whether or not a screen
//! filters its items (e.g. NSFW hiding).
use crossterm::event::KeyCode;

/// Outcome of a navigation key, returned to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListNav {
    /// The cursor moved (or the key was a no-op nav key); the screen has nothing
    /// further to do for this key.
    Moved,
    /// The user scrolled past the end and another page is available — the screen
    /// should mark itself loading and request the next page.
    LoadMore,
    /// Not a navigation key; the screen should handle it (Enter, refresh, …).
    Ignored,
}

/// Apply a navigation key to `selected` over a view of `view_len` items.
///
/// `has_more` is whether a next cursor page exists. Returns [`ListNav::LoadMore`]
/// when scrolling down off the end with more to load, [`ListNav::Moved`] for any
/// handled cursor move, and [`ListNav::Ignored`] for non-navigation keys.
pub fn navigate(key: KeyCode, selected: &mut usize, view_len: usize, has_more: bool) -> ListNav {
    match key {
        KeyCode::Char('j') | KeyCode::Down => {
            if view_len > 0 && *selected + 1 < view_len {
                *selected += 1;
                ListNav::Moved
            } else if has_more {
                ListNav::LoadMore
            } else {
                ListNav::Moved
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            *selected = selected.saturating_sub(1);
            ListNav::Moved
        }
        KeyCode::Char('g') | KeyCode::Home => {
            *selected = 0;
            ListNav::Moved
        }
        KeyCode::Char('G') | KeyCode::End => {
            *selected = view_len.saturating_sub(1);
            ListNav::Moved
        }
        KeyCode::Char('n') | KeyCode::PageDown if has_more => ListNav::LoadMore,
        _ => ListNav::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn j_advances_until_the_last_visible_item() {
        let mut sel = 0;
        assert_eq!(
            navigate(KeyCode::Char('j'), &mut sel, 3, false),
            ListNav::Moved
        );
        assert_eq!(sel, 1);
        navigate(KeyCode::Char('j'), &mut sel, 3, false);
        assert_eq!(sel, 2);
        // At the bottom with no more pages: stays put, no load.
        assert_eq!(
            navigate(KeyCode::Char('j'), &mut sel, 3, false),
            ListNav::Moved
        );
        assert_eq!(sel, 2);
    }

    #[test]
    fn j_at_bottom_loads_more_when_a_page_exists() {
        let mut sel = 2;
        assert_eq!(
            navigate(KeyCode::Char('j'), &mut sel, 3, true),
            ListNav::LoadMore
        );
        assert_eq!(sel, 2, "load-more must not also move the cursor");
    }

    #[test]
    fn down_arrow_mirrors_j() {
        let mut sel = 0;
        assert_eq!(navigate(KeyCode::Down, &mut sel, 2, false), ListNav::Moved);
        assert_eq!(sel, 1);
    }

    #[test]
    fn k_decrements_without_underflow() {
        let mut sel = 1;
        navigate(KeyCode::Char('k'), &mut sel, 3, false);
        assert_eq!(sel, 0);
        navigate(KeyCode::Char('k'), &mut sel, 3, false);
        assert_eq!(sel, 0, "saturates at zero");
    }

    #[test]
    fn g_and_capital_g_jump_to_ends() {
        let mut sel = 1;
        navigate(KeyCode::Char('G'), &mut sel, 5, false);
        assert_eq!(sel, 4);
        navigate(KeyCode::Char('g'), &mut sel, 5, false);
        assert_eq!(sel, 0);
    }

    #[test]
    fn capital_g_on_empty_view_stays_at_zero() {
        let mut sel = 0;
        navigate(KeyCode::Char('G'), &mut sel, 0, false);
        assert_eq!(sel, 0);
    }

    #[test]
    fn n_and_pagedown_load_more_only_with_a_cursor() {
        let mut sel = 0;
        assert_eq!(
            navigate(KeyCode::Char('n'), &mut sel, 3, true),
            ListNav::LoadMore
        );
        assert_eq!(
            navigate(KeyCode::PageDown, &mut sel, 3, true),
            ListNav::LoadMore
        );
        // No next page → these are not nav keys (the screen may use them).
        assert_eq!(
            navigate(KeyCode::Char('n'), &mut sel, 3, false),
            ListNav::Ignored
        );
    }

    #[test]
    fn space_is_not_a_load_more_key() {
        // Load-on-scroll (j/Down at the bottom) covers paging, so Space is a
        // no-op the screen is free to handle, not a hidden load-more trigger.
        let mut sel = 0;
        assert_eq!(
            navigate(KeyCode::Char(' '), &mut sel, 3, true),
            ListNav::Ignored
        );
        assert_eq!(sel, 0);
    }

    #[test]
    fn empty_view_with_more_loads_on_down() {
        let mut sel = 0;
        assert_eq!(
            navigate(KeyCode::Char('j'), &mut sel, 0, true),
            ListNav::LoadMore
        );
    }

    #[test]
    fn non_navigation_keys_are_ignored() {
        let mut sel = 0;
        for k in [
            KeyCode::Enter,
            KeyCode::Char('r'),
            KeyCode::Char('b'),
            KeyCode::Char('d'),
        ] {
            assert_eq!(navigate(k, &mut sel, 3, true), ListNav::Ignored);
        }
        assert_eq!(sel, 0);
    }
}

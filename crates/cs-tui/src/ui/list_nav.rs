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

/// Whether `key` is one this module navigates with.
///
/// Exists so a screen can tell "the reader is navigating" from "the reader has
/// started typing" without keeping its own copy of the key list, which would be
/// free to drift from the match below. cIRC needs exactly that: in message
/// select mode an unrecognised letter drops back to the composer and types
/// itself, and it must not do that to `j` or `k`.
#[must_use]
pub fn is_nav_key(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::Char('j')
            | KeyCode::Char('k')
            | KeyCode::Char('g')
            | KeyCode::Char('G')
            | KeyCode::Char('n')
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    )
}

/// Apply a navigation key to `selected` over a view of `view_len` items.
///
/// `has_more` is whether a next cursor page exists. Returns [`ListNav::LoadMore`]
/// when scrolling down off the end with more to load, [`ListNav::Moved`] for any
/// handled cursor move, and [`ListNav::Ignored`] for non-navigation keys.
pub fn navigate(key: KeyCode, selected: &mut usize, view_len: usize, has_more: bool) -> ListNav {
    navigate_paged(key, selected, view_len, has_more, 1)
}

/// [`navigate`] with a real page size for PgUp and PgDn.
///
/// `page` is how many whole items the last render fitted on screen, from
/// [`super::list::TabState::page_items`]. A fixed jump is the obvious
/// alternative and is wrong: items are not a uniform height, so a feed card of
/// three rows and one carrying an inline picture of a dozen would page by wildly
/// different amounts of screen.
///
/// PgDn at the very end still asks for the next page of results when there is
/// one, which is what `n` does and what the key has always meant here; that way
/// paging to the bottom of a feed keeps going rather than stopping dead.
pub fn navigate_paged(
    key: KeyCode,
    selected: &mut usize,
    view_len: usize,
    has_more: bool,
    page: usize,
) -> ListNav {
    let page = page.max(1);
    match key {
        KeyCode::PageUp => {
            *selected = selected.saturating_sub(page);
            return ListNav::Moved;
        }
        KeyCode::PageDown => {
            let last = view_len.saturating_sub(1);
            if *selected >= last {
                // Already at the end: fall through to the load-more behaviour
                // this key has always had.
                return if has_more {
                    ListNav::LoadMore
                } else {
                    ListNav::Moved
                };
            }
            *selected = selected.saturating_add(page).min(last);
            return ListNav::Moved;
        }
        _ => {}
    }
    navigate_inner(key, selected, view_len, has_more)
}

fn navigate_inner(key: KeyCode, selected: &mut usize, view_len: usize, has_more: bool) -> ListNav {
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
        // `n` stays the explicit "next page of results" key. PgDn reaches
        // load-more only from the end of the list, in `navigate_paged`.
        KeyCode::Char('n') if has_more => ListNav::LoadMore,
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
    fn n_asks_for_the_next_page_only_when_there_is_one() {
        let mut sel = 0;
        assert_eq!(
            navigate(KeyCode::Char('n'), &mut sel, 3, true),
            ListNav::LoadMore
        );
        // No next page: not a nav key, so the screen may use it for its own.
        assert_eq!(
            navigate(KeyCode::Char('n'), &mut sel, 3, false),
            ListNav::Ignored
        );
    }

    #[test]
    fn pagedown_pages_and_only_loads_more_from_the_end() {
        // PgDn used to mean "load the next page of results" from anywhere,
        // which made it a duplicate of `n` and left no way to page through a
        // list at all. It now moves a screenful, and keeps the load-more
        // behaviour at the point where paging would otherwise stop dead.
        let mut sel = 0;
        assert_eq!(
            navigate_paged(KeyCode::PageDown, &mut sel, 20, true, 5),
            ListNav::Moved
        );
        assert_eq!(sel, 5, "a page is a page, not one item");

        sel = 19;
        assert_eq!(
            navigate_paged(KeyCode::PageDown, &mut sel, 20, true, 5),
            ListNav::LoadMore,
            "at the end it still fetches, so paging down a feed keeps going",
        );

        sel = 19;
        assert_eq!(
            navigate_paged(KeyCode::PageDown, &mut sel, 20, false, 5),
            ListNav::Moved,
            "with nothing left to fetch it simply stays put",
        );
    }

    #[test]
    fn pageup_moves_a_page_and_stops_at_the_top() {
        let mut sel = 12;
        assert_eq!(
            navigate_paged(KeyCode::PageUp, &mut sel, 20, false, 5),
            ListNav::Moved
        );
        assert_eq!(sel, 7);

        sel = 2;
        assert_eq!(
            navigate_paged(KeyCode::PageUp, &mut sel, 20, false, 5),
            ListNav::Moved
        );
        assert_eq!(sel, 0, "saturating, never wrapping to the bottom");
    }

    #[test]
    fn paging_never_runs_off_the_end_of_the_list() {
        let mut sel = 0;
        navigate_paged(KeyCode::PageDown, &mut sel, 3, false, 50);
        assert_eq!(sel, 2, "clamped to the last item, not to the page size");
    }

    #[test]
    fn a_page_of_zero_still_moves_one() {
        // `page_items` is 1 before the first render; a literal zero would make
        // the key silently dead.
        let mut sel = 3;
        navigate_paged(KeyCode::PageUp, &mut sel, 20, false, 0);
        assert_eq!(sel, 2);
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

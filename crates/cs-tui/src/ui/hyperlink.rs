//! OSC 8 terminal hyperlinks for ratatui buffers.
//!
//! ratatui draws a grid of single-grapheme cells, and its text paths
//! (`Buffer::set_stringn`, `Span::styled_graphemes`) strip every grapheme that
//! contains a control character — so an OSC 8 escape sequence can't ride inside a
//! `Span`. The supported way to emit raw escapes is to write the whole sequence
//! into one cell's `symbol` (which the crossterm backend prints verbatim) and
//! mark the cells it visually covers as "skip" so the frame diff leaves them
//! alone. This is the same trick `ratatui-image` uses to overlay graphics; here
//! the cell carries a hyperlink instead.
//!
//! The entire `OSC 8 open · visible text · OSC 8 close` triple lives in a SINGLE
//! cell, which keeps it atomic: whenever the diff redraws that cell it emits the
//! complete, self-contained link, and when it doesn't, nothing leaks. Splitting
//! the open and close across two cells would let the active-link state bleed onto
//! whatever unrelated cells the diff happens to redraw between them.

use ratatui::buffer::Buffer;

/// ESC, the lead byte of every escape/OSC sequence.
const ESC: char = '\u{1b}';

/// Strip control characters (C0 + DEL) from `s`.
///
/// Writing into a cell's `symbol` bypasses ratatui's own control-char filtering
/// (which normally protects against escape-sequence injection from posted
/// content). Since the link `url` is attacker-controlled, an embedded `ESC`/`BEL`
/// could terminate the OSC 8 sequence early and inject arbitrary terminal
/// escapes. Dropping every control char closes that hole: with no `ESC` left, no
/// further escape or OSC sequence can be formed. Real URLs contain no control
/// characters, so well-formed links are unaffected.
fn strip_controls(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Wrap `text` in an OSC 8 hyperlink to `url`:
/// `ESC ] 8 ; ; url ST   text   ESC ] 8 ; ; ST`, where the string terminator
/// `ST` is `ESC \`. Terminals that don't understand OSC 8 swallow the two OSC
/// sequences and print just `text`, so this degrades to plain text.
///
/// Both `url` and `text` are stripped of control characters first — see
/// [`strip_controls`] — so neither can break out of the sequence.
pub fn osc8(url: &str, text: &str) -> String {
    let url = strip_controls(url);
    let text = strip_controls(text);
    format!("{ESC}]8;;{url}{ESC}\\{text}{ESC}]8;;{ESC}\\")
}

/// Turn a horizontal run of already-painted cells into an OSC 8 hyperlink.
///
/// Starting at (`x`, `y`) in `buf`, the visible glyphs already on screen (e.g. a
/// URL a paragraph drew) are collected up to the first blank cell, the row's
/// right edge, or `max_cols` columns — whichever comes first. Those glyphs become
/// the link's visible text and `url` its target: the first cell's symbol is
/// replaced with the atomic OSC 8 sequence, and the rest of the run is flagged
/// `skip` so the diff keeps the glyphs the terminal printed from that sequence.
/// Cell styling (colour/underline) is left untouched. Returns the number of
/// columns linked, or 0 when there was nothing to link.
pub fn linkify_run(buf: &mut Buffer, x: u16, y: u16, url: &str, max_cols: u16) -> u16 {
    let area = buf.area;
    if max_cols == 0
        || url.trim().is_empty()
        || y < area.y
        || y >= area.y.saturating_add(area.height)
        || x < area.x
    {
        return 0;
    }
    // Don't run past the row's right edge or the caller's column budget.
    let limit = (area.x.saturating_add(area.width)).min(x.saturating_add(max_cols));

    // Collect the contiguous non-blank glyphs that form the link's visible text.
    let mut text = String::new();
    let mut cx = x;
    while cx < limit {
        let Some(cell) = buf.cell((cx, y)) else { break };
        let sym = cell.symbol();
        if sym.is_empty() || sym == " " {
            break;
        }
        text.push_str(sym);
        cx += 1;
    }
    if cx == x {
        return 0;
    }

    // First cell carries the whole atomic sequence; the trailing cells are
    // skipped so the diff leaves on screen the glyphs that sequence printed.
    let sequence = osc8(url.trim(), &text);
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(&sequence);
    }
    for sx in (x + 1)..cx {
        if let Some(cell) = buf.cell_mut((sx, y)) {
            cell.set_skip(true);
        }
    }
    cx - x
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    const ST: &str = "\u{1b}\\"; // string terminator

    #[test]
    fn osc8_wraps_text_in_open_and_close() {
        let seq = osc8("https://x.example/p", "click");
        assert_eq!(
            seq,
            format!("\u{1b}]8;;https://x.example/p{ST}click\u{1b}]8;;{ST}")
        );
    }

    #[test]
    fn osc8_strips_control_chars_to_block_escape_injection() {
        // A malicious URL tries to terminate the OSC early (ESC \) and inject a
        // clear-screen (ESC [ 2 J) plus a BEL. Every control byte is dropped, so
        // the only ESCs left are the ones the builder itself emits, and none of
        // them lead an injected sequence.
        let evil = "http://x\u{1b}\\\u{1b}[2J\u{07}/p";
        let seq = osc8(evil, "label");
        // The builder emits four ESCs: open-OSC, ST, close-OSC, ST. The URL's two
        // injected ESCs are gone, leaving exactly those four.
        assert_eq!(
            seq.matches('\u{1b}').count(),
            4,
            "only the builder's own four ESCs remain: {seq:?}"
        );
        assert!(!seq.contains('\u{07}'), "BEL stripped: {seq:?}");
        assert!(
            !seq.contains("\u{1b}[2J"),
            "the clear-screen escape never forms (no ESC precedes [2J): {seq:?}"
        );
        // The leftover bytes survive only as inert, control-free link text.
        assert!(
            seq.contains("http://x\\[2J/p"),
            "control-free url body: {seq:?}"
        );
    }

    fn buf_with(text: &str, width: u16) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 1));
        buf.set_string(0, 0, text, Style::default());
        buf
    }

    #[test]
    fn linkify_run_wraps_the_visible_url_and_skips_trailing_cells() {
        let url = "https://x.example/p";
        let mut buf = buf_with(url, 40);
        let cols = linkify_run(&mut buf, 0, 0, url, 40);
        assert_eq!(cols, url.len() as u16, "every glyph of the URL is linked");

        // The first cell now carries the whole atomic OSC 8 sequence.
        let first = buf.cell((0, 0)).unwrap().symbol().to_string();
        assert_eq!(first, osc8(url, url), "first cell holds open+text+close");

        // The remaining covered cells are skipped so the diff won't redraw them.
        for x in 1..cols {
            assert!(buf.cell((x, 0)).unwrap().skip, "cell {x} is skipped");
        }
        // The cell just past the link is untouched (not skipped).
        assert!(
            !buf.cell((cols, 0)).unwrap().skip,
            "cell past the link is live"
        );
    }

    #[test]
    fn linkify_run_stops_at_the_first_blank_cell() {
        // "url then words" — only the URL (up to the space) should be linked.
        let mut buf = buf_with("ab cd", 40);
        let cols = linkify_run(&mut buf, 0, 0, "https://full", 40);
        assert_eq!(cols, 2, "stops at the space after 'ab'");
        let first = buf.cell((0, 0)).unwrap().symbol().to_string();
        // Visible text is the on-screen glyphs ("ab"), target is the full URL.
        assert_eq!(first, osc8("https://full", "ab"));
    }

    #[test]
    fn linkify_run_truncates_visible_text_to_the_column_budget() {
        // A long URL that would wrap: only the columns that fit are linked, but
        // the full URL stays the click target.
        let url = "https://x.example/very/long/path/that/would/wrap";
        let mut buf = buf_with(url, 80);
        let cols = linkify_run(&mut buf, 0, 0, url, 10);
        assert_eq!(cols, 10, "limited to the 10-column budget");
        let first = buf.cell((0, 0)).unwrap().symbol().to_string();
        assert_eq!(
            first,
            osc8(url, &url[..10]),
            "visible text is the first 10 cols, target is the full URL"
        );
    }

    #[test]
    fn linkify_run_is_a_noop_on_a_blank_start_or_empty_url() {
        let mut buf = buf_with("   url", 40);
        assert_eq!(
            linkify_run(&mut buf, 0, 0, "https://x", 40),
            0,
            "blank start"
        );
        let mut buf2 = buf_with("url", 40);
        assert_eq!(linkify_run(&mut buf2, 0, 0, "   ", 40), 0, "empty url");
    }

    #[test]
    fn linkify_run_rejects_out_of_bounds_positions() {
        let mut buf = buf_with("url", 40);
        assert_eq!(
            linkify_run(&mut buf, 0, 5, "https://x", 40),
            0,
            "y past area"
        );
        assert_eq!(
            linkify_run(&mut buf, 0, 0, "https://x", 0),
            0,
            "zero budget"
        );
    }
}

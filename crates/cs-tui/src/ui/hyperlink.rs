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
//!
//! [`find_link_targets`] + [`apply_link_targets`] make every URL in a rendered
//! paragraph clickable: the first scans the logical lines for URLs, the second
//! overlays the links after the paragraph is drawn. They're split so the caller
//! can hand the lines to the (consuming) `Paragraph` in between.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

/// ESC, the lead byte of every escape/OSC sequence.
const ESC: char = '\u{1b}';

/// Strip control characters (C0 + DEL) from `s`.
///
/// Writing into a cell's `symbol` bypasses ratatui's own control-char filtering
/// (which normally protects against escape-sequence injection from posted
/// content). Since a link `url` is attacker-controlled, an embedded `ESC`/`BEL`
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
///
/// Guard: the collected glyphs must be a prefix of `url`. If they aren't, the
/// position was wrong (e.g. word-wrap moved the URL to another row) and the run
/// is left untouched rather than wrapping unrelated text.
pub fn linkify_run(buf: &mut Buffer, x: u16, y: u16, url: &str, max_cols: u16) -> u16 {
    let area = buf.area;
    let target = url.trim();
    if max_cols == 0
        || target.is_empty()
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
    if cx == x || !target.starts_with(text.as_str()) {
        return 0;
    }

    // First cell carries the whole atomic sequence; the trailing cells are
    // skipped so the diff leaves on screen the glyphs that sequence printed.
    let sequence = osc8(target, &text);
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

/// A URL found on a logical line: the wrapped-row `row` it sits on (counted from
/// the top of the content, before scroll), the `col` it starts at, its display
/// `width`, and the target `url`.
pub struct LinkTarget {
    row: u32,
    col: u16,
    width: u16,
    url: String,
}

/// URL schemes we linkify. Each renders with its visible text identical to the
/// link target, so the on-screen glyphs and the OSC 8 URI always agree (and the
/// [`linkify_run`] prefix guard holds).
const SCHEMES: [&str; 4] = ["https://", "http://", "mailto:", "ftp://"];

/// Find every linkifiable URL across `lines` (the logical lines about to be drawn
/// into a `Wrap { trim: false }` paragraph `wrap_width` columns wide) and record
/// where each lands. Pair with [`apply_link_targets`] after the paragraph is
/// drawn. Each URL is anchored to its line's first wrapped row; one that word-wrap
/// reflows onto a later row is filtered out at apply time by the position guard.
pub fn find_link_targets(lines: &[Line<'_>], wrap_width: u16) -> Vec<LinkTarget> {
    let cols = u32::from(wrap_width).max(1);
    let row_count = |w: u32| if w <= cols { 1 } else { w.div_ceil(cols) + 1 };
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    for line in lines {
        for (col, width, url) in line_urls(line) {
            out.push(LinkTarget {
                row: acc,
                col,
                width,
                url,
            });
        }
        acc += row_count(line.width() as u32);
    }
    out
}

/// Overlay the hyperlinks found by [`find_link_targets`] onto `buf`, given the
/// paragraph's `area` and vertical `scroll` (in wrapped rows; 0 if it doesn't
/// scroll). Only targets whose row is on screen, and whose glyphs the buffer
/// actually shows at the computed spot, are linked.
pub fn apply_link_targets(buf: &mut Buffer, area: Rect, scroll: u16, targets: &[LinkTarget]) {
    for t in targets {
        let rel = i64::from(t.row) - i64::from(scroll);
        if rel < 0 || rel >= i64::from(area.height) || t.col >= area.width {
            continue;
        }
        let max = t.width.min(area.width - t.col);
        linkify_run(buf, area.x + t.col, area.y + rel as u16, &t.url, max);
    }
}

/// Render `lines` as a `Wrap { trim: false }` paragraph into `area` (scrolled by
/// `scroll` wrapped rows), then overlay OSC 8 hyperlinks on every URL — when
/// `enabled`. The find/apply pair brackets the render so the paragraph can
/// consume `lines` in between. For the common text-only screen; the post detail,
/// which interleaves image overlays, drives [`find_link_targets`] /
/// [`apply_link_targets`] itself.
pub fn render_linked_paragraph(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: Vec<Line<'_>>,
    scroll: u16,
    enabled: bool,
) {
    let targets = if enabled {
        find_link_targets(&lines, area.width)
    } else {
        Vec::new()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
    if enabled {
        apply_link_targets(frame.buffer_mut(), area, scroll, &targets);
    }
}

/// Find URL tokens on one logical line, each as (start column, display width,
/// url). Builds the line's text with a per-character column map — so wide glyphs
/// before a URL shift its column correctly — then scans for scheme-led tokens.
fn line_urls(line: &Line<'_>) -> Vec<(u16, u16, String)> {
    let mut chars: Vec<char> = Vec::new();
    let mut col_of: Vec<u16> = Vec::new();
    let mut col: u16 = 0;
    for span in &line.spans {
        for ch in span.content.chars() {
            col_of.push(col);
            chars.push(ch);
            col = col.saturating_add(UnicodeWidthChar::width(ch).unwrap_or(0) as u16);
        }
    }
    col_of.push(col); // end sentinel

    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(scheme_len) = scheme_at(&chars, i) {
            let mut j = i + scheme_len;
            while j < chars.len() && is_url_char(chars[j]) {
                j += 1;
            }
            // Trim trailing punctuation that usually abuts a URL rather than
            // belonging to it ("(see https://x.com)." → "https://x.com").
            while j > i + scheme_len && is_trailing_punct(chars[j - 1]) {
                j -= 1;
            }
            if j > i + scheme_len {
                let url: String = chars[i..j].iter().collect();
                let c0 = col_of[i];
                let c1 = col_of[j];
                out.push((c0, c1.saturating_sub(c0), url));
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// If one of [`SCHEMES`] starts at `chars[i]` (case-insensitive), its char length.
fn scheme_at(chars: &[char], i: usize) -> Option<usize> {
    SCHEMES.iter().find_map(|s| {
        let sc: Vec<char> = s.chars().collect();
        let fits = i + sc.len() <= chars.len();
        if fits
            && chars[i..i + sc.len()]
                .iter()
                .zip(&sc)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            Some(sc.len())
        } else {
            None
        }
    })
}

/// Characters allowed inside a URL body: anything printable that isn't
/// whitespace or one of the few delimiters that never appear in a bare URL.
fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '\\' | '^' | '`')
}

/// Punctuation trimmed from the tail of a matched URL.
fn is_trailing_punct(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | ')' | ']' | '}' | '>' | '…'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;
    use ratatui::text::Span;

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
        // The URL ends at the space; only those glyphs are linked.
        let mut buf = buf_with("https://x.com more", 40);
        let cols = linkify_run(&mut buf, 0, 0, "https://x.com", 40);
        assert_eq!(cols, "https://x.com".len() as u16, "stops at the space");
        let first = buf.cell((0, 0)).unwrap().symbol().to_string();
        assert_eq!(first, osc8("https://x.com", "https://x.com"));
    }

    #[test]
    fn linkify_run_truncates_visible_text_to_the_column_budget() {
        // A long URL that would wrap: only the columns that fit are linked, but the
        // full URL stays the click target. The visible prefix still satisfies the
        // prefix guard.
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
    fn linkify_run_guard_rejects_a_mismatched_position() {
        // The cells say "hello" but we claim a URL is here — refuse to wrap it.
        let mut buf = buf_with("hello world", 40);
        assert_eq!(linkify_run(&mut buf, 0, 0, "https://x.com", 40), 0);
        assert_eq!(
            buf.cell((0, 0)).unwrap().symbol(),
            "h",
            "buffer left untouched"
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

    fn line(spans: Vec<Span<'static>>) -> Line<'static> {
        Line::from(spans)
    }

    #[test]
    fn line_urls_finds_a_scheme_url_with_its_column_and_width() {
        // Two-space indent, then the URL (the surfaced-link layout).
        let l = line(vec![Span::raw("  https://x.example/page")]);
        let found = line_urls(&l);
        assert_eq!(found.len(), 1);
        let (col, width, url) = &found[0];
        assert_eq!(*col, 2, "URL starts past the indent");
        assert_eq!(*url, "https://x.example/page");
        assert_eq!(*width, "https://x.example/page".len() as u16);
    }

    #[test]
    fn line_urls_accounts_for_wide_glyphs_before_the_url() {
        // A width-2 emoji then a label, mirroring the profile website line.
        let l = line(vec![Span::raw("🔗 site (https://x.com)")]);
        let found = line_urls(&l);
        assert_eq!(found.len(), 1);
        let (col, _w, url) = &found[0];
        assert_eq!(*url, "https://x.com", "trailing paren trimmed");
        // "🔗"=2 + " "=1 + "site"=4 + " "=1 + "("=1 => column 9.
        assert_eq!(*col, 9, "column shifted past the wide emoji");
    }

    #[test]
    fn line_urls_ignores_plain_text_and_bare_words() {
        let l = line(vec![Span::raw("just some words, no link here")]);
        assert!(line_urls(&l).is_empty());
    }

    #[test]
    fn find_and_apply_overlay_a_link_on_the_right_row() {
        let lines = vec![
            line(vec![Span::raw("header")]),
            line(vec![Span::raw("")]),
            line(vec![Span::raw("  https://x.example/p")]),
        ];
        let targets = find_link_targets(&lines, 40);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].row, 2, "URL is on the third logical row");

        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 5));
        for (y, l) in lines.iter().enumerate() {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            buf.set_string(0, y as u16, text, Style::default());
        }
        apply_link_targets(&mut buf, Rect::new(0, 0, 40, 5), 0, &targets);

        let cell = buf.cell((2, 2)).unwrap().symbol().to_string();
        assert!(
            cell.contains("\u{1b}]8;;https://x.example/p\u{1b}\\"),
            "row 2 carries the hyperlink: {cell:?}"
        );
    }

    #[test]
    fn apply_skips_targets_scrolled_out_of_view() {
        let lines = vec![line(vec![Span::raw("  https://x.example/p")])];
        let targets = find_link_targets(&lines, 40);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 3));
        buf.set_string(0, 0, "  https://x.example/p", Style::default());
        // Scrolled down by 5 rows: the only target (row 0) is off-screen.
        apply_link_targets(&mut buf, Rect::new(0, 0, 40, 3), 5, &targets);
        assert_eq!(
            buf.cell((2, 0)).unwrap().symbol(),
            "h",
            "nothing linked when scrolled past"
        );
    }

    #[test]
    fn osc8_bytes_reach_the_crossterm_wire() {
        // The in-memory buffer tests prove apply_link_targets sets the symbol; this
        // proves the real crossterm backend actually EMITS the escape bytes — the
        // diff drives `Print(symbol)` — so a terminal can act on them. The backend
        // is driven directly (not via `Terminal`) so the test needs no TTY: a real
        // `Terminal` would query the terminal size and fail in headless CI.
        use ratatui::backend::{Backend, CrosstermBackend};
        use std::cell::RefCell;
        use std::rc::Rc;

        #[derive(Clone)]
        struct Sink(Rc<RefCell<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        // Paint the URL glyphs into a buffer (as a paragraph would), then overlay.
        let area = Rect::new(0, 0, 30, 1);
        let lines = vec![line(vec![Span::raw("  https://x.example/page")])];
        let targets = find_link_targets(&lines, area.width);
        let prev = Buffer::empty(area);
        let mut next = Buffer::empty(area);
        next.set_string(0, 0, "  https://x.example/page", Style::default());
        apply_link_targets(&mut next, area, 0, &targets);

        // Drive the crossterm backend with the diff and capture what it writes.
        let sink = Sink(Rc::new(RefCell::new(Vec::new())));
        let mut backend = CrosstermBackend::new(sink.clone());
        backend.draw(prev.diff(&next).into_iter()).unwrap();
        backend.flush().unwrap();

        let bytes = sink.0.borrow().clone();
        let wire = String::from_utf8_lossy(&bytes);
        assert!(
            wire.contains("\u{1b}]8;;https://x.example/page\u{1b}\\"),
            "OSC 8 open sequence must be written to the terminal: {wire:?}"
        );
    }
}

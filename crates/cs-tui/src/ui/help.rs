//! `?`-triggered help overlay: a centered modal listing global navigation and
//! the common keys shared across list/reading screens. Screen-specific keys are
//! intentionally left to each screen's status bar — this overlay only documents
//! what's true everywhere, so it can't drift out of sync with one screen.
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

/// A single `key → description` row in the help body.
struct Row {
    keys: &'static str,
    desc: &'static str,
}

const SECTIONS: &[Row] = &[
    Row {
        keys: "1",
        desc: "Feed",
    },
    Row {
        keys: "2",
        desc: "Notifications",
    },
    Row {
        keys: "3",
        desc: "Bookmarks",
    },
    Row {
        keys: "4",
        desc: "Topics",
    },
    Row {
        keys: "5",
        desc: "Profile",
    },
    Row {
        keys: "6",
        desc: "Journal",
    },
    Row {
        keys: "7",
        desc: "Settings",
    },
    Row {
        keys: "8",
        desc: "Guilds",
    },
];

const GLOBAL: &[Row] = &[
    Row {
        keys: "Esc",
        desc: "back — or the menu on a top-level section",
    },
    Row {
        keys: "Backspace",
        desc: "back",
    },
    Row {
        keys: "1-8 / ← →",
        desc: "jump to / cycle sections",
    },
    Row {
        keys: "Tab / Shift+Tab",
        desc: "switch sub-tabs (profile, guild)",
    },
    Row {
        keys: "mouse",
        desc: "drag to select/copy · ctrl/⌘-click opens links (run --mouse for wheel scroll)",
    },
    Row {
        keys: "i",
        desc: "toggle inline images (turn off to recover if a post renders as garbage)",
    },
    Row {
        keys: "?",
        desc: "this help",
    },
    Row {
        keys: "Ctrl+C",
        desc: "quit",
    },
];

const COMMON: &[Row] = &[
    Row {
        keys: "j / ↓",
        desc: "move down",
    },
    Row {
        keys: "k / ↑",
        desc: "move up",
    },
    Row {
        keys: "g / Home",
        desc: "jump to top",
    },
    Row {
        keys: "G / End",
        desc: "jump to bottom",
    },
    Row {
        keys: "n / PgDn",
        desc: "next page",
    },
    Row {
        keys: "Enter",
        desc: "open / select",
    },
    Row {
        keys: "r",
        desc: "refresh",
    },
    Row {
        keys: "c",
        desc: "compose / new",
    },
    Row {
        keys: "b",
        desc: "bookmark (feed / post)",
    },
    Row {
        keys: "w",
        desc: "watch / unwatch thread (post detail)",
    },
    Row {
        keys: "/",
        desc: "search (topics)",
    },
];

const JUKEBOX: &[Row] = &[
    Row {
        keys: "p",
        desc: "play / pause the focused jukebox track",
    },
    Row {
        keys: "o",
        desc: "open the jukebox link in your browser",
    },
    Row {
        keys: "s",
        desc: "stop playback (also turns shuffle off)",
    },
    Row {
        keys: "S",
        desc: "shuffle: play random jukebox posts (press while idle to start)",
    },
    Row {
        keys: "< / >",
        desc: "previous / next track (next is a random pick at the newest)",
    },
    Row {
        keys: "[ / ]",
        desc: "volume down / up (needs mpv + yt-dlp)",
    },
];

const EDITOR: &[Row] = &[
    Row {
        keys: "type / Enter",
        desc: "write the post/reply/note body (built-in editor, no $EDITOR needed)",
    },
    Row {
        keys: "↑↓←→",
        desc: "move the cursor · lines soft-wrap · PgUp/PgDn page",
    },
    Row {
        keys: "Ctrl+D",
        desc: "save the body and continue to the post options",
    },
    Row {
        keys: "Esc / Ctrl+C",
        desc: "cancel and discard",
    },
    Row {
        keys: "paste",
        desc: "paste multi-line text directly (set `editor` in config for an external editor)",
    },
];

/// Build the help body. Kept separate from rendering so tests can assert on the
/// content without a terminal backend.
fn help_lines(theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let group = |lines: &mut Vec<Line<'static>>, title: &'static str, rows: &[Row]| {
        lines.push(Line::from(Span::styled(title, theme.accent_style())));
        for row in rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<12}", row.keys), theme.base()),
                Span::styled(row.desc, theme.muted_style()),
            ]));
        }
        lines.push(Line::from(""));
    };

    group(&mut lines, "Sections", SECTIONS);
    group(&mut lines, "Global", GLOBAL);
    group(&mut lines, "Lists & reading", COMMON);
    group(&mut lines, "Editor (compose)", EDITOR);
    group(&mut lines, "Jukebox", JUKEBOX);
    lines.push(Line::from(Span::styled(
        "Each screen shows its own keys in the status bar.",
        theme.muted_style(),
    )));
    lines.push(Line::from(Span::styled(
        "Press any key to close.",
        theme.muted_style(),
    )));
    lines
}

/// Render the help overlay centered over `area`.
pub fn render(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let lines = help_lines(theme);
    let h = (lines.len() as u16 + 2).min(area.height);
    let w = 52u16.min(area.width.saturating_sub(2));
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let card = Rect::new(x, y, w, h);

    frame.render_widget(Clear, card);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.accent_style())
        .title(Span::styled(" help ", theme.heading_style()));
    let inner = block.inner(card);
    frame.render_widget(block, card);
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_text(theme: &Theme) -> String {
        help_lines(theme)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn help_lists_sections_global_and_common_keys() {
        let text = body_text(&Theme::cyber());
        assert!(text.contains("Sections"));
        assert!(text.contains("Feed"));
        assert!(text.contains("Esc"));
        assert!(text.contains("Ctrl+C"));
        assert!(text.contains("compose"));
        assert!(text.contains("Jukebox"));
        assert!(text.contains("play / pause the focused jukebox track"));
        assert!(text.contains("Press any key to close."));
    }

    #[test]
    fn render_draws_help_box() {
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(80, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                render(f, area, &theme);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("help"), "title");
        assert!(text.contains("Notifications"), "section label");
    }
}

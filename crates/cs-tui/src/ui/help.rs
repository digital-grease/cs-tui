//! `?`-triggered help overlay: a centered, scrollable modal listing global
//! navigation, the keys shared across the list and reading screens, and the
//! per-screen actions that don't fit in a one-line status bar.
//!
//! The body is taller than a short terminal, so it scrolls (`j`/`k`, arrows,
//! PgUp/PgDn, `g`/`G`) and shows how much is left, instead of silently clipping
//! the groups at the bottom. Every other key still closes it, so the old "press
//! any key to dismiss" reflex keeps working.
use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

/// A single `key → description` row in the help body. An empty `keys` makes the
/// row a note: it lines up under the descriptions with no key beside it.
struct Row {
    keys: &'static str,
    desc: &'static str,
}

/// Card width in columns, clamped to the terminal on narrow screens.
const CARD_WIDTH: u16 = 64;

/// Columns the key column occupies, including its two-space indent. Wrapped
/// description rows are indented to the same place.
const KEY_COL: usize = 14;

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
        desc: "C-Mail",
    },
    Row {
        keys: "4",
        desc: "cIRC",
    },
    Row {
        keys: "5",
        desc: "Bookmarks",
    },
    Row {
        keys: "6",
        desc: "Topics",
    },
    Row {
        keys: "7",
        desc: "Profile",
    },
    Row {
        keys: "8",
        desc: "Journal",
    },
    Row {
        keys: "9",
        desc: "Guilds",
    },
    Row {
        keys: "0",
        desc: "Settings",
    },
];

const GLOBAL: &[Row] = &[
    Row {
        keys: "Esc",
        desc: "back, or the menu on a top-level section",
    },
    Row {
        keys: "Backspace",
        desc: "back",
    },
    Row {
        keys: "1-0 / ← →",
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
        keys: "Ctrl+F",
        desc: "search users, posts, and replies",
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

const POSTS: &[Row] = &[
    Row {
        keys: "e",
        desc: "edit your own entry (on post detail, the selected reply)",
    },
    Row {
        keys: "F",
        desc: "flag an entry or reply for review, with an optional reason",
    },
    Row {
        keys: "",
        desc: "the server allows an edit for 5 minutes after posting, on supporter accounts",
    },
];

const PROFILE: &[Row] = &[
    Row {
        keys: "F",
        desc: "follow / unfollow (someone else's profile)",
    },
    Row {
        keys: "m",
        desc: "start a C-Mail conversation with them",
    },
    Row {
        keys: "P",
        desc: "poke them (someone else's profile)",
    },
    Row {
        keys: "e",
        desc: "edit your own profile",
    },
    Row {
        keys: "E",
        desc: "edit the selected post (your own Posts tab)",
    },
    Row {
        keys: "P",
        desc: "pin / unpin the selected post (your own Posts tab)",
    },
    Row {
        keys: "",
        desc: "the guilds tab lists every guild they are in, the one on their profile badge first, then their apprenticeships; Enter opens one",
    },
];

const GUILDS: &[Row] = &[
    Row {
        keys: "J then y",
        desc: "join the guild you are looking at",
    },
    Row {
        keys: "",
        desc: "the server picks the role: your badge guild if you have none yet, otherwise an apprenticeship, and you can hold five of those",
    },
    Row {
        keys: "P then y",
        desc: "make this guild the badge on your profile; the guild it replaces becomes an apprenticeship, so you stay in both",
    },
    Row {
        keys: "L then y",
        desc: "leave (an apprenticeship too); founders cannot leave through the API",
    },
    Row {
        keys: "",
        desc: "join, promote and leave each ask before sending, and each is a 3/min, 15/day write, so a mistyped key costs nothing; any key but y cancels",
    },
    Row {
        keys: "c",
        desc: "start a thread; guild forums are open, so membership is not required",
    },
];

const CIRC: &[Row] = &[
    Row {
        keys: "",
        desc: "the composer always has focus, so every letter you type joins the message and the room's own actions are chords",
    },
    Row {
        keys: "Enter",
        desc: "send",
    },
    Row {
        keys: "Ctrl+E",
        desc: "expand the draft into the built-in editor",
    },
    Row {
        keys: "Ctrl+R",
        desc: "retry sends that failed",
    },
    Row {
        keys: "Ctrl+U",
        desc: "show / hide the room roster (who is in the room)",
    },
    Row {
        keys: "Ctrl+B",
        desc: "message select mode, for acting on one message (below)",
    },
    Row {
        keys: "↑ / ↓",
        desc: "scroll the room history (PgUp/PgDn and Home/End too)",
    },
    Row {
        keys: "",
        desc: "your presence is published while a room is open, so everyone sees you in its user list; set circ_presence = false in config.toml to stay invisible",
    },
];

const CIRC_SELECT: &[Row] = &[
    Row {
        keys: "j / k",
        desc: "pick a message",
    },
    Row {
        keys: "d then y",
        desc: "delete your own message (it can't be undone)",
    },
    Row {
        keys: "F",
        desc: "flag the message for review, with an optional reason",
    },
    Row {
        keys: "o",
        desc: "open the image or GIF, or play the track",
    },
    Row {
        keys: "v",
        desc: "reveal a spoiler",
    },
    Row {
        keys: "m",
        desc: "mute the author in this room",
    },
    Row {
        keys: "Esc",
        desc: "leave select mode, back to the composer",
    },
];

const CMAIL: &[Row] = &[
    Row {
        keys: "o",
        desc: "open the image or GIF, or play the track",
    },
    Row {
        keys: "v",
        desc: "reveal a spoiler",
    },
    Row {
        keys: "",
        desc: "your typing is published while a conversation is open, so the other person sees \"is typing\"; set cmail_typing = false in config.toml to stop that",
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

/// Build the help body, wrapping each description to `desc_width` columns so a
/// long line continues under the key column instead of being cut off. Kept
/// separate from rendering so tests can assert on the content without a
/// terminal backend.
fn help_lines(theme: &Theme, desc_width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let group = |lines: &mut Vec<Line<'static>>, title: &'static str, rows: &[Row]| {
        lines.push(Line::from(Span::styled(title, theme.accent_style())));
        for row in rows {
            for (i, chunk) in super::chat::word_wrap(row.desc, desc_width)
                .into_iter()
                .enumerate()
            {
                let keys = if i == 0 {
                    format!("  {:<width$}", row.keys, width = KEY_COL - 2)
                } else {
                    " ".repeat(KEY_COL)
                };
                lines.push(Line::from(vec![
                    Span::styled(keys, theme.base()),
                    Span::styled(chunk, theme.muted_style()),
                ]));
            }
        }
        lines.push(Line::from(""));
    };

    group(&mut lines, "Sections", SECTIONS);
    group(&mut lines, "Global", GLOBAL);
    group(&mut lines, "Lists & reading", COMMON);
    group(&mut lines, "Posts & replies", POSTS);
    group(&mut lines, "Profile", PROFILE);
    group(&mut lines, "Guilds", GUILDS);
    group(&mut lines, "cIRC room", CIRC);
    group(&mut lines, "cIRC message select (Ctrl+B)", CIRC_SELECT);
    group(&mut lines, "C-Mail conversation", CMAIL);
    group(&mut lines, "Editor (compose)", EDITOR);
    group(&mut lines, "Jukebox", JUKEBOX);
    lines.push(Line::from(Span::styled(
        "Each screen shows its own keys in the status bar.",
        theme.muted_style(),
    )));
    lines
}

/// What a key pressed while the help overlay is open means to the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpIntent {
    /// The key scrolled the overlay (or did nothing): keep it open.
    None,
    /// Dismiss the overlay and return to the screen underneath.
    Close,
    /// Ctrl+C: quit, the same as everywhere else.
    Quit,
}

/// The `?` help overlay. Holds the scroll offset so the body can be longer than
/// the terminal without any of it being unreachable.
#[derive(Debug, Default)]
pub struct HelpOverlay {
    /// First body row drawn, counted in wrapped rows from the top.
    scroll: u16,
    /// Max scroll offset for the last rendered size, recomputed each render (the
    /// content height depends on the card width, and the viewport on the
    /// terminal). Key handling clamps to it so scrolling stops at the last row.
    max_scroll: Cell<u16>,
    /// Body rows visible in the last render, so PgUp/PgDn move a real page.
    page: Cell<u16>,
}

impl HelpOverlay {
    /// A fresh overlay, scrolled to the top.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Route a key while the overlay is open. Scroll keys move the body and keep
    /// it open; anything else closes it, which preserves the long-standing
    /// "press any key to dismiss" behavior.
    pub fn handle_key(&mut self, key: KeyEvent) -> HelpIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return HelpIntent::Quit;
        }
        let max = self.max_scroll.get();
        let page = self.page.get().max(1);
        // A resize can leave the offset past the end; fold that in first so a
        // single `k` moves visibly rather than unwinding a stale offset.
        self.scroll = self.scroll.min(max);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1).min(max);
            }
            KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(page).min(max),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = max,
            _ => return HelpIntent::Close,
        }
        HelpIntent::None
    }

    /// Render the help overlay centered over `area`.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let w = CARD_WIDTH.min(area.width.saturating_sub(2));
        if w == 0 || area.height == 0 {
            return; // no room for a card at all
        }
        let desc_width = (w as usize).saturating_sub(2 + KEY_COL).max(8);
        let lines = help_lines(theme, desc_width);
        // Borders (2) plus the hint row (1).
        let h = (lines.len() as u16).saturating_add(3).min(area.height);
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let card = Rect::new(x, y, w, h);

        // Clear the underlying content, then repaint the card in the theme's own
        // background: `Clear` alone leaves the terminal default showing through,
        // which reads as a different opacity on most themes.
        frame.render_widget(Clear, card);
        frame.render_widget(Block::default().style(theme.base()), card);
        let block = Block::default()
            .borders(Borders::ALL)
            .style(theme.base())
            .border_style(theme.accent_style())
            .title(Span::styled(" help ", theme.heading_style()));
        let inner = block.inner(card);
        frame.render_widget(block, card);

        // Reserve the last inner row for the position + keys hint, but only when
        // there's more than one row to share.
        let show_hint = inner.height > 1;
        let body_h = if show_hint {
            inner.height - 1
        } else {
            inner.height
        };
        let total = lines.len() as u16;
        let max_scroll = total.saturating_sub(body_h);
        self.max_scroll.set(max_scroll);
        self.page.set(body_h);
        let scroll = self.scroll.min(max_scroll);

        let body = Rect::new(inner.x, inner.y, inner.width, body_h);
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), body);

        if show_hint {
            let hint = Rect::new(inner.x, inner.y + body_h, inner.width, 1);
            frame.render_widget(
                Paragraph::new(hint_line(scroll, body_h, total, theme)),
                hint,
            );
        }
    }
}

/// The bottom hint row: where you are in the body (only when some of it is off
/// screen), then the keys that move it.
fn hint_line(scroll: u16, view: u16, total: u16, theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    if total > view {
        spans.push(Span::styled(
            format!("{} ", scroll_position(scroll, view, total)),
            theme.accent_style(),
        ));
    }
    spans.push(Span::styled(
        "j/k scroll · g/G ends · any other key closes",
        theme.muted_style(),
    ));
    Line::from(spans)
}

/// "12-32/95 ▲▼": the visible row range, the total, and arrows for the
/// directions that still have content.
fn scroll_position(scroll: u16, view: u16, total: u16) -> String {
    let first = scroll.saturating_add(1).min(total);
    let last = scroll.saturating_add(view).min(total);
    let arrows = match (scroll > 0, last < total) {
        (true, true) => "▲▼",
        (true, false) => "▲",
        (false, true) => "▼",
        (false, false) => "",
    };
    format!("{first}-{last}/{total} {arrows}")
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};
    use unicode_width::UnicodeWidthStr;

    /// Description width of the default-width card, for content assertions.
    const TEST_DESC_WIDTH: usize = CARD_WIDTH as usize - 2 - KEY_COL;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn body_text(theme: &Theme) -> String {
        help_lines(theme, TEST_DESC_WIDTH)
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

    fn render_to_string(overlay: &HelpOverlay, w: u16, h: u16) -> String {
        let theme = Theme::cyber();
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                overlay.render(f, area, &theme);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
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
    }

    #[test]
    fn help_lists_the_per_screen_actions() {
        let text = body_text(&Theme::cyber());
        // Posts & replies.
        assert!(text.contains("edit your own entry"));
        assert!(text.contains("flag an entry or reply for review"));
        // Profile.
        assert!(text.contains("poke them"));
        assert!(text.contains("edit the selected post"));
        // cIRC room and its select mode.
        assert!(text.contains("Ctrl+U"));
        assert!(text.contains("show / hide the room roster"));
        assert!(text.contains("Ctrl+B"));
        assert!(text.contains("d then y"));
        assert!(text.contains("mute the author in this room"));
        // Attachments and spoilers, on both chat screens.
        assert_eq!(text.matches("reveal a spoiler").count(), 2);
        assert_eq!(
            text.matches("open the image or GIF, or play the track")
                .count(),
            2
        );
    }

    #[test]
    fn help_covers_the_guild_membership_keys() {
        let text = body_text(&Theme::cyber());
        assert!(text.contains("Guilds"), "no guild group");
        assert!(text.contains("join the guild"));
        assert!(text.contains("the server picks the role"));
        assert!(
            text.contains("make this guild the badge"),
            "promoting an apprenticeship is undocumented"
        );
        assert!(text.contains("leave (an apprenticeship too)"));
        assert!(text.contains("start a thread"));
        // The profile's own guilds tab is named alongside the profile keys.
        assert!(text.contains("the guilds tab lists every guild"));
    }

    #[test]
    fn help_names_the_config_switches_for_published_activity() {
        let text = body_text(&Theme::cyber());
        assert!(text.contains("circ_presence"), "cIRC presence switch");
        assert!(text.contains("cmail_typing"), "C-Mail typing switch");
    }

    #[test]
    fn long_descriptions_wrap_under_the_key_column() {
        let lines = help_lines(&Theme::cyber(), TEST_DESC_WIDTH);
        // Every row fits the card: key column + wrapped description.
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                UnicodeWidthStr::width(text.as_str()) <= CARD_WIDTH as usize - 2,
                "line overflows the card: {text:?}"
            );
        }
    }

    #[test]
    fn body_is_taller_than_a_short_terminal() {
        // The reason the overlay scrolls at all: it can't be shown at once on a
        // 24-row terminal, and silently clipping it hides whole groups.
        assert!(help_lines(&Theme::cyber(), TEST_DESC_WIDTH).len() > 22);
    }

    #[test]
    fn scroll_position_reports_range_and_direction() {
        assert_eq!(scroll_position(0, 10, 30), "1-10/30 ▼");
        assert_eq!(scroll_position(10, 10, 30), "11-20/30 ▲▼");
        assert_eq!(scroll_position(20, 10, 30), "21-30/30 ▲");
        assert_eq!(scroll_position(0, 30, 30), "1-30/30");
    }

    #[test]
    fn render_draws_help_box() {
        let overlay = HelpOverlay::new();
        let text = render_to_string(&overlay, 80, 40);
        assert!(text.contains("help"), "title");
        assert!(text.contains("Notifications"), "section label");
    }

    #[test]
    fn render_shows_that_there_is_more_below() {
        let overlay = HelpOverlay::new();
        let text = render_to_string(&overlay, 80, 24);
        assert!(text.contains("▼"), "no more-below marker: {text}");
        assert!(text.contains("j/k scroll"), "no scroll hint");
    }

    #[test]
    fn scrolling_to_the_end_reveals_the_last_group() {
        let mut overlay = HelpOverlay::new();
        // The first render records the viewport, which is what G scrolls to.
        let first = render_to_string(&overlay, 80, 24);
        assert!(!first.contains("volume down / up"), "last group already up");
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('G'))),
            HelpIntent::None
        );
        let last = render_to_string(&overlay, 80, 24);
        assert!(last.contains("volume down / up"), "last group unreachable");
        assert!(!last.contains("Notifications"), "body did not scroll");
        assert!(last.contains("▲"), "no more-above marker");
    }

    #[test]
    fn scroll_keys_move_the_body_and_keep_the_overlay_open() {
        let mut overlay = HelpOverlay::new();
        render_to_string(&overlay, 80, 24);
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('j'))),
            HelpIntent::None
        );
        assert_eq!(overlay.scroll, 1);
        assert_eq!(overlay.handle_key(key(KeyCode::Down)), HelpIntent::None);
        assert_eq!(overlay.scroll, 2);
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('k'))),
            HelpIntent::None
        );
        assert_eq!(overlay.scroll, 1);
        assert_eq!(overlay.handle_key(key(KeyCode::Up)), HelpIntent::None);
        assert_eq!(overlay.scroll, 0);
        // Already at the top: k is still a scroll key, not a dismiss.
        assert_eq!(overlay.handle_key(key(KeyCode::Up)), HelpIntent::None);
        assert_eq!(overlay.scroll, 0);
    }

    #[test]
    fn page_keys_move_a_whole_page() {
        let mut overlay = HelpOverlay::new();
        render_to_string(&overlay, 80, 24);
        let page = overlay.page.get();
        assert!(page > 1, "viewport not recorded");
        overlay.handle_key(key(KeyCode::PageDown));
        assert_eq!(overlay.scroll, page);
        overlay.handle_key(key(KeyCode::PageUp));
        assert_eq!(overlay.scroll, 0);
    }

    #[test]
    fn scrolling_stops_at_the_last_row() {
        let mut overlay = HelpOverlay::new();
        render_to_string(&overlay, 80, 24);
        let max = overlay.max_scroll.get();
        assert!(max > 0, "content should overflow a 24-row terminal");
        for _ in 0..(max + 10) {
            overlay.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(overlay.scroll, max);
        overlay.handle_key(key(KeyCode::Char('g')));
        assert_eq!(overlay.scroll, 0);
    }

    #[test]
    fn any_other_key_closes_and_ctrl_c_quits() {
        let mut overlay = HelpOverlay::new();
        render_to_string(&overlay, 80, 24);
        assert_eq!(overlay.handle_key(key(KeyCode::Esc)), HelpIntent::Close);
        assert_eq!(
            overlay.handle_key(key(KeyCode::Char('?'))),
            HelpIntent::Close
        );
        assert_eq!(overlay.handle_key(key(KeyCode::Enter)), HelpIntent::Close);
        let ctrl_c = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(overlay.handle_key(ctrl_c), HelpIntent::Quit);
    }

    #[test]
    fn narrow_terminal_still_renders() {
        // The card clamps to the terminal; nothing panics and the body is there.
        let overlay = HelpOverlay::new();
        let text = render_to_string(&overlay, 30, 10);
        assert!(text.contains("help"), "title missing on a narrow terminal");
    }
}

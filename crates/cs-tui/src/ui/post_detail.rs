//! Post detail screen — entry header + content + scrollable replies (oldest first).
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use cs_api::{Entry, Reply};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use time::OffsetDateTime;

use super::markdown::render_markdown;
use super::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostDetailIntent {
    /// Return to the previous screen.
    Back,
    /// Exit the app.
    Quit,
    /// Load the next page of replies using the held cursor.
    LoadMoreReplies,
    /// Re-fetch the replies from scratch.
    RefreshReplies,
    /// Start composing a reply to this post.
    Reply,
    /// User confirmed deletion of the entry.
    DeleteEntryConfirmed,
    None,
}

#[derive(Debug)]
pub struct PostDetailScreen {
    pub entry: Entry,
    pub replies: Vec<Reply>,
    pub next_replies_cursor: Option<String>,
    pub loading_replies: bool,
    pub error: Option<String>,
    pub scroll: u16,
    /// Optional reply id to highlight (set when arriving from a reply notification).
    pub highlight_reply_id: Option<String>,
    /// Two-step delete: first `d` arms confirmation; `y` confirms.
    pub confirming_delete: bool,
}

impl PostDetailScreen {
    pub fn new(entry: Entry) -> Self {
        Self {
            entry,
            replies: Vec::new(),
            next_replies_cursor: None,
            loading_replies: true,
            error: None,
            scroll: 0,
            highlight_reply_id: None,
            confirming_delete: false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PostDetailIntent {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return PostDetailIntent::Quit;
        }
        // While arming delete, only `y` confirms; anything else cancels the arming.
        if self.confirming_delete {
            self.confirming_delete = false;
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                return PostDetailIntent::DeleteEntryConfirmed;
            }
            return PostDetailIntent::None;
        }
        match key.code {
            KeyCode::Backspace => PostDetailIntent::Back,
            KeyCode::Char('R') => PostDetailIntent::Reply,
            KeyCode::Char('d') => {
                self.confirming_delete = true;
                PostDetailIntent::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
                PostDetailIntent::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                PostDetailIntent::None
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.scroll = self.scroll.saturating_add(10);
                PostDetailIntent::None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                PostDetailIntent::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.scroll = 0;
                PostDetailIntent::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                // ratatui's Paragraph clamps scroll to (lines - visible_height),
                // so saturating to u16::MAX effectively jumps to the end.
                self.scroll = u16::MAX;
                PostDetailIntent::None
            }
            KeyCode::Char('n') if self.next_replies_cursor.is_some() => {
                self.loading_replies = true;
                PostDetailIntent::LoadMoreReplies
            }
            KeyCode::Char('r') => {
                self.replies.clear();
                self.next_replies_cursor = None;
                self.loading_replies = true;
                self.error = None;
                PostDetailIntent::RefreshReplies
            }
            _ => PostDetailIntent::None,
        }
    }

    pub fn apply_replies_initial(&mut self, result: Result<(Vec<Reply>, Option<String>), String>) {
        self.loading_replies = false;
        match result {
            Ok((replies, cursor)) => {
                self.replies = replies;
                self.next_replies_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn apply_replies_more(&mut self, result: Result<(Vec<Reply>, Option<String>), String>) {
        self.loading_replies = false;
        match result {
            Ok((mut replies, cursor)) => {
                self.replies.append(&mut replies);
                self.next_replies_cursor = cursor;
                self.error = None;
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border_style())
            .title(Span::styled(
                format!(" post · @{} ", self.entry.author_username),
                theme.accent_style(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        let body_area = layout[0];
        let status_area = layout[1];

        let lines = self.compose_body(theme);
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(para, body_area);

        let status_text = if self.confirming_delete {
            "really delete this post? y=yes, any other key=cancel".to_string()
        } else if self.loading_replies && self.replies.is_empty() {
            "loading replies… · esc back · j/k scroll".to_string()
        } else if let Some(msg) = &self.error {
            format!("error: {msg} · esc back · r retry")
        } else if self.next_replies_cursor.is_some() {
            format!(
                "{} replies · more — n · esc back · j/k scroll · R reply · d delete (own) · r refresh",
                self.replies.len()
            )
        } else {
            format!(
                "{} replies · end · esc back · j/k scroll · R reply · d delete (own) · r refresh",
                self.replies.len()
            )
        };
        let status = Paragraph::new(Line::from(Span::styled(status_text, theme.muted_style())));
        frame.render_widget(status, status_area);
    }

    fn compose_body(&self, theme: &Theme) -> Vec<Line<'_>> {
        let mut lines = Vec::new();

        // Header
        let when = self
            .entry
            .created_at
            .map(format_full_timestamp)
            .unwrap_or_default();
        let topics = if self.entry.topics.is_empty() {
            String::new()
        } else {
            format!(" · #{}", self.entry.topics.join(" #"))
        };
        // v0.3.7: lead with the entry title (when set) as a headline above the
        // author/metadata line. Skipped for None/whitespace-only titles.
        if let Some(title) = self.entry.title.as_deref() {
            let title = title.trim();
            if !title.is_empty() {
                lines.push(Line::from(Span::styled(
                    title.to_string(),
                    theme.accent_style(),
                )));
            }
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("@{}", self.entry.author_username),
                theme.accent_style(),
            ),
            Span::styled(format!(" · {when}{topics}"), theme.muted_style()),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "{} replies · {} bookmarks{}",
                self.entry.replies_count,
                self.entry.bookmarks_count,
                if self.entry.is_nsfw { " · NSFW" } else { "" }
            ),
            theme.muted_style(),
        )));
        lines.push(Line::from(""));

        // Body — rendered with pulldown-cmark (markdown + @mention highlighting).
        for md_line in render_markdown(&self.entry.content, theme) {
            lines.push(md_line);
        }

        // Replies separator
        if !self.replies.is_empty() || self.loading_replies {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "─── replies ───",
                theme.muted_style(),
            )));
            lines.push(Line::from(""));
        }

        // Replies
        for reply in &self.replies {
            let highlight = self
                .highlight_reply_id
                .as_deref()
                .is_some_and(|id| id == reply.reply_id);
            let style = if highlight {
                theme.accent_style()
            } else {
                theme.base()
            };
            let when = reply
                .created_at
                .map(format_full_timestamp)
                .unwrap_or_default();
            let parent = if reply.parent_reply_id.is_some() {
                " · ↪"
            } else {
                ""
            };
            lines.push(Line::from(vec![
                Span::styled(format!("@{}", reply.author_username), theme.accent_style()),
                Span::styled(format!(" · {when}{parent}"), theme.muted_style()),
            ]));
            // Reply body — markdown-rendered. Highlight overrides via the loop below.
            for md_line in render_markdown(&reply.content, theme) {
                if highlight {
                    let restyled: Vec<Span<'_>> = md_line
                        .spans
                        .iter()
                        .map(|s| Span::styled(s.content.to_string(), style))
                        .collect();
                    lines.push(Line::from(restyled));
                } else {
                    lines.push(md_line);
                }
            }
            lines.push(Line::from(""));
        }

        if self.loading_replies && !self.replies.is_empty() {
            lines.push(Line::from(Span::styled(
                "loading more replies…",
                theme.accent_style(),
            )));
        }

        lines
    }
}

fn format_full_timestamp(t: OffsetDateTime) -> String {
    let dt = t.date();
    let tt = t.time();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        dt.year(),
        u8::from(dt.month()),
        dt.day(),
        tt.hour(),
        tt.minute()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn entry(post_id: &str) -> Entry {
        Entry {
            post_id: post_id.into(),
            author_id: "a".into(),
            author_username: "alice".into(),
            content: "hello\nworld".into(),
            title: None,
            slug: None,
            topics: vec!["music".into()],
            replies_count: 0,
            bookmarks_count: 0,
            is_public: false,
            is_nsfw: false,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    fn reply(reply_id: &str, post_id: &str) -> Reply {
        Reply {
            reply_id: reply_id.into(),
            post_id: post_id.into(),
            author_id: "b".into(),
            author_username: "bob".into(),
            content: format!("reply {reply_id}"),
            parent_reply_id: None,
            attachments: vec![],
            created_at: None,
            deleted: false,
        }
    }

    fn body_text(s: &PostDetailScreen) -> Vec<String> {
        let theme = Theme::cyber();
        s.compose_body(&theme)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn compose_body_leads_with_title_when_present() {
        let mut e = entry("p1");
        e.title = Some("Headline Here".into());
        let lines = body_text(&PostDetailScreen::new(e));
        assert_eq!(lines[0], "Headline Here", "title should be the first line");
    }

    #[test]
    fn compose_body_omits_title_when_none() {
        let lines = body_text(&PostDetailScreen::new(entry("p1"))); // title: None
        assert!(
            lines[0].starts_with("@alice"),
            "without a title the first line is the author header, got {:?}",
            lines[0]
        );
    }

    #[test]
    fn new_starts_loading_replies() {
        let s = PostDetailScreen::new(entry("p1"));
        assert!(s.loading_replies);
        assert!(s.replies.is_empty());
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn backspace_emits_back() {
        let mut s = PostDetailScreen::new(entry("p1"));
        assert_eq!(
            s.handle_key(key(KeyCode::Backspace)),
            PostDetailIntent::Back
        );
    }

    #[test]
    fn j_and_k_adjust_scroll_bounded() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.handle_key(key(KeyCode::Char('j')));
        s.handle_key(key(KeyCode::Char('j')));
        assert_eq!(s.scroll, 2);
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.scroll, 1);
        s.handle_key(key(KeyCode::Char('k')));
        s.handle_key(key(KeyCode::Char('k')));
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn g_jumps_to_top() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.scroll = 50;
        s.handle_key(key(KeyCode::Char('g')));
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn n_requests_more_only_with_cursor() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.loading_replies = false;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('n'))),
            PostDetailIntent::None
        );

        s.next_replies_cursor = Some("c".into());
        assert_eq!(
            s.handle_key(key(KeyCode::Char('n'))),
            PostDetailIntent::LoadMoreReplies
        );
        assert!(s.loading_replies);
    }

    #[test]
    fn r_resets_and_requests_refresh() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.replies = vec![reply("r1", "p1")];
        s.next_replies_cursor = Some("c".into());
        s.loading_replies = false;
        assert_eq!(
            s.handle_key(key(KeyCode::Char('r'))),
            PostDetailIntent::RefreshReplies
        );
        assert!(s.replies.is_empty());
        assert!(s.next_replies_cursor.is_none());
        assert!(s.loading_replies);
    }

    #[test]
    fn apply_replies_initial_populates() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], Some("cur".into()))));
        assert!(!s.loading_replies);
        assert_eq!(s.replies.len(), 1);
        assert_eq!(s.next_replies_cursor.as_deref(), Some("cur"));
    }

    #[test]
    fn apply_replies_initial_error_sets_error() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Err("boom".into()));
        assert_eq!(s.error.as_deref(), Some("boom"));
        assert!(!s.loading_replies);
    }

    #[test]
    fn apply_replies_more_appends() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], Some("c".into()))));
        s.apply_replies_more(Ok((vec![reply("r2", "p1")], None)));
        assert_eq!(s.replies.len(), 2);
        assert!(s.next_replies_cursor.is_none());
    }

    #[test]
    fn compose_body_includes_separator_when_replies_present() {
        let mut s = PostDetailScreen::new(entry("p1"));
        s.apply_replies_initial(Ok((vec![reply("r1", "p1")], None)));
        let lines = s.compose_body(&Theme::dark());
        // Look for the replies separator marker.
        let body_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.as_ref()))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(body_text.contains("replies"));
        assert!(body_text.contains("@bob"));
    }
}

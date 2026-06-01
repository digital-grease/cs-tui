//! Transient toast overlay — an auto-dismissing message drawn on top of any
//! screen. Producers: the rate-limit countdown (warning) and action
//! confirmations like "bookmarked" (success).
use std::time::{Duration, Instant};

use ratatui::style::Style;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::theme::Theme;

#[derive(Debug, Clone, Copy)]
pub enum ToastKind {
    /// Positive confirmation (green ✓).
    Success,
    /// Caution / failure (amber ⚠).
    Warning,
}

#[derive(Debug)]
pub struct Toast {
    text: String,
    expires_at: Instant,
    countdown: bool,
    kind: ToastKind,
}

impl Toast {
    /// A rate-limit warning that counts down the server's retry-after window
    /// (clamped to a sane visible range) and then auto-dismisses.
    pub fn rate_limited(retry_after_secs: u64) -> Self {
        let secs = retry_after_secs.clamp(1, 60);
        Self {
            text: "rate limited — slow down".to_string(),
            expires_at: Instant::now() + Duration::from_secs(secs),
            countdown: true,
            kind: ToastKind::Warning,
        }
    }

    /// A custom-text countdown toast (e.g. "rate limited — posting in (30s)"),
    /// clamped to a visible range. Auto-dismisses when the countdown elapses.
    pub fn countdown(text: impl Into<String>, secs: u64) -> Self {
        let secs = secs.clamp(1, 120);
        Self {
            text: text.into(),
            expires_at: Instant::now() + Duration::from_secs(secs),
            countdown: true,
            kind: ToastKind::Warning,
        }
    }

    /// A brief positive confirmation (e.g. "bookmarked").
    pub fn confirmation(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expires_at: Instant::now() + Duration::from_secs(2),
            countdown: false,
            kind: ToastKind::Success,
        }
    }

    /// A brief warning/failure notice that isn't a rate-limit countdown.
    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expires_at: Instant::now() + Duration::from_secs(3),
            countdown: false,
            kind: ToastKind::Warning,
        }
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    /// Seconds to display: rounded UP and floored at 1, so a fresh N-second
    /// toast shows exactly N (matching the server's retry-after) and counts
    /// down to 1 — never 0 — before [`tick_toast`](super::app) removes it.
    fn display_secs(&self) -> u64 {
        let d = self.expires_at.saturating_duration_since(Instant::now());
        (d.as_secs() + u64::from(d.subsec_nanos() > 0)).max(1)
    }

    fn glyph(&self) -> &'static str {
        match self.kind {
            ToastKind::Success => "✓",
            ToastKind::Warning => "⚠",
        }
    }

    fn label(&self) -> String {
        if self.countdown {
            format!(" {} {} ({}s) ", self.glyph(), self.text, self.display_secs())
        } else {
            format!(" {} {} ", self.glyph(), self.text)
        }
    }

    fn style(&self, theme: &Theme) -> Style {
        match self.kind {
            ToastKind::Success => theme.success_style(),
            ToastKind::Warning => theme.warning_style(),
        }
    }
}

/// Draw the toast as a bordered box pinned to the bottom-right of `area`.
pub fn render(frame: &mut Frame<'_>, area: Rect, toast: &Toast, theme: &Theme) {
    let label = toast.label();
    let w = (label.chars().count() as u16 + 2).min(area.width);
    let h = 3u16.min(area.height);
    if w == 0 || h == 0 {
        return;
    }
    // Bottom-right with a one-cell margin (clamped on tiny terminals).
    let x = area.x + area.width.saturating_sub(w).saturating_sub(1);
    let y = area.y + area.height.saturating_sub(h).saturating_sub(1);
    let rect = Rect::new(x, y, w, h);

    let style = toast.style(theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .style(theme.base());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(label, style))).block(block),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_clamps_and_shows_server_value() {
        // A fresh N-second toast displays exactly N (not N+1), the bug the
        // ceil-floored display_secs fixes.
        let t = Toast::rate_limited(10);
        assert_eq!(t.display_secs(), 10);
        assert!(!t.is_expired());
        assert!(t.label().contains("rate limited"));
        assert!(t.label().contains("(10s)"), "label was {:?}", t.label());

        // Visible countdown is clamped to ≤60s even for a huge hint.
        assert_eq!(Toast::rate_limited(9000).display_secs(), 60);
    }

    #[test]
    fn zero_retry_after_still_lives_briefly() {
        // A 0s hint is clamped up to 1s so the toast is actually seen, and the
        // displayed value never drops below 1.
        let t = Toast::rate_limited(0);
        assert!(!t.is_expired());
        assert_eq!(t.display_secs(), 1);
    }

    #[test]
    fn confirmation_and_warning_have_no_countdown() {
        let c = Toast::confirmation("bookmarked");
        assert!(!c.is_expired());
        assert!(c.label().contains("✓"));
        assert!(c.label().contains("bookmarked"));
        assert!(!c.label().contains("(1s)"), "confirmation must not count down");

        let w = Toast::warning("bookmark failed");
        assert!(w.label().contains("⚠"));
        assert!(w.label().contains("bookmark failed"));
    }
}

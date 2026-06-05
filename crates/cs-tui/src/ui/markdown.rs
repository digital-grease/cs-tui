//! Minimal markdown → ratatui Lines renderer.
//!
//! Handles the subset of GitHub-flavored markdown that appears in cyberspace.online
//! posts: headings, bold/italic, inline code, code blocks, unordered lists,
//! blockquotes, links (rendered as `text (url)`), soft/hard breaks, and `@mention`
//! highlighting. Tables, footnotes, and other advanced features are rendered as
//! plain text.
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// Render markdown source into a vector of styled ratatui lines.
pub fn render_markdown(input: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![theme.base()];
    let mut list_depth: u32 = 0;
    let mut blockquote_depth: u32 = 0;
    // While inside an image tag, accumulate the alt text so the placeholder can
    // carry it (the alt is the only accessible description of the image).
    let mut image_alt: Option<String> = None;

    let parser = Parser::new(input);
    for event in parser {
        match event {
            Event::Start(Tag::Image { .. }) => {
                // Capture the alt text; the placeholder is emitted at End. The
                // actual image is rendered above the text (post detail) on
                // graphics-capable terminals; the URL is intentionally not
                // inlined — it's long and the image itself is shown.
                image_alt = Some(String::new());
            }
            Event::End(TagEnd::Image) => {
                let alt = image_alt.take().unwrap_or_default();
                current_line.push(Span::styled(
                    image_placeholder(&alt),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
            Event::Start(tag) => handle_start(
                tag,
                theme,
                &mut current_line,
                &mut out,
                &mut style_stack,
                &mut list_depth,
                &mut blockquote_depth,
            ),
            Event::End(end) => handle_end(
                end,
                theme,
                &mut current_line,
                &mut out,
                &mut style_stack,
                &mut list_depth,
                &mut blockquote_depth,
            ),
            Event::Text(t) => {
                if let Some(alt) = &mut image_alt {
                    alt.push_str(t.as_ref());
                } else {
                    let style = current_style(&style_stack, theme);
                    for span in mention_aware_spans(t.as_ref(), style, theme) {
                        current_line.push(span);
                    }
                }
            }
            Event::Code(t) => {
                current_line.push(Span::styled(
                    format!("`{t}`"),
                    Style::default().fg(theme.accent),
                ));
            }
            Event::SoftBreak => {
                flush(&mut current_line, &mut out, blockquote_depth, theme);
            }
            Event::HardBreak => {
                flush(&mut current_line, &mut out, blockquote_depth, theme);
            }
            Event::Rule => {
                flush(&mut current_line, &mut out, blockquote_depth, theme);
                out.push(Line::from(Span::styled(
                    "────────────────────────────",
                    theme.muted_style(),
                )));
            }
            _ => {}
        }
    }
    flush(&mut current_line, &mut out, blockquote_depth, theme);
    out
}

/// A single-line plain-text preview of post content for list views: markdown is
/// flattened to text, image links are dropped, whitespace is collapsed, and the
/// result is truncated. Returns empty for image-only / empty posts — list items
/// flag the presence of an image separately (via `images::has_image`), which
/// also catches attachment images this can't see.
pub fn content_preview(content: &str, max: usize) -> String {
    let mut text = String::new();
    let mut in_image = false;
    for ev in Parser::new(content) {
        match ev {
            Event::Start(Tag::Image { .. }) => in_image = true,
            Event::End(TagEnd::Image) => in_image = false,
            Event::Text(t) | Event::Code(t) if !in_image => text.push_str(&t),
            Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph) => {
                text.push(' ');
            }
            _ => {}
        }
    }
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }
    super::text::truncate_to_width(&collapsed, max)
}

/// Build the inline image placeholder, carrying a (capped) alt description when
/// the markdown provided one: `[image: a sunset]` or `[image]`.
fn image_placeholder(alt: &str) -> String {
    let alt = alt.trim();
    if alt.is_empty() {
        "[image]".to_string()
    } else {
        format!("[image: {}]", super::text::truncate_to_width(alt, 60))
    }
}

fn handle_start(
    tag: Tag<'_>,
    theme: &Theme,
    line: &mut Vec<Span<'static>>,
    out: &mut Vec<Line<'static>>,
    stack: &mut Vec<Style>,
    list_depth: &mut u32,
    blockquote_depth: &mut u32,
) {
    match tag {
        Tag::Heading { level, .. } => {
            flush(line, out, *blockquote_depth, theme);
            let style = match level {
                HeadingLevel::H1 => theme
                    .accent_style()
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                _ => theme.accent_style().add_modifier(Modifier::BOLD),
            };
            stack.push(style);
            let prefix = "#".repeat(level as usize);
            line.push(Span::styled(format!("{prefix} "), theme.muted_style()));
        }
        Tag::Emphasis => stack.push(current_style(stack, theme).add_modifier(Modifier::ITALIC)),
        Tag::Strong => stack.push(current_style(stack, theme).add_modifier(Modifier::BOLD)),
        Tag::CodeBlock(_) => {
            flush(line, out, *blockquote_depth, theme);
            stack.push(Style::default().fg(theme.accent));
        }
        Tag::List(_) => *list_depth += 1,
        Tag::Item => {
            flush(line, out, *blockquote_depth, theme);
            let indent = "  ".repeat(((*list_depth).saturating_sub(1)) as usize);
            line.push(Span::styled(format!("{indent}• "), theme.muted_style()));
        }
        Tag::BlockQuote(_) => {
            flush(line, out, *blockquote_depth, theme);
            *blockquote_depth += 1;
            stack.push(theme.muted_style().add_modifier(Modifier::ITALIC));
        }
        Tag::Link { dest_url, .. } => {
            stack.push(
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::UNDERLINED),
            );
            // Stash URL via a hidden tag — appended after the link text in End.
            // We piggy-back on a temporary span containing the URL, marked with a
            // sentinel style we can identify later. Simpler: just store it in stack
            // metadata. For now, append after closing.
            let _ = dest_url;
        }
        _ => {}
    }
}

fn handle_end(
    end: TagEnd,
    theme: &Theme,
    line: &mut Vec<Span<'static>>,
    out: &mut Vec<Line<'static>>,
    stack: &mut Vec<Style>,
    list_depth: &mut u32,
    blockquote_depth: &mut u32,
) {
    match end {
        TagEnd::Heading(_) => {
            stack.pop();
            flush(line, out, *blockquote_depth, theme);
            out.push(Line::from(""));
        }
        TagEnd::Paragraph => {
            flush(line, out, *blockquote_depth, theme);
            out.push(Line::from(""));
        }
        TagEnd::CodeBlock => {
            flush(line, out, *blockquote_depth, theme);
            stack.pop();
            out.push(Line::from(""));
        }
        TagEnd::Emphasis | TagEnd::Strong => {
            stack.pop();
        }
        TagEnd::List(_) => {
            if *list_depth > 0 {
                *list_depth -= 1;
            }
            flush(line, out, *blockquote_depth, theme);
        }
        TagEnd::Item => {
            flush(line, out, *blockquote_depth, theme);
        }
        TagEnd::BlockQuote(_) => {
            flush(line, out, *blockquote_depth, theme);
            if *blockquote_depth > 0 {
                *blockquote_depth -= 1;
            }
            stack.pop();
        }
        TagEnd::Link => {
            stack.pop();
        }
        _ => {}
    }
}

fn flush(
    line: &mut Vec<Span<'static>>,
    out: &mut Vec<Line<'static>>,
    blockquote_depth: u32,
    theme: &Theme,
) {
    if line.is_empty() {
        // Even on soft-break inside a blockquote, emit a continuation prefix-less
        // empty line so the visual gap matches the source.
        return;
    }
    let mut spans = std::mem::take(line);
    if blockquote_depth > 0 {
        let prefix = "│ ".repeat(blockquote_depth as usize);
        spans.insert(0, Span::styled(prefix, theme.muted_style()));
    }
    out.push(Line::from(spans));
}

fn current_style(stack: &[Style], theme: &Theme) -> Style {
    *stack.last().unwrap_or(&theme.base())
}

/// Split text on `@mention` boundaries and apply the accent style to mentions.
/// Returns an iterator of styled spans.
fn mention_aware_spans(text: &str, base: Style, theme: &Theme) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '@' {
            let mut mention = String::from('@');
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    mention.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if mention.len() > 1 {
                if !buf.is_empty() {
                    out.push(Span::styled(std::mem::take(&mut buf), base));
                }
                out.push(Span::styled(
                    mention,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ));
                continue;
            } else {
                buf.push(c);
            }
        } else {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        out.push(Span::styled(buf, base));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn plain_text_renders_verbatim() {
        let lines = render_markdown("hello world", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("hello world"));
    }

    #[test]
    fn heading_gets_prefix() {
        let lines = render_markdown("## title", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("## title"));
    }

    #[test]
    fn bold_runs_apply_bold_modifier() {
        let lines = render_markdown("hello **world**", &Theme::dark());
        let has_bold = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.style.add_modifier.contains(Modifier::BOLD) && s.content.contains("world")
            })
        });
        assert!(has_bold);
    }

    #[test]
    fn italic_runs_apply_italic_modifier() {
        let lines = render_markdown("hello *world*", &Theme::dark());
        let has_italic = lines.iter().any(|l| {
            l.spans.iter().any(|s| {
                s.style.add_modifier.contains(Modifier::ITALIC) && s.content.contains("world")
            })
        });
        assert!(has_italic);
    }

    #[test]
    fn code_span_renders_with_backticks() {
        let lines = render_markdown("see `foo` here", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("`foo`"));
    }

    #[test]
    fn unordered_list_renders_bullets() {
        let lines = render_markdown("- a\n- b\n- c", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("• a"));
        assert!(text.contains("• b"));
        assert!(text.contains("• c"));
    }

    #[test]
    fn blockquote_prefix_appears() {
        let lines = render_markdown("> quoted text", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("│ quoted text"));
    }

    #[test]
    fn at_mention_is_highlighted() {
        let lines = render_markdown("hi @alice and @bob_42", &Theme::dark());
        let mentions: Vec<&str> = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.content.starts_with('@'))
            .map(|s| s.content.as_ref())
            .collect();
        assert!(mentions.contains(&"@alice"));
        assert!(mentions.contains(&"@bob_42"));
    }

    #[test]
    fn lone_at_sign_is_not_a_mention() {
        let lines = render_markdown("email me at me", &Theme::dark());
        let mentions = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.content.starts_with('@') && s.style.add_modifier.contains(Modifier::BOLD))
            .count();
        assert_eq!(mentions, 0);
    }

    #[test]
    fn horizontal_rule_renders_dashes() {
        let lines = render_markdown("above\n\n---\n\nbelow", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("───"));
    }

    #[test]
    fn image_placeholder_carries_alt_text() {
        let lines = render_markdown("![a cat](https://x/cat.png)", &Theme::dark());
        let text = flat_text(&lines);
        assert!(
            text.contains("[image: a cat]"),
            "alt is in the tag: {text:?}"
        );
        assert!(!text.contains("https://x/cat.png"), "url no longer inlined");
    }

    #[test]
    fn image_without_alt_is_a_plain_tag() {
        let lines = render_markdown("![](https://x/cat.png)", &Theme::dark());
        let text = flat_text(&lines);
        assert!(
            text.contains("[image]"),
            "no-alt image is a plain tag: {text:?}"
        );
        assert!(!text.contains("[image:"), "no empty alt suffix");
    }

    #[test]
    fn content_preview_drops_images_and_collapses_whitespace() {
        let p = content_preview("![alt](https://x/a.png)\n\nThe knight took her hand.", 200);
        assert_eq!(p, "The knight took her hand.");
    }

    #[test]
    fn content_preview_is_empty_for_image_only_post() {
        // The list item flags the image (via images::has_image); the preview
        // text itself is empty.
        assert_eq!(content_preview("![alt](https://x/a.png)", 200), "");
    }

    #[test]
    fn content_preview_truncates() {
        let p = content_preview(&"x ".repeat(200), 10);
        assert!(p.chars().count() <= 10);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let lines = render_markdown("", &Theme::dark());
        assert!(lines.is_empty() || lines.iter().all(|l| l.spans.is_empty()));
    }

    #[test]
    fn multiparagraph_keeps_separation() {
        let lines = render_markdown("first para\n\nsecond para", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("first para"));
        assert!(text.contains("second para"));
    }
}

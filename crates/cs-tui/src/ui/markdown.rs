//! Minimal markdown → ratatui Lines renderer.
//!
//! Handles the subset of GitHub-flavored markdown that appears in cyberspace.online
//! posts: headings, bold/italic, inline code, code blocks, unordered lists,
//! blockquotes, links (the visible text, then the bare URL on its own line),
//! soft/hard breaks, and `@mention` highlighting. Tables, footnotes, and other
//! advanced features are rendered as plain text.
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// Whether [`render_markdown`] surfaces an image's destination URL below the
/// `[image]` placeholder. Contexts that draw the image as terminal graphics (the
/// post detail, which now renders images inline) pass [`ImageUrls::Hide`];
/// contexts that only show the placeholder (journal entries) pass
/// [`ImageUrls::Show`] so the URL is visible and the terminal's own URL detection
/// makes it clickable.
///
/// Surfaced URLs are safe to render as text: ratatui's `Span::styled_graphemes`
/// filters every grapheme containing a control char, so escape/OSC-8 sequences in
/// a malicious URL are stripped before they reach the terminal. The OSC 8
/// hyperlink overlay ([`super::hyperlink`]) writes the URL into a cell symbol,
/// which bypasses that filter, so it strips control chars itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageUrls {
    /// Surface the URL on its own line below the placeholder (visible/clickable).
    Show,
    /// Suppress the URL — the image itself is drawn elsewhere.
    Hide,
}

/// A surfaced link/image URL and where it landed: `line` is its index into the
/// returned lines, `col` the column the bare URL text starts at (past any
/// blockquote prefix and the indent). A scrolling renderer uses these to overlay
/// an OSC 8 hyperlink onto exactly the URL glyphs — see [`super::hyperlink`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRef {
    pub line: usize,
    pub col: u16,
    pub url: String,
}

/// Render markdown source into a vector of styled ratatui lines. Image URLs are
/// surfaced as plain text (see [`ImageUrls`]); use [`render_markdown_with`] to
/// suppress them where the image is drawn as graphics.
pub fn render_markdown(input: &str, theme: &Theme) -> Vec<Line<'static>> {
    render_markdown_with(input, theme, ImageUrls::Show)
}

/// Render markdown, choosing whether image placeholders carry their URL.
pub fn render_markdown_with(
    input: &str,
    theme: &Theme,
    image_urls: ImageUrls,
) -> Vec<Line<'static>> {
    render_markdown_collect(input, theme, image_urls).0
}

/// Render markdown and also report every surfaced link/image URL (see
/// [`LinkRef`]) so a scrolling renderer can make those rows clickable via OSC 8.
/// Callers that don't need the links use [`render_markdown_with`].
pub fn render_markdown_collect(
    input: &str,
    theme: &Theme,
    image_urls: ImageUrls,
) -> (Vec<Line<'static>>, Vec<LinkRef>) {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut links: Vec<LinkRef> = Vec::new();
    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![theme.base()];
    let mut list_depth: u32 = 0;
    let mut blockquote_depth: u32 = 0;
    // While inside an image tag, accumulate the alt text so the placeholder can
    // carry it (the alt is the only accessible description of the image), and
    // hold the destination URL so it can be surfaced after the placeholder.
    let mut image_alt: Option<String> = None;
    let mut image_url: Option<String> = None;
    // While inside a link, hold the destination URL and accumulate the visible
    // text so the closing tag can surface the URL on its own line — unless the
    // text already conveys the URL (autolinks), which would render it twice.
    let mut link_url: Option<String> = None;
    let mut link_text = String::new();

    let parser = Parser::new(input);
    for event in parser {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                // Capture the alt text and URL; the placeholder is emitted at End.
                image_alt = Some(String::new());
                image_url = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                let alt = image_alt.take().unwrap_or_default();
                current_line.push(Span::styled(
                    image_placeholder(&alt),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::UNDERLINED),
                ));
                // Where the image isn't drawn as graphics (journal), surface its
                // URL on its own line so it's visible and the terminal can make it
                // clickable. The post detail passes `Hide`: it draws the image
                // inline in the body itself.
                if let Some(url) = image_url.take() {
                    if image_urls == ImageUrls::Show && !url.trim().is_empty() {
                        surface_url(
                            &url,
                            &mut current_line,
                            &mut out,
                            blockquote_depth,
                            theme,
                            &mut links,
                        );
                    }
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                style_stack.push(
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::UNDERLINED),
                );
                link_url = Some(dest_url.to_string());
                link_text.clear();
            }
            Event::End(TagEnd::Link) => {
                style_stack.pop();
                if let Some(url) = link_url.take() {
                    let text = std::mem::take(&mut link_text);
                    if !url.trim().is_empty() && !link_suffix_redundant(&text, &url) {
                        surface_url(
                            &url,
                            &mut current_line,
                            &mut out,
                            blockquote_depth,
                            theme,
                            &mut links,
                        );
                    }
                }
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
                    if link_url.is_some() {
                        link_text.push_str(t.as_ref());
                    }
                    let style = current_style(&style_stack, theme);
                    for span in mention_aware_spans(t.as_ref(), style, theme) {
                        current_line.push(span);
                    }
                }
            }
            Event::Code(t) => {
                // Inside an image's alt, or a link's visible text, the code
                // content is part of the accumulated string used for the
                // placeholder / autolink-dedup comparison.
                if let Some(alt) = &mut image_alt {
                    alt.push_str(t.as_ref());
                } else {
                    if link_url.is_some() {
                        link_text.push_str(t.as_ref());
                    }
                    current_line.push(Span::styled(
                        format!("`{t}`"),
                        Style::default().fg(theme.accent),
                    ));
                }
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
    (out, links)
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
        // Links and images are handled inline in `render_markdown_with` so they
        // can surface the destination URL.
        _ => {}
    }
}

/// Width of the indent `surface_url` puts before the bare URL.
const URL_INDENT: u16 = 2;

/// Surface a link/image's destination `url` on its own line. Flushes whatever
/// text precedes it (the link text or `[image]` placeholder) as a finished line,
/// then emits the bare URL — indented, in the accent colour rather than muted —
/// alone on the next line. On its own line the URL can't be wrapped mid-string
/// across two rows, so the terminal's URL detector links the whole thing (a
/// wrapped URL is unclickable in every terminal); a scrolling renderer can also
/// overlay a proper OSC 8 hyperlink onto it from the recorded [`LinkRef`].
fn surface_url(
    url: &str,
    line: &mut Vec<Span<'static>>,
    out: &mut Vec<Line<'static>>,
    blockquote_depth: u32,
    theme: &Theme,
    links: &mut Vec<LinkRef>,
) {
    flush(line, out, blockquote_depth, theme);
    let mut url_line = vec![Span::styled(
        format!("{}{}", " ".repeat(URL_INDENT as usize), url.trim()),
        Style::default().fg(theme.accent),
    )];
    flush(&mut url_line, out, blockquote_depth, theme);
    // The URL is now the last line in `out`. `flush` prepended a `│ ` (2-col)
    // blockquote bar per depth, then our indent — so the URL glyphs start there.
    let col = (blockquote_depth as u16)
        .saturating_mul(2)
        .saturating_add(URL_INDENT);
    links.push(LinkRef {
        line: out.len().saturating_sub(1),
        col,
        url: url.trim().to_string(),
    });
}

/// Whether the link's visible `text` already conveys the `url`, so surfacing the
/// URL again would render it twice. Covers plain autolinks (`<https://x>` → text
/// == url) and email autolinks (`<a@b>` → text `a@b`, url `mailto:a@b`).
fn link_suffix_redundant(text: &str, url: &str) -> bool {
    let text = text.trim();
    let url = url.trim();
    text == url || url.strip_prefix("mailto:").is_some_and(|addr| addr == text)
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
    fn image_placeholder_carries_alt_text_and_url_when_shown() {
        // Default (Show): the alt rides the tag and the URL is surfaced so the
        // terminal can make it clickable (replies/journal, which draw no graphics).
        let lines = render_markdown("![a cat](https://x/cat.png)", &Theme::dark());
        let text = flat_text(&lines);
        assert!(
            text.contains("[image: a cat]"),
            "alt is in the tag: {text:?}"
        );
        assert!(
            text.contains("https://x/cat.png"),
            "url is surfaced as text: {text:?}"
        );
    }

    #[test]
    fn image_url_is_hidden_when_image_is_drawn_as_graphics() {
        // The post body draws the image itself, so the long URL is suppressed.
        let lines = render_markdown_with(
            "![a cat](https://x/cat.png)",
            &Theme::dark(),
            ImageUrls::Hide,
        );
        let text = flat_text(&lines);
        assert!(
            text.contains("[image: a cat]"),
            "alt is in the tag: {text:?}"
        );
        assert!(
            !text.contains("https://x/cat.png"),
            "url not inlined: {text:?}"
        );
    }

    #[test]
    fn collect_reports_surfaced_link_url_position() {
        let (lines, links) = render_markdown_collect(
            "see [the site](https://x.example/page)",
            &Theme::dark(),
            ImageUrls::Show,
        );
        assert_eq!(links.len(), 1, "one link recorded: {links:?}");
        let lr = &links[0];
        assert_eq!(lr.url, "https://x.example/page");
        assert_eq!(lr.col, 2, "URL starts past the 2-space indent");
        // The recorded line index points at the bare-URL line.
        let line_text: String = lines[lr.line]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(line_text.trim(), "https://x.example/page");
        // The recorded column is exactly where the URL glyphs begin on that line.
        assert_eq!(&line_text[lr.col as usize..], "https://x.example/page");
    }

    #[test]
    fn collect_reports_no_links_for_plain_text() {
        let (_lines, links) =
            render_markdown_collect("just words, no links", &Theme::dark(), ImageUrls::Show);
        assert!(links.is_empty(), "no links recorded: {links:?}");
    }

    #[test]
    fn collect_records_image_url_position_when_shown() {
        // Graphics-off contexts surface the image URL as text; it gets a LinkRef
        // too, so the row can be made clickable.
        let (_lines, links) = render_markdown_collect(
            "![a cat](https://x/cat.png)",
            &Theme::dark(),
            ImageUrls::Show,
        );
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://x/cat.png");
    }

    #[test]
    fn image_without_alt_is_a_plain_tag() {
        let lines = render_markdown_with("![](https://x/cat.png)", &Theme::dark(), ImageUrls::Hide);
        let text = flat_text(&lines);
        assert!(
            text.contains("[image]"),
            "no-alt image is a plain tag: {text:?}"
        );
        assert!(!text.contains("[image:"), "no empty alt suffix");
    }

    #[test]
    fn link_surfaces_its_url_on_its_own_line() {
        let lines = render_markdown("see [the site](https://x.example/page)", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("the site"), "link text shown: {text:?}");
        // The URL is surfaced bare (no parens) on its own line below the text, so
        // it can't wrap mid-string and the terminal can linkify the whole thing.
        assert!(
            text.lines().any(|l| l.trim() == "https://x.example/page"),
            "url is alone on its own line: {text:?}"
        );
        assert!(
            !text.contains("(https://x.example/page)"),
            "url is no longer parenthesised inline: {text:?}"
        );
    }

    #[test]
    fn autolink_does_not_duplicate_the_url() {
        // The visible text already IS the URL — don't render `url (url)`.
        let lines = render_markdown("<https://x.example/page>", &Theme::dark());
        let text = flat_text(&lines);
        assert!(
            text.contains("https://x.example/page"),
            "url shown: {text:?}"
        );
        assert_eq!(
            text.matches("https://x.example/page").count(),
            1,
            "url appears exactly once: {text:?}"
        );
    }

    #[test]
    fn email_autolink_does_not_duplicate_with_mailto() {
        // `<a@b>` => visible text "a@b", dest_url "mailto:a@b" — the suffix would
        // read `a@b (mailto:a@b)`; the mailto-aware dedup suppresses it.
        let lines = render_markdown("<user@example.com>", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("user@example.com"), "address shown: {text:?}");
        assert!(
            !text.contains("mailto:"),
            "no redundant mailto suffix: {text:?}"
        );
    }

    #[test]
    fn link_with_inline_code_matching_url_is_not_duplicated() {
        // `[`url`](url)`: the code content IS the URL, so the visible text already
        // conveys it — the suffix must be suppressed (code is accumulated to the
        // dedup string).
        let lines = render_markdown(
            "[`https://x.example/p`](https://x.example/p)",
            &Theme::dark(),
        );
        let text = flat_text(&lines);
        assert_eq!(
            text.matches("https://x.example/p").count(),
            1,
            "code-as-url link not duplicated: {text:?}"
        );
    }

    #[test]
    fn link_with_inline_code_and_distinct_url_still_surfaces_url() {
        // Mixed link text including code, distinct from the URL — surface the URL.
        let lines = render_markdown("[run `cmd` now](https://x.example/p)", &Theme::dark());
        let text = flat_text(&lines);
        assert!(text.contains("run "), "text shown: {text:?}");
        assert!(text.contains("`cmd`"), "code shown: {text:?}");
        assert!(
            text.lines().any(|l| l.trim() == "https://x.example/p"),
            "distinct url surfaced on its own line: {text:?}"
        );
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

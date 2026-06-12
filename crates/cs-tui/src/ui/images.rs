//! Image discovery for posts: markdown image links plus image attachments.
use cs_api::{Attachment, Entry, Reply};
use pulldown_cmark::{Event, Parser, Tag};

/// Every image URL referenced by `content` (markdown `![](url)` links) followed
/// by `attachments` (image attachments) — de-duplicated, in order of appearance.
fn collect_image_urls(content: &str, attachments: &[Attachment]) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut push = |u: &str| {
        let u = u.trim();
        if !u.is_empty() && !urls.iter().any(|e| e == u) {
            urls.push(u.to_string());
        }
    };
    for ev in Parser::new(content) {
        if let Event::Start(Tag::Image { dest_url, .. }) = ev {
            push(dest_url.as_ref());
        }
    }
    for att in attachments {
        if let Attachment::Image { src, .. } = att {
            push(src);
        }
    }
    urls
}

/// Every image URL an entry references — markdown links then image attachments,
/// de-duplicated, in order of appearance.
pub fn entry_image_urls(entry: &Entry) -> Vec<String> {
    collect_image_urls(&entry.content, &entry.attachments)
}

/// Every image URL a reply references, same rules as [`entry_image_urls`]. Used
/// to render the selected reply's image in the post-detail image strip.
pub fn reply_image_urls(reply: &Reply) -> Vec<String> {
    collect_image_urls(&reply.content, &reply.attachments)
}

/// Whether an entry references any image — a markdown `![](url)` link OR an
/// image attachment. Cheaper than [`entry_image_urls`]: it short-circuits on the
/// first image instead of building the deduped list. Used to flag posts with
/// images in list views (the feed snippet only sees markdown, not attachments).
#[must_use]
pub fn has_image(entry: &Entry) -> bool {
    entry
        .attachments
        .iter()
        .any(|a| matches!(a, Attachment::Image { .. }))
        || Parser::new(&entry.content).any(|ev| matches!(ev, Event::Start(Tag::Image { .. })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(content: &str, attachments: Vec<Attachment>) -> Entry {
        Entry {
            content: content.into(),
            attachments,
            ..Default::default()
        }
    }

    #[test]
    fn collects_markdown_images_then_attachments_deduped() {
        let e = entry(
            "see ![a](https://x/a.png) and ![b](https://x/b.png)",
            vec![
                Attachment::Image {
                    src: "https://x/c.png".into(),
                    width: 0,
                    height: 0,
                },
                // duplicate of a markdown one — should not repeat
                Attachment::Image {
                    src: "https://x/a.png".into(),
                    width: 0,
                    height: 0,
                },
            ],
        );
        let urls = entry_image_urls(&e);
        assert_eq!(
            urls,
            vec!["https://x/a.png", "https://x/b.png", "https://x/c.png"]
        );
    }

    #[test]
    fn ignores_audio_attachments_and_empty() {
        let e = entry(
            "no images here",
            vec![Attachment::Audio {
                src: "https://x/song.mp3".into(),
                origin: String::new(),
                artist: String::new(),
                title: String::new(),
                genre: String::new(),
            }],
        );
        assert!(entry_image_urls(&e).is_empty());
    }

    #[test]
    fn reply_image_urls_collects_markdown_then_attachments() {
        let r = Reply {
            content: "see ![a](https://x/a.png)".into(),
            attachments: vec![Attachment::Image {
                src: "https://x/b.png".into(),
                width: 0,
                height: 0,
            }],
            ..Default::default()
        };
        assert_eq!(
            reply_image_urls(&r),
            vec!["https://x/a.png", "https://x/b.png"]
        );
    }

    #[test]
    fn has_image_detects_markdown_or_attachment() {
        // markdown image link in content
        assert!(has_image(&entry("see ![a](https://x/a.png)", vec![])));
        // text-only content but an image ATTACHMENT (the case the feed missed)
        assert!(has_image(&entry(
            "just text",
            vec![Attachment::Image {
                src: "https://x/c.png".into(),
                width: 0,
                height: 0,
            }],
        )));
        // neither
        assert!(!has_image(&entry("no images", vec![])));
    }
}

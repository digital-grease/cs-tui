//! Image discovery for posts: markdown image links plus image attachments.
use cs_api::{Attachment, Entry};
use pulldown_cmark::{Event, Parser, Tag};

/// Every image URL an entry references — markdown `![](url)` links in the
/// content, then image attachments — de-duplicated, in order of appearance.
pub fn entry_image_urls(entry: &Entry) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut push = |u: &str| {
        let u = u.trim();
        if !u.is_empty() && !urls.iter().any(|e| e == u) {
            urls.push(u.to_string());
        }
    };
    for ev in Parser::new(&entry.content) {
        if let Event::Start(Tag::Image { dest_url, .. }) = ev {
            push(dest_url.as_ref());
        }
    }
    for att in &entry.attachments {
        if let Attachment::Image { src, .. } = att {
            push(src);
        }
    }
    urls
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
}

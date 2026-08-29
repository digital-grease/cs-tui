//! Image discovery for posts: markdown image links plus image attachments.
use cs_api::{Attachment, Entry, Reply};
use pulldown_cmark::{Event, Parser, Tag};
use ratatui_image::FilterType;

use crate::config::ImageSharpness;

/// The resampling filter a given sharpness asks for.
///
/// `None` is nearest neighbour, which is `ratatui_image`'s own default and what
/// every call site passed before this was configurable. That is why `Crisp`
/// maps to `None` rather than to an explicit `FilterType::Nearest`: the default
/// path stays byte-for-byte the call it always was.
///
/// Split from [`filter`] so it can be tested. [`filter`] reads the process-wide
/// runtime config, which a test cannot set (it is a `OnceLock`), so a test of
/// the mapping has to be handed the input instead.
#[must_use]
pub fn filter_for(sharpness: ImageSharpness) -> Option<FilterType> {
    match sharpness {
        ImageSharpness::Crisp => None,
        ImageSharpness::Smooth => Some(FilterType::Triangle),
        ImageSharpness::Medium => Some(FilterType::CatmullRom),
        ImageSharpness::Sharp => Some(FilterType::Lanczos3),
    }
}

/// The resampling filter the configured `image_sharpness` asks for.
///
/// Every `Resize::Fit` in the client passes this, so the setting reaches the
/// feed, post detail, cIRC, C-Mail and the fullscreen modal from one place.
#[must_use]
pub fn filter() -> Option<FilterType> {
    filter_for(crate::config::get().image_sharpness)
}

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

/// Largest image we will hold or decode, in bytes of the encoded file.
///
/// These bytes arrive from a URL in someone else's message, so the size is not
/// ours to trust.
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Largest decoded pixel count we will allocate for.
///
/// The encoded size bounds the download, but not the decode: a few hundred
/// kilobytes of PNG can expand to gigabytes of pixels, which is a
/// decompression bomb whether or not it was meant as one. 40 megapixels is far
/// past anything a terminal can show.
pub const MAX_IMAGE_PIXELS: u64 = 40_000_000;

/// Largest number of images one screen keeps bytes and protocols for.
///
/// The caches were insert-only for the life of the session, so a reader
/// scrolling a busy room accumulated every picture they had ever passed,
/// encoded protocol included, which defeated the message-buffer cap entirely.
pub const MAX_CACHED_IMAGES: usize = 24;

/// Drop entries from an image cache until it is back under the cap.
///
/// Evicts anything not in `keep`, which the caller passes as the URLs currently
/// reachable on screen, and only then falls back to dropping arbitrary entries.
/// Cheap and approximate on purpose: this runs from a render, and a perfect LRU
/// would cost more bookkeeping than the memory it saves.
pub fn evict_to_cap<V>(cache: &mut std::collections::HashMap<String, V>, keep: &[String]) {
    if cache.len() <= MAX_CACHED_IMAGES {
        return;
    }
    cache.retain(|url, _| keep.iter().any(|k| k == url));
    while cache.len() > MAX_CACHED_IMAGES {
        let Some(victim) = cache.keys().next().cloned() else {
            break;
        };
        cache.remove(&victim);
    }
}

/// Decode `bytes` into an image, refusing anything oversized.
///
/// Prefer this to `image::load_from_memory` anywhere the bytes came from the
/// network. Returns `Err` rather than panicking or allocating unboundedly.
///
/// # Errors
///
/// If the payload is too large, too many pixels, or not a decodable image.
pub fn decode_bounded(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("image is {} bytes, over the cap", bytes.len()));
    }
    // Read the header first and refuse on dimensions, before anything
    // allocates room for the pixels.
    if let Some((w, h)) = probe_dimensions(bytes) {
        let pixels = u64::from(w) * u64::from(h);
        if pixels > MAX_IMAGE_PIXELS {
            return Err(format!("image is {w}x{h}, over the pixel cap"));
        }
    }
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

/// An image's pixel dimensions, read from its header alone.
///
/// Cheap on purpose: the layout needs the size to decide how many rows to
/// reserve, and decoding the whole picture to learn it would do the expensive
/// work for something that may never scroll into view. `None` when the bytes
/// are not a recognisable image, in which case the caller should keep its
/// fallback rather than guess.
#[must_use]
pub fn probe_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
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
    fn crisp_maps_to_the_library_default_and_the_rest_are_distinct() {
        // `Crisp` must stay `None`, not `Some(Nearest)`. They render the same,
        // but `None` is the call every site made before this setting existed,
        // so the default path is unchanged rather than merely equivalent.
        assert_eq!(filter_for(ImageSharpness::Crisp), None);

        let named = [
            filter_for(ImageSharpness::Smooth),
            filter_for(ImageSharpness::Medium),
            filter_for(ImageSharpness::Sharp),
        ];
        assert!(named.iter().all(Option::is_some), "only crisp may be None");
        // A setting whose values collapse onto one filter would look like it
        // works while doing nothing.
        for (i, a) in named.iter().enumerate() {
            for b in &named[i + 1..] {
                assert_ne!(a, b, "two sharpness levels resolve to the same filter");
            }
        }
    }

    #[test]
    fn every_sharpness_name_in_the_template_parses() {
        // The template offers these four by name; a name that does not parse
        // would silently fall back to crisp and look like the setting is dead.
        for (name, want) in [
            ("crisp", ImageSharpness::Crisp),
            ("smooth", ImageSharpness::Smooth),
            ("medium", ImageSharpness::Medium),
            ("sharp", ImageSharpness::Sharp),
        ] {
            assert_eq!(ImageSharpness::parse(name), Some(want), "{name}");
        }
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

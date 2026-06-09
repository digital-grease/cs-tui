//! Jukebox (audio) attachment surfacing for posts and replies.
//!
//! On cyberspace.online a "jukebox" track is modeled as an [`Attachment::Audio`]
//! (almost always a YouTube link, `origin: "youtube"`) carrying the track's
//! `artist`, `title`, and `genre` alongside the `src` URL. The TUI can't stream
//! audio inline, but the link and its metadata are worth keeping: previously the
//! whole attachment was dropped, leaving only the post's text. Here we detect
//! audio attachments (to flag them in list views) and render a compact card (for
//! the detail view), mirroring [`super::images`] for images.
use cs_api::{Attachment, Entry};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// Whether an entry carries an audio ("jukebox") attachment. Mirrors
/// [`super::images::has_image`]; used to flag posts in list views where the
/// snippet only sees markdown text, not attachments.
#[must_use]
pub fn has_audio(entry: &Entry) -> bool {
    entry
        .attachments
        .iter()
        .any(|a| matches!(a, Attachment::Audio { .. }))
}

/// Render audio ("jukebox") attachments as a compact card placed below the
/// post/reply body: the track title, then the artist, a genre chip, and the
/// source URL on its own line styled like a link so it stays visible and
/// copyable. Returns an empty vec when there are no audio attachments. Shared by
/// the post body and each reply (both expose `attachments`).
#[must_use]
pub fn audio_lines(attachments: &[Attachment], theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for att in attachments {
        let Attachment::Audio {
            src,
            artist,
            title,
            genre,
            ..
        } = att
        else {
            continue;
        };
        let title = title.trim();
        let artist = artist.trim();
        let genre = genre.trim();
        let src = src.trim();

        // Title line, prefixed with a music note. Falls back to "jukebox" when
        // the track has no title so the card never renders blank.
        let heading = if title.is_empty() { "jukebox" } else { title };
        lines.push(Line::from(vec![
            Span::styled("♪ ", theme.accent_style()),
            Span::styled(
                heading.to_string(),
                theme.accent_style().add_modifier(Modifier::BOLD),
            ),
        ]));
        if !artist.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {artist}"),
                theme.base(),
            )));
        }
        if !genre.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  [{genre}]"),
                theme.muted_style(),
            )));
        }
        if !src.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {src}"),
                theme.accent_style().add_modifier(Modifier::UNDERLINED),
            )));
        }
    }
    lines
}

/// A playable jukebox track: the source URL plus the metadata shown in the
/// now-playing bar. Resolved from an [`Attachment::Audio`] for playback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JukeboxTrack {
    pub url: String,
    pub artist: String,
    pub title: String,
}

/// The source URL of the first audio ("jukebox") attachment, if any. This is the
/// link the "open in browser" action hands to the desktop.
#[must_use]
pub fn jukebox_url(attachments: &[Attachment]) -> Option<String> {
    attachments.iter().find_map(|a| match a {
        Attachment::Audio { src, .. } if !src.trim().is_empty() => Some(src.trim().to_string()),
        _ => None,
    })
}

/// The first playable jukebox track in `attachments`, if any. Returns `None`
/// when there's no audio attachment or it carries no usable source URL.
#[must_use]
pub fn jukebox_track(attachments: &[Attachment]) -> Option<JukeboxTrack> {
    attachments.iter().find_map(|a| match a {
        Attachment::Audio {
            src, artist, title, ..
        } if !src.trim().is_empty() => Some(JukeboxTrack {
            url: src.trim().to_string(),
            artist: artist.trim().to_string(),
            title: title.trim().to_string(),
        }),
        _ => None,
    })
}

/// Extract a YouTube video ID from a watch / short-link / embed / shorts URL.
/// Handles the common `youtube.com/watch?v=ID`, `youtu.be/ID`,
/// `youtube.com/embed/ID`, and `youtube.com/shorts/ID` forms (ignoring extra
/// query parameters) and the `youtube-nocookie.com` / `m.` / `www.` variants.
/// Returns `None` for non-YouTube or unrecognized URLs.
fn youtube_id(src: &str) -> Option<String> {
    let src = src.trim();
    let after_scheme = src
        .strip_prefix("https://")
        .or_else(|| src.strip_prefix("http://"))
        .unwrap_or(src);
    let (host, path_and_query) = after_scheme.split_once('/').unwrap_or((after_scheme, ""));
    // Hostnames are case-insensitive and may carry a :port; normalize before
    // matching. Only the host is lowercased — the video id (in path/query) is
    // case-sensitive and must be left untouched.
    let host = host.to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or(&host);
    let host = host
        .trim_start_matches("www.")
        .trim_start_matches("m.")
        .trim_start_matches("music.");
    let first_segment = |s: &str| {
        s.split(['?', '&', '/', '#'])
            .next()
            .unwrap_or("")
            .to_string()
    };
    let raw = match host {
        "youtu.be" => first_segment(path_and_query),
        "youtube.com" | "youtube-nocookie.com" => {
            if let Some(rest) = path_and_query.strip_prefix("embed/") {
                first_segment(rest)
            } else if let Some(rest) = path_and_query.strip_prefix("shorts/") {
                first_segment(rest)
            } else {
                // watch?v=ID (the `v` param may sit anywhere in the query).
                path_and_query
                    .split_once('?')
                    .map_or("", |(_, q)| q)
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("v="))
                    .map(str::to_string)
                    .unwrap_or_default()
            }
        }
        _ => String::new(),
    };
    // YouTube IDs are short tokens of [A-Za-z0-9_-]; reject anything else so a
    // malformed src never becomes a bogus thumbnail request.
    if !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Some(raw)
    } else {
        None
    }
}

/// Whether `url` is a YouTube link (so mpv will need yt-dlp to play it).
#[must_use]
pub fn is_youtube(url: &str) -> bool {
    youtube_id(url).is_some()
}

/// The cover-art image URL for an audio attachment: the YouTube thumbnail
/// derived from the track's `src`. Returns `None` for non-YouTube sources or
/// when the video ID can't be parsed. Uses `hqdefault.jpg`, which always exists
/// for a valid video (unlike `maxresdefault.jpg`).
#[must_use]
pub fn cover_art_url(att: &Attachment) -> Option<String> {
    let Attachment::Audio { src, .. } = att else {
        return None;
    };
    let id = youtube_id(src)?;
    Some(format!("https://img.youtube.com/vi/{id}/hqdefault.jpg"))
}

/// The cover-art URL for an entry's first audio attachment, if any. Used to fill
/// the post-detail image slot for jukebox posts (which carry no image, since the
/// API allows at most one attachment per post).
#[must_use]
pub fn entry_cover_art_url(entry: &Entry) -> Option<String> {
    entry.attachments.iter().find_map(cover_art_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(artist: &str, title: &str, genre: &str, src: &str) -> Attachment {
        Attachment::Audio {
            src: src.into(),
            origin: "youtube".into(),
            artist: artist.into(),
            title: title.into(),
            genre: genre.into(),
        }
    }

    fn entry_with(attachments: Vec<Attachment>) -> Entry {
        Entry {
            attachments,
            ..Default::default()
        }
    }

    fn flat(lines: &[Line<'_>]) -> String {
        lines
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
    fn has_audio_detects_audio_attachment_only() {
        assert!(has_audio(&entry_with(vec![audio(
            "Art of Noise",
            "Paranoimia",
            "electronic",
            "https://youtu.be/x"
        )])));
        // image attachment is not audio
        assert!(!has_audio(&entry_with(vec![Attachment::Image {
            src: "https://x/a.png".into(),
            width: 0,
            height: 0,
        }])));
        assert!(!has_audio(&entry_with(vec![])));
    }

    #[test]
    fn audio_lines_render_metadata_and_link() {
        let theme = Theme::dark();
        let lines = audio_lines(
            &[audio(
                "Art of Noise",
                "Paranoimia",
                "electronic",
                "https://www.youtube.com/watch?v=abc",
            )],
            &theme,
        );
        let text = flat(&lines);
        assert!(text.contains("♪ Paranoimia"), "title: {text:?}");
        assert!(text.contains("Art of Noise"), "artist: {text:?}");
        assert!(text.contains("[electronic]"), "genre chip: {text:?}");
        assert!(
            text.contains("https://www.youtube.com/watch?v=abc"),
            "the link must be retained: {text:?}"
        );
    }

    #[test]
    fn audio_lines_fall_back_to_jukebox_when_title_missing() {
        let theme = Theme::dark();
        let lines = audio_lines(&[audio("", "", "", "https://youtu.be/x")], &theme);
        let text = flat(&lines);
        assert!(text.contains("♪ jukebox"), "fallback heading: {text:?}");
        assert!(
            text.contains("https://youtu.be/x"),
            "link still shown: {text:?}"
        );
    }

    #[test]
    fn youtube_id_parses_common_url_forms() {
        let id = "dQw4w9WgXcQ";
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
            "https://www.youtube.com/watch?list=PL123&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "http://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(youtube_id(url).as_deref(), Some(id), "failed on {url}");
        }
    }

    #[test]
    fn youtube_id_is_host_case_insensitive_and_ignores_port() {
        let id = "dQw4w9WgXcQ";
        for url in [
            "https://YouTube.com/watch?v=dQw4w9WgXcQ",
            "https://WWW.YOUTUBE.COM/watch?v=dQw4w9WgXcQ",
            "https://YOUTU.BE/dQw4w9WgXcQ",
            "https://www.youtube.com:443/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(youtube_id(url).as_deref(), Some(id), "failed on {url}");
        }
        // The video id itself stays case-sensitive (not lowercased).
        assert_eq!(
            youtube_id("https://youtu.be/AbCdEfGhIjK").as_deref(),
            Some("AbCdEfGhIjK")
        );
    }

    #[test]
    fn youtube_id_rejects_non_youtube_and_garbage() {
        assert_eq!(youtube_id("https://example.com/watch?v=x"), None);
        assert_eq!(youtube_id("https://soundcloud.com/foo/bar"), None);
        assert_eq!(youtube_id("not a url"), None);
        assert_eq!(youtube_id("https://www.youtube.com/watch?v="), None);
    }

    #[test]
    fn cover_art_url_builds_youtube_thumbnail() {
        let att = audio("A", "T", "g", "https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(
            cover_art_url(&att).as_deref(),
            Some("https://img.youtube.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
        );
        // Non-YouTube audio (e.g. a direct file) has no derivable thumbnail.
        assert_eq!(
            cover_art_url(&audio("A", "T", "g", "https://x/song.mp3")),
            None
        );
        // Image attachments aren't audio.
        assert_eq!(
            cover_art_url(&Attachment::Image {
                src: "https://x/a.png".into(),
                width: 0,
                height: 0,
            }),
            None
        );
    }

    #[test]
    fn entry_cover_art_url_finds_the_audio_attachment() {
        let e = entry_with(vec![audio(
            "A",
            "T",
            "g",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        )]);
        assert_eq!(
            entry_cover_art_url(&e).as_deref(),
            Some("https://img.youtube.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
        );
        assert_eq!(entry_cover_art_url(&entry_with(vec![])), None);
    }

    #[test]
    fn audio_lines_ignore_non_audio_and_empty() {
        let theme = Theme::dark();
        assert!(audio_lines(&[], &theme).is_empty());
        assert!(audio_lines(
            &[Attachment::Image {
                src: "https://x/a.png".into(),
                width: 0,
                height: 0,
            }],
            &theme,
        )
        .is_empty());
    }
}

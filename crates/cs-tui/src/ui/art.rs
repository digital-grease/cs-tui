//! Decoding the `/art` payload carried by a chat message (API v0.8.4).
//!
//! Art is posted with `/art` (§ Commands) and comes back as a message whose
//! `style` is `"art"` and whose `content` is base64-encoded rather than readable
//! text (§ Message fields). The canvas the server enforces is 80 columns by 25
//! lines, and the spec is explicit that leading spaces are preserved "because
//! they're the picture", so nothing in this module trims, reflows or otherwise
//! tidies a line.
//!
//! The spec leaves three things open. What this module does about each:
//!
//! - **Which base64 alphabet.** § Message fields says only "base64-encoded". A
//!   browser `btoa()`, which is what the website posts with, emits standard
//!   base64 *with* padding, so that engine is tried first. The padless and
//!   URL-safe variants follow, and the first engine that yields something
//!   text-shaped wins. (The URL-safe padless engine used elsewhere in this
//!   workspace is for JWTs, a different problem, and is deliberately not the
//!   first guess here.)
//! - **Whether the decoded bytes are UTF-8.** They are not guaranteed to be, so
//!   the conversion is lossy: an undecodable byte becomes U+FFFD rather than
//!   costing the reader the whole picture. The known limitation is CP437 art,
//!   the DOS box-drawing set, whose high bytes are not UTF-8 and so arrive as
//!   replacement characters. Rendering that faithfully needs a codepage table,
//!   which stays out of scope until a real message proves it is needed.
//! - **What to do when nothing decodes.** Show the raw `content`. A message
//!   rendered oddly is strictly better than a message that vanishes.
//!
//! Wrapping and styling are not done here: [`super::chat`] hard-wraps the
//! decoded lines to the pane so that runs of spaces survive, which the
//! word-wrapping path would collapse.
use base64::engine::general_purpose;
use base64::Engine as _;

/// At most this share of the decoded bytes may be control bytes for the decode
/// to be believed, as a reciprocal (5 means one fifth, so 20%).
///
/// Real art is printable text plus newlines. A run of control bytes means some
/// engine happily decoded something that was never base64 in the first place,
/// and the raw content is the better answer.
const CONTROL_BYTE_LIMIT: usize = 5;

/// Decode the `content` of a `style: "art"` message into displayable text.
///
/// Line breaks are preserved as `\n` and every leading space survives, so the
/// result is ready to hand to [`super::chat::hard_wrap`]. Falls back to
/// `content` itself, newline-normalized, when no base64 engine produces
/// text-shaped bytes. Never panics, and never returns nothing for a non-empty
/// input.
///
/// ```ignore
/// let picture = art::decode_art(&message.content);
/// ```
#[must_use]
pub fn decode_art(content: &str) -> String {
    decode_base64(content).unwrap_or_else(|| normalize(content))
}

/// Try every base64 alphabet in turn, returning the first decode that looks
/// like text. ASCII whitespace is stripped first, so art that arrives wrapped
/// across several base64 lines still decodes.
fn decode_base64(content: &str) -> Option<String> {
    let compact: String = content.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return None;
    }
    // Standard-with-padding first: that is what a browser `btoa()` emits.
    for engine in [
        general_purpose::STANDARD,
        general_purpose::STANDARD_NO_PAD,
        general_purpose::URL_SAFE,
        general_purpose::URL_SAFE_NO_PAD,
    ] {
        let Ok(bytes) = engine.decode(&compact) else {
            continue;
        };
        if !looks_like_text(&bytes) {
            continue;
        }
        return Some(normalize(&String::from_utf8_lossy(&bytes)));
    }
    None
}

/// Whether `bytes` are plausibly the text of a picture rather than the result
/// of reading something that was never base64.
fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let control = bytes
        .iter()
        .filter(|b| b.is_ascii_control() && !matches!(b, b'\n' | b'\r' | b'\t'))
        .count();
    control * CONTROL_BYTE_LIMIT <= bytes.len()
}

/// Normalize line endings and drop one trailing newline.
///
/// A bare `\r` would be filtered out by ratatui's own control-character
/// stripping and silently join two rows of the picture, so carriage returns
/// become line breaks instead. Exactly one trailing newline is dropped, since a
/// picture stored with a final newline would otherwise render a phantom blank
/// row under itself; any further blank lines are kept, because they may be part
/// of the composition.
fn normalize(text: &str) -> String {
    let mut unified = text.replace("\r\n", "\n").replace('\r', "\n");
    if unified.ends_with('\n') {
        unified.pop();
    }
    unified
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cat from § Commands' own `/art` example.
    const CAT: &str = " /\\_/\\\n( o.o )\n > ^ <";

    fn encode_standard(text: &str) -> String {
        general_purpose::STANDARD.encode(text)
    }

    /// The decoded picture split into rows, the way the chat renderer sees it.
    fn rows_of(content: &str) -> Vec<String> {
        decode_art(content)
            .split('\n')
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn standard_padded_base64_is_decoded() {
        // A browser `btoa()` emits standard base64 with padding, so that is the
        // engine tried first.
        assert_eq!(decode_art(&encode_standard(CAT)), CAT);

        // A payload whose length forces `=` padding, which the padless engines
        // would refuse outright.
        let padded = encode_standard("ab");
        assert!(padded.ends_with('='), "the fixture should carry padding");
        assert_eq!(decode_art(&padded), "ab");
    }

    #[test]
    fn padless_and_url_safe_alphabets_also_decode() {
        // A payload whose encoding actually differs between the two alphabets,
        // so the URL-safe cases are genuinely exercised rather than decoding a
        // string that happens to be alphabet-neutral.
        let payload = "ab~ab?";
        assert_ne!(
            general_purpose::STANDARD.encode(payload),
            general_purpose::URL_SAFE.encode(payload),
            "the fixture must use the alphabet-specific characters",
        );
        for encoded in [
            general_purpose::STANDARD.encode(payload),
            general_purpose::STANDARD_NO_PAD.encode(payload),
            general_purpose::URL_SAFE.encode(payload),
            general_purpose::URL_SAFE_NO_PAD.encode(payload),
        ] {
            assert_eq!(decode_art(&encoded), payload, "failed on {encoded}");
        }
    }

    #[test]
    fn leading_spaces_are_preserved_exactly() {
        // § Commands: leading spaces are the picture, so they must survive the
        // round trip untouched, including a run of them.
        let picture = "    /\\\n   /  \\\n  /____\\";
        assert_eq!(
            rows_of(&encode_standard(picture)),
            vec!["    /\\", "   /  \\", "  /____\\"],
        );
    }

    #[test]
    fn interior_blank_lines_survive() {
        let picture = "top\n\n\nbottom";
        assert_eq!(rows_of(&encode_standard(picture)).len(), 4);
    }

    #[test]
    fn a_single_trailing_newline_is_dropped_but_not_two() {
        assert_eq!(rows_of(&encode_standard("hi\n")), vec!["hi"]);
        assert_eq!(rows_of(&encode_standard("hi\n\n")), vec!["hi", ""]);
    }

    #[test]
    fn carriage_returns_become_line_breaks() {
        assert_eq!(rows_of(&encode_standard("a\r\nb\rc")), vec!["a", "b", "c"]);
    }

    #[test]
    fn base64_split_across_lines_still_decodes() {
        // Whitespace inside the payload is stripped before decoding, so art that
        // arrives wrapped is not lost.
        let encoded = encode_standard(CAT);
        let wrapped = format!("{}\n{}", &encoded[..8], &encoded[8..]);
        assert_eq!(decode_art(&wrapped), CAT);
    }

    #[test]
    fn undecodable_content_falls_back_to_the_raw_text() {
        // Losing the message would be worse than showing it oddly.
        assert_eq!(decode_art(CAT), CAT, "the raw picture is shown as-is");
        assert_eq!(decode_art("not base64!!"), "not base64!!");
    }

    #[test]
    fn a_decode_full_of_control_bytes_is_rejected() {
        // Some inputs decode under one alphabet or another into binary noise.
        // That is a false positive, so the raw content wins instead.
        let noise: Vec<u8> = (0u8..32).collect();
        let encoded = general_purpose::STANDARD.encode(noise);
        assert_eq!(decode_art(&encoded), encoded);
    }

    #[test]
    fn non_utf8_bytes_degrade_to_replacement_characters() {
        // The documented CP437 limitation: high bytes are not UTF-8, so they
        // arrive as U+FFFD instead of panicking or dropping the picture.
        let encoded = general_purpose::STANDARD.encode([b'a', 0xc9, 0xcd, b'b']);
        let decoded = decode_art(&encoded);
        assert!(decoded.starts_with('a'), "{decoded:?}");
        assert!(decoded.ends_with('b'), "{decoded:?}");
        assert!(decoded.contains('\u{fffd}'), "{decoded:?}");
    }

    #[test]
    fn empty_content_yields_one_empty_line() {
        assert_eq!(decode_art(""), "");
        assert_eq!(rows_of(""), vec![String::new()]);
        assert_eq!(rows_of("   "), vec!["   "], "whitespace-only is kept");
    }

    #[test]
    fn a_full_canvas_decodes_row_for_row() {
        // § Commands puts the canvas at 80 columns by 25 lines. Nothing here
        // clamps to it, but a picture that fills it must come back intact.
        let picture: String = (0..25)
            .map(|row| format!("{}{row}", " ".repeat(78)))
            .collect::<Vec<_>>()
            .join("\n");
        let decoded = rows_of(&encode_standard(&picture));
        assert_eq!(decoded.len(), 25);
        assert!(decoded[7].starts_with("       "), "{:?}", decoded[7]);
        assert!(decoded[7].ends_with('7'));
    }
}

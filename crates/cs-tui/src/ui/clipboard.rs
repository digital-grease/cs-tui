//! Copying text to the reader's clipboard over OSC 52.
//!
//! A terminal escape rather than a platform clipboard crate, for two reasons:
//! it needs no dependency and no display server, and it is the only mechanism
//! that works when the terminal is somewhere other than the machine we run on.
//!
//! **Support is not universal.** kitty, Ghostty, WezTerm, Alacritty, foot and
//! recent xterm honour it; some terminals ignore it, and tmux and screen need it
//! enabled (`set -g set-clipboard on`). There is no reply to read, so a terminal
//! that ignores the sequence is indistinguishable from one that acted on it, and
//! [`copy`] can only report that it wrote the bytes. Callers should say
//! "copied", not "copied successfully".
//!
//! Nothing here reads the clipboard. OSC 52 can request its contents, and a
//! page that can read what you copied elsewhere is a hazard we have no use for.
use std::io::Write;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

/// Largest payload written, in bytes of text before encoding.
///
/// Terminals cap the escape they will accept and truncate silently past it, and
/// a half-copied message is worse than a refusal because nothing says it
/// happened. 64 KiB is comfortably inside every implementation seen, and far
/// larger than any chat message the API permits.
const MAX_BYTES: usize = 64 * 1024;

/// Put `text` on the clipboard, returning whether the sequence was written.
///
/// `Ok(false)` means the text was too large and nothing was sent, which the
/// caller should report rather than silently drop.
///
/// # Errors
///
/// If the terminal cannot be written to or flushed.
pub fn copy(text: &str) -> std::io::Result<bool> {
    if text.is_empty() || text.len() > MAX_BYTES {
        return Ok(false);
    }
    let encoded = STANDARD.encode(text);
    // `c` is the selection: the system clipboard rather than the X primary.
    // Terminated with BEL rather than ST, which is the form the widest set of
    // terminals accepts.
    let seq = format!("\u{1b}]52;c;{encoded}\u{7}");
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes())?;
    out.flush()?;
    Ok(true)
}

/// The permalink for an entry on the website, for a "copy link" action.
///
/// Mirrors the site's own `/{username}/{slug}` shape. Returns `None` when either
/// half is missing, since a half-built link is worse than none.
#[must_use]
pub fn entry_permalink(username: &str, slug: &str) -> Option<String> {
    let username = username.trim();
    let slug = slug.trim();
    (!username.is_empty() && !slug.is_empty())
        .then(|| format!("https://cyberspace.online/{username}/{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_or_oversized_payload_is_refused_rather_than_truncated() {
        // A terminal truncates silently past its cap, and a half-copied message
        // is worse than a refusal because nothing tells the reader.
        assert_eq!(copy("").ok(), Some(false));
        let huge = "x".repeat(MAX_BYTES + 1);
        assert_eq!(copy(&huge).ok(), Some(false));
    }

    #[test]
    fn a_permalink_needs_both_halves() {
        assert_eq!(
            entry_permalink("neo", "my-entry").as_deref(),
            Some("https://cyberspace.online/neo/my-entry"),
        );
        assert_eq!(entry_permalink("", "my-entry"), None);
        assert_eq!(entry_permalink("neo", "  "), None);
    }

    #[test]
    fn the_payload_is_base64_of_the_text() {
        // Pinning the encoding rather than the write, since the write goes to a
        // real terminal. `STANDARD` with padding is what OSC 52 expects.
        assert_eq!(STANDARD.encode("hi"), "aGk=");
        assert_eq!(STANDARD.encode("héllo"), "aMOpbGxv");
    }
}

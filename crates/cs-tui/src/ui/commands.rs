//! Which slash commands the server will recognize (API v0.8.6, § Commands).
//!
//! Both cIRC and C-Mail expand slash commands server-side, and anything that
//! is not a command is posted as plain text. That is the problem this module
//! exists for: a typo'd `/dcie 2d6` is not an error to the server, it is a
//! message reading "/dcie 2d6" posted to the room for everyone to see. Checking
//! the name here lets the client refuse to send it instead.
//!
//! **Names only.** Whether `/dice 4d6kh3` parses is the server's business, and
//! a recognized command with bad syntax still round-trips for its
//! `400 VALIDATION_ERROR`. This only answers "is this word a command at all".
//!
//! Three details from § Commands that are easy to get wrong:
//!
//! - **`/img` is known**, even though it is website-only. The spec says to send
//!   it and it "posts as plain text", so rejecting it locally would block
//!   documented behavior. It is in [`BASE`] for that reason, not by accident.
//! - **`/dice` takes colons in the command word itself** (`/dice:6`,
//!   `/dice:4:20`), so the word is matched up to its first colon rather than
//!   whole.
//! - **Styles chain with `+`** (`/comic+rainbow`), and every style combines
//!   except `/spoiler`, which is rejected in a chain even though each name in
//!   it is individually valid.
use std::collections::HashSet;

/// Which surface is being posted to.
///
/// § Commands marks `/art` and the `/mute` family cIRC-only; sending one in a
/// C-Mail conversation is a `400`, so the client should not offer it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// A cIRC room.
    Circ,
    /// A C-Mail conversation. Not yet wired to a caller: C-Mail sends still go
    /// unchecked, and closing that is the natural follow-up to the cIRC gate.
    #[allow(dead_code)]
    Cmail,
}

/// Commands both surfaces accept.
const BASE: [&str; 12] = [
    "me", "poke", "hug", "hi5", "slap", "dice", "8ball", "fortune", "gif", "song", "help",
    // Website-only, but documented as posting as plain text rather than
    // failing, so it is never rejected here. See the module doc.
    "img",
];

/// Commands only a cIRC room accepts.
const CIRC_ONLY: [&str; 5] = ["art", "mute", "unmute", "muted", "unmuteall"];

/// The twelve text styles, each usable alone or chained with `+`.
const STYLES: [&str; 12] = [
    "blink", "l33t", "comic", "cursive", "times", "rainbow", "flip", "quiet", "slow", "glitch",
    "spoiler", "wave",
];

/// The one style that may not appear in a chain.
const UNCHAINABLE: &str = "spoiler";

/// Whether `draft` starts with something the server would read as a command.
///
/// A draft that does not begin with `/` is ordinary text and always sends.
#[must_use]
pub fn looks_like_command(draft: &str) -> bool {
    draft.trim_start().starts_with('/')
}

/// The command word of `draft`, `/` included, as the user typed it.
///
/// For the error message, so it echoes back what they actually wrote rather
/// than a normalized form.
#[must_use]
pub fn command_word(draft: &str) -> &str {
    draft.split_whitespace().next().unwrap_or_default()
}

/// Whether the server would recognize the command starting `draft`.
///
/// Returns `true` for a draft that is not a command at all, so the caller can
/// use this as a plain "may I send this" gate.
///
/// ```ignore
/// if !commands::is_known(&draft, Surface::Circ) {
///     // refuse locally rather than posting the typo to the room
/// }
/// ```
#[must_use]
pub fn is_known(draft: &str, surface: Surface) -> bool {
    if !looks_like_command(draft) {
        return true;
    }
    let word = command_word(draft);
    let Some(rest) = word.strip_prefix('/') else {
        return true;
    };
    // `/dice:4:20` carries its arguments in the command word; match the name.
    let name = rest.split(':').next().unwrap_or_default().to_lowercase();
    if name.is_empty() {
        return false;
    }
    if name.contains('+') {
        return is_style_chain(&name);
    }
    if STYLES.contains(&name.as_str()) || BASE.contains(&name.as_str()) {
        return true;
    }
    surface == Surface::Circ && CIRC_ONLY.contains(&name.as_str())
}

/// Whether a `+`-joined name is a legal style chain.
///
/// Every part has to be a style, and `spoiler` may not be one of them unless it
/// is the only one, which § Commands states as "every style combines except
/// `/spoiler`". Duplicates are rejected too: `/rainbow+rainbow` is not
/// something the website can produce, so it is far more likely a typo than
/// intent.
fn is_style_chain(name: &str) -> bool {
    let parts: Vec<&str> = name.split('+').collect();
    if parts.len() < 2 {
        return false;
    }
    if !parts.iter().all(|p| STYLES.contains(p)) {
        return false;
    }
    if parts.contains(&UNCHAINABLE) {
        return false;
    }
    let unique: HashSet<&&str> = parts.iter().collect();
    unique.len() == parts.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_never_treated_as_a_command() {
        for draft in ["hello", "  hello", "50/50 odds", ""] {
            assert!(!looks_like_command(draft), "{draft:?}");
            assert!(is_known(draft, Surface::Circ), "{draft:?}");
        }
    }

    #[test]
    fn every_command_in_the_spec_table_is_recognized() {
        for name in [
            "/me", "/poke", "/hug", "/hi5", "/slap", "/dice", "/8ball", "/fortune", "/gif",
            "/song", "/help",
        ] {
            assert!(
                is_known(&format!("{name} some args"), Surface::Circ),
                "{name} is in the spec's command table"
            );
            assert!(is_known(name, Surface::Cmail), "{name} works in C-Mail too");
        }
    }

    #[test]
    fn every_style_in_the_spec_table_is_recognized_alone() {
        for style in STYLES {
            assert!(
                is_known(&format!("/{style} hello"), Surface::Circ),
                "/{style} is a style the server accepts"
            );
        }
    }

    #[test]
    fn a_typo_is_refused_on_both_surfaces() {
        for draft in ["/dcie 2d6", "/halp", "/rainbo hi", "/"] {
            assert!(!is_known(draft, Surface::Circ), "{draft:?}");
            assert!(!is_known(draft, Surface::Cmail), "{draft:?}");
        }
    }

    #[test]
    fn the_circ_only_family_is_refused_in_cmail() {
        for name in [
            "/art",
            "/mute someone",
            "/unmute someone",
            "/muted",
            "/unmuteall",
        ] {
            assert!(is_known(name, Surface::Circ), "{name} is a cIRC command");
            assert!(
                !is_known(name, Surface::Cmail),
                "{name} is cIRC-only per the spec, and is a 400 in C-Mail"
            );
        }
    }

    #[test]
    fn img_is_known_because_the_spec_says_it_posts_as_plain_text() {
        // Rejecting this would block behavior the spec documents. It is
        // website-only, not invalid.
        assert!(is_known("/img https://example.com/a.png", Surface::Circ));
        assert!(is_known("/img https://example.com/a.png", Surface::Cmail));
    }

    #[test]
    fn dice_carries_its_arguments_in_the_command_word() {
        for draft in [
            "/dice",
            "/dice:6",
            "/dice:4:20",
            "/dice 4d6kh3",
            "/dice:6 extra",
        ] {
            assert!(
                is_known(draft, Surface::Circ),
                "{draft:?} is a documented /dice form"
            );
        }
    }

    #[test]
    fn styles_chain_with_plus() {
        assert!(is_known("/comic+rainbow hello", Surface::Circ));
        assert!(is_known("/blink+l33t+wave hi", Surface::Circ));
        assert!(
            !is_known("/comic+nonsense hi", Surface::Circ),
            "a chain is only as good as its worst part"
        );
    }

    #[test]
    fn spoiler_is_the_one_style_that_cannot_chain() {
        assert!(
            is_known("/spoiler hidden", Surface::Circ),
            "alone it is fine"
        );
        for draft in ["/spoiler+rainbow hi", "/rainbow+spoiler hi"] {
            assert!(
                !is_known(draft, Surface::Circ),
                "{draft:?}: the spec says every style combines except /spoiler"
            );
        }
    }

    #[test]
    fn a_repeated_style_in_a_chain_reads_as_a_typo() {
        assert!(!is_known("/rainbow+rainbow hi", Surface::Circ));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_known("/DICE:6", Surface::Circ));
        assert!(is_known("/Comic+RAINBOW hi", Surface::Circ));
    }

    #[test]
    fn the_error_echoes_back_what_was_typed() {
        assert_eq!(command_word("/dcie 2d6"), "/dcie");
        assert_eq!(command_word("  /Halp now"), "/Halp");
    }

    #[test]
    fn article_is_not_the_art_command() {
        // `/art` is a prefix of `/article`, and only whole-word matching keeps
        // a message that happens to start with a longer word from being read
        // as the command.
        assert!(!is_known("/article about rust", Surface::Circ));
    }
}

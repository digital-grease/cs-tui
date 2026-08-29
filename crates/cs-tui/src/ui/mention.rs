//! Completing an `@mention` in a chat composer.
//!
//! Pure text logic, kept apart from the screen so the awkward parts (where a
//! mention token starts, what counts as one, how to splice a completion back in)
//! can be tested without a terminal.
//!
//! **Indices here are character offsets, not byte offsets**, because that is
//! what [`super::circ::Composer`]'s caret is. Mixing the two would put the caret
//! inside a multi-byte character the first time anyone typed an accent.
//!
//! What is deliberately *not* here: which names are candidates. That is the
//! screen's business, since it depends on who is in the room and who has spoken.

/// A mention token in progress: where its `@` sits, and what has been typed
/// after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// Character index of the `@`.
    pub at: usize,
    /// The prefix typed after the `@`, which may be empty.
    pub prefix: String,
}

/// Find the mention token the caret is sitting in, if any.
///
/// A token starts at an `@` that begins the text or follows whitespace, so
/// `user@host` is an address rather than a mention of `host`. The caret has to
/// be inside the token, and the token may not contain whitespace.
///
/// Returns `None` when the caret is not in a mention, which is the common case
/// and the one that must stay cheap.
#[must_use]
pub fn query_at(text: &str, cursor: usize) -> Option<Query> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    // Walk back to the `@`, refusing to cross whitespace.
    let mut i = cursor;
    while i > 0 {
        let c = chars[i - 1];
        if c == '@' {
            // Must start the text or follow whitespace, or it is an address.
            let ok = i < 2 || chars[i - 2].is_whitespace();
            if !ok {
                return None;
            }
            let prefix: String = chars[i..cursor].iter().collect();
            return Some(Query { at: i - 1, prefix });
        }
        if c.is_whitespace() {
            return None;
        }
        i -= 1;
    }
    None
}

/// The candidates in `pool` that a mention `prefix` selects, order preserved.
///
/// An empty prefix offers everyone, which is what makes a bare `@` plus Tab a
/// way to browse the room. Matching is case-insensitive, since the API treats
/// `@username` that way for notifications.
#[must_use]
pub fn matches<'a>(pool: &'a [String], prefix: &str) -> Vec<&'a str> {
    let needle = prefix.to_lowercase();
    pool.iter()
        .filter(|name| name.to_lowercase().starts_with(&needle))
        .map(String::as_str)
        .collect()
}

/// The part of `candidate` still to be typed, given what already has been.
///
/// This is what the ghost shows. `None` when the candidate adds nothing, so a
/// fully typed name draws no ghost rather than an empty one.
#[must_use]
pub fn remainder(candidate: &str, prefix: &str) -> Option<String> {
    let rest: String = candidate.chars().skip(prefix.chars().count()).collect();
    (!rest.is_empty()).then_some(rest)
}

/// Replace the in-progress token with `candidate`, returning the new text and
/// where the caret lands.
///
/// The caret goes to the end of the inserted name, so typing continues after it
/// rather than inside it.
#[must_use]
pub fn splice(text: &str, query: &Query, cursor: usize, candidate: &str) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let head: String = chars[..query.at].iter().collect();
    let tail: String = chars[cursor..].iter().collect();
    let caret = query.at + 1 + candidate.chars().count();
    (format!("{head}@{candidate}{tail}"), caret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(at: usize, prefix: &str) -> Option<Query> {
        Some(Query {
            at,
            prefix: prefix.to_string(),
        })
    }

    #[test]
    fn a_mention_is_found_at_the_start_and_after_a_space() {
        assert_eq!(query_at("@ne", 3), q(0, "ne"));
        assert_eq!(query_at("hey @ne", 7), q(4, "ne"));
        assert_eq!(query_at("hey @", 5), q(4, ""), "a bare @ offers everyone");
    }

    #[test]
    fn an_email_address_is_not_a_mention() {
        // The reason the "start or after whitespace" rule exists at all.
        assert_eq!(query_at("you@example", 11), None);
        assert_eq!(query_at("a@b", 3), None);
    }

    #[test]
    fn a_caret_outside_the_token_finds_nothing() {
        assert_eq!(query_at("hey @neo there", 14), None, "past the token");
        assert_eq!(query_at("hey @neo", 3), None, "before the @");
        assert_eq!(query_at("plain text", 5), None, "no @ at all");
    }

    #[test]
    fn a_caret_inside_the_token_completes_from_what_precedes_it() {
        // Not the whole token: completing from text after the caret would
        // offer names that do not match what the user is looking at.
        assert_eq!(query_at("@neon", 3), q(0, "ne"));
    }

    #[test]
    fn matching_is_case_insensitive_and_keeps_pool_order() {
        let pool = vec![
            "Trinity".to_string(),
            "trace".to_string(),
            "neo".to_string(),
        ];
        assert_eq!(matches(&pool, "tr"), vec!["Trinity", "trace"]);
        assert_eq!(matches(&pool, "TR"), vec!["Trinity", "trace"]);
        assert_eq!(matches(&pool, ""), vec!["Trinity", "trace", "neo"]);
        assert!(matches(&pool, "zz").is_empty());
    }

    #[test]
    fn the_ghost_shows_only_what_is_left_to_type() {
        assert_eq!(remainder("trinity", "tr"), Some("inity".to_string()));
        assert_eq!(
            remainder("neo", "neo"),
            None,
            "a fully typed name draws no ghost rather than an empty one"
        );
    }

    #[test]
    fn the_ghost_respects_characters_not_bytes() {
        // A byte-based skip would slice this one mid-character and panic.
        assert_eq!(remainder("émile", "é"), Some("mile".to_string()));
    }

    #[test]
    fn splicing_replaces_the_token_and_leaves_the_caret_after_it() {
        let query = Query {
            at: 4,
            prefix: "tr".into(),
        };
        let (text, caret) = splice("hey tr", &query, 6, "trinity");
        assert_eq!(text, "hey @trinity");
        assert_eq!(caret, 12, "character index, not a byte offset");
    }

    #[test]
    fn splicing_keeps_whatever_followed_the_token() {
        let query = Query {
            at: 0,
            prefix: "ne".into(),
        };
        let (text, caret) = splice("@ne there", &query, 3, "neo");
        assert_eq!(text, "@neo there");
        assert_eq!(caret, 4, "the caret sits after the name, before the space");
    }

    #[test]
    fn splicing_a_multibyte_name_lands_the_caret_by_character() {
        let query = Query {
            at: 0,
            prefix: "é".into(),
        };
        let (text, caret) = splice("@é", &query, 2, "émile");
        assert_eq!(text, "@émile");
        assert_eq!(caret, 6, "six characters, not the eight bytes they occupy");
    }
}

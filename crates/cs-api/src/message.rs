//! Optional message fields shared by cIRC and C-Mail (API v0.8.4, § Message
//! fields under Commands).
//!
//! Both chat systems expand the same slash commands server-side, so both come
//! back with the same optional extras beyond `content`: an attachment, a text
//! style, the flags that say a message was an action, a dice roll, an 8-ball or
//! a fortune, and cIRC's deletion tombstone. They are modelled once here and
//! `#[serde(flatten)]`-ed into `CircMessage` and `CmailMessage`.
//!
//! Everything is optional. A plain text message carries none of it, and an RTDB
//! patch may carry a single field on its own, so no field may be required.
//!
//! Every field here is also null-tolerant, via
//! [`crate::types::null_as_default`] and the fallbacks below, because one
//! unexpected value must never cost more than the field it arrived on: a room's
//! history decodes as a single page, and an RTDB event decodes behind an
//! `.ok()?` that would drop the message without a word.
use serde::{Deserialize, Deserializer};

use crate::types::null_as_default;

/// A jukebox track attached with `/song` (§ Commands).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioAttachment {
    /// The track URL, a YouTube link in practice.
    #[serde(default, deserialize_with = "null_as_default")]
    pub src: String,

    /// Documented as `"youtube"`. Kept a `String` rather than an enum so a new
    /// origin cannot fail the decode of the whole message.
    #[serde(default, deserialize_with = "null_as_default")]
    pub origin: String,

    /// Performer, empty when the `/song` line did not name one.
    #[serde(default, deserialize_with = "null_as_default")]
    pub artist: String,

    /// Track title, empty when the `/song` line did not name one.
    #[serde(default, deserialize_with = "null_as_default")]
    pub title: String,

    /// Optional: the `/song` syntax makes the trailing genre field optional.
    #[serde(default)]
    pub genre: Option<String>,
}

/// The `style` field, which is either one style name or an array of them.
///
/// Styles chain with `+` (`/comic+rainbow hello`), and the server sends a
/// single name for a single style and an array for a chain, so both shapes have
/// to decode.
///
/// Style is presentational, so an unrecognized shape is worth strictly less
/// than the message carrying it. [`MessageStyle::Other`] is the same bargain
/// [`AudioAttachment::origin`] strikes by staying a `String`: a style the
/// client cannot read degrades to no styles at all rather than failing the
/// decode of the whole message, and of the page it came in.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum MessageStyle {
    /// A single style, e.g. `"rainbow"`.
    One(String),
    /// A chain of styles, e.g. `["comic", "rainbow"]`.
    Many(Vec<String>),
    /// Any other shape, e.g. `{"name": "rainbow"}`, kept verbatim.
    ///
    /// Reported as carrying no style names at all, so nothing downstream has to
    /// know it exists.
    Other(serde_json::Value),
}

/// Compared by value, including the raw JSON of a [`MessageStyle::Other`].
///
/// Written out rather than derived because `serde_json::Value` is only
/// `PartialEq`: its number arm holds an `f64`, which does not implement `Eq`.
/// JSON has no NaN (serde_json will neither parse nor build one), so equality
/// really is reflexive for any value that arrived over the wire. `Eq` is kept
/// because the message types that flatten these extras derive it.
impl Eq for MessageStyle {}

impl MessageStyle {
    /// Every style name on the message, whichever wire shape it arrived in.
    ///
    /// Empty for a shape this client does not recognize, which renders as
    /// unstyled text.
    #[must_use]
    pub fn names(&self) -> &[String] {
        match self {
            Self::One(name) => std::slice::from_ref(name),
            Self::Many(names) => names,
            Self::Other(_) => &[],
        }
    }

    /// Whether `name` is among the styles, compared case-insensitively.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.names().iter().any(|s| s.eq_ignore_ascii_case(name))
    }

    /// Whether this is ASCII art (`/art`).
    ///
    /// Worth its own accessor because `art` is the one style that changes how
    /// `content` should be read: the content is base64-encoded and has to be
    /// decoded before display. Every other style is presentational.
    #[must_use]
    pub fn is_art(&self) -> bool {
        self.contains("art")
    }
}

/// The optional extras a cIRC or C-Mail message may carry beyond `content`
/// (§ Message fields).
///
/// Safe to `#[serde(flatten)]` into a message type, and safe to decode on its
/// own from a partial object such as an RTDB delete patch
/// (`{ "content": "[DELETED]", "deleted": true }`), because every field
/// defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageExtras {
    /// An attached image. Posted from the website; render it alongside the text.
    #[serde(default)]
    pub image_url: Option<String>,

    /// An attached animated GIF (`/gif`).
    #[serde(default)]
    pub gif_url: Option<String>,

    /// A jukebox track (`/song`).
    #[serde(default, deserialize_with = "lenient_audio_attachment")]
    pub audio_attachment: Option<AudioAttachment>,

    /// A text style name, or several for a chained style.
    #[serde(default)]
    pub style: Option<MessageStyle>,

    /// The message is a third-person action (`/me` and the emotes),
    /// conventionally rendered as `* username content`.
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_action: bool,

    /// The action was a dice roll (`/dice`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_dice: bool,

    /// The action was an 8-ball question (`/8ball`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_eightball: bool,

    /// The 8-ball's answer on its own, for clients that want to highlight it.
    #[serde(default)]
    pub eightball_answer: Option<String>,

    /// The action was a fortune cookie (`/fortune`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_fortune: bool,

    /// The fortune text on its own, for clients that want to highlight it.
    #[serde(default)]
    pub fortune_text: Option<String>,

    /// cIRC only. The message was deleted by its author: `content` is
    /// `[DELETED]` and every other field above is gone. Render a tombstone
    /// rather than text.
    #[serde(default, deserialize_with = "null_as_default")]
    pub deleted: bool,
}

/// Decode `audioAttachment`, degrading to `None` on anything unexpected.
///
/// The track is an extra hung off the message, so, like an unreadable
/// [`MessageStyle`], it is worth less than the message it decorates. Absent,
/// `null`, not a JSON object, or an object the documented
/// `{ src, origin, artist, title, genre }` shape cannot absorb all mean the
/// same thing here: there is no track to play, and the message still renders.
///
/// The object check is deliberate. Serde will happily read a struct out of a
/// JSON array, so without it `"audioAttachment": []` would decode to an empty
/// track and put a blank jukebox card under the message.
fn lenient_audio_attachment<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<AudioAttachment>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw
        .filter(serde_json::Value::is_object)
        .and_then(|value| serde_json::from_value(value).ok()))
}

impl MessageExtras {
    /// Whether the message carries an image, a GIF or a track.
    #[must_use]
    pub fn has_attachment(&self) -> bool {
        self.image_url.is_some() || self.gif_url.is_some() || self.audio_attachment.is_some()
    }

    /// Whether `content` is base64-encoded ASCII art (`style: "art"`) and so
    /// has to be decoded before display.
    #[must_use]
    pub fn is_art(&self) -> bool {
        self.style.as_ref().is_some_and(MessageStyle::is_art)
    }

    /// The message text worth rendering, or `None` when there is nothing to
    /// print.
    ///
    /// Two rules from § Message fields, in one place so both chat screens and
    /// both message types apply them identically:
    ///
    /// - `content` may be empty, because an attachment can be the whole
    ///   message.
    /// - a message posted from the website with an attachment and no caption
    ///   sometimes repeats the attachment URL as its `content`, which should be
    ///   skipped rather than printed under the picture it already is.
    ///
    /// The text is returned unchanged (not trimmed) when it survives, since
    /// leading whitespace is meaningful for art.
    #[must_use]
    pub fn display_content<'a>(&self, content: &'a str) -> Option<&'a str> {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return None;
        }
        let duplicates_attachment = [self.image_url.as_deref(), self.gif_url.as_deref()]
            .into_iter()
            .flatten()
            .any(|url| url.trim() == trimmed);
        if duplicates_attachment {
            return None;
        }
        Some(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_message_has_no_extras() {
        let extras: MessageExtras =
            serde_json::from_str(r#"{"id":"m1","content":"hello","timestamp":1719700000000}"#)
                .unwrap();
        assert_eq!(extras, MessageExtras::default());
        assert!(!extras.has_attachment());
        assert!(!extras.is_art());
    }

    #[test]
    fn single_style_decodes() {
        let extras: MessageExtras =
            serde_json::from_str(r#"{"content":"hi","style":"rainbow"}"#).unwrap();
        let style = extras.style.expect("style");
        assert_eq!(style.names(), ["rainbow".to_string()].as_slice());
        assert!(style.contains("RAINBOW"), "matching is case-insensitive");
        assert!(!style.is_art());
    }

    #[test]
    fn chained_style_array_decodes() {
        let extras: MessageExtras =
            serde_json::from_str(r#"{"content":"hi","style":["comic","rainbow"]}"#).unwrap();
        let style = extras.style.expect("style");
        assert_eq!(
            style.names(),
            ["comic".to_string(), "rainbow".to_string()].as_slice()
        );
        assert!(style.contains("comic") && style.contains("rainbow"));
    }

    #[test]
    fn art_style_is_flagged_in_either_shape() {
        let single: MessageExtras = serde_json::from_str(r#"{"style":"art"}"#).unwrap();
        assert!(single.is_art());

        let chained: MessageExtras = serde_json::from_str(r#"{"style":["art","quiet"]}"#).unwrap();
        assert!(chained.is_art());

        let neither: MessageExtras = serde_json::from_str(r#"{"style":"l33t"}"#).unwrap();
        assert!(!neither.is_art());
    }

    #[test]
    fn audio_attachment_decodes_with_and_without_genre() {
        let extras: MessageExtras = serde_json::from_str(
            r#"{"content":"","audioAttachment":{"src":"https://www.youtube.com/watch?v=x","origin":"youtube","artist":"A","title":"T","genre":"electronic"}}"#,
        )
        .unwrap();
        assert!(extras.has_attachment());
        let audio = extras.audio_attachment.expect("audioAttachment");
        assert_eq!(audio.src, "https://www.youtube.com/watch?v=x");
        assert_eq!(audio.origin, "youtube");
        assert_eq!(audio.artist, "A");
        assert_eq!(audio.title, "T");
        assert_eq!(audio.genre.as_deref(), Some("electronic"));

        let no_genre: MessageExtras = serde_json::from_str(
            r#"{"audioAttachment":{"src":"https://youtu.be/y","origin":"youtube","artist":"A","title":"T"}}"#,
        )
        .unwrap();
        assert!(no_genre.audio_attachment.unwrap().genre.is_none());
    }

    #[test]
    fn unknown_audio_origin_still_decodes() {
        let extras: MessageExtras = serde_json::from_str(
            r#"{"audioAttachment":{"src":"https://x/y","origin":"bandcamp"}}"#,
        )
        .unwrap();
        assert_eq!(extras.audio_attachment.unwrap().origin, "bandcamp");
    }

    #[test]
    fn command_flags_decode() {
        let extras: MessageExtras = serde_json::from_str(
            r#"{"content":"* neo waves","isAction":true,"isDice":false,"isEightball":true,"eightballAnswer":"Ask again later","isFortune":true,"fortuneText":"You will ship it"}"#,
        )
        .unwrap();
        assert!(extras.is_action);
        assert!(!extras.is_dice);
        assert!(extras.is_eightball);
        assert_eq!(extras.eightball_answer.as_deref(), Some("Ask again later"));
        assert!(extras.is_fortune);
        assert_eq!(extras.fortune_text.as_deref(), Some("You will ship it"));
    }

    #[test]
    fn partial_rtdb_patch_decodes() {
        // A deletion arrives as a patch carrying only these two keys, so the
        // type has to decode from a partial object.
        let extras: MessageExtras =
            serde_json::from_str(r#"{"content":"[DELETED]","deleted":true}"#).unwrap();
        assert!(extras.deleted);
        assert!(extras.image_url.is_none());
        assert!(extras.style.is_none());
    }

    #[test]
    fn display_content_skips_a_duplicated_attachment_url() {
        let extras = MessageExtras {
            image_url: Some("https://cdn.example/pic.png".into()),
            ..MessageExtras::default()
        };
        assert_eq!(extras.display_content("https://cdn.example/pic.png"), None);
        // Whitespace around the caption doesn't hide the duplication.
        assert_eq!(
            extras.display_content("  https://cdn.example/pic.png "),
            None
        );
        // A real caption survives, unchanged.
        assert_eq!(extras.display_content("look at this"), Some("look at this"));

        let gif = MessageExtras {
            gif_url: Some("https://cdn.example/a.gif".into()),
            ..MessageExtras::default()
        };
        assert_eq!(gif.display_content("https://cdn.example/a.gif"), None);
        assert_eq!(
            gif.display_content("https://cdn.example/other.gif"),
            Some("https://cdn.example/other.gif")
        );
    }

    #[test]
    fn display_content_skips_empty_and_whitespace() {
        let extras = MessageExtras::default();
        assert_eq!(extras.display_content(""), None);
        assert_eq!(extras.display_content("   \n "), None);
        // Indentation is preserved for anything that does print, because for
        // art the leading spaces are the picture.
        assert_eq!(extras.display_content("  hi"), Some("  hi"));
    }

    #[test]
    fn explicit_null_flags_decode_as_false() {
        // Each of these used to fail with "invalid type: null, expected a
        // boolean" and take the whole message with it.
        let extras: MessageExtras = serde_json::from_str(
            r#"{"content":"hi","isAction":null,"isDice":null,"isEightball":null,"isFortune":null,"deleted":null}"#,
        )
        .unwrap();
        assert_eq!(extras, MessageExtras::default());
    }

    #[test]
    fn explicit_null_optional_fields_decode_as_none() {
        let extras: MessageExtras = serde_json::from_str(
            r#"{"content":"hi","imageUrl":null,"gifUrl":null,"audioAttachment":null,"style":null,"eightballAnswer":null,"fortuneText":null}"#,
        )
        .unwrap();
        assert_eq!(extras, MessageExtras::default());
        assert!(!extras.has_attachment());
    }

    #[test]
    fn explicit_nulls_inside_an_audio_attachment_decode_as_empty() {
        let extras: MessageExtras = serde_json::from_str(
            r#"{"audioAttachment":{"src":null,"origin":null,"artist":null,"title":"T","genre":null}}"#,
        )
        .unwrap();
        let audio = extras.audio_attachment.expect("audioAttachment");
        assert!(audio.src.is_empty());
        assert!(audio.origin.is_empty());
        assert!(audio.artist.is_empty());
        assert_eq!(audio.title, "T");
        assert!(audio.genre.is_none());
    }

    #[test]
    fn a_null_field_no_longer_sinks_the_rest_of_the_page() {
        // A room's history decodes as one Vec, so a single null used to cost
        // every message beside it.
        let page: Vec<MessageExtras> =
            serde_json::from_str(r#"[{"content":"first"},{"content":"second","isAction":null}]"#)
                .unwrap();
        assert_eq!(page.len(), 2);
        assert!(!page[1].is_action);
    }

    #[test]
    fn a_null_field_survives_being_flattened_into_a_message() {
        // MessageExtras is flattened into CircMessage and CmailMessage, and
        // serde buffers a flattened struct's fields before decoding them. This
        // stands in for those two so the null tolerance is proven on the shape
        // the messages actually use.
        #[derive(Debug, Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Message {
            id: String,
            #[serde(flatten)]
            extras: MessageExtras,
        }

        let page: Vec<Message> = serde_json::from_str(
            r#"[{"id":"m1","content":"first"},
                {"id":"m2","content":"second","isAction":null,"deleted":null,"style":{"name":"rainbow"},"audioAttachment":"https://youtu.be/x"}]"#,
        )
        .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[1].id, "m2");
        assert!(!page[1].extras.is_action);
        assert!(!page[1].extras.deleted);
        assert!(page[1].extras.style.as_ref().unwrap().names().is_empty());
        assert!(page[1].extras.audio_attachment.is_none());
    }

    #[test]
    fn unexpected_style_shape_degrades_to_no_styles() {
        // An object (or anything else that isn't a string or a list of them)
        // used to fail the decode of the entire message.
        let extras: MessageExtras =
            serde_json::from_str(r#"{"content":"hi","style":{"name":"rainbow"}}"#).unwrap();
        let style = extras.style.as_ref().expect("style");
        assert!(matches!(style, MessageStyle::Other(_)));
        assert!(style.names().is_empty());
        assert!(!style.contains("rainbow"));
        assert!(!style.is_art());
        assert!(!extras.is_art());
        assert_eq!(extras.display_content("hi"), Some("hi"));
    }

    #[test]
    fn unexpected_style_shapes_all_decode() {
        for raw in [
            r#"{"style":{"name":"rainbow"}}"#,
            r#"{"style":["comic",7]}"#,
            r#"{"style":42}"#,
            r#"{"style":true}"#,
        ] {
            let extras: MessageExtras = serde_json::from_str(raw).unwrap();
            assert!(
                extras.style.expect("style").names().is_empty(),
                "expected {raw} to decode as no styles"
            );
        }
    }

    #[test]
    fn malformed_audio_attachment_decodes_as_no_track() {
        // A bare URL where the object should be used to fail the whole message.
        let extras: MessageExtras =
            serde_json::from_str(r#"{"content":"hi","audioAttachment":"https://youtu.be/x"}"#)
                .unwrap();
        assert!(extras.audio_attachment.is_none());
        assert!(!extras.has_attachment());
        assert_eq!(extras.display_content("hi"), Some("hi"));

        for raw in [
            r#"{"audioAttachment":[]}"#,
            r#"{"audioAttachment":7}"#,
            r#"{"audioAttachment":{"src":[]}}"#,
        ] {
            let extras: MessageExtras = serde_json::from_str(raw).unwrap();
            assert!(
                extras.audio_attachment.is_none(),
                "expected {raw} to decode as no track"
            );
        }
    }
}

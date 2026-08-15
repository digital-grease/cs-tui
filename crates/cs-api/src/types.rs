//! Domain types matching the cyberspace.online API response shapes.
//!
//! Field names use Rust snake_case via serde `rename_all = "camelCase"`. The one
//! exception is `isNSFW`, which the API keeps fully uppercase.
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;

use crate::error::{ApiError, Result};

/// Max length of the optional `reason` on any flag request
/// (v0.8.4 § Content Limits).
pub(crate) const MAX_FLAG_REASON_LEN: usize = 500;

/// Decode a field that has a `Default`, treating an explicit JSON `null` as
/// that default.
///
/// Plain `#[serde(default)]` only fills in a *missing* key. A present-but-null
/// value still errors ("invalid type: null, expected a boolean"), and because
/// every list endpoint decodes a whole page in a single `Vec`, one null sinks
/// every item on the page rather than the one record that carried it. Over the
/// realtime database it is quieter and worse: the decode ends in `.ok()?`, so
/// the event is dropped without a trace.
///
/// The API does emit explicit nulls, v0.8.4 § Who's in a room documents
/// `lastActivity` as "ms epoch, or `null`", so nulls have to decode rather than
/// fail. This is the same treatment `deserialize_metadata` gives a
/// notification's `metadata`, generalized so every crate-owned type can share
/// it.
///
/// Pair it with `#[serde(default)]` on a field the server may also omit:
///
/// ```ignore
/// #[serde(default, deserialize_with = "crate::types::null_as_default")]
/// pub is_action: bool,
/// ```
///
/// Used on its own it leaves the field required when the key is absent while
/// still accepting an explicit null:
///
/// ```ignore
/// #[serde(deserialize_with = "crate::types::null_as_default")]
/// pub content: String,
/// ```
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A post (the spec calls these "entries").
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(deserialize_with = "null_as_default")]
    pub post_id: String,

    #[serde(deserialize_with = "null_as_default")]
    pub author_id: String,
    #[serde(deserialize_with = "null_as_default")]
    pub author_username: String,

    #[serde(deserialize_with = "null_as_default")]
    pub content: String,

    /// Optional free-form title (v0.3.7+, max 100 chars).
    #[serde(default)]
    pub title: Option<String>,

    /// Optional per-author URL slug (v0.3.7+, lowercase a-z0-9- max 60 chars).
    /// Server-derived from content if omitted on create.
    #[serde(default)]
    pub slug: Option<String>,

    #[serde(default, deserialize_with = "null_as_default")]
    pub topics: Vec<String>,

    #[serde(default, deserialize_with = "null_as_default")]
    pub replies_count: u32,

    #[serde(default, deserialize_with = "null_as_default")]
    pub bookmarks_count: u32,

    #[serde(default, deserialize_with = "null_as_default")]
    pub is_public: bool,

    /// Spec field is literally `isNSFW`; the rest are camelCase.
    #[serde(default, rename = "isNSFW", deserialize_with = "null_as_default")]
    pub is_nsfw: bool,

    #[serde(default, deserialize_with = "null_as_default")]
    pub attachments: Vec<Attachment>,

    /// RFC 3339. Some entries may be missing this in degenerate responses; we
    /// accept `None` rather than refusing to decode.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,

    /// RFC 3339. Present only once the entry has been edited (v0.8.4 § Edit
    /// Entry: "The entry then carries an `editedAt` timestamp"). The spec's
    /// entry example omits it, so absent decodes as `None`.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub edited_at: Option<OffsetDateTime>,

    #[serde(default, deserialize_with = "null_as_default")]
    pub deleted: bool,
}

/// A reply on a post. The spec doesn't publish the full response shape; this
/// mirrors the create-response field name (`replyId`) plus the obvious fields
/// from related endpoints.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    #[serde(deserialize_with = "null_as_default")]
    pub reply_id: String,
    #[serde(deserialize_with = "null_as_default")]
    pub post_id: String,

    #[serde(deserialize_with = "null_as_default")]
    pub author_id: String,
    #[serde(deserialize_with = "null_as_default")]
    pub author_username: String,

    #[serde(deserialize_with = "null_as_default")]
    pub content: String,

    /// Set when this reply is a reply-to-a-reply.
    #[serde(default)]
    pub parent_reply_id: Option<String>,

    #[serde(default, deserialize_with = "null_as_default")]
    pub attachments: Vec<Attachment>,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,

    /// RFC 3339. Present only once the reply has been edited (v0.8.4 § Edit
    /// Reply: "The reply then carries an `editedAt` timestamp").
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub edited_at: Option<OffsetDateTime>,

    #[serde(default, deserialize_with = "null_as_default")]
    pub deleted: bool,
}

/// Media attachment on an entry or reply.
///
/// The spec includes `attachments: []` in response examples but does not
/// publish the per-attachment schema. This shape mirrors what reference clients
/// observe in the wild, image dimensions and YouTube-style audio metadata.
/// Fields are tolerant of missing values so the type survives spec drift, and
/// so is the `type` tag itself: anything that is not one of the two known
/// shapes decodes to [`Attachment::Unknown`] instead of failing, which would
/// otherwise sink every entry on the page it arrived with.
#[derive(Debug, Clone, PartialEq)]
pub enum Attachment {
    /// An image, with its dimensions when the server reports them.
    Image {
        src: String,
        width: u32,
        height: u32,
    },
    /// A jukebox track, the same shape `/song` produces in chat.
    Audio {
        src: String,
        origin: String,
        artist: String,
        title: String,
        genre: String,
    },
    /// An attachment this client does not understand, held exactly as it
    /// arrived.
    ///
    /// v0.8.4 § Edit Entry puts attachments on the write path ("replaces the
    /// existing attachments. Send `[]` to remove them"), so the preserved JSON
    /// is re-emitted verbatim on serialization. An attachment type added to the
    /// API after this client shipped therefore survives an edit rather than
    /// being silently rewritten or dropped.
    Unknown(serde_json::Value),
}

/// Compared by value, including the preserved JSON of an
/// [`Attachment::Unknown`].
///
/// Written out rather than derived because `serde_json::Value` is only
/// `PartialEq`: its number arm holds an `f64`, which does not implement `Eq`.
/// JSON has no NaN (serde_json will neither parse nor build one), so equality
/// really is reflexive for any value that arrived over the wire.
impl Eq for Attachment {}

/// The attachment shapes this client understands, tried first when decoding an
/// [`Attachment`] and falling through to [`Attachment::Unknown`] on any
/// mismatch. Its variants and fields deliberately mirror `Attachment`'s own,
/// and have to be kept in step with them.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum KnownAttachment {
    Image {
        #[serde(deserialize_with = "null_as_default")]
        src: String,
        #[serde(default, deserialize_with = "null_as_default")]
        width: u32,
        #[serde(default, deserialize_with = "null_as_default")]
        height: u32,
    },
    Audio {
        #[serde(deserialize_with = "null_as_default")]
        src: String,
        #[serde(default, deserialize_with = "null_as_default")]
        origin: String,
        #[serde(default, deserialize_with = "null_as_default")]
        artist: String,
        #[serde(default, deserialize_with = "null_as_default")]
        title: String,
        #[serde(default, deserialize_with = "null_as_default")]
        genre: String,
    },
}

impl From<KnownAttachment> for Attachment {
    fn from(known: KnownAttachment) -> Self {
        match known {
            KnownAttachment::Image { src, width, height } => Self::Image { src, width, height },
            KnownAttachment::Audio {
                src,
                origin,
                artist,
                title,
                genre,
            } => Self::Audio {
                src,
                origin,
                artist,
                title,
                genre,
            },
        }
    }
}

impl<'de> Deserialize<'de> for Attachment {
    /// Decodes a known shape when it matches and keeps the raw JSON otherwise.
    ///
    /// Serde's internally tagged enums cannot express a data-carrying
    /// `#[serde(other)]` arm, so the value is buffered into a
    /// `serde_json::Value` first and the strict decode is attempted against
    /// that. Anything the strict decode rejects, an unknown `type`, a missing
    /// `type`, a missing `src`, or an attachment that is not an object at all,
    /// becomes [`Attachment::Unknown`].
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        match KnownAttachment::deserialize(&raw) {
            Ok(known) => Ok(known.into()),
            Err(_) => Ok(Self::Unknown(raw)),
        }
    }
}

impl Serialize for Attachment {
    /// Emits the internally tagged shape the API uses (`{"type": "image", ...}`)
    /// for the known variants, and the preserved JSON untouched for
    /// [`Attachment::Unknown`], so an unknown attachment round-trips through
    /// § Edit Entry unchanged.
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Image { src, width, height } => {
                let mut out = serializer.serialize_struct("Attachment", 4)?;
                out.serialize_field("type", "image")?;
                out.serialize_field("src", src)?;
                out.serialize_field("width", width)?;
                out.serialize_field("height", height)?;
                out.end()
            }
            Self::Audio {
                src,
                origin,
                artist,
                title,
                genre,
            } => {
                let mut out = serializer.serialize_struct("Attachment", 6)?;
                out.serialize_field("type", "audio")?;
                out.serialize_field("src", src)?;
                out.serialize_field("origin", origin)?;
                out.serialize_field("artist", artist)?;
                out.serialize_field("title", title)?;
                out.serialize_field("genre", genre)?;
                out.end()
            }
            Self::Unknown(raw) => raw.serialize(serializer),
        }
    }
}

/// Response from any of the three flag endpoints (v0.8.4 § Flag an Entry,
/// § Flag a Reply, § Flag a Message).
///
/// One type covers both outcomes. A fresh report is `201` with
/// `{ ..., flagId, flagged: true }`; reporting the same thing again is `200`
/// with `{ ..., flagged: true, alreadyFlagged: true }` and no `flagId`.
///
/// The resource id travels under a different key per endpoint (`postId`,
/// `replyId`, `messageId`) and is deliberately not captured here, since the
/// caller already knows what it flagged.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagResponse {
    /// True on both outcomes: the resource is now reported.
    #[serde(default, deserialize_with = "null_as_default")]
    pub flagged: bool,

    /// Set only when this report duplicated one you had already filed, in which
    /// case nothing new was filed.
    #[serde(default, deserialize_with = "null_as_default")]
    pub already_flagged: bool,

    /// The id of the report just filed. Absent on a repeat report.
    #[serde(default)]
    pub flag_id: Option<String>,
}

impl FlagResponse {
    /// Whether this call filed a new report, as opposed to landing on one the
    /// caller had already filed. Reporting is idempotent, so a repeat is a
    /// success, just a quieter one to tell the user about.
    #[must_use]
    pub fn is_new(&self) -> bool {
        !self.already_flagged
    }
}

/// Request body for any of the three flag endpoints, which take the same single
/// optional field.
///
/// With no reason this serializes to `{}` rather than being omitted: the spec
/// documents `reason` as an optional field *inside* a body, so an empty object
/// is the shape it describes.
#[derive(Debug, Serialize)]
pub(crate) struct FlagBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<&'a str>,
}

/// Length-check the optional `reason` on a flag request. Shared by the entry,
/// reply and cIRC-message flag endpoints, which all take the same field.
/// `None` (no reason given) is always fine, the field is optional.
pub(crate) fn validate_flag_reason(reason: Option<&str>) -> Result<()> {
    let Some(reason) = reason else {
        return Ok(());
    };
    if reason.chars().count() > MAX_FLAG_REASON_LEN {
        return Err(ApiError::Config(format!(
            "flag reason exceeds {MAX_FLAG_REASON_LEN} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_decodes_full_example_from_spec() {
        let raw = r#"{
            "postId": "abc123",
            "authorId": "uid",
            "authorUsername": "someone",
            "content": "markdown content",
            "topics": ["music", "linux"],
            "repliesCount": 5,
            "bookmarksCount": 2,
            "isPublic": false,
            "isNSFW": false,
            "attachments": [],
            "createdAt": "2026-03-27T10:12:01.516Z",
            "deleted": false
        }"#;
        let e: Entry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.post_id, "abc123");
        assert_eq!(e.author_username, "someone");
        assert_eq!(e.topics, vec!["music", "linux"]);
        assert_eq!(e.replies_count, 5);
        assert_eq!(e.bookmarks_count, 2);
        assert!(!e.is_public);
        assert!(!e.is_nsfw);
        assert!(e.attachments.is_empty());
        assert!(e.created_at.is_some());
        assert!(!e.deleted);
    }

    #[test]
    fn entry_tolerates_missing_optional_fields() {
        let raw = r#"{
            "postId": "p",
            "authorId": "a",
            "authorUsername": "u",
            "content": "c"
        }"#;
        let e: Entry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.post_id, "p");
        assert!(e.topics.is_empty());
        assert_eq!(e.replies_count, 0);
        assert!(e.created_at.is_none());
        assert!(!e.deleted);
    }

    #[test]
    fn entry_decodes_is_nsfw_uppercase_field() {
        // The spec uses "isNSFW" (all caps), unlike every other field which is camelCase.
        let raw =
            r#"{"postId":"p","authorId":"a","authorUsername":"u","content":"c","isNSFW":true}"#;
        let e: Entry = serde_json::from_str(raw).unwrap();
        assert!(e.is_nsfw);
    }

    #[test]
    fn reply_decodes_with_parent_reply_id() {
        let raw = r#"{
            "replyId": "r1",
            "postId": "p1",
            "authorId": "a",
            "authorUsername": "u",
            "content": "hi",
            "parentReplyId": "r0",
            "createdAt": "2026-03-27T10:12:01Z"
        }"#;
        let r: Reply = serde_json::from_str(raw).unwrap();
        assert_eq!(r.reply_id, "r1");
        assert_eq!(r.parent_reply_id.as_deref(), Some("r0"));
    }

    #[test]
    fn reply_top_level_has_no_parent() {
        let raw =
            r#"{"replyId":"r1","postId":"p","authorId":"a","authorUsername":"u","content":"x"}"#;
        let r: Reply = serde_json::from_str(raw).unwrap();
        assert!(r.parent_reply_id.is_none());
    }

    #[test]
    fn attachment_image_decodes() {
        let raw = r#"{"type":"image","src":"https://x/y.png","width":640,"height":480}"#;
        let a: Attachment = serde_json::from_str(raw).unwrap();
        match a {
            Attachment::Image { src, width, height } => {
                assert_eq!(src, "https://x/y.png");
                assert_eq!(width, 640);
                assert_eq!(height, 480);
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[test]
    fn attachment_audio_decodes() {
        let raw = r#"{"type":"audio","src":"https://www.youtube.com/watch?v=x","origin":"youtube","artist":"A","title":"T","genre":"electronic"}"#;
        let a: Attachment = serde_json::from_str(raw).unwrap();
        match a {
            Attachment::Audio {
                src,
                origin,
                artist,
                title,
                genre,
            } => {
                assert_eq!(src, "https://www.youtube.com/watch?v=x");
                assert_eq!(origin, "youtube");
                assert_eq!(artist, "A");
                assert_eq!(title, "T");
                assert_eq!(genre, "electronic");
            }
            other => panic!("expected Audio, got {other:?}"),
        }
    }

    #[test]
    fn entry_decodes_edited_at_when_present() {
        let raw = r#"{
            "postId": "p",
            "authorId": "a",
            "authorUsername": "u",
            "content": "c",
            "createdAt": "2026-03-27T10:12:01.516Z",
            "editedAt": "2026-03-27T10:15:44.000Z"
        }"#;
        let e: Entry = serde_json::from_str(raw).unwrap();
        assert!(e.edited_at.is_some());
        assert!(e.created_at.is_some());
    }

    #[test]
    fn entry_without_edited_at_decodes_as_none() {
        // The spec's own entry example carries no editedAt, so it has to be
        // tolerated as absent rather than required.
        let raw = r#"{"postId":"p","authorId":"a","authorUsername":"u","content":"c"}"#;
        let e: Entry = serde_json::from_str(raw).unwrap();
        assert!(e.edited_at.is_none());
    }

    #[test]
    fn reply_decodes_edited_at_present_and_absent() {
        let edited: Reply = serde_json::from_str(
            r#"{"replyId":"r","postId":"p","authorId":"a","authorUsername":"u","content":"c","editedAt":"2026-03-27T10:15:44Z"}"#,
        )
        .unwrap();
        assert!(edited.edited_at.is_some());

        let untouched: Reply = serde_json::from_str(
            r#"{"replyId":"r","postId":"p","authorId":"a","authorUsername":"u","content":"c"}"#,
        )
        .unwrap();
        assert!(untouched.edited_at.is_none());
    }

    #[test]
    fn flag_response_decodes_a_fresh_report() {
        let raw = r#"{"postId":"p1","flagId":"f1","flagged":true}"#;
        let r: FlagResponse = serde_json::from_str(raw).unwrap();
        assert!(r.flagged);
        assert!(!r.already_flagged);
        assert_eq!(r.flag_id.as_deref(), Some("f1"));
        assert!(r.is_new());
    }

    #[test]
    fn flag_response_decodes_a_repeat_report() {
        // The 200 body carries no flagId and adds alreadyFlagged.
        let raw = r#"{"replyId":"r1","flagged":true,"alreadyFlagged":true}"#;
        let r: FlagResponse = serde_json::from_str(raw).unwrap();
        assert!(r.flagged);
        assert!(r.already_flagged);
        assert!(r.flag_id.is_none());
        assert!(!r.is_new());
    }

    #[test]
    fn flag_response_decodes_the_circ_shape_too() {
        let raw = r#"{"roomId":"general","messageId":"m1","flagId":"f2","flagged":true}"#;
        let r: FlagResponse = serde_json::from_str(raw).unwrap();
        assert!(r.is_new());
        assert_eq!(r.flag_id.as_deref(), Some("f2"));
    }

    #[test]
    fn flag_reason_is_optional_and_length_capped() {
        assert!(validate_flag_reason(None).is_ok());
        assert!(validate_flag_reason(Some("spam")).is_ok());
        let long = "x".repeat(MAX_FLAG_REASON_LEN + 1);
        assert!(matches!(
            validate_flag_reason(Some(&long)),
            Err(ApiError::Config(_))
        ));
        let exact = "x".repeat(MAX_FLAG_REASON_LEN);
        assert!(validate_flag_reason(Some(&exact)).is_ok());
    }

    #[test]
    fn flag_body_omits_an_absent_reason() {
        // One shared body for all three flag endpoints, so this is asserted
        // once here rather than per module.
        assert_eq!(
            serde_json::to_string(&FlagBody { reason: None }).unwrap(),
            "{}"
        );
        assert_eq!(
            serde_json::to_string(&FlagBody {
                reason: Some("harassment")
            })
            .unwrap(),
            r#"{"reason":"harassment"}"#
        );
    }

    #[test]
    fn attachment_image_with_missing_dimensions_defaults_to_zero() {
        let raw = r#"{"type":"image","src":"https://x/y.png"}"#;
        let a: Attachment = serde_json::from_str(raw).unwrap();
        match a {
            Attachment::Image { width, height, .. } => {
                assert_eq!(width, 0);
                assert_eq!(height, 0);
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn explicit_nulls_decode_as_defaults_on_an_entry() {
        // Every one of these used to be an "invalid type: null" decode error,
        // and each took the whole response down with it.
        let raw = r#"{
            "postId": "p",
            "authorId": "a",
            "authorUsername": "u",
            "content": null,
            "topics": null,
            "repliesCount": null,
            "bookmarksCount": null,
            "isPublic": null,
            "isNSFW": null,
            "attachments": null,
            "createdAt": null,
            "editedAt": null,
            "deleted": null
        }"#;
        let e: Entry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.post_id, "p");
        assert!(e.content.is_empty());
        assert!(e.topics.is_empty());
        assert_eq!(e.replies_count, 0);
        assert_eq!(e.bookmarks_count, 0);
        assert!(!e.is_public);
        assert!(!e.is_nsfw);
        assert!(e.attachments.is_empty());
        assert!(e.created_at.is_none());
        assert!(e.edited_at.is_none());
        assert!(!e.deleted);
    }

    #[test]
    fn explicit_nulls_decode_as_defaults_on_a_reply_and_a_flag_response() {
        let r: Reply = serde_json::from_str(
            r#"{"replyId":"r","postId":"p","authorId":"a","authorUsername":null,"content":null,"attachments":null,"deleted":null}"#,
        )
        .unwrap();
        assert_eq!(r.reply_id, "r");
        assert!(r.author_username.is_empty());
        assert!(r.content.is_empty());
        assert!(r.attachments.is_empty());
        assert!(!r.deleted);

        let f: FlagResponse =
            serde_json::from_str(r#"{"postId":"p","flagged":null,"alreadyFlagged":null}"#).unwrap();
        assert!(!f.flagged);
        assert!(f.is_new());
    }

    #[test]
    fn a_null_field_no_longer_sinks_the_rest_of_the_page() {
        // A feed page decodes as one Vec, so a single bad record used to cost
        // every record beside it.
        let raw = r#"[
            {"postId":"p1","authorId":"a","authorUsername":"u","content":"first"},
            {"postId":"p2","authorId":"a","authorUsername":"u","content":"second","repliesCount":null}
        ]"#;
        let page: Vec<Entry> = serde_json::from_str(raw).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].content, "first");
        assert_eq!(page[1].content, "second");
        assert_eq!(page[1].replies_count, 0);
    }

    #[test]
    fn unknown_attachment_type_decodes_as_unknown() {
        let raw = r#"{"type":"video","src":"https://x/y.mp4","poster":"https://x/y.png"}"#;
        let a: Attachment = serde_json::from_str(raw).unwrap();
        let Attachment::Unknown(value) = &a else {
            panic!("expected Unknown, got {a:?}");
        };
        assert_eq!(value["type"], "video");
        assert_eq!(value["poster"], "https://x/y.png");
    }

    #[test]
    fn malformed_attachments_decode_as_unknown_rather_than_failing() {
        // No tag at all, a tag of the wrong JSON type, a known tag missing its
        // required src, and an attachment that isn't an object.
        for raw in [
            r#"{"src":"https://x/y.png"}"#,
            r#"{"type":7,"src":"https://x/y.png"}"#,
            r#"{"type":"image"}"#,
            r#""https://x/y.png""#,
        ] {
            let a: Attachment = serde_json::from_str(raw).unwrap();
            assert!(
                matches!(a, Attachment::Unknown(_)),
                "expected {raw} to decode as Unknown, got {a:?}"
            );
        }
    }

    #[test]
    fn unknown_attachment_round_trips_verbatim() {
        // § Edit Entry sends attachments back to the server, so an attachment
        // this client didn't understand has to come out exactly as it went in.
        let raw = r#"{"type":"video","src":"https://x/y.mp4","width":1920,"captions":[{"lang":"en","url":"https://x/y.vtt"}]}"#;
        let a: Attachment = serde_json::from_str(raw).unwrap();
        let re_encoded = serde_json::to_string(&a).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&re_encoded).unwrap(),
            serde_json::from_str::<serde_json::Value>(raw).unwrap()
        );
        // And decoding the re-encoded form gives back the same attachment.
        assert_eq!(serde_json::from_str::<Attachment>(&re_encoded).unwrap(), a);
    }

    #[test]
    fn known_attachments_round_trip_with_the_spec_field_names() {
        for raw in [
            r#"{"type":"image","src":"https://x/y.png","width":640,"height":480}"#,
            r#"{"type":"audio","src":"https://youtu.be/x","origin":"youtube","artist":"A","title":"T","genre":"electronic"}"#,
        ] {
            let a: Attachment = serde_json::from_str(raw).unwrap();
            let re_encoded = serde_json::to_string(&a).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&re_encoded).unwrap(),
                serde_json::from_str::<serde_json::Value>(raw).unwrap()
            );
            assert_eq!(serde_json::from_str::<Attachment>(&re_encoded).unwrap(), a);
        }
    }

    #[test]
    fn an_unknown_attachment_no_longer_sinks_the_rest_of_the_page() {
        let raw = r#"[
            {"postId":"p1","authorId":"a","authorUsername":"u","content":"first",
             "attachments":[{"type":"image","src":"https://x/y.png"}]},
            {"postId":"p2","authorId":"a","authorUsername":"u","content":"second",
             "attachments":[{"type":"video","src":"https://x/y.mp4"}]}
        ]"#;
        let page: Vec<Entry> = serde_json::from_str(raw).unwrap();
        assert_eq!(page.len(), 2);
        assert!(matches!(
            page[0].attachments.as_slice(),
            [Attachment::Image { .. }]
        ));
        assert!(matches!(
            page[1].attachments.as_slice(),
            [Attachment::Unknown(_)]
        ));
    }

    #[test]
    fn null_as_default_leaves_a_missing_required_field_required() {
        // deserialize_with on its own must not smuggle in a default for an
        // absent key; only an explicit null decodes as the default.
        let missing = serde_json::from_str::<Entry>(r#"{"authorId":"a","content":"c"}"#);
        assert!(missing.is_err(), "postId is still required when absent");
    }
}

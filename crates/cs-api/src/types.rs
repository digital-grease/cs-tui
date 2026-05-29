//! Domain types matching the cyberspace.online v0.3.6 response shapes.
//!
//! Field names use Rust snake_case via serde `rename_all = "camelCase"`. The one
//! exception is `isNSFW`, which the API keeps fully uppercase.
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A post (the spec calls these "entries").
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub post_id: String,

    pub author_id: String,
    pub author_username: String,

    pub content: String,

    /// Optional free-form title (v0.3.7+, max 100 chars).
    #[serde(default)]
    pub title: Option<String>,

    /// Optional per-author URL slug (v0.3.7+, lowercase a-z0-9- max 60 chars).
    /// Server-derived from content if omitted on create.
    #[serde(default)]
    pub slug: Option<String>,

    #[serde(default)]
    pub topics: Vec<String>,

    #[serde(default)]
    pub replies_count: u32,

    #[serde(default)]
    pub bookmarks_count: u32,

    #[serde(default)]
    pub is_public: bool,

    /// Spec field is literally `isNSFW`; the rest are camelCase.
    #[serde(default, rename = "isNSFW")]
    pub is_nsfw: bool,

    #[serde(default)]
    pub attachments: Vec<Attachment>,

    /// RFC 3339. Some entries may be missing this in degenerate responses; we
    /// accept `None` rather than refusing to decode.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,

    #[serde(default)]
    pub deleted: bool,
}

/// A reply on a post. The spec doesn't publish the full response shape; this
/// mirrors the create-response field name (`replyId`) plus the obvious fields
/// from related endpoints.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub reply_id: String,
    pub post_id: String,

    pub author_id: String,
    pub author_username: String,

    pub content: String,

    /// Set when this reply is a reply-to-a-reply.
    #[serde(default)]
    pub parent_reply_id: Option<String>,

    #[serde(default)]
    pub attachments: Vec<Attachment>,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,

    #[serde(default)]
    pub deleted: bool,
}

/// Media attachment on an entry or reply.
///
/// The v0.3.6 spec includes `attachments: []` in response examples but does not
/// publish the per-attachment schema. This shape mirrors what reference clients
/// observe in the wild — image dimensions and YouTube-style audio metadata.
/// Fields are tolerant of missing values so the type survives spec drift.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Attachment {
    Image {
        src: String,
        #[serde(default)]
        width: u32,
        #[serde(default)]
        height: u32,
    },
    Audio {
        src: String,
        #[serde(default)]
        origin: String,
        #[serde(default)]
        artist: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        genre: String,
    },
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
}

//! Entry (post) read and write endpoints (`/v1/posts`, API v0.8.4).
//!
//! Covers the feed, single-entry reads, slug resolution, create, edit, delete
//! and reporting. Editing (§ Edit Entry) is limited to supporters, within 5
//! minutes of publishing, on their own entries. The server owns both rules, so
//! this module sends the request and lets the `403` surface.
use reqwest::Method;
use serde::{Deserialize, Serialize, Serializer};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};
use crate::types::{validate_flag_reason, Attachment, Entry, FlagBody, FlagResponse};

const MAX_CONTENT_LEN: usize = 32_768;
const MAX_TOPICS: usize = 3;
const MAX_TITLE_LEN: usize = 100;
const MAX_SLUG_LEN: usize = 60;

/// Slugs the server reserves and will reject if submitted.
const RESERVED_SLUGS: &[&str] = &[
    "blog", "jukebox", "public", "replies", "index", "edit", "new", "admin",
];

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;

impl Client {
    /// `GET /v1/posts` — the home feed. Pass `None` for the first page; thread
    /// the returned cursor for subsequent pages. `limit` is clamped to 1–50
    /// (spec ceiling) with a default of 20.
    pub async fn list_entries(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Entry>, Option<String>)> {
        let limit = clamp_limit(limit);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.request_page(EndpointKey::EntriesList, Method::GET, "/v1/posts", &query)
            .await
    }

    /// `GET /v1/posts/:id` — fetch a single entry.
    pub async fn get_entry(&self, post_id: &str) -> Result<Entry> {
        let path = format!("/v1/posts/{post_id}");
        self.request::<Entry, ()>(EndpointKey::EntriesGet, Method::GET, &path, &[], None)
            .await
    }

    /// `POST /v1/posts` — create a new entry. Returns the created entry's id,
    /// final slug (server may suffix on collision), and any echo-back title.
    ///
    /// Rate limit: 2/min, 10/day.
    pub async fn create_entry(
        &self,
        content: &str,
        title: Option<&str>,
        slug: Option<&str>,
        topics: &[String],
        is_public: bool,
        is_nsfw: bool,
    ) -> Result<CreatedEntry> {
        validate_content_topics(content, topics)?;
        if let Some(t) = title {
            validate_title(t)?;
        }
        if let Some(s) = slug {
            validate_slug(s)?;
        }
        let body = CreateEntryBody {
            content,
            title,
            slug,
            topics,
            is_public,
            is_nsfw,
        };
        let r: CreateEntryResponse = self
            .request(
                EndpointKey::EntriesCreate,
                Method::POST,
                "/v1/posts",
                &[],
                Some(&body),
            )
            .await?;
        Ok(CreatedEntry {
            post_id: r.post_id,
            slug: r.slug,
            title: r.title,
        })
    }

    /// `PATCH /v1/posts/:id`, edit an entry you published (v0.8.4 § Edit
    /// Entry). Only the fields set on `edit` are sent, and only what is sent
    /// changes.
    ///
    /// Returns the echoed `postId`. The server does not return the updated
    /// entry, so re-fetch with [`get_entry`](Client::get_entry) when the UI
    /// needs the new `editedAt`. `createdAt` never changes and an edit sends no
    /// notifications.
    ///
    /// Editing is a supporter feature and only works within 5 minutes of
    /// publishing, on your own entry; outside that the server answers `403`.
    ///
    /// An empty `edit` is a `400` server-side, so it is rejected here before it
    /// costs a rate-limit token.
    ///
    /// Rate limit: 5/min, 30/day.
    pub async fn edit_entry(&self, post_id: &str, edit: &EntryEdit) -> Result<String> {
        if edit.is_empty() {
            return Err(ApiError::Config(
                "entry edit must change at least one field".into(),
            ));
        }
        if let Some(content) = &edit.content {
            validate_content(content)?;
        }
        if let Some(title) = &edit.title {
            validate_title(title.as_str())?;
        }
        if let Some(topics) = &edit.topics {
            validate_topics(topics)?;
        }
        let path = format!("/v1/posts/{post_id}");
        let r: EditEntryResponse = self
            .request(
                EndpointKey::EntriesEdit,
                Method::PATCH,
                &path,
                &[],
                Some(edit),
            )
            .await?;
        Ok(r.post_id)
    }

    /// `DELETE /v1/posts/:id` — soft-delete an entry. Only the author can.
    pub async fn delete_entry(&self, post_id: &str) -> Result<()> {
        let path = format!("/v1/posts/{post_id}");
        self.request_unit(EndpointKey::EntriesDelete, Method::DELETE, &path, &[])
            .await
    }

    /// `POST /v1/posts/:id/flag`, report an entry for review (v0.8.4 § Flag an
    /// Entry). `reason` is optional, max 500 characters.
    ///
    /// Reporting is idempotent: reporting the same entry again files nothing
    /// new and answers `200` with `alreadyFlagged`, which is a success and not
    /// an error. Branch on [`FlagResponse::is_new`] rather than on the status
    /// code, and retry freely. Reports cannot be withdrawn, and reporting your
    /// own entry is a `403`.
    ///
    /// Rate limit: 5/min, 20/hour, 50/day, one budget shared with
    /// [`flag_reply`](Client::flag_reply) and the cIRC message flag endpoint.
    pub async fn flag_entry(&self, post_id: &str, reason: Option<&str>) -> Result<FlagResponse> {
        validate_flag_reason(reason)?;
        let body = FlagBody { reason };
        let path = format!("/v1/posts/{post_id}/flag");
        self.request(EndpointKey::Flag, Method::POST, &path, &[], Some(&body))
            .await
    }

    /// `GET /v1/users/:username/posts/:slug` — resolve an entry by its
    /// per-author URL slug (v0.3.7+). Returns the same shape as `get_entry`;
    /// 404 if no entry matches that `(username, slug)` pair.
    pub async fn get_entry_by_slug(&self, username: &str, slug: &str) -> Result<Entry> {
        let path = format!("/v1/users/{username}/posts/{slug}");
        self.request::<Entry, ()>(
            EndpointKey::UsersGetPostBySlug,
            Method::GET,
            &path,
            &[],
            None,
        )
        .await
    }
}

/// Result of [`Client::create_entry`].
#[derive(Debug, Clone)]
pub struct CreatedEntry {
    pub post_id: String,
    /// The final stored slug — may differ from what was submitted (server
    /// appends `-2`, `-3`… on per-author collisions).
    pub slug: Option<String>,
    /// Echoed back only when a title was set.
    pub title: Option<String>,
}

/// What an edit does to an entry's title (v0.8.4 § Edit Entry).
///
/// Removing a title and leaving it alone are different operations: omitting
/// `title` keeps the current one, whereas sending `""` removes it. A plain
/// `Option<String>` cannot say both, so [`EntryEdit::title`] carries an
/// `Option<TitleEdit>`: `None` omits the field, `Some(TitleEdit::Remove)` sends
/// the empty string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleEdit {
    /// Replace the title with this text (max 100 characters).
    Set(String),
    /// Drop the entry's title. Serializes as `""`, which is what the server
    /// reads as a removal, not `null` and not an omitted field.
    Remove,
}

impl TitleEdit {
    /// The exact string this edit puts on the wire: the new title, or `""` for
    /// [`TitleEdit::Remove`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            TitleEdit::Set(title) => title,
            TitleEdit::Remove => "",
        }
    }

    /// Whether this edit removes the title rather than setting one. An empty
    /// [`TitleEdit::Set`] counts, since it puts the same `""` on the wire.
    #[must_use]
    pub fn is_remove(&self) -> bool {
        self.as_str().is_empty()
    }
}

impl Serialize for TitleEdit {
    fn serialize<S: Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

/// Partial edit body for [`Client::edit_entry`] (`PATCH /v1/posts/:id`,
/// v0.8.4 § Edit Entry).
///
/// Every field is optional and only what is sent changes, so build one from
/// [`Default`] and fill in just the fields the user touched. `None` leaves a
/// field alone; sending nothing at all is a `400`.
///
/// Two fields separate "leave it" from "clear it":
/// - `title`: `None` keeps the current title, `Some(TitleEdit::Remove)` sends
///   `""` and removes it.
/// - `attachments`: `None` keeps the current attachments, `Some(Vec::new())`
///   sends `[]` and removes them. A non-empty list replaces the whole set.
///
/// `topics` likewise replaces the existing list wholesale.
///
/// There is deliberately no `slug` field. The slug is frozen once an entry is
/// published so share links keep working, and sending one is a `400`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryEdit {
    /// Replacement content (markdown), max 32,768 characters. Blanking it is
    /// rejected client-side; an entry needs a body, use `delete_entry` instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Replacement title, or its removal. See [`TitleEdit`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<TitleEdit>,

    /// Replacement topic list, max 3, lowercase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topics: Option<Vec<String>>,

    /// Whether the entry is visible without login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_public: Option<bool>,

    /// Content-warning flag. The spec field is literally `isNSFW`, unlike every
    /// other camelCase field.
    #[serde(rename = "isNSFW", skip_serializing_if = "Option::is_none")]
    pub is_nsfw: Option<bool>,

    /// Replacement attachment list. `Some(Vec::new())` removes them all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
}

impl EntryEdit {
    /// Whether this edit would send no fields at all. The server answers `400`
    /// to an empty body, so [`Client::edit_entry`] rejects one up front rather
    /// than spending a rate-limit token on a certain failure.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.content.is_none()
            && self.title.is_none()
            && self.topics.is_none()
            && self.is_public.is_none()
            && self.is_nsfw.is_none()
            && self.attachments.is_none()
    }
}

/// Title length check, shared with guild-thread creation. An empty title is
/// accepted: on an edit it is how a title is removed (v0.8.4 § Edit Entry).
pub(crate) fn validate_title(title: &str) -> Result<()> {
    if title.chars().count() > MAX_TITLE_LEN {
        return Err(ApiError::Config(format!(
            "title exceeds {MAX_TITLE_LEN} characters"
        )));
    }
    Ok(())
}

pub(crate) fn validate_slug(s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(ApiError::Config("slug cannot be empty".into()));
    }
    if s.chars().count() > MAX_SLUG_LEN {
        return Err(ApiError::Config(format!(
            "slug exceeds {MAX_SLUG_LEN} characters"
        )));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ApiError::Config(
            "slug must be lowercase a-z, 0-9, or hyphen".into(),
        ));
    }
    if s.starts_with('_') {
        return Err(ApiError::Config(
            "slug cannot start with underscore (server-reserved)".into(),
        ));
    }
    if RESERVED_SLUGS.contains(&s) {
        return Err(ApiError::Config(format!(
            "slug {s:?} is reserved by the server"
        )));
    }
    Ok(())
}

/// Content check on its own, so an edit that only changes the content does not
/// have to invent a topic list to validate against.
pub(crate) fn validate_content(content: &str) -> Result<()> {
    if content.trim().is_empty() {
        return Err(ApiError::Config("content cannot be empty".into()));
    }
    if content.chars().count() > MAX_CONTENT_LEN {
        return Err(ApiError::Config(format!(
            "content exceeds {MAX_CONTENT_LEN} characters"
        )));
    }
    Ok(())
}

/// Topic count and charset check on its own, so an edit that only replaces the
/// topic list does not have to re-send the content to validate it.
pub(crate) fn validate_topics(topics: &[String]) -> Result<()> {
    if topics.len() > MAX_TOPICS {
        return Err(ApiError::Config(format!(
            "at most {MAX_TOPICS} topics allowed"
        )));
    }
    for t in topics {
        if t.chars()
            .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_')
        {
            return Err(ApiError::Config(format!(
                "topic {t:?} must be lowercase a-z, 0-9, or underscore"
            )));
        }
    }
    Ok(())
}

/// Both checks together, for the create paths that always send content and
/// topics as a pair.
pub(crate) fn validate_content_topics(content: &str, topics: &[String]) -> Result<()> {
    validate_content(content)?;
    validate_topics(topics)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateEntryBody<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slug: Option<&'a str>,
    topics: &'a [String],
    is_public: bool,
    #[serde(rename = "isNSFW")]
    is_nsfw: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateEntryResponse {
    post_id: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

/// `PATCH /v1/posts/:id` answers with the id alone, not the updated entry.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditEntryResponse {
    post_id: String,
}

fn clamp_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_limit_uses_default_when_absent() {
        assert_eq!(clamp_limit(None), DEFAULT_PAGE_LIMIT);
    }

    #[test]
    fn clamp_limit_enforces_ceiling() {
        assert_eq!(clamp_limit(Some(9999)), MAX_PAGE_LIMIT);
    }

    #[test]
    fn clamp_limit_enforces_floor() {
        assert_eq!(clamp_limit(Some(0)), 1);
    }

    #[test]
    fn clamp_limit_passes_valid_through() {
        assert_eq!(clamp_limit(Some(25)), 25);
    }

    #[test]
    fn create_entry_body_uses_spec_field_names() {
        let topics = vec!["music".to_string()];
        let body = CreateEntryBody {
            content: "hi",
            title: None,
            slug: None,
            topics: &topics,
            is_public: true,
            is_nsfw: true,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""content":"hi""#));
        assert!(s.contains(r#""topics":["music"]"#));
        assert!(s.contains(r#""isPublic":true"#));
        assert!(s.contains(r#""isNSFW":true"#));
        // Optional fields omitted when None.
        assert!(!s.contains(r#""title""#));
        assert!(!s.contains(r#""slug""#));
    }

    #[test]
    fn create_entry_body_includes_title_and_slug_when_set() {
        let body = CreateEntryBody {
            content: "hi",
            title: Some("My Title"),
            slug: Some("my-title"),
            topics: &[],
            is_public: false,
            is_nsfw: false,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""title":"My Title""#));
        assert!(s.contains(r#""slug":"my-title""#));
    }

    #[test]
    fn create_entry_response_decodes_with_optional_fields() {
        let r: CreateEntryResponse =
            serde_json::from_str(r#"{"postId":"p1","slug":"hello","title":"Hello"}"#).unwrap();
        assert_eq!(r.post_id, "p1");
        assert_eq!(r.slug.as_deref(), Some("hello"));
        assert_eq!(r.title.as_deref(), Some("Hello"));
    }

    #[test]
    fn create_entry_response_decodes_minimal() {
        let r: CreateEntryResponse = serde_json::from_str(r#"{"postId":"p1"}"#).unwrap();
        assert_eq!(r.post_id, "p1");
        assert!(r.slug.is_none());
        assert!(r.title.is_none());
    }

    #[test]
    fn validate_slug_accepts_lowercase_alnum_hyphen() {
        assert!(validate_slug("hello-world-2026").is_ok());
    }

    #[test]
    fn validate_slug_rejects_uppercase() {
        assert!(validate_slug("Hello").is_err());
    }

    #[test]
    fn validate_slug_rejects_underscore_prefix() {
        assert!(validate_slug("_internal").is_err());
    }

    #[test]
    fn validate_slug_rejects_reserved() {
        assert!(validate_slug("admin").is_err());
        assert!(validate_slug("new").is_err());
    }

    #[test]
    fn validate_slug_rejects_overlong() {
        let big = "x".repeat(MAX_SLUG_LEN + 1);
        assert!(validate_slug(&big).is_err());
    }

    #[test]
    fn entry_decodes_with_title_and_slug() {
        let raw = r#"{
            "postId": "abc",
            "authorId": "u",
            "authorUsername": "a",
            "content": "hi",
            "title": "Hello",
            "slug": "hello"
        }"#;
        let e: crate::Entry = serde_json::from_str(raw).unwrap();
        assert_eq!(e.title.as_deref(), Some("Hello"));
        assert_eq!(e.slug.as_deref(), Some("hello"));
    }

    #[test]
    fn validate_rejects_empty_content() {
        let r = validate_content_topics("   ", &[]);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_rejects_overlong_content() {
        let big = "x".repeat(MAX_CONTENT_LEN + 1);
        let r = validate_content_topics(&big, &[]);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_rejects_too_many_topics() {
        let topics = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let r = validate_content_topics("ok", &topics);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_rejects_uppercase_topic() {
        let topics = vec!["Music".into()];
        let r = validate_content_topics("ok", &topics);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_accepts_lowercase_underscore_topic() {
        let topics = vec!["retro_music".into(), "linux".into(), "2026".into()];
        assert!(validate_content_topics("ok", &topics).is_ok());
    }

    #[test]
    fn split_validators_check_one_field_each() {
        // An edit validates only what it is actually sending.
        assert!(validate_content("ok").is_ok());
        assert!(matches!(validate_content("   "), Err(ApiError::Config(_))));
        let big = "x".repeat(MAX_CONTENT_LEN + 1);
        assert!(matches!(validate_content(&big), Err(ApiError::Config(_))));

        assert!(validate_topics(&[]).is_ok());
        assert!(validate_topics(&["music".to_string()]).is_ok());
        let too_many: Vec<String> = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        assert!(matches!(
            validate_topics(&too_many),
            Err(ApiError::Config(_))
        ));
        let shouty: Vec<String> = vec!["Music".into()];
        assert!(matches!(validate_topics(&shouty), Err(ApiError::Config(_))));
    }

    #[test]
    fn empty_title_validates_because_it_removes_the_title() {
        assert!(validate_title("").is_ok());
    }

    #[test]
    fn title_edit_reports_what_it_puts_on_the_wire() {
        assert_eq!(TitleEdit::Set("Hello".into()).as_str(), "Hello");
        assert_eq!(TitleEdit::Remove.as_str(), "");
        assert!(TitleEdit::Remove.is_remove());
        assert!(TitleEdit::Set(String::new()).is_remove());
        assert!(!TitleEdit::Set("Hello".into()).is_remove());
    }

    #[test]
    fn entry_edit_default_sends_nothing_and_is_empty() {
        let edit = EntryEdit::default();
        assert!(edit.is_empty());
        assert_eq!(serde_json::to_string(&edit).unwrap(), "{}");
    }

    #[test]
    fn entry_edit_sends_only_the_fields_that_are_set() {
        let edit = EntryEdit {
            content: Some("corrected".into()),
            ..Default::default()
        };
        assert!(!edit.is_empty());
        assert_eq!(
            serde_json::to_string(&edit).unwrap(),
            r#"{"content":"corrected"}"#
        );
    }

    #[test]
    fn entry_edit_serializes_every_field_with_spec_names() {
        let edit = EntryEdit {
            content: Some("body".into()),
            title: Some(TitleEdit::Set("New Title".into())),
            topics: Some(vec!["music".into()]),
            is_public: Some(true),
            is_nsfw: Some(false),
            attachments: Some(vec![Attachment::Image {
                src: "https://x/y.png".into(),
                width: 640,
                height: 480,
            }]),
        };
        let v: serde_json::Value = serde_json::to_value(&edit).unwrap();
        assert_eq!(v["content"], "body");
        assert_eq!(v["title"], "New Title");
        assert_eq!(v["topics"][0], "music");
        assert_eq!(v["isPublic"], true);
        assert_eq!(v["isNSFW"], false);
        assert_eq!(v["attachments"][0]["type"], "image");
        assert_eq!(v["attachments"][0]["src"], "https://x/y.png");
    }

    #[test]
    fn entry_edit_distinguishes_removing_a_title_from_leaving_it() {
        // Removal is the empty string, never null and never an omitted field.
        let remove = EntryEdit {
            title: Some(TitleEdit::Remove),
            ..Default::default()
        };
        assert_eq!(serde_json::to_string(&remove).unwrap(), r#"{"title":""}"#);
        assert!(!remove.is_empty());

        let leave_alone = EntryEdit {
            content: Some("c".into()),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&leave_alone).unwrap();
        assert!(!v.as_object().unwrap().contains_key("title"));
    }

    #[test]
    fn entry_edit_distinguishes_clearing_attachments_from_leaving_them() {
        let clear = EntryEdit {
            attachments: Some(Vec::new()),
            ..Default::default()
        };
        assert!(!clear.is_empty());
        assert_eq!(
            serde_json::to_string(&clear).unwrap(),
            r#"{"attachments":[]}"#
        );

        let leave_alone = EntryEdit {
            content: Some("c".into()),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&leave_alone).unwrap();
        assert!(!v.as_object().unwrap().contains_key("attachments"));
    }

    #[test]
    fn entry_edit_never_sends_a_slug() {
        // The slug is frozen once published and sending one is a 400, so the
        // body type has no field for it at all.
        let edit = EntryEdit {
            content: Some("c".into()),
            title: Some(TitleEdit::Set("t".into())),
            topics: Some(vec!["music".into()]),
            is_public: Some(true),
            is_nsfw: Some(true),
            attachments: Some(Vec::new()),
        };
        let v: serde_json::Value = serde_json::to_value(&edit).unwrap();
        assert!(!v.as_object().unwrap().contains_key("slug"));
    }

    #[test]
    fn edit_entry_response_decodes() {
        let r: EditEntryResponse = serde_json::from_str(r#"{"postId":"p1"}"#).unwrap();
        assert_eq!(r.post_id, "p1");
    }

    #[test]
    fn flag_body_omits_an_absent_reason() {
        let s = serde_json::to_string(&FlagBody { reason: None }).unwrap();
        assert_eq!(s, "{}");
    }

    #[test]
    fn flag_body_sends_a_reason_when_given() {
        let s = serde_json::to_string(&FlagBody {
            reason: Some("spam"),
        })
        .unwrap();
        assert_eq!(s, r#"{"reason":"spam"}"#);
    }

    #[test]
    fn entry_flag_response_decodes_both_outcomes() {
        let fresh: FlagResponse =
            serde_json::from_str(r#"{"postId":"p1","flagId":"f1","flagged":true}"#).unwrap();
        assert!(fresh.flagged);
        assert!(fresh.is_new());
        assert_eq!(fresh.flag_id.as_deref(), Some("f1"));

        // A repeat report is a success, just a quieter one: 200, no flagId.
        let repeat: FlagResponse =
            serde_json::from_str(r#"{"postId":"p1","flagged":true,"alreadyFlagged":true}"#)
                .unwrap();
        assert!(repeat.flagged);
        assert!(!repeat.is_new());
        assert!(repeat.flag_id.is_none());
    }

    #[test]
    fn flag_reason_length_is_capped_before_sending() {
        assert!(validate_flag_reason(None).is_ok());
        let long = "x".repeat(501);
        assert!(matches!(
            validate_flag_reason(Some(&long)),
            Err(ApiError::Config(_))
        ));
    }
}

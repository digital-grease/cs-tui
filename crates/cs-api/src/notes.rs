//! Private notes (journal) endpoints (`/v1/notes/*`).
//!
//! Notes are private to the author. Editing creates a new revision; the API
//! returns the latest revision by default and exposes a per-note history.
use reqwest::Method;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 50;
const MAX_CONTENT_LEN: usize = 32_768;
const MAX_TOPICS: usize = 3;

/// A private note. `revision_number` is the latest committed revision; use
/// `Client::get_note_revision` to retrieve an earlier one.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    #[serde(alias = "id")]
    pub note_id: String,

    #[serde(default)]
    pub author_id: String,

    pub content: String,

    #[serde(default)]
    pub topics: Vec<String>,

    #[serde(default)]
    pub revision_number: u32,

    #[serde(default)]
    pub deleted: bool,

    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
}

/// A specific historical revision of a note.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRevision {
    #[serde(default)]
    pub revision_number: u32,
    pub content: String,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
}

impl Client {
    /// `GET /v1/notes` — list your notes (latest revision of each), newest first.
    pub async fn list_notes(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Note>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        self.request_page(EndpointKey::NotesList, Method::GET, "/v1/notes", &query)
            .await
    }

    /// `GET /v1/notes/:id` — fetch the latest revision of a note.
    pub async fn get_note(&self, note_id: &str) -> Result<Note> {
        let path = format!("/v1/notes/{note_id}");
        self.request::<Note, ()>(EndpointKey::NotesGet, Method::GET, &path, &[], None)
            .await
    }

    /// `GET /v1/notes/:id?revision=N` — fetch a specific revision.
    pub async fn get_note_revision(&self, note_id: &str, revision: u32) -> Result<Note> {
        let path = format!("/v1/notes/{note_id}");
        let query: Vec<(&str, String)> = vec![("revision", revision.to_string())];
        self.request::<Note, ()>(
            EndpointKey::NotesGetRevision,
            Method::GET,
            &path,
            &query,
            None,
        )
        .await
    }

    /// `GET /v1/notes/:id/revisions` — list all historical revisions of a note,
    /// newest first by revision number.
    pub async fn list_note_revisions(
        &self,
        note_id: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<NoteRevision>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let path = format!("/v1/notes/{note_id}/revisions");
        self.request_page(EndpointKey::NotesListRevisions, Method::GET, &path, &query)
            .await
    }

    /// `POST /v1/notes` — create a new note. Returns the new `noteId`.
    /// Rate limit: 3/min, 20/day.
    pub async fn create_note(&self, content: &str, topics: &[String]) -> Result<String> {
        validate_note_input(content, topics)?;
        let body = NoteBody { content, topics };
        let r: CreateNoteResponse = self
            .request(
                EndpointKey::NotesCreate,
                Method::POST,
                "/v1/notes",
                &[],
                Some(&body),
            )
            .await?;
        Ok(r.note_id)
    }

    /// `PATCH /v1/notes/:id` — update an existing note. Creates a new revision;
    /// the previous content remains accessible via the revisions endpoint.
    pub async fn update_note(&self, note_id: &str, content: &str, topics: &[String]) -> Result<()> {
        validate_note_input(content, topics)?;
        let body = NoteBody { content, topics };
        let path = format!("/v1/notes/{note_id}");
        // The spec doesn't document the response shape; ignore it.
        let _: serde_json::Value = self
            .request(
                EndpointKey::NotesUpdate,
                Method::PATCH,
                &path,
                &[],
                Some(&body),
            )
            .await?;
        Ok(())
    }

    /// `DELETE /v1/notes/:id` — soft-delete all revisions of a note.
    pub async fn delete_note(&self, note_id: &str) -> Result<()> {
        let path = format!("/v1/notes/{note_id}");
        self.request_unit(EndpointKey::NotesDelete, Method::DELETE, &path, &[])
            .await
    }
}

fn validate_note_input(content: &str, topics: &[String]) -> Result<()> {
    if content.trim().is_empty() {
        return Err(ApiError::Config("note content cannot be empty".into()));
    }
    if content.chars().count() > MAX_CONTENT_LEN {
        return Err(ApiError::Config(format!(
            "note content exceeds {MAX_CONTENT_LEN} characters"
        )));
    }
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

#[derive(Debug, Serialize)]
struct NoteBody<'a> {
    content: &'a str,
    topics: &'a [String],
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateNoteResponse {
    note_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_decodes_with_revision() {
        let raw = r#"{
            "noteId": "n1",
            "authorId": "u1",
            "content": "private thoughts",
            "topics": ["journal"],
            "revisionNumber": 3,
            "deleted": false,
            "createdAt": "2026-03-27T10:12:01Z"
        }"#;
        let n: Note = serde_json::from_str(raw).unwrap();
        assert_eq!(n.note_id, "n1");
        assert_eq!(n.content, "private thoughts");
        assert_eq!(n.topics, vec!["journal"]);
        assert_eq!(n.revision_number, 3);
    }

    #[test]
    fn note_accepts_id_alias() {
        let raw = r#"{"id":"n1","content":"hi"}"#;
        let n: Note = serde_json::from_str(raw).unwrap();
        assert_eq!(n.note_id, "n1");
        assert_eq!(n.content, "hi");
    }

    #[test]
    fn note_revision_decodes() {
        let raw = r#"{
            "revisionNumber": 2,
            "content": "edit",
            "topics": ["journal"],
            "createdAt": "2026-03-27T10:12:01Z"
        }"#;
        let r: NoteRevision = serde_json::from_str(raw).unwrap();
        assert_eq!(r.revision_number, 2);
    }

    #[test]
    fn note_body_serializes_fields() {
        let topics: Vec<String> = vec!["journal".into()];
        let body = NoteBody {
            content: "hi",
            topics: &topics,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains(r#""content":"hi""#));
        assert!(s.contains(r#""topics":["journal"]"#));
    }

    #[test]
    fn create_note_response_decodes() {
        let r: CreateNoteResponse = serde_json::from_str(r#"{"noteId":"n1"}"#).unwrap();
        assert_eq!(r.note_id, "n1");
    }

    #[test]
    fn validate_rejects_empty_content() {
        let r = validate_note_input("  ", &[]);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }

    #[test]
    fn validate_rejects_too_many_topics() {
        let topics = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let r = validate_note_input("ok", &topics);
        assert!(matches!(r, Err(ApiError::Config(_))));
    }
}

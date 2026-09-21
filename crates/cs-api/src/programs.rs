//! The program registry (`/v1/programs`, API v0.8.10 § Programs).
//!
//! A program is a file a member wrote on one of Cyberspace's terminals. The
//! gallery lists every published one, the source of a published program is
//! readable by any member, and versions are machine-assigned with an
//! append-only release history.
//!
//! Two machines share the registry and run different formats, so every program
//! declares a [`Runtime`]. cs-tui runs none of them: it is a client for reading
//! the gallery, reading source, and publishing or recalling your own. Reading
//! source before running it is the spec's own advice (§ Read the Source,
//! "publishing is open to every member"), and it is the only thing this client
//! can help with, since it cannot execute what it fetches.
//!
//! Size and count ceilings are tier-dependent (members 20 programs of 128 KB,
//! supporters 100 of 1 MB, staff unlimited and 10 MB) and checked against the
//! *decoded* bytes. The client knows its tier only from `isSupporter` on the
//! profile, which says nothing about staff, so the ceilings stay server-owned:
//! [`Client::publish_program`] validates the rules the spec states flatly (name
//! charset and length, description length, the wasm magic number) and lets a
//! `413`/`403` surface for the rest.
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};

/// Default `limit` for the gallery (§ Browse the Gallery).
const DEFAULT_PAGE_LIMIT: u32 = 30;
/// Ceiling on `limit` (§ Browse the Gallery: "`limit` is 1-50").
const MAX_PAGE_LIMIT: u32 = 50;

/// Max program name length (§ Publish: "letters, digits, `. _ -`, max 32
/// chars").
pub const MAX_PROGRAM_NAME_LEN: usize = 32;
/// Max program description length (§ Publish: "required, max 256 characters").
pub const MAX_PROGRAM_DESCRIPTION_LEN: usize = 256;

/// The four bytes every WebAssembly module starts with (`\0asm`). § Publish
/// requires the decoded bytes of a `wasm` program to begin with them.
const WASM_MAGIC: &[u8] = b"\0asm";

/// Which machine a program runs on, and therefore what shape its source takes
/// (§ Programs).
///
/// A program with no `runtime` field is [`Runtime::Web`]: everything published
/// before the field existed came from the website. That is a documented default
/// rather than a guess, so it is applied on decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// `export default { name, description, run(ctx, args) }`, run by the
    /// website's `/terminal`.
    #[default]
    Web,
    /// `export default async (p) => number`, run by the terminal machine.
    Term,
    /// A wasm32-wasi binary, stdio only, run by the terminal machine.
    Wasm,
    /// A runtime added after this client shipped. Carried through so a listing
    /// still decodes, but never something to publish under.
    #[serde(other)]
    Unknown,
}

impl Runtime {
    /// The wire value, for the `?runtime=` filter and the publish body.
    ///
    /// [`Runtime::Unknown`] has no wire value of its own — it stands for
    /// "something this build has not heard of" — and answers `None` so a caller
    /// cannot invent one.
    #[must_use]
    pub fn wire(self) -> Option<&'static str> {
        match self {
            Self::Web => Some("web"),
            Self::Term => Some("term"),
            Self::Wasm => Some("wasm"),
            Self::Unknown => None,
        }
    }

    /// Whether this runtime's source is base64 rather than text (§ Read the
    /// Source: "`encoding` is `utf8` for `web` and `term`, and `base64` for
    /// `wasm`").
    #[must_use]
    pub fn is_binary(self) -> bool {
        matches!(self, Self::Wasm)
    }
}

/// One row of the gallery (§ Browse the Gallery).
///
/// `is_published`, `taken_down` and `hash` are only sent for your own programs
/// (`?mine=1`); on a public listing they decode to their defaults. Read them
/// through [`Program::is_draft`] and friends rather than testing the raw fields,
/// which cannot tell "not yours" from "not published".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner_username: String,
    #[serde(default)]
    pub description: String,

    /// Absent means [`Runtime::Web`] (§ Programs), which is what `Default` gives.
    #[serde(default)]
    pub runtime: Runtime,

    /// The current release number. Machine-assigned and append-only.
    #[serde(default)]
    pub release: u32,

    /// Milliseconds since the Unix epoch, like every other program timestamp.
    #[serde(default)]
    pub published_at: Option<i64>,

    /// `?mine=1` only: whether the program is currently in the gallery.
    #[serde(default)]
    pub is_published: Option<bool>,

    /// `?mine=1` only: whether a moderator took the program down. A taken-down
    /// program cannot be purged (§ Recall).
    #[serde(default)]
    pub taken_down: Option<bool>,

    /// `?mine=1` only: SHA-256 of the source the current release was published
    /// from (§ Browse the Gallery), so a caller holding the working copy can
    /// tell an edited one from a clean one without fetching the release back.
    ///
    /// Decoded and handed on; this crate does not compare it. cs-tui has no
    /// working copy to compare against — it publishes from a path the user
    /// names and then forgets it — so the comparison, and the SHA-256
    /// implementation it needed, lived here with no caller.
    #[serde(default)]
    pub hash: Option<String>,
}

impl Program {
    /// Whether this is one of your own programs that is not in the gallery —
    /// never published, or recalled.
    ///
    /// `false` for any row from a public listing, which does not carry
    /// `isPublished` at all: a program you can see in the gallery is published
    /// by definition.
    #[must_use]
    pub fn is_draft(&self) -> bool {
        self.is_published == Some(false)
    }

    /// Whether a moderator has taken this program down.
    #[must_use]
    pub fn is_taken_down(&self) -> bool {
        self.taken_down == Some(true)
    }
}

/// A program's source at one release (§ Read the Source).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramSource {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner_username: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub runtime: Runtime,
    #[serde(default)]
    pub release: u32,

    /// `utf8` or `base64` (§ Read the Source). Kept as sent rather than as an
    /// enum: it decides how `source` is read, so an unrecognised value has to
    /// be visible rather than silently folded into one of the two.
    #[serde(default)]
    pub encoding: String,

    /// The source as it arrived — text for `utf8`, base64 for `base64`. Go
    /// through [`ProgramSource::bytes`] or [`ProgramSource::text`] instead of
    /// reading this directly.
    #[serde(default)]
    pub source: String,
}

impl ProgramSource {
    /// Whether `source` needs base64-decoding before it means anything.
    ///
    /// Reads the server's `encoding` when it sent one, and falls back to the
    /// runtime, which documents the same thing (§ Read the Source). The two
    /// agree in every documented case; trusting `encoding` first means a
    /// `base64` payload under an unrecognised runtime still decodes.
    #[must_use]
    pub fn is_base64(&self) -> bool {
        match self.encoding.trim().to_ascii_lowercase().as_str() {
            "base64" => true,
            "utf8" | "utf-8" => false,
            _ => self.runtime.is_binary(),
        }
    }

    /// The source as bytes, base64-decoded when it needs to be.
    ///
    /// This is what to write to a file: a `wasm` program's `source` is base64
    /// and writing it verbatim produces a file that is not a wasm module.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        if !self.is_base64() {
            return Ok(self.source.clone().into_bytes());
        }
        // Whitespace is not part of the payload but a server or an intermediary
        // is free to wrap long base64, so strip it rather than failing on it.
        let packed: String = self.source.split_whitespace().collect();
        BASE64
            .decode(packed.as_bytes())
            .map_err(|e| decode_error(format!("program source is not valid base64: {e}")))
    }

    /// The source as text, for a program a human can read.
    ///
    /// `None` for a binary program: a wasm module has no text form, and a
    /// caller that wants to show something should say so rather than render
    /// mojibake. Also `None` when a `utf8` program somehow fails to be UTF-8,
    /// which a `String` on the wire cannot be but a base64 payload can decode
    /// into.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        if !self.is_base64() {
            return Some(self.source.clone());
        }
        if self.runtime.is_binary() {
            return None;
        }
        String::from_utf8(self.bytes().ok()?).ok()
    }
}

/// What a listing asks the gallery for (§ Browse the Gallery).
///
/// Built from [`Default`], which is the plain newest-first gallery. The three
/// documented forms are selective in different ways and the spec presents them
/// separately, so they are set separately here too rather than being squeezed
/// into one enum a caller would have to read the spec to use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramQuery {
    /// Keep only these runtimes. Empty asks for every kind.
    ///
    /// A filtered page can come back shorter than `limit`, or empty, and still
    /// have a cursor — follow it until it is null rather than stopping at the
    /// first short page.
    pub runtimes: Vec<Runtime>,

    /// Your own programs, drafts and recalled ones included, with
    /// `is_published`, `taken_down` and `hash` filled in.
    pub mine: bool,

    /// Look one program up by its author and name. Both halves are needed; one
    /// on its own is not a documented form.
    pub author: Option<String>,
    /// See [`ProgramQuery::author`].
    pub name: Option<String>,
}

impl ProgramQuery {
    /// Your own programs, the `?mine=1` form.
    #[must_use]
    pub fn mine() -> Self {
        Self {
            mine: true,
            ..Self::default()
        }
    }

    /// One program by author and name.
    #[must_use]
    pub fn by_author_and_name(author: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            author: Some(author.into()),
            name: Some(name.into()),
            ..Self::default()
        }
    }

    /// Keep only the runtimes a caller can do something with.
    #[must_use]
    pub fn with_runtimes(mut self, runtimes: &[Runtime]) -> Self {
        self.runtimes = runtimes.to_vec();
        self
    }

    /// The query pairs this asks for, `limit` and `before` aside.
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let wanted: Vec<&str> = self.runtimes.iter().filter_map(|r| r.wire()).collect();
        if !wanted.is_empty() {
            out.push(("runtime", wanted.join(",")));
        }
        if self.mine {
            out.push(("mine", "1".to_string()));
        }
        if let (Some(author), Some(name)) = (&self.author, &self.name) {
            out.push(("author", author.clone()));
            out.push(("name", name.clone()));
        }
        out
    }
}

/// What a program to publish is made of (§ Publish).
///
/// `source` is held as bytes rather than as a `String` so a wasm binary and a
/// text program take the same path: [`Client::publish_program`] base64-encodes
/// it and sets `encoding` when the runtime calls for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProgram {
    /// Letters, digits, `.`, `_` and `-`, max 32 characters. Publishing an
    /// existing name releases the next version of that program.
    pub name: String,
    /// Required, max 256 characters.
    pub description: String,
    /// The program itself.
    pub source: Vec<u8>,
    /// Fixed at the first release: republishing a name under a different kind
    /// is refused, because everyone holding a copy installed the old one.
    pub runtime: Runtime,
    /// Optional release note.
    pub note: Option<String>,
}

impl NewProgram {
    /// A program to publish, with no release note.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        source: impl Into<Vec<u8>>,
        runtime: Runtime,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            source: source.into(),
            runtime,
            note: None,
        }
    }

    /// Attach a release note.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Check what the spec states flatly, before the request costs one of the
    /// three publishes a minute allows.
    ///
    /// Deliberately not a full check. The size and count ceilings are
    /// tier-dependent and the client cannot know its own tier (`isSupporter`
    /// says nothing about staff), and whether a name is already taken under a
    /// different runtime is server state. Those stay server-side and surface as
    /// the error they are.
    pub fn validate(&self) -> Result<()> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(ApiError::Config("a program needs a name".into()));
        }
        if name.chars().count() > MAX_PROGRAM_NAME_LEN {
            return Err(ApiError::Config(format!(
                "program name exceeds {MAX_PROGRAM_NAME_LEN} characters"
            )));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(ApiError::Config(
                "program name may hold only letters, digits, '.', '_' and '-'".into(),
            ));
        }
        let description = self.description.trim();
        if description.is_empty() {
            return Err(ApiError::Config("a program needs a description".into()));
        }
        if description.chars().count() > MAX_PROGRAM_DESCRIPTION_LEN {
            return Err(ApiError::Config(format!(
                "program description exceeds {MAX_PROGRAM_DESCRIPTION_LEN} characters"
            )));
        }
        if self.source.is_empty() {
            return Err(ApiError::Config("a program needs source".into()));
        }
        if self.runtime == Runtime::Unknown {
            return Err(ApiError::Config(
                "this build does not know that runtime, so it cannot publish under it".into(),
            ));
        }
        if self.runtime.is_binary() && !self.source.starts_with(WASM_MAGIC) {
            return Err(ApiError::Config(
                "a wasm program's decoded bytes must start with the \\0asm magic number".into(),
            ));
        }
        Ok(())
    }
}

/// Result of [`Client::publish_program`] (§ Publish).
///
/// Three outcomes share one shape. A new release sets `release` to the version
/// just cut; `unchanged` says the source was identical to the current release
/// and nothing was cut; `restored` says a recalled program went back into the
/// gallery as the same version it left at.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedProgram {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub release: u32,
    /// The source matched the current release, so no new version was cut.
    #[serde(default)]
    pub unchanged: bool,
    /// A recalled program went back into the gallery at its existing version.
    #[serde(default)]
    pub restored: bool,
}

impl PublishedProgram {
    /// Whether this call actually cut a new release, as opposed to landing on
    /// one that already stood.
    ///
    /// Publishing is idempotent in both quiet directions (§ Publish), so a
    /// no-op is a success with nothing to announce, not an error.
    #[must_use]
    pub fn is_new_release(&self) -> bool {
        !self.unchanged && !self.restored
    }
}

/// Result of [`Client::recall_program`] and [`Client::purge_program`]
/// (§ Recall).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecalledProgram {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Set by a plain recall: the program left the gallery, its release history
    /// stayed, and publishing again resumes from the next version.
    #[serde(default)]
    pub recalled: bool,
    /// Set by `?purge=1`: the record is gone and the slot it held against the
    /// program count is free again.
    #[serde(default)]
    pub deleted: bool,
}

impl Client {
    /// `GET /v1/programs` — the gallery, newest first (§ Browse the Gallery).
    ///
    /// `query` picks the form: the default is the public gallery,
    /// [`ProgramQuery::mine`] your own programs including drafts and recalled
    /// ones, and [`ProgramQuery::by_author_and_name`] one lookup. `limit` is
    /// clamped to 1–50 with a default of 30.
    ///
    /// `before` is the previous response's cursor. A filtered page can come back
    /// shorter than `limit`, or empty, and still carry one — follow the cursor
    /// until it is `None` rather than stopping at the first short page.
    pub async fn list_programs(
        &self,
        query: &ProgramQuery,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<(Vec<Program>, Option<String>)> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
        let mut params: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(cursor) = before {
            params.push(("before", cursor.to_string()));
        }
        params.extend(query.pairs());
        self.request_page(
            EndpointKey::ProgramsList,
            Method::GET,
            "/v1/programs",
            &params,
        )
        .await
    }

    /// `GET /v1/programs/:id/source` — the current release's source, or an
    /// earlier one by number (§ Read the Source).
    ///
    /// Release objects are immutable, so an old version is still exactly what
    /// went out. You can always read your own; anyone else's only while it is
    /// published.
    pub async fn get_program_source(
        &self,
        program_id: &str,
        release: Option<u32>,
    ) -> Result<ProgramSource> {
        let path = format!("/v1/programs/{program_id}/source");
        let query: Vec<(&str, String)> = match release {
            Some(n) => vec![("release", n.to_string())],
            None => Vec::new(),
        };
        self.request::<ProgramSource, ()>(
            EndpointKey::ProgramsSource,
            Method::GET,
            &path,
            &query,
            None,
        )
        .await
    }

    /// `POST /v1/programs` — publish a program under your account (§ Publish).
    ///
    /// Publishing an existing name releases the next version. Publishing
    /// unchanged source is a no-op, and publishing a recalled program puts it
    /// back at the same version; both come back as a success with `unchanged`
    /// or `restored` set, so branch on
    /// [`PublishedProgram::is_new_release`] rather than on the status code.
    ///
    /// A `wasm` program is sent base64-encoded with `"encoding": "base64"`, both
    /// of which this method sets from [`NewProgram::runtime`].
    ///
    /// Rate limit: 3/min, 40/day.
    pub async fn publish_program(&self, program: &NewProgram) -> Result<PublishedProgram> {
        program.validate()?;
        let binary = program.runtime.is_binary();
        let source = if binary {
            BASE64.encode(&program.source)
        } else {
            // A text program's bytes came from a `String` or a file the caller
            // read; anything that is not UTF-8 cannot go in a JSON string, and
            // saying so beats sending mojibake.
            String::from_utf8(program.source.clone()).map_err(|_| {
                ApiError::Config("a web or term program's source must be valid UTF-8".into())
            })?
        };
        let body = PublishBody {
            name: program.name.trim(),
            description: program.description.trim(),
            source: &source,
            runtime: program.runtime.wire(),
            encoding: binary.then_some("base64"),
            note: program.note.as_deref(),
        };
        self.request(
            EndpointKey::ProgramsPublish,
            Method::POST,
            "/v1/programs",
            &[],
            Some(&body),
        )
        .await
    }

    /// `DELETE /v1/programs/:id` — take your program out of the gallery
    /// (§ Recall).
    ///
    /// The release history stays and publishing again resumes from the next
    /// version, so this is reversible. It does *not* free the slot the program
    /// holds against your program count; [`purge_program`](Client::purge_program)
    /// is what does.
    pub async fn recall_program(&self, program_id: &str) -> Result<RecalledProgram> {
        let path = format!("/v1/programs/{program_id}");
        self.request::<RecalledProgram, ()>(
            EndpointKey::ProgramsRecall,
            Method::DELETE,
            &path,
            &[],
            None,
        )
        .await
    }

    /// `DELETE /v1/programs/:id?purge=1` — delete the record (§ Recall).
    ///
    /// Irreversible, and refused on a program a moderator has taken down.
    /// Copies other members installed are unaffected. This is the call that
    /// frees the slot the program holds against your program count.
    pub async fn purge_program(&self, program_id: &str) -> Result<RecalledProgram> {
        let path = format!("/v1/programs/{program_id}");
        self.request::<RecalledProgram, ()>(
            EndpointKey::ProgramsRecall,
            Method::DELETE,
            &path,
            &[("purge", "1".to_string())],
            None,
        )
        .await
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishBody<'a> {
    name: &'a str,
    description: &'a str,
    source: &'a str,
    /// Omitted for a runtime with no wire value, which `validate` has already
    /// refused; the server then applies its own `web` default.
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<&'static str>,
    /// Only ever `"base64"`, and only for `wasm` (§ Publish: "the two go
    /// together").
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

/// A decode failure carrying a message of our own.
///
/// [`ApiError::Decode`] wraps a `serde_json::Error` because every other decode
/// failure in this crate comes from serde, but base64 is decoded by hand here
/// and the failure means exactly the same thing to a caller: the server sent
/// something this client cannot read. `serde::de::Error::custom` is the
/// supported way to build one, so it goes in the right arm instead of being
/// mislabelled a configuration problem.
fn decode_error(message: String) -> ApiError {
    ApiError::Decode(<serde_json::Error as serde::de::Error>::custom(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_with_no_runtime_field_is_web() {
        // § Programs: "A program with no `runtime` field is `web`." Everything
        // published before the field existed came from the website, so guessing
        // anything else would mislabel the whole back catalogue.
        let p: Program = serde_json::from_str(
            r#"{"id":"p1","name":"hello","ownerUsername":"trinity",
                "description":"says hi","release":3,"publishedAt":1756000000000}"#,
        )
        .expect("must decode");
        assert_eq!(p.runtime, Runtime::Web);
        assert_eq!(p.release, 3);
        assert_eq!(p.published_at, Some(1_756_000_000_000));
        assert!(!p.is_draft(), "a public row says nothing about publication");
        assert!(!p.is_taken_down());
    }

    #[test]
    fn an_unknown_runtime_does_not_sink_the_listing() {
        // A fourth machine would otherwise fail the decode of the whole page it
        // arrived on, taking every program the client *can* show with it.
        let p: Program =
            serde_json::from_str(r#"{"id":"p1","name":"x","runtime":"quantum"}"#).unwrap();
        assert_eq!(p.runtime, Runtime::Unknown);
        assert_eq!(p.runtime.wire(), None);
    }

    #[test]
    fn a_mine_row_carries_the_fields_a_public_row_does_not() {
        let p: Program = serde_json::from_str(
            r#"{"id":"p1","name":"draft","runtime":"term","isPublished":false,
                "takenDown":false,"hash":"abc123"}"#,
        )
        .unwrap();
        assert_eq!(p.runtime, Runtime::Term);
        assert!(p.is_draft());
        assert!(!p.is_taken_down());
        assert_eq!(p.hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn source_encoding_decides_how_source_is_read() {
        let text: ProgramSource = serde_json::from_str(
            r#"{"id":"p1","runtime":"term","encoding":"utf8","source":"export default 1"}"#,
        )
        .unwrap();
        assert!(!text.is_base64());
        assert_eq!(text.text().as_deref(), Some("export default 1"));
        assert_eq!(text.bytes().unwrap(), b"export default 1");

        // `\0asm\x01\0\0\0`, the header of a minimal wasm module.
        let binary: ProgramSource = serde_json::from_str(
            r#"{"id":"p2","runtime":"wasm","encoding":"base64","source":"AGFzbQEAAAA="}"#,
        )
        .unwrap();
        assert!(binary.is_base64());
        assert_eq!(binary.bytes().unwrap(), b"\0asm\x01\0\0\0");
        assert_eq!(
            binary.text(),
            None,
            "a wasm module has no text form to render"
        );
    }

    #[test]
    fn a_missing_encoding_falls_back_to_the_runtime() {
        // Both halves document the same thing, so either alone is enough.
        let binary: ProgramSource =
            serde_json::from_str(r#"{"id":"p1","runtime":"wasm","source":"AGFzbQEAAAA="}"#)
                .unwrap();
        assert!(binary.is_base64());

        let text: ProgramSource =
            serde_json::from_str(r#"{"id":"p1","runtime":"web","source":"x"}"#).unwrap();
        assert!(!text.is_base64());
    }

    #[test]
    fn wrapped_base64_still_decodes() {
        // Not the server's documented behaviour, but a newline in a long base64
        // payload must not cost the user the program.
        let s = ProgramSource {
            encoding: "base64".into(),
            source: "AGFz\nbQEA\nAAA=".into(),
            ..Default::default()
        };
        assert_eq!(s.bytes().unwrap(), b"\0asm\x01\0\0\0");
    }

    #[test]
    fn a_query_builds_the_documented_forms() {
        assert!(ProgramQuery::default().pairs().is_empty());

        assert_eq!(
            ProgramQuery::mine().pairs(),
            vec![("mine", "1".to_string())]
        );

        assert_eq!(
            ProgramQuery::default()
                .with_runtimes(&[Runtime::Web, Runtime::Term])
                .pairs(),
            vec![("runtime", "web,term".to_string())]
        );

        assert_eq!(
            ProgramQuery::by_author_and_name("trinity", "hello").pairs(),
            vec![
                ("author", "trinity".to_string()),
                ("name", "hello".to_string())
            ]
        );
    }

    #[test]
    fn an_unknown_runtime_is_dropped_from_a_filter() {
        // Its wire form is this client's placeholder, not a value the server
        // knows, and sending it would filter the gallery down to nothing.
        let q = ProgramQuery::default().with_runtimes(&[Runtime::Unknown]);
        assert!(
            q.pairs().is_empty(),
            "a filter of nothing but Unknown must ask for every kind, not for none"
        );

        let q = ProgramQuery::default().with_runtimes(&[Runtime::Wasm, Runtime::Unknown]);
        assert_eq!(q.pairs(), vec![("runtime", "wasm".to_string())]);
    }

    #[test]
    fn a_half_named_lookup_is_not_a_documented_form() {
        // § Browse the Gallery pairs `author` with `name`. Sending one alone
        // would quietly list somebody's whole shelf under the name the caller
        // asked for.
        let q = ProgramQuery {
            author: Some("trinity".into()),
            ..Default::default()
        };
        assert!(q.pairs().is_empty());
    }

    #[test]
    fn publish_validation_enforces_what_the_spec_states_flatly() {
        let ok = NewProgram::new(
            "hello.v2_x",
            "says hi",
            b"export default 1".to_vec(),
            Runtime::Term,
        );
        assert!(ok.validate().is_ok());

        let bad_name = NewProgram::new("hello world", "d", b"x".to_vec(), Runtime::Web);
        assert!(
            bad_name.validate().is_err(),
            "spaces are not in the charset"
        );

        let long_name = NewProgram::new("a".repeat(33), "d", b"x".to_vec(), Runtime::Web);
        assert!(long_name.validate().is_err());
        let at_limit = NewProgram::new("a".repeat(32), "d", b"x".to_vec(), Runtime::Web);
        assert!(at_limit.validate().is_ok(), "32 is allowed, 33 is not");

        let no_description = NewProgram::new("hello", "   ", b"x".to_vec(), Runtime::Web);
        assert!(
            no_description.validate().is_err(),
            "description is required"
        );

        let long_description =
            NewProgram::new("hello", "d".repeat(257), b"x".to_vec(), Runtime::Web);
        assert!(long_description.validate().is_err());

        let empty = NewProgram::new("hello", "d", Vec::new(), Runtime::Web);
        assert!(empty.validate().is_err());

        let unknown = NewProgram::new("hello", "d", b"x".to_vec(), Runtime::Unknown);
        assert!(unknown.validate().is_err());
    }

    #[test]
    fn a_wasm_program_must_carry_the_magic_number() {
        // § Publish: "the decoded bytes must start with `\0asm`". Catching it
        // here turns a wasted publish into an explanation.
        let not_wasm = NewProgram::new("prog", "d", b"export default 1".to_vec(), Runtime::Wasm);
        assert!(not_wasm.validate().is_err());

        let wasm = NewProgram::new("prog", "d", b"\0asm\x01\0\0\0".to_vec(), Runtime::Wasm);
        assert!(wasm.validate().is_ok());
    }

    #[test]
    fn publish_outcomes_are_told_apart() {
        let cut: PublishedProgram =
            serde_json::from_str(r#"{"id":"p1","name":"hello","release":4}"#).unwrap();
        assert!(cut.is_new_release());

        let same: PublishedProgram =
            serde_json::from_str(r#"{"id":"p1","name":"hello","release":4,"unchanged":true}"#)
                .unwrap();
        assert!(!same.is_new_release());

        let back: PublishedProgram =
            serde_json::from_str(r#"{"id":"p1","name":"hello","release":4,"restored":true}"#)
                .unwrap();
        assert!(!back.is_new_release());
    }

    #[test]
    fn a_publish_body_sends_base64_only_for_wasm() {
        let wasm = NewProgram::new("prog", "d", b"\0asm\x01\0\0\0".to_vec(), Runtime::Wasm);
        let body = PublishBody {
            name: &wasm.name,
            description: &wasm.description,
            source: &BASE64.encode(&wasm.source),
            runtime: wasm.runtime.wire(),
            encoding: Some("base64"),
            note: None,
        };
        let v: serde_json::Value = serde_json::to_value(&body).unwrap();
        assert_eq!(v["runtime"], "wasm");
        assert_eq!(v["encoding"], "base64");
        assert_eq!(v["source"], "AGFzbQEAAAA=");
        assert!(!v.as_object().unwrap().contains_key("note"));

        let term = NewProgram::new("prog", "d", b"export default 1".to_vec(), Runtime::Term)
            .with_note("first cut");
        let body = PublishBody {
            name: &term.name,
            description: &term.description,
            source: "export default 1",
            runtime: term.runtime.wire(),
            encoding: None,
            note: term.note.as_deref(),
        };
        let v: serde_json::Value = serde_json::to_value(&body).unwrap();
        assert_eq!(v["runtime"], "term");
        assert!(
            !v.as_object().unwrap().contains_key("encoding"),
            "encoding and base64 go together; a text program sends neither"
        );
        assert_eq!(v["note"], "first cut");
    }

    #[test]
    fn a_recall_and_a_purge_are_told_apart() {
        let recalled: RecalledProgram =
            serde_json::from_str(r#"{"id":"p1","name":"hello","recalled":true}"#).unwrap();
        assert!(recalled.recalled);
        assert!(!recalled.deleted);

        let purged: RecalledProgram =
            serde_json::from_str(r#"{"id":"p1","name":"hello","deleted":true}"#).unwrap();
        assert!(purged.deleted);
        assert!(!purged.recalled);
    }
}

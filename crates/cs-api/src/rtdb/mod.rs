//! Firebase Realtime Database transport client.
//!
//! Pure plumbing — no cyberspace.online-specific paths or message shapes. The
//! cs-online RTDB schema (cIRC rooms, C-Mail conversations, presence) is not
//! yet officially documented, so the typed application layer waits for the spec
//! and lives in Phase 8.
//!
//! Usage:
//! ```ignore
//! let project = rtdb::project_id_from_jwt(&tokens.rtdb_token)?;
//! let base = rtdb::base_url_for(&project);
//! let client = rtdb::Client::new(base, tokens.rtdb_token);
//! let value: serde_json::Value = client.get("/users/me", &[]).await?;
//! let mut events = client.subscribe("/users/me").await?;
//! while let Some(ev) = events.recv().await { /* ... */ }
//! ```
mod client;
mod jwt;

pub use client::{Client as RtdbClient, RtdbError, SseEvent, SseEventKind};
pub use jwt::{base_url_for, project_id_from_jwt};

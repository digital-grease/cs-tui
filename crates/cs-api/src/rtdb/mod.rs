//! Firebase Realtime Database transport client.
//!
//! Pure plumbing, with no cyberspace.online-specific paths or message shapes.
//! API v0.8.4 documents five RTDB paths: `dm_messages/<conversationId>`,
//! `dm_presence/<conversationId>` and `user_conversations/<uid>` for C-Mail,
//! `chat_messages/<roomId>` and `chat_presence/<roomId>` for cIRC. The typed
//! shapes and the path builders live in the `cmail` / `circ` modules.
//!
//! Read-only in practice: every path above is published through the REST API
//! (sending, typing indicators, presence heartbeats), so a client subscribes
//! here and writes there. The `put` / `patch` / `delete` methods below are
//! transport completeness, not a second way in.
//!
//! Decoding an event is not just deserialising its `data`: a `put` replaces the
//! value at its path while a `patch` merges only the fields it carries, so a
//! `patch` payload is a fragment rather than a whole object. cIRC deletions
//! arrive that way (§ Reading a room in real time), which is why
//! [`crate::circ_message_updates_from_rtdb_event`] takes the event kind.
//!
//! Usage:
//! ```ignore
//! let client = rtdb::Client::new(tokens.rtdb_url, tokens.id_token);
//! let params = [("orderBy", "%22timestamp%22"), ("limitToLast", "50")];
//! let value: serde_json::Value = client.get("/dm_messages/conversationId", &params).await?;
//! let mut events = client.subscribe("/dm_messages/conversationId", &params).await?;
//! while let Some(ev) = events.recv().await { /* ... */ }
//! ```
mod client;
mod jwt;

pub use client::{Client as RtdbClient, RtdbError, SseEvent, SseEventKind};
pub use jwt::{base_url_for, project_id_from_jwt, uid_from_jwt};

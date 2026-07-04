//! Firebase Realtime Database transport client.
//!
//! Pure plumbing — no cyberspace.online-specific paths or message shapes. API
//! v0.7 documents the C-Mail + cIRC RTDB paths (`dm_messages/<conversationId>`,
//! `chat_messages/<roomId>`, and `user_conversations/<uid>`); the typed message
//! shapes live in the `cmail` / `circ` modules.
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

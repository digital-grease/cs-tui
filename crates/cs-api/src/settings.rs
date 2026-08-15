//! User settings (`/v1/settings`, API v0.8.4 § Settings).
//!
//! The spec lists known fields, but some (`keyboardBindings`,
//! `mutedUsersByRoom`) are opaque JSON. `Settings` decodes everything verbatim
//! via `#[serde(flatten)]` into `extra`, so a round-trip preserves anything the
//! client doesn't model.
//!
//! `mutedUsersByRoom` is the one field the spec names without ever giving its
//! wire shape: § Settings lists it among the available fields and § Commands
//! ("Muting") says the `/mute` family stores your per-room mute list there, but
//! no example is published. It therefore stays a raw [`serde_json::Value`] so an
//! unexpected shape can never sink the whole settings decode, and the shape
//! guessing is confined to the read helpers [`Settings::is_muted`] and
//! [`Settings::muted_users_in_room`], which accept every plausible encoding and
//! answer "not muted" for anything they cannot read.
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::Result;

/// User-settable notification preferences (sub-object of `Settings`).
///
/// The spec example shows three keys (`bookmark`, `reply`, `poke`) but the
/// server may store more. Unknown keys round-trip via `extra`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct NotificationPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poke: Option<bool>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Full settings object as returned by `GET /v1/settings`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub notifications: NotificationPrefs,

    #[serde(default, rename = "filterNSFW")]
    pub filter_nsfw: Option<bool>,
    #[serde(default)]
    pub show_follower_count: Option<bool>,
    #[serde(default)]
    pub hide_images_in_feed: Option<bool>,
    #[serde(default)]
    pub hide_audio_in_feed: Option<bool>,
    #[serde(default)]
    pub auto_watch_on_reply: Option<bool>,
    #[serde(default)]
    pub use_legacy_menu_order: Option<bool>,
    #[serde(default)]
    pub default_public_post: Option<bool>,

    #[serde(default)]
    pub icon_theme: Option<String>,
    #[serde(default)]
    pub image_pixel_size: Option<String>,
    #[serde(default)]
    pub time_display_format: Option<String>,
    #[serde(default)]
    pub keyboard_preset: Option<String>,

    #[serde(default)]
    pub followed_topics: Option<Vec<String>>,
    #[serde(default)]
    pub muted_topics: Option<Vec<String>>,

    /// Opaque server-managed JSON; preserved on round-trip.
    #[serde(default)]
    pub keyboard_bindings: Option<serde_json::Value>,

    /// Per-room mute lists, maintained by the `/mute` family of cIRC commands
    /// (§ Commands, "Muting"). Deliberately untyped: the spec never publishes a
    /// shape for it, so a strict struct here would risk failing the whole
    /// settings decode over a field the client only reads. Go through
    /// [`Settings::is_muted`] / [`Settings::muted_users_in_room`] instead of
    /// picking this apart at the call site.
    #[serde(default)]
    pub muted_users_by_room: Option<serde_json::Value>,

    /// Any other fields the server adds in the future. Preserved verbatim.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Settings {
    /// Whether `username` is muted in `room` (§ Commands, "Muting").
    ///
    /// Nothing is filtered server-side: `GET /v1/circ/:roomId` still returns a
    /// muted user's messages and the client is expected to hide them, which is
    /// also what lets an unmute reveal history already on screen. Call this per
    /// message while rendering a room.
    ///
    /// Matching ignores ASCII case and a leading `@`, because handles are the
    /// same case-insensitive identifiers `@mentions` use and a stored entry may
    /// have kept the `@` the user typed.
    #[must_use]
    pub fn is_muted(&self, room: &str, username: &str) -> bool {
        let wanted = normalize_handle(username);
        if wanted.is_empty() {
            return false;
        }
        self.muted_users_in_room(room)
            .iter()
            .any(|u| normalize_handle(u).eq_ignore_ascii_case(wanted))
    }

    /// The handles muted in `room`, exactly as stored (§ Commands, "Muting").
    ///
    /// Empty when nothing is muted there, when the field is absent or `null`,
    /// and when the value is in a shape this client cannot read: a mute list is
    /// a display filter, so failing open (showing everything) is the safe way
    /// to be wrong. Order follows the stored value for a list, and is sorted by
    /// handle when the server keys the entries instead.
    #[must_use]
    pub fn muted_users_in_room(&self, room: &str) -> Vec<&str> {
        match room_mutes(self.muted_users_by_room.as_ref(), room) {
            Some(entry) => muted_handles(entry),
            None => Vec::new(),
        }
    }
}

/// Trim a handle down to the part that identifies the user: `" @Bob "` and
/// `"bob"` name the same account.
fn normalize_handle(name: &str) -> &str {
    name.trim().trim_start_matches('@')
}

/// The `mutedUsersByRoom` entry for `room`, or `None` when the field is absent,
/// `null`, not an object, or has no entry for that room.
fn room_mutes<'a>(
    value: Option<&'a serde_json::Value>,
    room: &str,
) -> Option<&'a serde_json::Value> {
    let rooms = value?.as_object()?;
    if let Some(entry) = rooms.get(room) {
        return Some(entry);
    }
    // Rooms are addressed by slug (§ cIRC: "addressed by its `roomId` (its
    // slug, e.g. `general`)"), which is already lower-case, but fall back to a
    // case-insensitive key match so a differently-cased key written by another
    // client still lines up.
    rooms
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(room))
        .map(|(_, entry)| entry)
}

/// Read the handles out of one room's entry, accepting every shape the
/// undocumented `mutedUsersByRoom` value plausibly takes.
fn muted_handles(entry: &serde_json::Value) -> Vec<&str> {
    use serde_json::Value;

    match entry {
        // `{"general": ["alice", "bob"]}`, a plain list, and the shape this
        // client has actually observed. Objects inside the list are read too,
        // in case the server ever attaches a timestamp to each mute.
        Value::Array(items) => items.iter().filter_map(handle_of).collect(),
        // `{"general": {"alice": true}}`, a keyed set. An explicit `false` or
        // `null` reads as "not muted" so an unmute recorded as a tombstone
        // doesn't keep hiding someone.
        Value::Object(map) => map
            .iter()
            .filter(|(_, flag)| !matches!(flag, Value::Bool(false) | Value::Null))
            .map(|(handle, _)| handle.as_str())
            .collect(),
        // `{"general": "alice"}`, a lone muted handle.
        Value::String(handle) => vec![handle.as_str()],
        _ => Vec::new(),
    }
}

/// One element of a list-shaped room entry: the handle itself, or an object
/// carrying it under `username`.
fn handle_of(item: &serde_json::Value) -> Option<&str> {
    match item {
        serde_json::Value::String(handle) => Some(handle.as_str()),
        serde_json::Value::Object(map) => map.get("username").and_then(serde_json::Value::as_str),
        _ => None,
    }
}

/// Partial update body for `PATCH /v1/settings`. Only `Some` fields are sent.
///
/// `mutedUsersByRoom` is deliberately missing from this write path even though
/// § Settings lists it as available. The documented way to change a mute list is
/// the `/mute`, `/unmute`, `/muted` and `/unmuteall` cIRC commands (§ Commands,
/// "Muting"), and the spec never states the field's wire shape, so a PATCH built
/// from a guessed shape could overwrite the user's real mute list, on the
/// website as well, with something the server reads as empty. Reads stay lenient
/// (see [`Settings::is_muted`]); writes go through the commands, after which the
/// client re-reads with [`Client::get_settings`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifications: Option<NotificationPrefs>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "filterNSFW")]
    pub filter_nsfw: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_follower_count: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_images_in_feed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_audio_in_feed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_watch_on_reply: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_legacy_menu_order: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_public_post: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_pixel_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_display_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyboard_preset: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub followed_topics: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted_topics: Option<Vec<String>>,
}

impl SettingsUpdate {
    /// No-op update — every field is `None`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.notifications.is_none()
            && self.filter_nsfw.is_none()
            && self.show_follower_count.is_none()
            && self.hide_images_in_feed.is_none()
            && self.hide_audio_in_feed.is_none()
            && self.auto_watch_on_reply.is_none()
            && self.use_legacy_menu_order.is_none()
            && self.default_public_post.is_none()
            && self.icon_theme.is_none()
            && self.image_pixel_size.is_none()
            && self.time_display_format.is_none()
            && self.keyboard_preset.is_none()
            && self.followed_topics.is_none()
            && self.muted_topics.is_none()
    }
}

impl Client {
    /// `GET /v1/settings`.
    ///
    /// Also the way to pick up a mute change: a `/mute`, `/unmute` or
    /// `/unmuteall` command returns only its reply string (§ Commands,
    /// "Muting") and updates `mutedUsersByRoom` server-side, so call this once
    /// the command comes back and re-read with [`Settings::is_muted`].
    pub async fn get_settings(&self) -> Result<Settings> {
        self.request::<Settings, ()>(
            EndpointKey::SettingsGet,
            Method::GET,
            "/v1/settings",
            &[],
            None,
        )
        .await
    }

    /// `PATCH /v1/settings`. Only non-`None` fields in `update` are sent.
    /// Returns the updated `Settings` (or a no-op fetch when `update` is empty).
    pub async fn update_settings(&self, update: &SettingsUpdate) -> Result<Settings> {
        if update.is_empty() {
            return self.get_settings().await;
        }
        self.request::<Settings, SettingsUpdate>(
            EndpointKey::SettingsUpdate,
            Method::PATCH,
            "/v1/settings",
            &[],
            Some(update),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_decodes_known_and_unknown_fields() {
        let raw = r#"{
            "notifications": {"bookmark": true, "reply": false, "extraNotif": 1},
            "filterNSFW": true,
            "showFollowerCount": false,
            "iconTheme": "cyber",
            "keyboardBindings": {"j": "down"},
            "futureUnknownField": "ok"
        }"#;
        let s: Settings = serde_json::from_str(raw).unwrap();
        assert_eq!(s.notifications.bookmark, Some(true));
        assert_eq!(s.notifications.reply, Some(false));
        assert!(s.notifications.extra.contains_key("extraNotif"));
        assert_eq!(s.filter_nsfw, Some(true));
        assert_eq!(s.show_follower_count, Some(false));
        assert_eq!(s.icon_theme.as_deref(), Some("cyber"));
        assert!(s.keyboard_bindings.is_some());
        assert!(s.extra.contains_key("futureUnknownField"));
    }

    #[test]
    fn update_serializes_only_some_fields() {
        let u = SettingsUpdate {
            filter_nsfw: Some(true),
            icon_theme: Some("cyber".into()),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&u).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.get("filterNSFW").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(obj.get("iconTheme").and_then(|v| v.as_str()), Some("cyber"));
        // None fields must not appear.
        assert!(!obj.contains_key("showFollowerCount"));
        assert!(!obj.contains_key("notifications"));
    }

    #[test]
    fn empty_update_is_empty() {
        let u = SettingsUpdate::default();
        assert!(u.is_empty());
        let s = serde_json::to_string(&u).unwrap();
        assert_eq!(s, "{}");
    }

    #[test]
    fn nested_notifications_update_serializes() {
        let u = SettingsUpdate {
            notifications: Some(NotificationPrefs {
                bookmark: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&u).unwrap();
        assert_eq!(v["notifications"]["bookmark"], false);
    }

    #[test]
    fn round_trip_preserves_opaque_fields() {
        let raw = r#"{
            "filterNSFW": true,
            "keyboardBindings": {"j": "down", "k": "up"},
            "mutedUsersByRoom": {"general": ["u1", "u2"]},
            "newUnknownField": [1, 2, 3]
        }"#;
        let parsed: Settings = serde_json::from_str(raw).unwrap();
        let serialized: serde_json::Value = serde_json::to_value(&parsed).unwrap();
        // Opaque fields preserved
        assert_eq!(serialized["keyboardBindings"]["j"], "down");
        assert_eq!(serialized["mutedUsersByRoom"]["general"][0], "u1");
        assert_eq!(serialized["newUnknownField"][0], 1);
    }

    /// Build a `Settings` whose `mutedUsersByRoom` is the given JSON text.
    fn with_mutes(raw: &str) -> Settings {
        serde_json::from_str(&format!(r#"{{"mutedUsersByRoom":{raw}}}"#)).expect("must decode")
    }

    #[test]
    fn muted_users_read_from_a_list_shaped_value() {
        let s = with_mutes(r#"{"general": ["alice", "bob"]}"#);
        assert_eq!(s.muted_users_in_room("general"), vec!["alice", "bob"]);
        assert!(s.is_muted("general", "alice"));
        assert!(s.is_muted("general", "bob"));
        assert!(!s.is_muted("general", "carol"));
        // Other rooms are unaffected.
        assert!(!s.is_muted("thesprawl", "alice"));
        assert!(s.muted_users_in_room("thesprawl").is_empty());
    }

    #[test]
    fn muted_users_read_from_a_keyed_shaped_value() {
        // The shape is undocumented, so an object of flags has to work too.
        let s = with_mutes(r#"{"general": {"alice": true, "bob": false, "carol": null}}"#);
        assert_eq!(s.muted_users_in_room("general"), vec!["alice"]);
        assert!(s.is_muted("general", "alice"));
        assert!(!s.is_muted("general", "bob"), "false must read as unmuted");
        assert!(!s.is_muted("general", "carol"), "null must read as unmuted");
    }

    #[test]
    fn muted_users_read_from_richer_entries() {
        // A list of records, and a lone handle, both still resolve.
        let s = with_mutes(r#"{"general": [{"username": "alice", "mutedAt": 1}, 7]}"#);
        assert_eq!(s.muted_users_in_room("general"), vec!["alice"]);
        assert!(s.is_muted("general", "alice"));

        let s = with_mutes(r#"{"general": "alice"}"#);
        assert_eq!(s.muted_users_in_room("general"), vec!["alice"]);
    }

    #[test]
    fn muted_lookup_ignores_case_and_a_leading_at() {
        let s = with_mutes(r#"{"General": ["@Alice"]}"#);
        // Room keys match case-insensitively, handles too, and the `@` a user
        // may have typed into `/mute` is not part of the identity.
        assert!(s.is_muted("general", "alice"));
        assert!(s.is_muted("GENERAL", "@ALICE"));
        assert!(s.is_muted("general", " alice "));
        // The list itself is returned exactly as stored.
        assert_eq!(s.muted_users_in_room("general"), vec!["@Alice"]);
    }

    #[test]
    fn muted_lookup_tolerates_absent_null_and_unreadable_shapes() {
        // Absent field.
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.muted_users_by_room.is_none());
        assert!(s.muted_users_in_room("general").is_empty());
        assert!(!s.is_muted("general", "alice"));

        // Explicit null, and shapes no reading of the spec predicts. None of
        // them may fail the decode or claim someone is muted.
        for raw in [
            "null",
            "42",
            r#""nonsense""#,
            r#"["alice"]"#,
            r#"{"general": 7}"#,
        ] {
            let s = with_mutes(raw);
            assert!(
                s.muted_users_in_room("general").is_empty(),
                "shape {raw} must read as no mutes"
            );
            assert!(!s.is_muted("general", "alice"), "shape {raw}");
        }
    }

    #[test]
    fn an_unreadable_mute_shape_does_not_sink_the_settings_decode() {
        // The whole point of leaving the field untyped: a surprise here must
        // not cost the user every other setting.
        let raw = r#"{
            "filterNSFW": true,
            "iconTheme": "cyber",
            "mutedUsersByRoom": [{"room": "general", "users": ["alice"]}]
        }"#;
        let s: Settings = serde_json::from_str(raw).expect("must still decode");
        assert_eq!(s.filter_nsfw, Some(true));
        assert_eq!(s.icon_theme.as_deref(), Some("cyber"));
        assert!(!s.is_muted("general", "alice"));
    }

    #[test]
    fn is_muted_rejects_an_empty_handle() {
        let s = with_mutes(r#"{"general": ["alice", ""]}"#);
        assert!(!s.is_muted("general", ""));
        assert!(!s.is_muted("general", "@"));
    }

    #[test]
    fn update_never_writes_muted_users_by_room() {
        // The `/mute` commands own that field; see the `SettingsUpdate` docs.
        let u = SettingsUpdate {
            filter_nsfw: Some(true),
            muted_topics: Some(vec!["spoilers".into()]),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&u).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("mutedUsersByRoom"));
        assert!(obj.contains_key("mutedTopics"));
    }
}

//! User settings (`/v1/settings`).
//!
//! The v0.3.6 spec lists known fields, but some (`keyboardBindings`,
//! `mutedUsersByRoom`) are opaque JSON. `Settings` decodes everything verbatim
//! via `#[serde(flatten)]` into `extra`, so a round-trip preserves anything the
//! client doesn't model.
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
    #[serde(default)]
    pub muted_users_by_room: Option<serde_json::Value>,

    /// Any other fields the server adds in the future. Preserved verbatim.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Partial update body for `PATCH /v1/settings`. Only `Some` fields are sent.
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
}

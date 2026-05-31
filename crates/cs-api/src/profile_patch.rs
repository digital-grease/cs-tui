//! `PATCH /v1/users/me` — profile editing.
//!
//! The endpoint distinguishes three field states:
//! - omitted (no change),
//! - set to a value (replace),
//! - explicit `null` (clear).
//!
//! [`Patch`] encodes those three states and serializes correctly with
//! `#[serde(skip_serializing_if = "Patch::is_skip")]` on each field.
use reqwest::Method;
use serde::{Serialize, Serializer};

use crate::client::Client;
use crate::endpoint::EndpointKey;
use crate::error::{ApiError, Result};

/// Three-valued field state for a PATCH body.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Patch<T> {
    /// Don't send this field at all.
    #[default]
    Skip,
    /// Send the field with this value.
    Set(T),
    /// Send `null` to clear the value.
    Clear,
}

impl<T> Patch<T> {
    /// Used in `skip_serializing_if`.
    #[must_use]
    pub fn is_skip(&self) -> bool {
        matches!(self, Patch::Skip)
    }
}

impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Patch::Skip => {
                // `skip_serializing_if` should have prevented us from getting here.
                // Fall back to null in case someone embeds Patch outside a struct.
                ser.serialize_none()
            }
            Patch::Set(v) => v.serialize(ser),
            Patch::Clear => ser.serialize_none(),
        }
    }
}

/// Body for `PATCH /v1/users/me`. Only non-`Skip` fields are sent.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdate {
    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub bio: Patch<String>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub display_name: Patch<String>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub pinned_post_id: Patch<String>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub website_url: Patch<String>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub website_name: Patch<String>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub website_image_url: Patch<String>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub location_latitude: Patch<f64>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub location_longitude: Patch<f64>,

    #[serde(skip_serializing_if = "Patch::is_skip")]
    pub location_name: Patch<String>,
}

impl ProfileUpdate {
    /// Returns true when no fields will be sent — saves a no-op API call.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bio.is_skip()
            && self.display_name.is_skip()
            && self.pinned_post_id.is_skip()
            && self.website_url.is_skip()
            && self.website_name.is_skip()
            && self.website_image_url.is_skip()
            && self.location_latitude.is_skip()
            && self.location_longitude.is_skip()
            && self.location_name.is_skip()
    }

    /// Validate spec-documented constraints before sending.
    /// Returns `Err(message)` describing the first violation, or `Ok(())`.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if let Patch::Set(s) = &self.bio {
            if s.chars().count() > 640 {
                return Err("bio must be ≤640 characters".into());
            }
        }
        if let Patch::Set(s) = &self.display_name {
            if s.chars().count() > 64 {
                return Err("displayName must be ≤64 characters".into());
            }
        }
        if let Patch::Set(s) = &self.website_url {
            if !(s.starts_with("http://") || s.starts_with("https://")) {
                return Err("websiteUrl must start with http:// or https://".into());
            }
            if s.len() > 2048 {
                return Err("websiteUrl must be ≤2048 characters".into());
            }
        }
        if let Patch::Set(s) = &self.website_name {
            if s.chars().count() > 64 {
                return Err("websiteName must be ≤64 characters".into());
            }
        }
        if let Patch::Set(s) = &self.website_image_url {
            if !(s.starts_with("http://") || s.starts_with("https://")) {
                return Err("websiteImageUrl must start with http:// or https://".into());
            }
            if s.len() > 2048 {
                return Err("websiteImageUrl must be ≤2048 characters".into());
            }
        }
        if let Patch::Set(s) = &self.location_name {
            if s.chars().count() > 64 {
                return Err("locationName must be ≤64 characters".into());
            }
        }
        if let Patch::Set(v) = &self.location_latitude {
            if !(-90.0..=90.0).contains(v) {
                return Err("locationLatitude must be between -90 and 90".into());
            }
        }
        if let Patch::Set(v) = &self.location_longitude {
            if !(-180.0..=180.0).contains(v) {
                return Err("locationLongitude must be between -180 and 180".into());
            }
        }
        // Lat/lng must be sent together when setting.
        let lat_set = matches!(self.location_latitude, Patch::Set(_));
        let lng_set = matches!(self.location_longitude, Patch::Set(_));
        if lat_set ^ lng_set {
            return Err("locationLatitude and locationLongitude must be set together".into());
        }
        Ok(())
    }
}

impl Client {
    /// `PATCH /v1/users/me` — update the authenticated user's profile, returning
    /// the refreshed `User`.
    ///
    /// The live PATCH response is **not** the full `User` the spec implies (it
    /// omits `username` and other fields), so its body is decoded loosely and
    /// discarded; the canonical profile is then re-fetched via `GET
    /// /v1/users/me`, which is the shape the rest of the app already relies on.
    pub async fn update_own_profile(&self, update: &ProfileUpdate) -> Result<crate::users::User> {
        update
            .validate()
            .map_err(|m| ApiError::Config(format!("profile update invalid: {m}")))?;
        if update.is_empty() {
            return self.get_own_profile().await;
        }
        let _: serde_json::Value = self
            .request(
                EndpointKey::UsersUpdateMe,
                Method::PATCH,
                "/v1/users/me",
                &[],
                Some(update),
            )
            .await?;
        self.get_own_profile().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let p = ProfileUpdate::default();
        assert!(p.is_empty());
    }

    #[test]
    fn skip_fields_are_omitted_in_json() {
        let p = ProfileUpdate {
            bio: Patch::Set("hi".into()),
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, r#"{"bio":"hi"}"#);
    }

    #[test]
    fn clear_field_sent_as_null() {
        let p = ProfileUpdate {
            bio: Patch::Clear,
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, r#"{"bio":null}"#);
    }

    #[test]
    fn multi_field_update_serializes_camel_case() {
        let p = ProfileUpdate {
            bio: Patch::Set("new bio".into()),
            display_name: Patch::Set("Alice".into()),
            website_url: Patch::Set("https://example.com".into()),
            location_latitude: Patch::Set(51.5),
            location_longitude: Patch::Set(-0.1),
            location_name: Patch::Set("London".into()),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["bio"], "new bio");
        assert_eq!(v["displayName"], "Alice");
        assert_eq!(v["websiteUrl"], "https://example.com");
        assert!(v.as_object().unwrap().contains_key("locationLatitude"));
    }

    #[test]
    fn bio_length_validated() {
        // v0.4 caps bio at 640 chars.
        let ok = ProfileUpdate {
            bio: Patch::Set("x".repeat(640)),
            ..Default::default()
        };
        assert!(ok.validate().is_ok());

        let too_long = ProfileUpdate {
            bio: Patch::Set("x".repeat(641)),
            ..Default::default()
        };
        assert!(too_long.validate().is_err());
    }

    #[test]
    fn website_url_scheme_validated() {
        let p = ProfileUpdate {
            website_url: Patch::Set("example.com".into()),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn latitude_range_validated() {
        let p = ProfileUpdate {
            location_latitude: Patch::Set(120.0),
            location_longitude: Patch::Set(0.0),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn lat_without_lng_rejected() {
        let p = ProfileUpdate {
            location_latitude: Patch::Set(45.0),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn valid_update_passes_validation() {
        let p = ProfileUpdate {
            bio: Patch::Set("hello".into()),
            display_name: Patch::Clear,
            website_url: Patch::Set("https://x".into()),
            location_latitude: Patch::Set(10.0),
            location_longitude: Patch::Set(20.0),
            ..Default::default()
        };
        assert!(p.validate().is_ok());
    }
}

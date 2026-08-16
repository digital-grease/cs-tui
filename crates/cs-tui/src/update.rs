//! Update check against the project's GitHub releases.
//!
//! Releases are cut by pushing a `v*` tag, which builds the cross-platform
//! binaries and attaches them to a GitHub Release, so the release list is the
//! authoritative answer to "is there a newer cs-tui". This module asks that
//! question once a day and nothing more.
//!
//! Three deliberate limits:
//!
//! - **It never touches the Cyberspace API.** Checking for a client update is
//!   not something the service is for, and the Terms forbid automated requests
//!   that no human drove. This traffic goes to GitHub and nowhere else.
//! - **It uses its own HTTP client**, not [`cs_api::Client`], which carries the
//!   session bearer token. That token must never be sent to another host.
//! - **It never installs anything.** A terminal client that rewrites its own
//!   binary is a trust problem and fights whatever package manager put it there,
//!   so the most this does is report a version and a link.
//!
//! Every failure is silent. The check is a convenience, and a rate-limited or
//! offline lookup that produced an error message would be worse than no check.
use std::time::Duration;

use serde::Deserialize;

/// The release the check reads. `latest` excludes drafts and prereleases, so a
/// beta tag never nags anyone toward it.
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/digital-grease/cs-tui/releases/latest";

/// GitHub rejects unauthenticated requests that do not identify themselves.
/// This is the same string the API client sends.
const USER_AGENT: &str = concat!("cs-tui/", env!("CARGO_PKG_VERSION"));

/// Give up quickly: nothing waits on this, but a socket left hanging would keep
/// the task (and its runtime slot) alive long after it stopped being useful.
const TIMEOUT: Duration = Duration::from_secs(10);

/// How long a check is good for. GitHub allows 60 unauthenticated requests an
/// hour per address; once a day is far inside that and is as often as a release
/// could plausibly matter.
pub const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// A published release newer than the running binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The version as published, without the tag's leading `v` (e.g. `0.4.4`).
    pub version: String,
    /// The release page, for the user to open.
    pub url: String,
}

/// The subset of GitHub's release object worth decoding.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    html_url: String,
}

/// The running binary's version, as compiled in.
#[must_use]
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release page for `version`.
///
/// Releases are cut from a `v`-prefixed tag, so the page is derivable and does
/// not need storing alongside the version. That is what lets a launch inside the
/// once-a-day window still offer the link for a release found earlier, without
/// asking GitHub again.
#[must_use]
pub fn release_url(version: &str) -> String {
    format!(
        "https://github.com/digital-grease/cs-tui/releases/tag/v{}",
        version.trim().trim_start_matches('v')
    )
}

/// A release recorded on an earlier run that is still newer than this binary.
///
/// Without this the menu entry exists only on the launch that discovered the
/// release: every launch for the next 24 hours would skip the check, hold no
/// release, and drop the entry, so the link the announcement pointed at would
/// be gone.
#[must_use]
pub fn remembered(last_seen: Option<&str>, current: &str) -> Option<Release> {
    let version = last_seen?.trim().to_string();
    if !is_newer(&version, current) {
        return None;
    }
    let url = release_url(&version);
    Some(Release { version, url })
}

/// Ask GitHub for the latest release, returning it only when it is newer than
/// `current`.
///
/// `None` covers every uninteresting outcome and they are deliberately not
/// distinguished: already current, offline, rate limited, a malformed tag, or a
/// local build that is ahead of the last release. Callers have nothing useful to
/// do with the difference.
pub async fn check(current: &str) -> Option<Release> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(TIMEOUT)
        .build()
        .ok()?;

    let response = client
        .get(LATEST_RELEASE_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?;

    if !response.status().is_success() {
        tracing::debug!(status = %response.status(), "update check refused");
        return None;
    }

    let release: GithubRelease = response.json().await.ok()?;
    let latest = release.tag_name.trim().trim_start_matches('v');
    if !is_newer(latest, current) {
        return None;
    }
    Some(Release {
        version: latest.to_string(),
        url: release.html_url,
    })
}

/// Whether `latest` is a strictly higher version than `current`.
///
/// Compares numerically, so 0.10.0 beats 0.9.0. A string comparison would get
/// that backwards, which is the classic way this feature goes wrong the first
/// time a minor version reaches double digits.
///
/// Anything unparseable answers `false`, including a prerelease suffix: the
/// endpoint should never hand one back, and staying quiet is the right response
/// to a tag this does not understand.
#[must_use]
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Parse `MAJOR.MINOR.PATCH` into a comparable tuple.
///
/// Rejects a prerelease or build suffix rather than stripping it, so a `-rc1`
/// tag can never be mistaken for the release it precedes.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    if s.contains('-') || s.contains('+') {
        return None;
    }
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_not_lexically() {
        // The bug this exists to prevent: "0.10.0" sorts BEFORE "0.9.0" as text.
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.4.4", "0.4.3"));
    }

    #[test]
    fn the_same_version_is_not_an_update() {
        assert!(!is_newer("0.4.3", "0.4.3"));
    }

    #[test]
    fn a_local_build_ahead_of_the_release_says_nothing() {
        // Developing on an unreleased version must not nag about the older
        // published one.
        assert!(!is_newer("0.4.3", "0.5.0"));
    }

    #[test]
    fn the_leading_v_on_a_tag_is_optional() {
        assert!(is_newer("v0.4.4", "0.4.3"));
        assert!(!is_newer("v0.4.3", "0.4.3"));
    }

    #[test]
    fn an_unparseable_or_prerelease_version_is_ignored() {
        assert!(!is_newer("0.5.0-rc1", "0.4.3"), "a prerelease never nags");
        assert!(!is_newer("0.5.0+build7", "0.4.3"));
        assert!(!is_newer("nightly", "0.4.3"));
        assert!(!is_newer("0.5", "0.4.3"), "too few parts");
        assert!(!is_newer("0.5.0.1", "0.4.3"), "too many parts");
        assert!(!is_newer("", "0.4.3"));
    }

    #[test]
    fn the_compiled_version_parses() {
        // If the crate version ever stops looking like MAJOR.MINOR.PATCH the
        // check would silently never fire, so pin it.
        assert!(
            parse_version(current_version()).is_some(),
            "current version {} is unparseable",
            current_version(),
        );
    }

    #[test]
    fn a_release_payload_decodes() {
        let r: GithubRelease = serde_json::from_str(
            r#"{"tag_name":"v0.4.4","html_url":"https://github.com/x/y/releases/tag/v0.4.4","extra":1}"#,
        )
        .expect("decodes, ignoring fields we do not model");
        assert_eq!(r.tag_name, "v0.4.4");
        assert!(r.html_url.ends_with("v0.4.4"));
    }

    #[test]
    fn a_release_payload_missing_fields_still_decodes() {
        // Absent rather than failing: a shape change must not turn into an
        // error path, it should just mean "no update".
        let r: GithubRelease = serde_json::from_str("{}").expect("decodes");
        assert!(r.tag_name.is_empty());
        assert!(!is_newer(&r.tag_name, current_version()));
    }
}

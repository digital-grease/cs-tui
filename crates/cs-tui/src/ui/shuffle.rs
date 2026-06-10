//! Shuffle mode: continuous random jukebox playback.
//!
//! With shuffle on, a track ending naturally doesn't clear the player — the app
//! picks a random jukebox post and plays that, indefinitely, until the user
//! stops it or turns the mode off. This module owns the candidate pool and the
//! pick logic; the app wires it to playback and to the background refill task.
//!
//! The pool fills from two sources, both read-only and human-driven:
//! - harvesting: every feed/topic/profile page the user browses is scanned for
//!   audio attachments as it arrives, at zero extra API cost;
//! - refilling: when the pool runs low during shuffle, a background task walks
//!   a few pages of the global feed (the API has no audio-only listing, so the
//!   filtering is client-side) through the client's own rate limiter.
//!
//! A refill that finds nothing new marks the pool "dry" so shuffle never loops
//! re-walking the feed head; browsing fresh tracks (or re-enabling shuffle)
//! clears the marker.
use std::collections::{HashSet, VecDeque};

use cs_api::Entry;

use super::audio::{jukebox_track, JukeboxTrack};

/// How many recently played URLs to avoid when picking. Guards against quick
/// repeats, not a full no-repeat cycle; small enough that tiny pools still
/// rotate freely via the eligibility fallbacks in [`ShufflePool::pick_seeded`].
const RECENT_CAP: usize = 8;

/// Hard cap on stored tracks so a marathon session can't grow the pool without
/// bound. The `seen` set still records evicted URLs, so nothing is re-added.
const POOL_CAP: usize = 500;

/// Kick a background refill when the pool drops below this many tracks.
const REFILL_BELOW: usize = 10;

/// Pages of the global feed one refill walk may fetch (at the spec-max 50
/// entries each). Bounded so a music-poor feed never turns shuffle into a
/// crawler; the per-endpoint rate limiter throttles the requests besides.
pub const REFILL_MAX_PAGES: usize = 4;

/// Stop a refill walk early once it has found this many candidate tracks
/// (pre-dedup; the pool drops ones it has already seen).
pub const REFILL_TARGET: usize = 20;

/// The pool of jukebox tracks shuffle picks from, plus the bookkeeping the app
/// needs around the background refill (in-flight flag, feed cursor, the
/// play-on-arrival latch).
pub struct ShufflePool {
    /// Candidate tracks, deduped by URL.
    tracks: Vec<JukeboxTrack>,
    /// Every URL ever admitted — covers tracks evicted by [`POOL_CAP`] too, so
    /// a refill never re-adds what was already considered.
    seen: HashSet<String>,
    /// URLs played most recently (newest last), to avoid quick repeats.
    recent: VecDeque<String>,
    /// Where the next refill walk resumes in the global feed (`None` = top).
    pub cursor: Option<String>,
    /// A refill task is in flight; only one runs at a time.
    pub fetch_inflight: bool,
    /// Start playback as soon as the in-flight refill delivers tracks. Set when
    /// shuffle needed a track but the pool was empty.
    pub pending_play: bool,
    /// The last refill walk added nothing new; suppress further refills until
    /// browsing harvests a fresh track or shuffle is toggled back on.
    refill_dry: bool,
}

impl ShufflePool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tracks: Vec::new(),
            seen: HashSet::new(),
            recent: VecDeque::new(),
            cursor: None,
            fetch_inflight: false,
            pending_play: false,
            refill_dry: false,
        }
    }

    /// Pool size, for test assertions. The app itself never needs the count —
    /// it acts on [`Self::needs_refill`] and what [`Self::pick`] returns.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Scan browsed entries for jukebox tracks and admit the new ones. Called
    /// from the feed/topic/profile page handlers as pages arrive — harvesting
    /// what the user already fetched costs no extra API calls. Returns how many
    /// tracks were new.
    pub fn harvest(&mut self, entries: &[Entry]) -> usize {
        let tracks: Vec<_> = entries
            .iter()
            .filter_map(|e| jukebox_track(&e.attachments))
            .collect();
        self.add_tracks(tracks)
    }

    /// Admit tracks the pool hasn't seen before, up to [`POOL_CAP`]. Any new
    /// track also clears the dry marker — there's fresh material again.
    pub fn add_tracks(&mut self, tracks: Vec<JukeboxTrack>) -> usize {
        let mut added = 0;
        for t in tracks {
            if self.tracks.len() >= POOL_CAP {
                break;
            }
            if self.seen.insert(t.url.clone()) {
                self.tracks.push(t);
                added += 1;
            }
        }
        if added > 0 {
            self.refill_dry = false;
        }
        added
    }

    /// Whether the app should kick a background refill: the pool is running
    /// low, nothing is already fetching, and the last walk wasn't dry.
    #[must_use]
    pub fn needs_refill(&self) -> bool {
        self.tracks.len() < REFILL_BELOW && !self.fetch_inflight && !self.refill_dry
    }

    /// Record a finished refill walk: where to resume next time, and whether it
    /// found anything (a walk that adds nothing marks the pool dry).
    pub fn finish_refill(&mut self, added: usize, next_cursor: Option<String>) {
        self.fetch_inflight = false;
        self.cursor = next_cursor;
        if added == 0 {
            self.refill_dry = true;
        }
    }

    /// Clear the dry marker (used when the user re-enables shuffle, signalling
    /// "try again" even if the last walk found nothing).
    pub fn retry_refills(&mut self) {
        self.refill_dry = false;
    }

    /// Evict a track that turned out to be unplayable (dead link). It stays in
    /// the seen-set, so a refill won't re-admit it; without eviction a small
    /// pool keeps cycling back into the same silent failures.
    pub fn remove(&mut self, url: &str) {
        self.tracks.retain(|t| t.url != url);
    }

    /// Pick a random track using OS-seeded entropy. See [`Self::pick_seeded`].
    pub fn pick(&mut self, current_url: Option<&str>) -> Option<JukeboxTrack> {
        self.pick_seeded(current_url, entropy())
    }

    /// Pick a track at `seed % candidates`, preferring ones that aren't the
    /// current track and weren't played recently. Falls back to ignoring the
    /// recency filter, then the current-track filter, so a pool of any size
    /// always yields something. Records the pick in the recent list.
    pub fn pick_seeded(&mut self, current_url: Option<&str>, seed: u64) -> Option<JukeboxTrack> {
        if self.tracks.is_empty() {
            return None;
        }
        let not_current: Vec<usize> = (0..self.tracks.len())
            .filter(|&i| current_url != Some(self.tracks[i].url.as_str()))
            .collect();
        let fresh: Vec<usize> = not_current
            .iter()
            .copied()
            .filter(|&i| !self.recent.contains(&self.tracks[i].url))
            .collect();
        let candidates = if !fresh.is_empty() {
            fresh
        } else if !not_current.is_empty() {
            not_current
        } else {
            // Pool of one and it's the current track: repeat it rather than
            // going silent — the refill machinery is busy widening the pool.
            (0..self.tracks.len()).collect()
        };
        let idx = candidates[(seed % candidates.len() as u64) as usize];
        let track = self.tracks[idx].clone();
        self.recent.push_back(track.url.clone());
        while self.recent.len() > RECENT_CAP {
            self.recent.pop_front();
        }
        Some(track)
    }

    /// Drop everything (used on logout — the pool is session-scoped).
    pub fn clear(&mut self) {
        *self = Self::new();
    }
}

/// A random `u64` without a rand dependency: `RandomState` is seeded from OS
/// entropy (per thread, then differentiated per instance), so the finish value
/// of an empty hasher differs per call. Plenty for picking a shuffle index.
fn entropy() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_api::Attachment;

    fn track(url: &str) -> JukeboxTrack {
        JukeboxTrack {
            url: url.into(),
            artist: "a".into(),
            title: "t".into(),
        }
    }

    fn audio_entry(url: &str) -> Entry {
        Entry {
            attachments: vec![Attachment::Audio {
                src: url.into(),
                origin: "youtube".into(),
                artist: "a".into(),
                title: "t".into(),
                genre: String::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn harvest_admits_audio_entries_and_dedups() {
        let mut pool = ShufflePool::new();
        let entries = vec![
            audio_entry("https://youtu.be/one"),
            Entry::default(), // no attachment
            audio_entry("https://youtu.be/two"),
            audio_entry("https://youtu.be/one"), // dup within the page
        ];
        assert_eq!(pool.harvest(&entries), 2);
        // A second pass over the same page adds nothing.
        assert_eq!(pool.harvest(&entries), 0);
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn add_tracks_caps_the_pool_but_remembers_evictions() {
        let mut pool = ShufflePool::new();
        let many: Vec<_> = (0..POOL_CAP + 50)
            .map(|i| track(&format!("https://youtu.be/v{i}")))
            .collect();
        assert_eq!(pool.add_tracks(many.clone()), POOL_CAP);
        assert_eq!(pool.len(), POOL_CAP);
        // Everything under the cap was seen; re-adding stays a no-op.
        assert_eq!(pool.add_tracks(many[..10].to_vec()), 0);
    }

    #[test]
    fn pick_avoids_current_and_recent_tracks() {
        let mut pool = ShufflePool::new();
        pool.add_tracks(vec![track("u1"), track("u2"), track("u3")]);
        // Whatever the seed, the current track must not be picked while
        // alternatives exist.
        for seed in 0..20 {
            let mut p = ShufflePool::new();
            p.add_tracks(vec![track("u1"), track("u2"), track("u3")]);
            let got = p.pick_seeded(Some("u2"), seed).expect("non-empty pool");
            assert_ne!(got.url, "u2", "seed {seed} picked the current track");
        }
        // Recent tracks are skipped while fresh ones remain: after playing u1
        // and u2, every pick must be u3.
        let first = pool.pick_seeded(Some("zzz"), 0).unwrap();
        let second = pool
            .pick_seeded(Some(first.url.as_str()), 0)
            .map(|t| t.url)
            .unwrap();
        let third = pool.pick_seeded(Some(second.as_str()), 7).unwrap();
        assert_ne!(third.url, first.url);
        assert_ne!(third.url, second);
    }

    #[test]
    fn pick_falls_back_when_everything_is_recent_or_current() {
        // Pool of one, and it's the current track: repeat rather than go silent.
        let mut pool = ShufflePool::new();
        pool.add_tracks(vec![track("only")]);
        let got = pool
            .pick_seeded(Some("only"), 3)
            .expect("repeats the track");
        assert_eq!(got.url, "only");

        // Two tracks, both recent: recency is waived, current still avoided.
        let mut pool = ShufflePool::new();
        pool.add_tracks(vec![track("a"), track("b")]);
        for seed in 0..4 {
            let _ = pool.pick_seeded(None, seed);
        }
        let got = pool.pick_seeded(Some("a"), 1).unwrap();
        assert_eq!(got.url, "b");
    }

    #[test]
    fn pick_returns_none_on_an_empty_pool() {
        let mut pool = ShufflePool::new();
        assert!(pool.pick_seeded(None, 42).is_none());
        assert!(pool.pick(None).is_none());
    }

    #[test]
    fn dry_refills_suppress_until_new_material_arrives() {
        let mut pool = ShufflePool::new();
        assert!(pool.needs_refill(), "empty pool wants a refill");
        pool.fetch_inflight = true;
        assert!(!pool.needs_refill(), "not while one is in flight");
        pool.finish_refill(0, None);
        assert!(!pool.fetch_inflight);
        assert!(!pool.needs_refill(), "dry walk suppresses re-walking");
        // Harvesting something new clears the dry marker.
        pool.harvest(&[audio_entry("https://youtu.be/new")]);
        assert!(pool.needs_refill(), "fresh material re-arms refills");
        // As does an explicit retry (shuffle re-toggled).
        pool.finish_refill(0, None);
        pool.retry_refills();
        assert!(pool.needs_refill());
    }

    #[test]
    fn finish_refill_threads_the_cursor() {
        let mut pool = ShufflePool::new();
        pool.fetch_inflight = true;
        pool.finish_refill(3, Some("c123".into()));
        assert_eq!(pool.cursor.as_deref(), Some("c123"));
        // 3 > 0, so the pool isn't dry even though we added via finish (the
        // caller adds tracks separately; `added` is what it reports).
        assert!(pool.needs_refill());
    }

    #[test]
    fn remove_evicts_the_track_but_keeps_it_seen() {
        let mut pool = ShufflePool::new();
        pool.add_tracks(vec![track("dead"), track("ok")]);
        pool.remove("dead");
        assert_eq!(pool.len(), 1);
        // The dead link must never be picked again…
        for seed in 0..6 {
            assert_eq!(pool.pick_seeded(None, seed).unwrap().url, "ok");
        }
        // …and a refill can't re-admit it either.
        assert_eq!(pool.add_tracks(vec![track("dead")]), 0);
    }

    #[test]
    fn clear_resets_everything() {
        let mut pool = ShufflePool::new();
        pool.add_tracks(vec![track("x")]);
        pool.pending_play = true;
        pool.fetch_inflight = true;
        pool.cursor = Some("c".into());
        pool.clear();
        assert!(pool.is_empty());
        assert!(!pool.pending_play);
        assert!(!pool.fetch_inflight);
        assert!(pool.cursor.is_none());
        // The seen-set is gone too: the same track can be admitted again.
        assert_eq!(pool.add_tracks(vec![track("x")]), 1);
    }

    #[test]
    fn entropy_varies_between_calls() {
        // Not a randomness-quality test — just that successive calls don't
        // return one constant (which would make shuffle deterministic).
        let vals: HashSet<u64> = (0..8).map(|_| entropy()).collect();
        assert!(vals.len() > 1, "entropy() returned a constant");
    }
}

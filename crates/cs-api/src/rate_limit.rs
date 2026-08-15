//! Client-side rate limiting that mirrors the server's rolling windows.
//!
//! Independent windows per (endpoint, scope): one each for the per-minute,
//! per-hour, and per-day limits that combination declares. `acquire()` waits
//! until every present window can admit one more request, then records the grant
//! in all of them atomically. Uses `std::sync::Mutex` (never held across
//! `.await`).
//!
//! The model is the one § Rate Limits documents: "Limits use a rolling window
//! (24 hours for daily, 60 seconds for per-minute)." Each window therefore keeps
//! the grant times of the requests it let through and admits another only while
//! fewer than `capacity` of them lie inside the trailing span. A refilling token
//! bucket would hand a token back partway through the window, which the server
//! never does: spend a 2/min budget at t=0 and a bucket dripping at 2/60 per
//! second is writable again at t=30, while the server's window ending at t=30
//! still holds both requests and answers `429`.
//!
//! Some v0.8.4 limits are two-dimensional: cIRC presence is 15/min *per room*
//! and 90/min overall, C-Mail typing 40/min *per conversation* and 120/min
//! overall. A call to one of those endpoints draws from two budgets at once, the
//! scoped one and the overall one, and both have to admit it before the request
//! goes out.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::endpoint::EndpointKey;

/// How many distinct scopes (rooms, conversations) may be tracked before the
/// limiter prunes the ones that owe nothing. See [`EndpointLimiter`].
const MAX_TRACKED_SCOPES: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct RateLimit {
    pub per_minute: Option<u32>,
    pub per_hour: Option<u32>,
    pub per_day: Option<u32>,
}

impl RateLimit {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            per_minute: None,
            per_hour: None,
            per_day: None,
        }
    }

    #[must_use]
    pub const fn per_minute(n: u32) -> Self {
        Self {
            per_minute: Some(n),
            per_hour: None,
            per_day: None,
        }
    }

    #[must_use]
    pub const fn with_day(per_minute: u32, per_day: u32) -> Self {
        Self {
            per_minute: Some(per_minute),
            per_hour: None,
            per_day: Some(per_day),
        }
    }

    /// A limit with no per-minute window at all, only an hourly and a daily
    /// one. Used by Poke, which v0.8.4 caps at 1/hour and 8/day (§ Poke a User)
    /// and which the write table leaves blank in the per-minute column.
    #[must_use]
    pub const fn per_hour_with_day(per_hour: u32, per_day: u32) -> Self {
        Self {
            per_minute: None,
            per_hour: Some(per_hour),
            per_day: Some(per_day),
        }
    }

    /// A minute-plus-hour limit with no daily cap. Used by the two auth-side
    /// endpoints that declare exactly those two windows: `POST
    /// /v1/auth/check-username` at 10/min and 60/hour (§ Check Username
    /// Availability) and `POST /v1/auth/resend-verification` at 1/min and
    /// 5/hour (§ Resend Verification Email).
    #[must_use]
    pub const fn per_minute_with_hour(per_minute: u32, per_hour: u32) -> Self {
        Self {
            per_minute: Some(per_minute),
            per_hour: Some(per_hour),
            per_day: None,
        }
    }

    /// A limit that declares all three windows, minute, hour, and day. Used by
    /// the C-Mail write endpoints, whose hourly cap is stricter than
    /// `per_minute * 60` and so has to be modelled explicitly.
    #[must_use]
    pub const fn full(per_minute: u32, per_hour: u32, per_day: u32) -> Self {
        Self {
            per_minute: Some(per_minute),
            per_hour: Some(per_hour),
            per_day: Some(per_day),
        }
    }
}

/// The source of "now" for every window in the limiter.
///
/// Production always uses [`Clock::System`]. Tests use the manual variant, which
/// moves only when `advance` is called, so a test can wind a rolling window past
/// a minute (or an hour) without sleeping for one.
#[derive(Debug)]
enum Clock {
    System,
    #[cfg(test)]
    Manual {
        base: Instant,
        offset_nanos: std::sync::atomic::AtomicU64,
    },
}

impl Clock {
    fn now(&self) -> Instant {
        match self {
            Self::System => Instant::now(),
            #[cfg(test)]
            Self::Manual { base, offset_nanos } => {
                let nanos = offset_nanos.load(std::sync::atomic::Ordering::Relaxed);
                *base + Duration::from_nanos(nanos)
            }
        }
    }

    #[cfg(test)]
    fn advance(&self, by: Duration) {
        match self {
            Self::System => panic!("only a manual clock can be advanced"),
            Self::Manual { offset_nanos, .. } => {
                let nanos = u64::try_from(by.as_nanos()).expect("test advance fits in u64 nanos");
                offset_nanos.fetch_add(nanos, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

/// One rolling window: the times of the requests let through inside a trailing
/// span, capped at `capacity`.
///
/// Per § Rate Limits the server counts requests over a trailing span rather than
/// refilling an allowance gradually, so this records grant times and admits a
/// request only while fewer than `capacity` of them lie inside the span ending
/// now. `capacity` is at most 300 (the cIRC and C-Mail daily caps), so the
/// record is a few kilobytes at worst.
#[derive(Debug)]
struct Window {
    capacity: usize,
    span: Duration,
    grants: VecDeque<Instant>,
}

impl Window {
    fn new(capacity: u32, span: Duration) -> Self {
        Self {
            capacity: capacity as usize,
            span,
            grants: VecDeque::new(),
        }
    }

    /// Index of the first grant still inside the window ending at `now`.
    /// Grants are recorded in time order, so everything before it has aged out.
    fn first_live(&self, now: Instant) -> usize {
        self.grants
            .iter()
            .position(|at| now.saturating_duration_since(*at) < self.span)
            .unwrap_or(self.grants.len())
    }

    /// How many grants the window ending at `now` still holds.
    fn live_at(&self, now: Instant) -> usize {
        self.grants.len() - self.first_live(now)
    }

    /// Drop the grants that have aged out of the window ending at `now`.
    fn expire(&mut self, now: Instant) {
        for _ in 0..self.first_live(now) {
            self.grants.pop_front();
        }
    }

    /// How long until this window could admit one more request, evaluated at
    /// `now` without mutating anything. Zero while it is under capacity,
    /// otherwise the time until its oldest live grant ages out.
    fn wait_at(&self, now: Instant) -> Duration {
        let first = self.first_live(now);
        if self.grants.len() - first < self.capacity {
            return Duration::ZERO;
        }
        self.grants.get(first).map_or(Duration::ZERO, |oldest| {
            (*oldest + self.span).saturating_duration_since(now)
        })
    }

    /// Record one granted request at `now`.
    fn record(&mut self, now: Instant) {
        self.grants.push_back(now);
    }

    /// Remove the most recent grant, for a request the server never counted.
    /// A no-op on an empty record: a refund can never invent a grant.
    fn undo_last(&mut self) {
        self.grants.pop_back();
    }
}

/// Every window governing one budget, plus any penalty the server has imposed.
///
/// Any of the three window slots may be `None` when the endpoint declares no
/// limit for that span; a budget with none of them is unlimited but still
/// carries the penalty, which is what lets a `429` gate an endpoint the spec
/// documents as uncapped.
#[derive(Debug)]
struct Budget {
    minute: Option<Window>,
    hour: Option<Window>,
    day: Option<Window>,
    /// Set from a server `429`: nothing goes out on this budget before then,
    /// whatever the windows say. See [`EndpointLimiter::penalise`].
    penalty_until: Option<Instant>,
}

impl Budget {
    fn new(limit: RateLimit) -> Self {
        Self {
            minute: limit
                .per_minute
                .map(|c| Window::new(c, Duration::from_secs(60))),
            hour: limit
                .per_hour
                .map(|c| Window::new(c, Duration::from_secs(3_600))),
            day: limit
                .per_day
                .map(|c| Window::new(c, Duration::from_secs(86_400))),
            penalty_until: None,
        }
    }

    fn windows(&self) -> impl Iterator<Item = &Window> {
        [&self.minute, &self.hour, &self.day].into_iter().flatten()
    }

    fn windows_mut(&mut self) -> impl Iterator<Item = &mut Window> {
        [&mut self.minute, &mut self.hour, &mut self.day]
            .into_iter()
            .flatten()
    }

    /// The longest wait any window or the penalty imposes at `now`, without
    /// mutating anything.
    fn wait_at(&self, now: Instant) -> Duration {
        let mut wait = match self.penalty_until {
            Some(until) => until.saturating_duration_since(now),
            None => Duration::ZERO,
        };
        for window in self.windows() {
            wait = wait.max(window.wait_at(now));
        }
        wait
    }

    /// Drop everything that has aged out by `now`, then report [`Budget::wait_at`].
    fn expire_and_wait(&mut self, now: Instant) -> Duration {
        if matches!(self.penalty_until, Some(until) if until <= now) {
            self.penalty_until = None;
        }
        for window in self.windows_mut() {
            window.expire(now);
        }
        self.wait_at(now)
    }

    /// Record one granted request in every window. Only ever called after
    /// [`Budget::expire_and_wait`] reported zero for this budget.
    fn record(&mut self, now: Instant) {
        for window in self.windows_mut() {
            window.record(now);
        }
    }

    /// Take back the most recent grant in every window, for a request the
    /// server never counted.
    fn undo_last(&mut self) {
        for window in self.windows_mut() {
            window.undo_last();
        }
    }

    /// Whether this budget now holds nothing an observer could distinguish from
    /// one that has never been used: no live grants and no live penalty.
    fn is_idle(&self, now: Instant) -> bool {
        !matches!(self.penalty_until, Some(until) if until > now)
            && self.windows().all(|w| w.live_at(now) == 0)
    }
}

/// Map key for one budget: an endpoint plus which of its budgets this is.
///
/// `scope: None` is the overall (per-endpoint) budget, which every call draws
/// from. `scope: Some(id)` is the extra per-room / per-conversation budget that
/// only the endpoints in [`EndpointKey::scoped_rate_limit`] declare, keyed by
/// the opaque server id (a `roomId` or a `conversationId`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BudgetKey {
    endpoint: EndpointKey,
    scope: Option<String>,
}

impl BudgetKey {
    fn overall(endpoint: EndpointKey) -> Self {
        Self {
            endpoint,
            scope: None,
        }
    }

    fn scoped(endpoint: EndpointKey, scope: &str) -> Self {
        Self {
            endpoint,
            scope: Some(scope.to_string()),
        }
    }
}

/// The scoped budget key for a call, if the endpoint declares a per-scope limit
/// *and* the caller supplied an id. An endpoint with no per-scope dimension
/// ignores any scope it is handed, and a scoped endpoint called without an id
/// falls back to the overall budget alone.
fn scoped_key(key: EndpointKey, scope: Option<&str>) -> Option<BudgetKey> {
    match (scope, key.scoped_rate_limit()) {
        (Some(id), Some(_)) => Some(BudgetKey::scoped(key, id)),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct EndpointLimiter {
    budgets: Mutex<HashMap<BudgetKey, Budget>>,
    clock: Clock,
}

impl EndpointLimiter {
    pub fn new() -> Self {
        Self {
            budgets: Mutex::new(HashMap::new()),
            clock: Clock::System,
        }
    }

    /// A limiter whose clock only moves when [`EndpointLimiter::advance`] is
    /// called. Test-only: it makes the rolling windows testable without
    /// sleeping. Do not `acquire` a drained budget on one, that would spin.
    #[cfg(test)]
    pub fn manual() -> Self {
        Self {
            budgets: Mutex::new(HashMap::new()),
            clock: Clock::Manual {
                base: Instant::now(),
                offset_nanos: std::sync::atomic::AtomicU64::new(0),
            },
        }
    }

    /// Wind a manual limiter's clock forward. Test-only; panics on a limiter
    /// built with [`EndpointLimiter::new`].
    #[cfg(test)]
    pub fn advance(&self, by: Duration) {
        self.clock.advance(by);
    }

    /// How many grants the per-minute window of `key` still holds, for the
    /// overall budget (`scope: None`) or a scoped one. `None` when that budget
    /// has never been touched, so a test can tell "not charged" from "no such
    /// budget". Test-only: the call-site tests use it to prove which budget a
    /// request drew on.
    #[cfg(test)]
    pub fn live_minute_grants(&self, key: EndpointKey, scope: Option<&str>) -> Option<usize> {
        let budgets = self.budgets.lock().expect("rate-limit mutex poisoned");
        let now = self.clock.now();
        let budget_key = match scope {
            Some(id) => BudgetKey::scoped(key, id),
            None => BudgetKey::overall(key),
        };
        budgets
            .get(&budget_key)
            .and_then(|b| b.minute.as_ref())
            .map(|w| w.live_at(now))
    }

    /// Block until every budget that governs `key` (minute, hour, day, as
    /// declared) can admit one more request, then record the grant in all of
    /// them. Returns immediately if the endpoint has no rate limit and no live
    /// penalty.
    ///
    /// `scope` is the opaque id an endpoint's per-scope budget is keyed on (a
    /// `roomId` for cIRC presence, a `conversationId` for C-Mail typing), and
    /// `None` for the endpoints with no second dimension, which is most of
    /// them. When both budgets apply, both must admit the call and the grant is
    /// all-or-nothing.
    pub async fn acquire(&self, key: EndpointKey, scope: Option<&str>) {
        loop {
            let wait = self.try_take_or_wait(key, scope);
            if wait.is_zero() {
                return;
            }
            tokio::time::sleep(wait).await;
        }
    }

    /// Record a grant if every budget governing `key` can admit one right now,
    /// and report whether it did. Never waits.
    ///
    /// For the caller that has somewhere better to be than blocked: the 429
    /// retry in the request layer uses it so a retry cannot go out on a budget
    /// that has nothing left, and cannot block the request for the hour it
    /// would take `acquire` to find a grant on a 1/hour endpoint either.
    pub fn try_acquire(&self, key: EndpointKey, scope: Option<&str>) -> bool {
        self.try_take_or_wait(key, scope).is_zero()
    }

    /// Non-mutating estimate of the wait until `key` in `scope` could send
    /// again, without consuming anything. Zero if writable now, or if the
    /// endpoint is unlimited or has never been touched (an untouched budget
    /// holds no grants). For a scoped call this is the longer of the scoped and
    /// the overall wait, since the call has to satisfy both.
    pub fn peek_wait(&self, key: EndpointKey, scope: Option<&str>) -> Duration {
        let budgets = self.budgets.lock().expect("rate-limit mutex poisoned");
        let now = self.clock.now();
        let overall = budgets
            .get(&BudgetKey::overall(key))
            .map_or(Duration::ZERO, |b| b.wait_at(now));
        let scoped = scoped_key(key, scope)
            .and_then(|sk| budgets.get(&sk).map(|b| b.wait_at(now)))
            .unwrap_or(Duration::ZERO);
        overall.max(scoped)
    }

    /// Take back the most recent grant in both the scoped and the overall
    /// budget for `key`.
    ///
    /// Used when the server rejects a request without charging it: a poke that
    /// comes back `400`/`403`/`404` doesn't count against the poke budget
    /// (§ Poke a User), so neither should our local mirror of it. A refund on a
    /// budget holding no grants does nothing, so a stray refund can never mint
    /// an allowance the server did not give.
    pub fn refund(&self, key: EndpointKey, scope: Option<&str>) {
        let mut budgets = self.budgets.lock().expect("rate-limit mutex poisoned");
        if let Some(budget) = budgets.get_mut(&BudgetKey::overall(key)) {
            budget.undo_last();
        }
        if let Some(sk) = scoped_key(key, scope) {
            if let Some(budget) = budgets.get_mut(&sk) {
                budget.undo_last();
            }
        }
    }

    /// Record a server-side rejection: the server answered `429` for `key`, so
    /// nothing more may go out on that endpoint until `retry_after` has passed.
    ///
    /// The windows model the limits the spec documents, but the server is the
    /// authority and a `429` is the only evidence that the two have drifted:
    /// another client on the same account, an endpoint the spec leaves
    /// uncapped, a stricter limit than the one documented. Without this the
    /// limiter would keep answering "writable now" and the next call would fire
    /// straight into another `429`.
    ///
    /// The penalty lands on the endpoint's overall budget, not on the scoped
    /// one, even for a scoped call: the response says nothing about which of
    /// the two dimensions was tripped, and holding the whole endpoint is the
    /// only reading that cannot immediately earn a second `429`. An existing
    /// penalty is never shortened.
    pub fn penalise(&self, key: EndpointKey, retry_after: Duration) {
        let mut budgets = self.budgets.lock().expect("rate-limit mutex poisoned");
        let until = self.clock.now() + retry_after;
        let budget = budgets
            .entry(BudgetKey::overall(key))
            .or_insert_with(|| Budget::new(key.rate_limit()));
        let extend = match budget.penalty_until {
            Some(current) => current < until,
            None => true,
        };
        if extend {
            budget.penalty_until = Some(until);
        }
    }

    /// One all-or-nothing attempt. Returns `Duration::ZERO` having recorded a
    /// grant in every budget involved, or the wait until another attempt could
    /// succeed.
    ///
    /// INVARIANT: a scoped call must never record a grant in one budget while
    /// the other cannot admit it. Spending the overall grant while the room's
    /// budget is exhausted (or the reverse) would burn allowance on a request
    /// that never goes out, and since `acquire` retries in a loop, a client
    /// blocked on one busy room would slowly drain the overall allowance it
    /// never got to use. Both budgets are therefore inspected and charged
    /// inside a single lock acquisition, and nothing is charged unless both can
    /// pay.
    fn try_take_or_wait(&self, key: EndpointKey, scope: Option<&str>) -> Duration {
        let mut budgets = self.budgets.lock().expect("rate-limit mutex poisoned");
        let now = self.clock.now();
        let overall_key = BudgetKey::overall(key);
        let scoped = scoped_key(key, scope);

        if scoped.is_some() {
            evict_idle_scopes(&mut budgets, now);
        }

        budgets
            .entry(overall_key.clone())
            .or_insert_with(|| Budget::new(key.rate_limit()));
        if let Some(sk) = &scoped {
            let limit = key
                .scoped_rate_limit()
                .expect("scoped_key only yields a key for endpoints with a scoped limit");
            budgets
                .entry(sk.clone())
                .or_insert_with(|| Budget::new(limit));
        }

        // Two passes rather than one: `HashMap` hands out a single mutable
        // borrow at a time, so the waits are measured first and the grants are
        // recorded only once both budgets have agreed to pay.
        let overall_wait = budgets
            .get_mut(&overall_key)
            .expect("overall budget just inserted")
            .expire_and_wait(now);
        let scoped_wait = match &scoped {
            Some(sk) => budgets
                .get_mut(sk)
                .expect("scoped budget just inserted")
                .expire_and_wait(now),
            None => Duration::ZERO,
        };

        let wait = overall_wait.max(scoped_wait);
        if !wait.is_zero() {
            return wait;
        }

        budgets
            .get_mut(&overall_key)
            .expect("overall budget just inserted")
            .record(now);
        if let Some(sk) = &scoped {
            budgets
                .get_mut(sk)
                .expect("scoped budget just inserted")
                .record(now);
        }
        Duration::ZERO
    }
}

/// Bound the scoped half of the map.
///
/// The endpoint-keyed half is bounded by the `EndpointKey` enum; the scoped half
/// is keyed by server-supplied ids (rooms, conversations) and so is unbounded
/// for the first time. Prune it by dropping scope entries that are idle, meaning
/// every grant they hold has aged out of its window and no penalty is live. That
/// is behaviourally identical to keeping them: a missing key is recreated empty
/// on next use (see `try_take_or_wait`) and `peek_wait` reports zero for a
/// missing key exactly as it does for an idle one, so an idle entry holds no
/// accounting anybody can observe. Entries that still hold grants are kept,
/// whatever the map size.
fn evict_idle_scopes(budgets: &mut HashMap<BudgetKey, Budget>, now: Instant) {
    let tracked = budgets.keys().filter(|k| k.scope.is_some()).count();
    if tracked <= MAX_TRACKED_SCOPES {
        return;
    }
    budgets.retain(|k, budget| k.scope.is_none() || !budget.is_idle(now));
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    /// Grants live in the per-minute window of a budget that must exist.
    fn minute_grants(limiter: &EndpointLimiter, key: EndpointKey, scope: Option<&str>) -> usize {
        limiter
            .live_minute_grants(key, scope)
            .expect("budget should exist with a per-minute window")
    }

    #[tokio::test]
    async fn unlimited_endpoint_returns_immediately() {
        let limiter = EndpointLimiter::new();
        let start = Instant::now();
        limiter.acquire(EndpointKey::AuthLogin, None).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn a_spent_window_stays_shut_for_the_whole_span() {
        // EntriesCreate is 2/min. § Rate Limits makes that a rolling window, so
        // both requests keep counting until each is a full minute old. A token
        // bucket dripping at 2/60 per second would have handed one back at
        // t=30s and let a third request through, into the server's 429.
        let limiter = EndpointLimiter::manual();
        limiter.acquire(EndpointKey::EntriesCreate, None).await; // t = 0s
        limiter.advance(Duration::from_secs(1));
        limiter.acquire(EndpointKey::EntriesCreate, None).await; // t = 1s

        limiter.advance(Duration::from_secs(29)); // t = 30s
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesCreate, None),
            Duration::from_secs(30),
            "the window ending at t=30s still holds both requests"
        );
        assert!(
            !limiter
                .try_take_or_wait(EndpointKey::EntriesCreate, None)
                .is_zero(),
            "and a third request must not go out"
        );

        limiter.advance(Duration::from_secs(30)); // t = 60s
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesCreate, None),
            Duration::ZERO,
            "the t=0s request has aged out"
        );
        limiter.acquire(EndpointKey::EntriesCreate, None).await;
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesCreate, None),
            Duration::from_secs(1),
            "only one slot came free: the t=1s request ages out at t=61s"
        );
    }

    #[tokio::test]
    async fn peek_wait_estimates_without_consuming() {
        let limiter = EndpointLimiter::manual();
        // Untouched budget holds no grants, so it is writable now.
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesCreate, None),
            Duration::ZERO
        );
        // Unlimited endpoints are always writable.
        assert_eq!(
            limiter.peek_wait(EndpointKey::AuthLogin, None),
            Duration::ZERO
        );

        // Spend the 2/min budget.
        limiter.acquire(EndpointKey::EntriesCreate, None).await;
        limiter.acquire(EndpointKey::EntriesCreate, None).await;

        // Peek reports the wait and, crucially, does NOT consume, so two reads
        // in a row return the same (non-zero) estimate.
        let w1 = limiter.peek_wait(EndpointKey::EntriesCreate, None);
        let w2 = limiter.peek_wait(EndpointKey::EntriesCreate, None);
        assert_eq!(w1, MINUTE, "expected the full window, got {w1:?}");
        assert_eq!(w2, MINUTE, "peek must not consume a grant");
        assert_eq!(minute_grants(&limiter, EndpointKey::EntriesCreate, None), 2);
    }

    #[tokio::test]
    async fn hourly_cap_gates_even_when_the_minute_window_is_clear() {
        // CmailSend declares 15/min, 150/hour, 300/day. Modelling the hourly
        // window matters because 15/min * 60 = 900 would let the client blow
        // past the 150/hour server cap. Spend the hour's allowance 15 at a
        // time, a minute apart, so the minute window is empty at the end and
        // only the hour window can be gating.
        let limiter = EndpointLimiter::manual();
        for _ in 0..10 {
            for _ in 0..15 {
                limiter.acquire(EndpointKey::CmailSend, None).await;
            }
            limiter.advance(MINUTE);
        }
        assert_eq!(
            minute_grants(&limiter, EndpointKey::CmailSend, None),
            0,
            "the minute window has drained"
        );
        let wait = limiter.peek_wait(EndpointKey::CmailSend, None);
        assert!(
            wait > Duration::from_secs(2_000),
            "the 150 sends this hour must gate the 151st, got {wait:?}"
        );
    }

    #[tokio::test]
    async fn scoped_budget_blocks_its_own_scope_only() {
        // cIRC presence: 15/min per room, 90/min overall.
        let limiter = EndpointLimiter::manual();
        for _ in 0..15 {
            limiter
                .acquire(EndpointKey::CircPresence, Some("general"))
                .await;
        }

        assert_eq!(
            limiter.peek_wait(EndpointKey::CircPresence, Some("general")),
            MINUTE,
            "the room's own budget is spent for the rest of the window"
        );
        assert_eq!(
            limiter.peek_wait(EndpointKey::CircPresence, Some("lounge")),
            Duration::ZERO,
            "a different room has its own 15/min budget"
        );
    }

    #[tokio::test]
    async fn overall_budget_blocks_every_scope() {
        // 6 rooms * 15 heartbeats = the whole 90/min overall allowance.
        let limiter = EndpointLimiter::manual();
        for room in 0..6 {
            let id = format!("room{room}");
            for _ in 0..15 {
                limiter.acquire(EndpointKey::CircPresence, Some(&id)).await;
            }
        }
        assert_eq!(
            limiter.peek_wait(EndpointKey::CircPresence, Some("untouched-room")),
            MINUTE,
            "the overall budget gates a room that has spent nothing"
        );
    }

    #[tokio::test]
    async fn no_grant_is_recorded_when_only_the_scoped_budget_is_spent() {
        let limiter = EndpointLimiter::manual();
        for _ in 0..15 {
            limiter
                .acquire(EndpointKey::CircPresence, Some("general"))
                .await;
        }
        let overall_before = minute_grants(&limiter, EndpointKey::CircPresence, None);
        assert_eq!(overall_before, 15);

        let wait = limiter.try_take_or_wait(EndpointKey::CircPresence, Some("general"));
        assert!(!wait.is_zero(), "the room's budget is spent");

        assert_eq!(
            minute_grants(&limiter, EndpointKey::CircPresence, None),
            overall_before,
            "the overall budget must not be charged for a call that never went out"
        );
    }

    #[tokio::test]
    async fn no_grant_is_recorded_when_only_the_overall_budget_is_spent() {
        let limiter = EndpointLimiter::manual();
        for room in 0..6 {
            let id = format!("room{room}");
            for _ in 0..15 {
                limiter.acquire(EndpointKey::CircPresence, Some(&id)).await;
            }
        }

        // A room that has spent nothing still can't send: the overall budget is
        // spent, and its own budget must keep every one of its 15 slots.
        let wait = limiter.try_take_or_wait(EndpointKey::CircPresence, Some("fresh-room"));
        assert!(!wait.is_zero(), "the overall budget is spent");
        assert_eq!(
            minute_grants(&limiter, EndpointKey::CircPresence, Some("fresh-room")),
            0,
            "the room's own budget must be untouched"
        );
    }

    #[tokio::test]
    async fn refund_restores_both_budgets() {
        let limiter = EndpointLimiter::manual();
        limiter.acquire(EndpointKey::CmailTyping, Some("c1")).await;
        assert_eq!(minute_grants(&limiter, EndpointKey::CmailTyping, None), 1);
        assert_eq!(
            minute_grants(&limiter, EndpointKey::CmailTyping, Some("c1")),
            1
        );

        limiter.refund(EndpointKey::CmailTyping, Some("c1"));
        assert_eq!(
            minute_grants(&limiter, EndpointKey::CmailTyping, None),
            0,
            "the overall budget is whole again"
        );
        assert_eq!(
            minute_grants(&limiter, EndpointKey::CmailTyping, Some("c1")),
            0,
            "the conversation's budget is whole again"
        );
    }

    #[tokio::test]
    async fn refund_takes_back_the_most_recent_grant_only() {
        // Two sends a minute apart: refunding the second must leave the first
        // counting until its own window closes, not shift it.
        let limiter = EndpointLimiter::manual();
        limiter.acquire(EndpointKey::EntriesCreate, None).await; // t = 0s
        limiter.advance(Duration::from_secs(30));
        limiter.acquire(EndpointKey::EntriesCreate, None).await; // t = 30s
        limiter.refund(EndpointKey::EntriesCreate, None);

        assert_eq!(minute_grants(&limiter, EndpointKey::EntriesCreate, None), 1);
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesCreate, None),
            Duration::ZERO,
            "one of the two slots is free again"
        );
        limiter.acquire(EndpointKey::EntriesCreate, None).await; // t = 30s
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesCreate, None),
            Duration::from_secs(30),
            "the surviving t=0s grant still ages out on its own schedule"
        );
    }

    #[tokio::test]
    async fn refund_restores_an_unscoped_budget() {
        // Poke is 1/hour, 8/day and unscoped; a rejected poke is refunded.
        let limiter = EndpointLimiter::manual();
        limiter.acquire(EndpointKey::UsersPoke, None).await;
        assert!(
            limiter.peek_wait(EndpointKey::UsersPoke, None) > Duration::from_secs(60),
            "the hourly poke slot is spent"
        );

        limiter.refund(EndpointKey::UsersPoke, None);
        assert_eq!(
            limiter.peek_wait(EndpointKey::UsersPoke, None),
            Duration::ZERO,
            "a rejected poke doesn't count against the budget"
        );
    }

    #[tokio::test]
    async fn refund_cannot_invent_a_grant() {
        let limiter = EndpointLimiter::manual();
        limiter.acquire(EndpointKey::EntriesCreate, None).await;
        limiter.refund(EndpointKey::EntriesCreate, None);
        // The extra refunds have nothing left to take back and must not create
        // headroom the server never granted.
        limiter.refund(EndpointKey::EntriesCreate, None);
        limiter.refund(EndpointKey::EntriesCreate, None);
        assert_eq!(minute_grants(&limiter, EndpointKey::EntriesCreate, None), 0);

        limiter.acquire(EndpointKey::EntriesCreate, None).await;
        limiter.acquire(EndpointKey::EntriesCreate, None).await;
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesCreate, None),
            MINUTE,
            "still only 2/min after the stray refunds"
        );
    }

    #[tokio::test]
    async fn a_scope_on_an_unscoped_endpoint_is_ignored() {
        // CircSend has no per-room dimension, so passing a room id must not
        // create a second budget the endpoint doesn't have.
        let limiter = EndpointLimiter::manual();
        limiter
            .acquire(EndpointKey::CircSend, Some("general"))
            .await;
        let budgets = limiter.budgets.lock().unwrap();
        assert_eq!(budgets.len(), 1);
        assert!(budgets.contains_key(&BudgetKey::overall(EndpointKey::CircSend)));
    }

    #[tokio::test]
    async fn a_server_429_gates_an_endpoint_the_spec_leaves_uncapped() {
        let limiter = EndpointLimiter::manual();
        // EntriesGet declares no limit at all, so only the server can say stop.
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesGet, None),
            Duration::ZERO
        );

        limiter.penalise(EndpointKey::EntriesGet, Duration::from_secs(30));
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesGet, None),
            Duration::from_secs(30),
            "the server's Retry-After must gate the next call"
        );
        assert!(
            !limiter
                .try_take_or_wait(EndpointKey::EntriesGet, None)
                .is_zero(),
            "and nothing may go out while it stands"
        );

        limiter.advance(Duration::from_secs(30));
        assert_eq!(
            limiter.peek_wait(EndpointKey::EntriesGet, None),
            Duration::ZERO,
            "the penalty expires on its own"
        );
    }

    #[tokio::test]
    async fn a_penalty_is_never_shortened_by_a_later_hint() {
        let limiter = EndpointLimiter::manual();
        limiter.penalise(EndpointKey::Search, Duration::from_secs(60));
        limiter.penalise(EndpointKey::Search, Duration::from_secs(5));
        assert_eq!(
            limiter.peek_wait(EndpointKey::Search, None),
            Duration::from_secs(60)
        );
    }

    #[tokio::test]
    async fn a_penalty_gates_every_scope_of_a_scoped_endpoint() {
        // The 429 says nothing about which dimension tripped, so it holds the
        // endpoint as a whole rather than the one room we happened to call.
        let limiter = EndpointLimiter::manual();
        limiter.penalise(EndpointKey::CircPresence, Duration::from_secs(10));
        assert_eq!(
            limiter.peek_wait(EndpointKey::CircPresence, Some("some-other-room")),
            Duration::from_secs(10)
        );
    }

    #[tokio::test]
    async fn idle_scope_entries_are_evicted_once_the_map_grows() {
        let limiter = EndpointLimiter::manual();
        for i in 0..=MAX_TRACKED_SCOPES {
            let id = format!("room{i}");
            limiter.acquire(EndpointKey::CircPresence, Some(&id)).await;
        }
        // Let every grant age out of its window, so each scoped budget holds
        // nothing and dropping it loses nothing.
        limiter.advance(Duration::from_secs(61));
        let before = limiter.budgets.lock().unwrap().len();
        assert!(before > MAX_TRACKED_SCOPES);

        limiter
            .acquire(EndpointKey::CircPresence, Some("one-more"))
            .await;
        let after = limiter.budgets.lock().unwrap().len();
        assert!(
            after < before,
            "idle scope entries should have been pruned: {before} -> {after}"
        );
        // Pruning is invisible: the pruned rooms are still writable, exactly as
        // they were before.
        assert_eq!(
            limiter.peek_wait(EndpointKey::CircPresence, Some("room0")),
            Duration::ZERO
        );
    }

    #[tokio::test]
    async fn scopes_that_still_hold_grants_survive_eviction() {
        let limiter = EndpointLimiter::manual();
        // A crowd of conversations, each with one grant that then ages out.
        for i in 0..MAX_TRACKED_SCOPES {
            let id = format!("c{i}");
            limiter.acquire(EndpointKey::CmailTyping, Some(&id)).await;
        }
        limiter.advance(Duration::from_secs(61));

        // "busy" spends its whole per-conversation budget now, so it is the one
        // entry the limiter still owes accounting for.
        for _ in 0..40 {
            limiter
                .acquire(EndpointKey::CmailTyping, Some("busy"))
                .await;
        }
        limiter
            .acquire(EndpointKey::CmailTyping, Some("trigger"))
            .await;

        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CmailTyping, Some("busy")),
            Some(40),
            "a scope that still holds grants must not be evicted"
        );
        assert_eq!(
            limiter.live_minute_grants(EndpointKey::CmailTyping, Some("c0")),
            None,
            "an idle scope is dropped"
        );
    }

    #[test]
    fn hour_and_day_only_limit_has_no_minute_window() {
        let rl = RateLimit::per_hour_with_day(1, 8);
        assert_eq!(rl.per_minute, None);
        assert_eq!(rl.per_hour, Some(1));
        assert_eq!(rl.per_day, Some(8));
    }

    #[test]
    fn minute_and_hour_limit_has_no_day_window() {
        let rl = RateLimit::per_minute_with_hour(10, 60);
        assert_eq!(rl.per_minute, Some(10));
        assert_eq!(rl.per_hour, Some(60));
        assert_eq!(rl.per_day, None);
    }
}

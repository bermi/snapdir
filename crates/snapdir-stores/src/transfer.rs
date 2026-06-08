//! Transfer configuration, rate limiting, and bounded-concurrency driver.
//!
//! This module is the foundation for concurrent object transfers and bandwidth
//! limiting. It provides:
//!
//! - [`TransferConfig`] — how many objects to transfer in parallel and an
//!   optional aggregate byte-rate cap.
//! - [`RateLimiter`] — a zero-dependency async token bucket built on
//!   [`tokio::time`], shareable across tasks via [`Arc`].
//! - [`run_concurrent`] — a generic bounded-concurrency driver that runs up to
//!   `concurrency` async operations in flight and returns the first error.
//!
//! Nothing here changes the existing (sequential) push / fetch loops yet; the
//! stores merely carry a [`TransferConfig`] so later gates can wire these
//! primitives into their transfer loops.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{self, StreamExt, TryStreamExt};
use snapdir_core::store::StoreError;
use tokio::sync::Mutex;

use crate::adaptive::{AdaptiveGate, OpResult};

/// Upper bound on the auto-detected default concurrency.
const DEFAULT_CONCURRENCY_CAP: usize = 16;

/// Whether (and how) a transfer adaptively tunes its concurrency / byte-rate.
///
/// This is the **config-level** policy carried by [`TransferConfig`] (distinct
/// from [`crate::adaptive::AdaptivePolicy`], which is the controller's
/// always-on tuning view). `Off` — the default — selects the historical fixed
/// `concurrency` + fixed `max_bytes_per_sec` path, byte-for-byte unchanged.
/// `On` selects the adaptive path, which sizes the in-flight window to
/// `ceiling` and lets a live controller drive the effective concurrency in
/// `[1, ceiling]` and the byte-rate from in-band per-op feedback. Adaptive
/// **only** changes scheduling/rate: the exact bytes/objects transferred and
/// the resulting snapshot are identical to the `Off` path.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum AdaptivePolicy {
    /// Fixed concurrency + fixed rate (the default; historical behavior).
    #[default]
    Off,
    /// Adaptive concurrency + rate, bounded by `ceiling`, aiming for
    /// `fraction × discovered-knee`.
    On {
        /// Target operating fraction of the discovered knee (clamped to
        /// `(0, 1]`; `0.8` is the usual default).
        fraction: f64,
        /// Absolute concurrency ceiling; the effective limit never exceeds it.
        ceiling: usize,
    },
}

/// Configuration for object transfers: how many to run in parallel, an optional
/// aggregate byte-rate cap, and whether to tune those adaptively.
///
/// `Default` auto-detects the available parallelism (capped at
/// [`DEFAULT_CONCURRENCY_CAP`]), leaves bandwidth unlimited, and disables
/// adaptive tuning ([`AdaptivePolicy::Off`]).
#[derive(Debug, Clone)]
pub struct TransferConfig {
    /// Maximum number of object transfers to run concurrently. In the adaptive
    /// (`On`) path this is the slow-start *seed*; the effective in-flight
    /// window is sized to the policy ceiling and gated to the live limit.
    pub concurrency: NonZeroUsize,
    /// Optional aggregate bandwidth cap, in bytes per second. `None` means
    /// unlimited. In the adaptive path this is the rate *cap* (`max_rate`); the
    /// controller may target a lower live rate.
    pub max_bytes_per_sec: Option<u64>,
    /// Optional aggregate request-rate cap, in requests per second. `None`
    /// means unlimited. A later gate wires this into the live store call sites
    /// (`key_exists` / `get_bytes` / `put_bytes`) by pacing each request through
    /// a [`RateLimiter`] / [`BlockingRateLimiter`] whose "tokens" are requests
    /// (`acquire(1)` per request). The per-backend defaults live in
    /// [`crate::limits::for_scheme`].
    pub max_requests_per_sec: Option<u64>,
    /// Whether to tune concurrency / rate adaptively. [`AdaptivePolicy::Off`]
    /// (the default) keeps the historical fixed-concurrency path byte-for-byte.
    pub adaptive: AdaptivePolicy,
}

impl TransferConfig {
    /// Builds a non-adaptive config, clamping `concurrency` to at least 1.
    ///
    /// The adaptive policy defaults to [`AdaptivePolicy::Off`], so existing
    /// callers behave exactly as before. Use
    /// [`with_adaptive`](Self::with_adaptive) to opt in.
    #[must_use]
    pub fn new(concurrency: usize, max_bytes_per_sec: Option<u64>) -> Self {
        Self {
            concurrency: NonZeroUsize::new(concurrency.max(1)).unwrap_or(NonZeroUsize::MIN),
            max_bytes_per_sec,
            max_requests_per_sec: None,
            adaptive: AdaptivePolicy::Off,
        }
    }

    /// Returns this config with its adaptive policy set to `policy` (builder
    /// style). `Off` is byte-identical to a plain [`new`](Self::new) config.
    #[must_use]
    pub fn with_adaptive(mut self, policy: AdaptivePolicy) -> Self {
        self.adaptive = policy;
        self
    }
}

impl Default for TransferConfig {
    fn default() -> Self {
        let detected = std::thread::available_parallelism()
            .map_or(1, NonZeroUsize::get)
            .clamp(1, DEFAULT_CONCURRENCY_CAP);
        Self {
            // `detected` is >= 1, so the NonZeroUsize is always Some.
            concurrency: NonZeroUsize::new(detected).unwrap_or(NonZeroUsize::MIN),
            max_bytes_per_sec: None,
            max_requests_per_sec: None,
            adaptive: AdaptivePolicy::Off,
        }
    }
}

/// Classifies a [`StoreError`] for the adaptive controller's congestion signal.
///
/// Returns [`OpResult::Throttle`] for clearly *transient / backpressure*
/// failures the controller should back off on (HTTP 429 / `SlowDown` / 503 /
/// `RESOURCE_EXHAUSTED`, request timeouts, connection reset/closed, and the
/// local-FS backpressure errno class — `WouldBlock`, EMFILE "too many open
/// files", and a full disk). Everything else — `NotFound`, `Integrity`,
/// `Parse`, and ordinary I/O / backend errors — is [`OpResult::HardErr`].
///
/// This is **conservative**: anything not clearly transient defaults to
/// `HardErr`, so a real failure never masquerades as throttling. It inspects
/// `StoreError::Backend`'s message + wrapped source string (the SDKs surface
/// their status that way) and `StoreError::Io`'s [`std::io::ErrorKind`].
#[must_use]
pub fn classify_error(err: &StoreError) -> OpResult {
    match err {
        StoreError::Io(io_err) => classify_io_kind(io_err),
        StoreError::Backend { message, source } => {
            let mut text = message.to_ascii_lowercase();
            if let Some(src) = source {
                text.push(' ');
                text.push_str(&src.to_string().to_ascii_lowercase());
            }
            if text_is_transient(&text) {
                OpResult::Throttle
            } else {
                OpResult::HardErr
            }
        }
        // NotFound / Integrity / Parse are never transient backpressure, and
        // `StoreError` is `#[non_exhaustive]` so any future variant is — by the
        // conservative rule — a hard error until proven transient.
        _ => OpResult::HardErr,
    }
}

/// Classifies a local-filesystem [`std::io::Error`] as transient backpressure
/// vs a hard error. Only the clear backpressure errno classes
/// (`WouldBlock`, a full filesystem, and EMFILE "too many open files", which
/// stable Rust still surfaces as `Uncategorized` with that message) are
/// treated as [`OpResult::Throttle`].
fn classify_io_kind(err: &std::io::Error) -> OpResult {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::WouldBlock | ErrorKind::StorageFull => OpResult::Throttle,
        // EMFILE / ENFILE land in the catch-all `Other`/`Uncategorized` kind on
        // stable Rust; sniff the message for "too many open files".
        _ => {
            if err
                .to_string()
                .to_ascii_lowercase()
                .contains("too many open files")
            {
                OpResult::Throttle
            } else {
                OpResult::HardErr
            }
        }
    }
}

/// Substring test for transient/backpressure SDK error text (case-folded).
fn text_is_transient(text: &str) -> bool {
    const TRANSIENT: &[&str] = &[
        "slowdown",
        "slow down",
        "429",
        "too many requests",
        "503",
        "service unavailable",
        "serviceunavailable",
        "resource_exhausted",
        "resource exhausted",
        "throttl",
        "request timeout",
        "requesttimeout",
        "timed out",
        "timeout",
        "connection reset",
        "connection closed",
        "connection refused",
        "broken pipe",
        "too many open files",
    ];
    TRANSIENT.iter().any(|needle| text.contains(needle))
}

/// Runs `op` over `items` with the in-flight window sized to `gate.ceiling()`
/// but the *effective* concurrency gated to the gate's live limit: each item
/// acquires a [`GatePermit`](crate::adaptive::GatePermit) before `op` runs and
/// holds it until `op` completes. This is the adaptive sibling of
/// [`run_concurrent`]: a background tick driver resizes the gate live, so the
/// number of simultaneously-running ops tracks the controller's limit while the
/// buffer window stays at the ceiling.
///
/// Semantics match [`run_concurrent`] otherwise: completion-independent order,
/// first-error-wins (remaining in-flight work is cancelled).
///
/// # Errors
///
/// Returns the first [`StoreError`] produced by any operation.
pub async fn run_adaptive<I, T, F, Fut>(
    items: I,
    gate: &AdaptiveGate,
    op: F,
) -> Result<Vec<T>, StoreError>
where
    I: IntoIterator,
    F: Fn(I::Item) -> Fut,
    Fut: std::future::Future<Output = Result<T, StoreError>>,
{
    let window = gate.ceiling().max(1);
    stream::iter(items)
        .map(|item| {
            let op = &op;
            async move {
                // Effective concurrency = the gate's current limit (<= ceiling),
                // even though up to `window` futures are buffered.
                let _permit = gate.acquire().await;
                op(item).await
            }
        })
        .buffer_unordered(window)
        .try_collect()
        .await
}

/// Shared token-bucket state, guarded by an async mutex.
///
/// The refill `rate`/`capacity` live **inside** the bucket (behind the same
/// mutex as the running `tokens`) so [`RateLimiter::set_rate`] can retune the
/// limiter live by relocking and updating them. A `rate` of `0.0` means
/// "unlimited" — [`acquire`](RateLimiter::acquire) returns immediately.
#[derive(Debug)]
struct Bucket {
    /// Refill rate in bytes per second. `0.0` means unlimited (no throttling).
    rate: f64,
    /// Maximum burst capacity, in bytes (~1 second's worth of budget).
    capacity: f64,
    /// Currently available tokens (bytes).
    tokens: f64,
    /// Last time the bucket was refilled.
    last_refill: tokio::time::Instant,
}

/// Inner state of a [`RateLimiter`].
#[derive(Debug)]
struct Inner {
    /// The live bucket state (rate/capacity/tokens). A `rate` of `0.0` models
    /// the unlimited case.
    bucket: Mutex<Bucket>,
}

/// An async token-bucket rate limiter that throttles aggregate transfer
/// throughput.
///
/// Construct with [`RateLimiter::new`]. When `max_bytes_per_sec` is `None` (or
/// `Some(0)`), the limiter is unlimited and [`acquire`](RateLimiter::acquire)
/// returns immediately. Otherwise tokens refill at `max_bytes_per_sec` per
/// second, allowing a burst of up to ~1 second's worth of budget.
///
/// The limiter is [`Arc`]-shareable and [`Clone`] (cloning shares the same
/// underlying bucket).
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Inner>,
}

impl RateLimiter {
    /// Builds a limiter. `None` (or `Some(0)`) yields an unlimited, no-op
    /// limiter whose [`acquire`](RateLimiter::acquire) never waits.
    #[must_use]
    pub fn new(max_bytes_per_sec: Option<u64>) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let (rate, capacity, tokens) = match max_bytes_per_sec {
            Some(r) if r > 0 => {
                let r = r as f64;
                (r, r, r)
            }
            _ => (0.0, 0.0, 0.0),
        };
        Self {
            inner: Arc::new(Inner {
                bucket: Mutex::new(Bucket {
                    rate,
                    capacity,
                    tokens,
                    last_refill: tokio::time::Instant::now(),
                }),
            }),
        }
    }

    /// Retunes the limiter's aggregate byte-rate cap **live**, so an adaptive
    /// controller can raise or lower throttling between operations.
    ///
    /// - `None` (or `Some(0)`) switches the limiter to **unlimited**: the rate
    ///   and capacity drop to `0` and the bucket is emptied, so the next
    ///   [`acquire`](RateLimiter::acquire) is a no-op.
    /// - `Some(r > 0)` installs (or replaces) a bucket refilling at `r`
    ///   bytes/sec with ~1 second of burst capacity. Switching from unlimited
    ///   to limited primes the bucket full (`tokens = capacity`) so a freshly
    ///   throttled limiter still allows one immediate burst.
    ///
    /// Calling `set_rate` is the only way the rate changes after [`new`];
    /// limiters that never call it behave exactly as before.
    pub async fn set_rate(&self, bytes_per_sec: Option<u64>) {
        let mut state = self.inner.bucket.lock().await;
        let was_unlimited = state.rate <= 0.0;
        #[allow(clippy::cast_precision_loss)]
        match bytes_per_sec {
            Some(r) if r > 0 => {
                let r = r as f64;
                state.rate = r;
                state.capacity = r;
                // Switching unlimited -> limited: prime a full burst. When
                // already limited, keep the running token count but clamp it to
                // the new capacity so a rate drop takes effect promptly.
                if was_unlimited {
                    state.tokens = r;
                } else {
                    state.tokens = state.tokens.min(r);
                }
                state.last_refill = tokio::time::Instant::now();
            }
            _ => {
                // Unlimited: empty the bucket; `acquire` short-circuits on rate==0.
                state.rate = 0.0;
                state.capacity = 0.0;
                state.tokens = 0.0;
            }
        }
    }

    /// Blocks until `n` bytes of budget are available, refilling the bucket at
    /// the configured rate. Unlimited limiters return immediately.
    ///
    /// A single request larger than the bucket capacity is still satisfied: the
    /// bucket is allowed to go negative and the caller waits out the deficit,
    /// so throttling is correct even for objects bigger than one second's
    /// worth of budget.
    pub async fn acquire(&self, n: u64) {
        if n == 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let need = n as f64;

        loop {
            let wait = {
                let mut state = self.inner.bucket.lock().await;
                if state.rate <= 0.0 {
                    return; // unlimited fast path (also covers live set_rate(None))
                }
                let now = tokio::time::Instant::now();
                let elapsed = now.duration_since(state.last_refill).as_secs_f64();
                state.tokens = (state.tokens + elapsed * state.rate).min(state.capacity);
                state.last_refill = now;

                if state.tokens >= need {
                    state.tokens -= need;
                    return;
                }
                // Not enough budget: compute how long until the deficit is
                // covered, then sleep (releasing the lock first).
                let deficit = need - state.tokens;
                deficit / state.rate
            };
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        }
    }
}

/// Shared token-bucket state for [`BlockingRateLimiter`], guarded by a
/// **synchronous** [`std::sync::Mutex`] (not tokio's async mutex).
///
/// As with [`Bucket`], the refill `rate`/`capacity` live inside the bucket so
/// [`BlockingRateLimiter::set_rate`] can retune live. `rate == 0.0` means
/// unlimited.
#[derive(Debug)]
struct BlockingBucket {
    /// Refill rate in bytes per second. `0.0` means unlimited (no throttling).
    rate: f64,
    /// Maximum burst capacity, in bytes (~1 second's worth of budget).
    capacity: f64,
    /// Currently available tokens (bytes).
    tokens: f64,
    /// Last time the bucket was refilled.
    last_refill: std::time::Instant,
}

/// Inner state of a [`BlockingRateLimiter`].
#[derive(Debug)]
struct BlockingInner {
    /// The live bucket state (rate/capacity/tokens). A `rate` of `0.0` models
    /// the unlimited case.
    bucket: std::sync::Mutex<BlockingBucket>,
}

/// A **synchronous** token-bucket rate limiter for the store-to-store sync
/// path.
///
/// This is the blocking sibling of [`RateLimiter`]. The
/// [`StreamStore`](crate::stream::StreamStore) methods are synchronous and
/// drive their backends' async SDK calls on an internal runtime via `block_on`,
/// so the store-to-store sync orchestrator parallelizes them across a **rayon**
/// thread pool of plain OS threads — it cannot use the async [`RateLimiter`]
/// (awaiting inside a `block_on`-ing rayon worker would nest tokio runtimes).
/// [`acquire_blocking`](BlockingRateLimiter::acquire_blocking) therefore parks
/// the calling OS thread with [`std::thread::sleep`] instead of `.await`.
///
/// When `max_bytes_per_sec` is `None` (or `Some(0)`), the limiter is unlimited
/// and [`acquire_blocking`](BlockingRateLimiter::acquire_blocking) returns
/// immediately. Otherwise tokens refill at `max_bytes_per_sec` per second,
/// allowing a burst of up to ~1 second's worth of budget. The token math
/// mirrors [`RateLimiter::acquire`] exactly.
///
/// The limiter is [`Arc`]-shareable and [`Clone`] (cloning shares the same
/// underlying bucket), so every rayon worker throttles against one aggregate
/// budget.
#[derive(Debug, Clone)]
pub struct BlockingRateLimiter {
    inner: Arc<BlockingInner>,
}

impl BlockingRateLimiter {
    /// Builds a synchronous limiter. `None` (or `Some(0)`) yields an unlimited,
    /// no-op limiter whose
    /// [`acquire_blocking`](BlockingRateLimiter::acquire_blocking) never waits.
    #[must_use]
    pub fn new(max_bytes_per_sec: Option<u64>) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let (rate, capacity, tokens) = match max_bytes_per_sec {
            Some(r) if r > 0 => {
                let r = r as f64;
                (r, r, r)
            }
            _ => (0.0, 0.0, 0.0),
        };
        Self {
            inner: Arc::new(BlockingInner {
                bucket: std::sync::Mutex::new(BlockingBucket {
                    rate,
                    capacity,
                    tokens,
                    last_refill: std::time::Instant::now(),
                }),
            }),
        }
    }

    /// Retunes the limiter's aggregate byte-rate cap **live** (the synchronous
    /// sibling of [`RateLimiter::set_rate`]). Same semantics: `None`/`Some(0)`
    /// switches to unlimited and empties the bucket; `Some(r > 0)` installs a
    /// bucket refilling at `r` bytes/sec (priming a full burst when switching
    /// from unlimited).
    pub fn set_rate(&self, bytes_per_sec: Option<u64>) {
        let mut state = self
            .inner
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let was_unlimited = state.rate <= 0.0;
        #[allow(clippy::cast_precision_loss)]
        match bytes_per_sec {
            Some(r) if r > 0 => {
                let r = r as f64;
                state.rate = r;
                state.capacity = r;
                if was_unlimited {
                    state.tokens = r;
                } else {
                    state.tokens = state.tokens.min(r);
                }
                state.last_refill = std::time::Instant::now();
            }
            _ => {
                state.rate = 0.0;
                state.capacity = 0.0;
                state.tokens = 0.0;
            }
        }
    }

    /// Blocks the calling OS thread until `n` bytes of budget are available,
    /// refilling the bucket at the configured rate. Unlimited limiters return
    /// immediately.
    ///
    /// A single request larger than the bucket capacity is still satisfied: the
    /// bucket is allowed to go negative and the caller waits out the deficit,
    /// so throttling is correct even for objects bigger than one second's worth
    /// of budget. Mirrors [`RateLimiter::acquire`], but parks the thread with
    /// [`std::thread::sleep`] instead of awaiting.
    pub fn acquire_blocking(&self, n: u64) {
        if n == 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        let need = n as f64;

        loop {
            let wait = {
                // A poisoned bucket only means a thread panicked mid-acquire;
                // the token state is still usable, so recover the guard.
                let mut state = self
                    .inner
                    .bucket
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.rate <= 0.0 {
                    return; // unlimited fast path (also covers live set_rate(None))
                }
                let now = std::time::Instant::now();
                let elapsed = now.duration_since(state.last_refill).as_secs_f64();
                state.tokens = (state.tokens + elapsed * state.rate).min(state.capacity);
                state.last_refill = now;

                if state.tokens >= need {
                    state.tokens -= need;
                    return;
                }
                // Not enough budget: compute how long until the deficit is
                // covered, then sleep (releasing the lock first).
                let deficit = need - state.tokens;
                deficit / state.rate
            };
            std::thread::sleep(Duration::from_secs_f64(wait));
        }
    }
}

/// Runs `op` over `items` with at most `concurrency` operations in flight,
/// collecting their results in completion-independent order and returning the
/// first error encountered (remaining in-flight work is cancelled).
///
/// This is the engine later gates use to drive concurrent uploads/downloads.
///
/// # Errors
///
/// Returns the first [`StoreError`] produced by any operation.
pub async fn run_concurrent<I, T, F, Fut>(
    items: I,
    concurrency: NonZeroUsize,
    op: F,
) -> Result<Vec<T>, StoreError>
where
    I: IntoIterator,
    F: Fn(I::Item) -> Fut,
    Fut: std::future::Future<Output = Result<T, StoreError>>,
{
    stream::iter(items)
        .map(op)
        .buffer_unordered(concurrency.get())
        .try_collect()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Builds a current-thread tokio runtime with time enabled, avoiding a
    /// dependency on the `#[tokio::test]` macro (keeps tokio's feature set
    /// minimal).
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build tokio runtime")
    }

    #[test]
    fn transfer_config_default_caps_concurrency() {
        let cfg = TransferConfig::default();
        assert!(cfg.concurrency.get() >= 1, "concurrency must be >= 1");
        assert!(
            cfg.concurrency.get() <= DEFAULT_CONCURRENCY_CAP,
            "default concurrency must be capped at {DEFAULT_CONCURRENCY_CAP}, got {}",
            cfg.concurrency.get()
        );
        assert_eq!(cfg.max_bytes_per_sec, None);
        assert_eq!(cfg.max_requests_per_sec, None);

        // The clamping ctor never yields 0.
        assert_eq!(TransferConfig::new(0, None).concurrency.get(), 1);
        assert_eq!(TransferConfig::new(7, Some(99)).concurrency.get(), 7);
        assert_eq!(TransferConfig::new(7, Some(99)).max_bytes_per_sec, Some(99));
    }

    /// Drives `run_concurrent` over N > concurrency items, recording the peak
    /// number of simultaneously-running ops, and asserts the bound is exactly
    /// `min(concurrency, N)` — and strictly 1 (sequential) when concurrency=1.
    fn max_in_flight_for(concurrency: usize, items: usize) -> usize {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let high_water = Arc::new(AtomicUsize::new(0));

        let rt = runtime();
        let result = rt.block_on(async {
            let in_flight = Arc::clone(&in_flight);
            let high_water = Arc::clone(&high_water);
            run_concurrent(
                0..items,
                NonZeroUsize::new(concurrency).unwrap(),
                move |_item| {
                    let in_flight = Arc::clone(&in_flight);
                    let high_water = Arc::clone(&high_water);
                    async move {
                        let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        high_water.fetch_max(cur, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, StoreError>(())
                    }
                },
            )
            .await
        });
        assert!(result.is_ok());
        high_water.load(Ordering::SeqCst)
    }

    #[test]
    fn transfer_config_run_concurrent_max_in_flight() {
        // concurrency=4 over 12 items: peak in-flight is exactly 4.
        assert_eq!(max_in_flight_for(4, 12), 4);
        // concurrency=1 over 5 items: strictly sequential, peak in-flight is 1.
        assert_eq!(max_in_flight_for(1, 5), 1);
        // concurrency greater than item count is bounded by the item count.
        assert_eq!(max_in_flight_for(8, 3), 3);
    }

    #[test]
    fn transfer_config_run_concurrent_propagates_error() {
        let rt = runtime();
        let result: Result<Vec<()>, StoreError> = rt.block_on(async {
            run_concurrent(0..10, NonZeroUsize::new(3).unwrap(), |item| async move {
                if item == 5 {
                    Err(StoreError::Backend {
                        message: "boom".to_owned(),
                        source: None,
                    })
                } else {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Ok(())
                }
            })
            .await
        });
        let err = result.expect_err("must surface the failing op's error");
        assert!(
            matches!(err, StoreError::Backend { ref message, .. } if message == "boom"),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn sync_snapshot_blocking_rate_limiter() {
        use std::time::Instant;

        // Unlimited: acquiring a large amount returns essentially instantly.
        let unlimited = BlockingRateLimiter::new(None);
        let start = Instant::now();
        unlimited.acquire_blocking(1_000_000);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "unlimited acquire_blocking should not block"
        );
        // Some(0) is also unlimited.
        let zero = BlockingRateLimiter::new(Some(0));
        let start = Instant::now();
        zero.acquire_blocking(1_000_000);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "Some(0) acquire_blocking should not block"
        );

        // Limited to 1000 bytes/sec. The bucket starts full (1000), so the
        // first 1000 bytes are free; acquiring another ~1000 bytes (2x the
        // per-second budget in total) must wait for the deficit to refill —
        // at least ~1s.
        let limiter = BlockingRateLimiter::new(Some(1000));
        let start = Instant::now();
        limiter.acquire_blocking(1000); // drains the initial burst
        limiter.acquire_blocking(1000); // must wait ~1s to refill
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "throttled acquire_blocking should take ~1s, took {elapsed:?}"
        );
    }

    #[test]
    fn transfer_config_rate_limiter_set_rate_live() {
        let rt = runtime();
        rt.block_on(async {
            // Start unlimited: a huge acquire returns instantly.
            let limiter = RateLimiter::new(None);
            let start = tokio::time::Instant::now();
            limiter.acquire(1_000_000).await;
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "unlimited acquire should not block before set_rate"
            );

            // Tighten to 1000 B/s live. The bucket is primed full (1000), so the
            // first 1000 bytes are free; the next 1000 must wait ~1s to refill.
            limiter.set_rate(Some(1000)).await;
            let start = tokio::time::Instant::now();
            limiter.acquire(1000).await; // drains the freshly-primed burst
            limiter.acquire(1000).await; // must wait ~1s
            let elapsed = start.elapsed();
            assert!(
                elapsed >= Duration::from_millis(900),
                "after set_rate(Some(1000)) a 2x-budget acquire should take ~1s, took {elapsed:?}"
            );

            // Raise the cap back to unlimited live: acquires stop waiting again.
            limiter.set_rate(None).await;
            let start = tokio::time::Instant::now();
            limiter.acquire(1_000_000).await;
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "after set_rate(None) acquire should no longer block"
            );
        });
    }

    #[test]
    fn sync_snapshot_blocking_rate_limiter_set_rate_live() {
        use std::time::Instant;

        // Start unlimited.
        let limiter = BlockingRateLimiter::new(None);
        let start = Instant::now();
        limiter.acquire_blocking(1_000_000);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "unlimited acquire_blocking should not block before set_rate"
        );

        // Tighten live to 1000 B/s.
        limiter.set_rate(Some(1000));
        let start = Instant::now();
        limiter.acquire_blocking(1000); // primed burst
        limiter.acquire_blocking(1000); // waits ~1s
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "after set_rate(Some(1000)) a 2x-budget acquire should take ~1s, took {elapsed:?}"
        );

        // Back to unlimited live.
        limiter.set_rate(Some(0));
        let start = Instant::now();
        limiter.acquire_blocking(1_000_000);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "after set_rate(Some(0)) acquire_blocking should no longer block"
        );
    }

    #[test]
    fn classify_error_throttle_vs_hard() {
        use crate::adaptive::OpResult;

        // Backend errors whose message/source look like backpressure -> Throttle.
        let transient_msgs = [
            "S3 PUT object failed: SlowDown",
            "got HTTP 503 Service Unavailable",
            "rate limited: 429 Too Many Requests",
            "RESOURCE_EXHAUSTED quota",
            "request timeout while uploading",
            "connection reset by peer",
            "os error: too many open files",
        ];
        for msg in transient_msgs {
            let err = StoreError::Backend {
                message: msg.to_owned(),
                source: None,
            };
            assert_eq!(
                classify_error(&err),
                OpResult::Throttle,
                "expected Throttle for {msg:?}"
            );
        }

        // Hard errors: NotFound / Integrity / Parse / ordinary backend failures.
        let not_found = StoreError::ObjectNotFound {
            checksum: "abc".to_owned(),
        };
        assert_eq!(classify_error(&not_found), OpResult::HardErr);
        let integrity = StoreError::Integrity {
            address: "x".to_owned(),
            expected: "a".to_owned(),
            actual: "b".to_owned(),
        };
        assert_eq!(classify_error(&integrity), OpResult::HardErr);
        let other = StoreError::Backend {
            message: "permission denied".to_owned(),
            source: None,
        };
        assert_eq!(classify_error(&other), OpResult::HardErr);

        // Local-FS backpressure errnos -> Throttle; a plain NotFound IO -> Hard.
        let emfile = StoreError::Io(std::io::Error::other("too many open files (os error 24)"));
        assert_eq!(classify_error(&emfile), OpResult::Throttle);
        let would_block = StoreError::Io(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        assert_eq!(classify_error(&would_block), OpResult::Throttle);
        let io_notfound = StoreError::Io(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(classify_error(&io_notfound), OpResult::HardErr);
    }

    #[test]
    fn run_adaptive_respects_gate_limit() {
        use crate::adaptive::AdaptiveGate;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let rt = runtime();
        let gate = AdaptiveGate::new(2, 8);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let high = Arc::new(AtomicUsize::new(0));
        let in_flight2 = Arc::clone(&in_flight);
        let high2 = Arc::clone(&high);

        let result: Result<Vec<()>, StoreError> = rt.block_on(async move {
            run_adaptive(0..20, &gate, move |_item| {
                let in_flight = Arc::clone(&in_flight2);
                let high = Arc::clone(&high2);
                async move {
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    high.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                }
            })
            .await
        });
        assert!(result.is_ok());
        // Window is the ceiling (8) but the gate's live limit is 2, so peak
        // effective concurrency never exceeds 2.
        assert!(
            high.load(Ordering::SeqCst) <= 2,
            "effective concurrency must be gated to the limit, got {}",
            high.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn transfer_config_rate_limiter() {
        let rt = runtime();
        rt.block_on(async {
            // Unlimited: acquiring a large amount returns essentially instantly.
            let unlimited = RateLimiter::new(None);
            let start = tokio::time::Instant::now();
            unlimited.acquire(1_000_000).await;
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "unlimited acquire should not block"
            );

            // Limited to 1000 bytes/sec. The bucket starts full (1000), so the
            // first 1000 bytes are free; acquiring another ~2000 bytes total
            // must wait for the deficit to refill — at least ~1s.
            let limiter = RateLimiter::new(Some(1000));
            let start = tokio::time::Instant::now();
            limiter.acquire(1000).await; // drains the initial burst
            limiter.acquire(1000).await; // must wait ~1s to refill
            let elapsed = start.elapsed();
            assert!(
                elapsed >= Duration::from_millis(900),
                "throttled acquire should take ~1s, took {elapsed:?}"
            );
        });
    }
}

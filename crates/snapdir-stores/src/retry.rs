//! Reusable retry/backoff engine for the network stores.
//!
//! This module is the **core** of snapdir's transient-failure retry policy: an
//! SDK-agnostic loop that re-runs a fallible operation under a *full-jitter*
//! exponential backoff schedule, honouring a server-supplied `Retry-After`
//! hint. Like [`crate::adaptive`], it performs **no** real I/O and reads **no**
//! real clock itself — the two impure dependencies (the jitter source and the
//! sleep) are *injected* via the [`Jitter`] and [`AsyncSleeper`] /
//! [`BlockingSleeper`] traits, so the engine and its backoff math are fully
//! deterministic under test.
//!
//! Three pieces:
//!
//! - [`RetryPolicy`] — the schedule (attempt budget + base/cap durations) and
//!   the [`backoff`](RetryPolicy::backoff) math (full-jitter exp with a server
//!   hint floor).
//! - [`Jitter`] — a `[0,1)` source. Production: [`DefaultJitter`], a tiny
//!   hand-rolled **`SplitMix64`** PRNG (no new dependency). Tests inject a fixed
//!   value.
//! - [`AsyncSleeper`] / [`BlockingSleeper`] — the (async/blocking) sleep, plus
//!   production impls ([`TokioSleeper`] / [`ThreadSleeper`]) and a recording
//!   test fake.
//!
//! The engine itself — [`retry_async`] / [`retry_blocking`] — drives an
//! operation that yields `Ok(T)` or `Err(`[`Attempt`]`)`; an [`Attempt`] carries
//! the [`StoreError`], whether it is `transient`, and an optional
//! `retry_after`. The next gate (`stores-backoff-wire`) feeds those from
//! [`classify_error`](crate::classify_error) at the call sites; this gate ships
//! only the engine + its unit tests and wires into nothing.

// The backoff math works in `f64` seconds-space and the jitter PRNG maps a
// `u64` to a `[0,1)` double; both are *advisory* timing signals (never a
// correctness path), so the pedantic cast lints are allowed module-wide,
// matching the `adaptive` controller's convention.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use snapdir_core::StoreError;

// ---------------------------------------------------------------------------
// RetryPolicy + backoff math.
// ---------------------------------------------------------------------------

/// The retry schedule: how many attempts to make and the exponential-backoff
/// base/cap bounding each inter-attempt delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total number of attempts, **including the first** (so `max_attempts = 5`
    /// means one try plus up to four retries). Clamped to at least 1 by the
    /// engine.
    pub max_attempts: u32,
    /// The base delay; the un-jittered exponential is `base × 2^n` for the
    /// 0-based attempt index `n`.
    pub base: Duration,
    /// The hard ceiling on the un-jittered exponential (and therefore on the
    /// jittered delay, absent a larger server hint).
    pub cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base: Duration::from_millis(250),
            cap: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// Computes the inter-attempt delay before the retry that follows the
    /// 0-based failed-attempt index `n`, under full-jitter exponential backoff.
    ///
    /// 1. `exp = min(cap, base × 2^n)` — the un-jittered envelope, saturated at
    ///    `cap` (the `2^n` growth is computed in `f64` and clamped, so it can
    ///    never overflow).
    /// 2. `jittered = jitter01 × exp` — full jitter spreads the delay uniformly
    ///    over `[0, exp)` (`jitter01 ∈ [0, 1)` is supplied by the [`Jitter`]
    ///    source so tests are deterministic).
    /// 3. `delay = max(server_hint.unwrap_or(0), jittered)` — a server
    ///    `Retry-After` acts as a floor: we never retry sooner than the server
    ///    asked, but may wait longer if the jittered exponential is larger.
    #[must_use]
    fn backoff(&self, n: u32, server_hint: Option<Duration>, jitter01: f64) -> Duration {
        let cap = self.cap;
        // exp = min(cap, base * 2^n), overflow-safe.
        // `2f64.powi` saturates to +inf for large n; min(cap_secs, ..) clamps it
        // before it ever leaves f64 space, so the cast back to Duration is safe.
        let base_secs = self.base.as_secs_f64();
        let cap_secs = cap.as_secs_f64();
        // n as i32 for powi; for n beyond i32::MAX (impossible here) saturate.
        let exp_secs = if n >= 1024 {
            // 2^1024 already overflows f64 to +inf; short-circuit to the cap.
            cap_secs
        } else {
            let factor = 2f64.powi(n as i32);
            (base_secs * factor).min(cap_secs)
        };
        // Defensive clamp: NaN/negative -> 0, and never exceed the cap.
        let exp_secs = if exp_secs.is_finite() {
            exp_secs.clamp(0.0, cap_secs)
        } else {
            cap_secs
        };
        // Full jitter over [0, exp). jitter01 is contractually in [0,1); clamp
        // defensively so a misbehaving source can never exceed the envelope.
        let frac = if jitter01.is_finite() {
            jitter01.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let jittered = Duration::from_secs_f64(exp_secs * frac);

        // Server hint is a floor (we honour "wait at least this long"). Without
        // a hint the delay is `jittered` (<= exp <= cap); with a hint it is
        // `max(hint, jittered)`, which the hint is allowed to push above cap.
        match server_hint {
            Some(hint) => hint.max(jittered),
            None => jittered,
        }
    }
}

// ---------------------------------------------------------------------------
// Jitter — [0,1) source (production SplitMix64 + injectable test stub).
// ---------------------------------------------------------------------------

/// A source of uniform `[0, 1)` fractions used to spread retry delays
/// (full-jitter). Injected so the backoff sequence is deterministic in tests.
pub trait Jitter {
    /// Returns the next jitter fraction in `[0, 1)`.
    fn jitter01(&self) -> f64;
}

/// Process-wide seed counter mixed into each [`DefaultJitter`] so two instances
/// constructed in the same nanosecond still diverge.
static JITTER_SEED_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Production [`Jitter`]: a tiny, hand-rolled **`SplitMix64`** PRNG (no external
/// dependency).
///
/// Seeded once at construction from a one-time nanosecond clock sample `XOR`ed
/// with a monotonically advancing process counter; each
/// [`jitter01`](Jitter::jitter01) advances the 64-bit state and maps the top 53
/// bits to a `[0, 1)` double. `SplitMix64` is the standard seeding PRNG (it is
/// what `rand`'s `SeedableRng::seed_from_u64` uses internally) and is more than
/// adequate for *jitter* — this is decorrelation, not cryptography.
///
/// Interior mutability ([`AtomicU64`]) lets `jitter01` take `&self` (matching
/// the trait) while still advancing the stream, so a single instance can be
/// shared across the retry loop.
#[derive(Debug)]
pub struct DefaultJitter {
    state: AtomicU64,
}

impl DefaultJitter {
    /// Builds a jitter source seeded from the current nanosecond clock `XOR`ed
    /// with a process-wide counter (so concurrent constructions diverge).
    #[must_use]
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let counter = JITTER_SEED_COUNTER.fetch_add(1, Ordering::Relaxed);
        // Mix the counter in via SplitMix64 so adjacent seeds aren't adjacent
        // states.
        let seed = nanos ^ splitmix64_mix(counter.wrapping_add(0x9E37_79B9_7F4A_7C15));
        Self {
            state: AtomicU64::new(seed),
        }
    }
}

impl Default for DefaultJitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Jitter for DefaultJitter {
    fn jitter01(&self) -> f64 {
        // Advance the SplitMix64 state and map the result to [0,1).
        let z = self
            .state
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        let bits = splitmix64_mix(z);
        u64_to_unit_f64(bits)
    }
}

/// The `SplitMix64` finalizing mix (the avalanche step of the `SplitMix64` PRNG).
#[inline]
fn splitmix64_mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Maps a uniformly random `u64` to a `[0, 1)` double using its top 53 bits
/// (the mantissa width), the standard unbiased construction.
#[inline]
fn u64_to_unit_f64(bits: u64) -> f64 {
    // 53 high bits / 2^53 lands in [0, 1).
    ((bits >> 11) as f64) / (1u64 << 53) as f64
}

/// A deterministic [`Jitter`] for tests: always returns the same fixed
/// fraction (clamped to `[0, 1)`).
#[derive(Clone, Copy, Debug)]
pub struct FixedJitter(pub f64);

impl Jitter for FixedJitter {
    fn jitter01(&self) -> f64 {
        // Keep within the [0,1) contract (strictly below 1.0).
        self.0.clamp(0.0, 1.0 - f64::EPSILON)
    }
}

// ---------------------------------------------------------------------------
// Sleeper — async + blocking sleep (production + recording test fake).
// ---------------------------------------------------------------------------

/// An async sleep, injected so the [`retry_async`] engine can be driven without
/// real waits in tests.
pub trait AsyncSleeper {
    /// Sleeps for `dur` (asynchronously).
    fn sleep(&self, dur: Duration) -> impl Future<Output = ()> + Send;
}

/// An OS-thread-blocking sleep, injected so the [`retry_blocking`] engine can be
/// driven without real waits in tests. Mirrors the blocking transfer path
/// ([`BlockingRateLimiter`](crate::BlockingRateLimiter)), which parks the thread
/// with [`std::thread::sleep`].
pub trait BlockingSleeper {
    /// Sleeps for `dur` (parks the calling thread).
    fn sleep(&self, dur: Duration);
}

/// Production [`AsyncSleeper`] backed by [`tokio::time::sleep`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioSleeper;

impl AsyncSleeper for TokioSleeper {
    fn sleep(&self, dur: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(dur)
    }
}

/// Production [`BlockingSleeper`] backed by [`std::thread::sleep`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadSleeper;

impl BlockingSleeper for ThreadSleeper {
    fn sleep(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

// ---------------------------------------------------------------------------
// Engine — SDK-agnostic over an attempt outcome.
// ---------------------------------------------------------------------------

/// The outcome of a single failed attempt, handed back to the retry engine.
///
/// The caller (a store operation) decides whether the failure is `transient`
/// (worth retrying) and threads through any server `retry_after` hint. The
/// next gate populates these from
/// [`classify_error`](crate::classify_error); the engine itself is SDK-agnostic.
#[derive(Debug)]
pub struct Attempt {
    /// The error this attempt produced (surfaced to the caller if retries are
    /// exhausted or the error is non-transient).
    pub err: StoreError,
    /// Whether the error is transient (throttle / timeout-class) and therefore
    /// retryable.
    pub transient: bool,
    /// An optional server-supplied minimum delay (`Retry-After`); acts as a
    /// floor on the next backoff.
    pub retry_after: Option<Duration>,
}

/// The most attempts the engine will ever make (defensive clamp on
/// [`RetryPolicy::max_attempts`]).
#[inline]
fn clamped_max_attempts(policy: &RetryPolicy) -> u32 {
    policy.max_attempts.max(1)
}

/// Runs `op` under `policy`'s full-jitter backoff, retrying transient failures.
///
/// The operation is invoked up to `policy.max_attempts` times. On `Ok(T)` the
/// value is returned immediately. On `Err(`[`Attempt`]`)`: if the attempt is
/// `transient` **and** attempts remain, the engine sleeps for
/// [`RetryPolicy::backoff`] (with the attempt's `retry_after` hint and a fresh
/// jitter sample) via `sleeper`, then retries; otherwise the attempt's
/// [`StoreError`] is returned. A non-transient error short-circuits on the
/// first failure (no sleep).
///
/// `sleeper` and `jitter` are injected so tests can record the delay sequence
/// without real waits.
///
/// # Errors
///
/// Returns the last [`Attempt`]'s [`StoreError`] when the operation does not
/// succeed within the attempt budget (or fails non-transiently).
pub async fn retry_async<T, F, Fut>(
    policy: &RetryPolicy,
    sleeper: &impl AsyncSleeper,
    jitter: &impl Jitter,
    mut op: F,
) -> Result<T, StoreError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Attempt>>,
{
    let max = clamped_max_attempts(policy);
    let mut n: u32 = 0;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(attempt) => {
                let attempts_used = n + 1;
                if attempt.transient && attempts_used < max {
                    let delay = policy.backoff(n, attempt.retry_after, jitter.jitter01());
                    sleeper.sleep(delay).await;
                    n += 1;
                } else {
                    return Err(attempt.err);
                }
            }
        }
    }
}

/// Blocking sibling of [`retry_async`]: same control flow over a synchronous
/// `op`, sleeping on the calling thread via a [`BlockingSleeper`]. Used by the
/// rayon store-to-store sync path.
///
/// # Errors
///
/// Returns the last [`Attempt`]'s [`StoreError`] when the operation does not
/// succeed within the attempt budget (or fails non-transiently).
pub fn retry_blocking<T, F>(
    policy: &RetryPolicy,
    sleeper: &impl BlockingSleeper,
    jitter: &impl Jitter,
    mut op: F,
) -> Result<T, StoreError>
where
    F: FnMut() -> Result<T, Attempt>,
{
    let max = clamped_max_attempts(policy);
    let mut n: u32 = 0;
    loop {
        match op() {
            Ok(value) => return Ok(value),
            Err(attempt) => {
                let attempts_used = n + 1;
                if attempt.transient && attempts_used < max {
                    let delay = policy.backoff(n, attempt.retry_after, jitter.jitter01());
                    sleeper.sleep(delay);
                    n += 1;
                } else {
                    return Err(attempt.err);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex;

    // ----- helpers ----------------------------------------------------------

    /// A recording fake [`AsyncSleeper`] / [`BlockingSleeper`]: never sleeps for
    /// real, just appends each requested duration so tests can assert the exact
    /// backoff sequence.
    #[derive(Default)]
    struct RecordingSleeper {
        delays: Mutex<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn recorded(&self) -> Vec<Duration> {
            self.delays.lock().unwrap().clone()
        }
    }

    impl AsyncSleeper for RecordingSleeper {
        fn sleep(&self, dur: Duration) -> impl Future<Output = ()> + Send {
            self.delays.lock().unwrap().push(dur);
            std::future::ready(())
        }
    }

    impl BlockingSleeper for RecordingSleeper {
        fn sleep(&self, dur: Duration) {
            self.delays.lock().unwrap().push(dur);
        }
    }

    /// A deterministic [`Jitter`] returning a fixed fraction (no clamping
    /// surprises — kept strictly in `[0,1)`).
    struct StubJitter(f64);
    impl Jitter for StubJitter {
        fn jitter01(&self) -> f64 {
            self.0
        }
    }

    fn boom() -> StoreError {
        StoreError::Backend {
            message: "boom".into(),
            source: None,
        }
    }

    fn transient_attempt(retry_after: Option<Duration>) -> Attempt {
        Attempt {
            err: boom(),
            transient: true,
            retry_after,
        }
    }

    fn hard_attempt() -> Attempt {
        Attempt {
            err: boom(),
            transient: false,
            retry_after: None,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build tokio runtime")
    }

    fn small_policy() -> RetryPolicy {
        // 5 attempts (=> up to 4 sleeps), tiny base/cap so jittered delays are
        // easy to reason about.
        RetryPolicy {
            max_attempts: 5,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(10),
        }
    }

    // ----- (1) exact attempt count on persistent transient failure ----------

    #[test]
    fn retry_async_persistent_transient_uses_full_attempt_budget() {
        let rt = runtime();
        rt.block_on(async {
            let policy = small_policy();
            let sleeper = RecordingSleeper::default();
            let jitter = StubJitter(0.5);
            let calls = AtomicUsize::new(0);

            let result: Result<(), StoreError> = retry_async(&policy, &sleeper, &jitter, || {
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                async { Err(transient_attempt(None)) }
            })
            .await;

            assert!(result.is_err(), "persistent transient => surfaces the err");
            assert_eq!(
                calls.load(AtomicOrdering::SeqCst),
                policy.max_attempts as usize,
                "op invoked exactly max_attempts times"
            );
            assert_eq!(
                sleeper.recorded().len(),
                (policy.max_attempts - 1) as usize,
                "max_attempts-1 sleeps recorded (no sleep after the last attempt)"
            );
        });
    }

    // ----- (2) immediate return on hard (non-transient) error ---------------

    #[test]
    fn retry_async_hard_error_returns_immediately_without_sleeping() {
        let rt = runtime();
        rt.block_on(async {
            let policy = small_policy();
            let sleeper = RecordingSleeper::default();
            let jitter = StubJitter(0.5);
            let calls = AtomicUsize::new(0);

            let result: Result<(), StoreError> = retry_async(&policy, &sleeper, &jitter, || {
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                async { Err(hard_attempt()) }
            })
            .await;

            assert!(result.is_err(), "hard error surfaces");
            assert_eq!(
                calls.load(AtomicOrdering::SeqCst),
                1,
                "non-transient error => op called exactly once"
            );
            assert!(
                sleeper.recorded().is_empty(),
                "no sleeps for a non-transient error"
            );
        });
    }

    // ----- (3) success after k transient failures ---------------------------

    #[test]
    fn retry_async_success_after_k_transient_fails() {
        let rt = runtime();
        rt.block_on(async {
            let policy = small_policy();
            let sleeper = RecordingSleeper::default();
            let jitter = StubJitter(0.25);
            let calls = AtomicUsize::new(0);
            let k = 3usize;

            let result: Result<u32, StoreError> = retry_async(&policy, &sleeper, &jitter, || {
                let prev = calls.fetch_add(1, AtomicOrdering::SeqCst);
                async move {
                    if prev < k {
                        Err(transient_attempt(None))
                    } else {
                        Ok(42)
                    }
                }
            })
            .await;

            assert_eq!(result.unwrap(), 42, "succeeds on the (k+1)-th attempt");
            assert_eq!(
                calls.load(AtomicOrdering::SeqCst),
                k + 1,
                "op invoked k+1 times"
            );
            assert_eq!(
                sleeper.recorded().len(),
                k,
                "exactly k sleeps recorded (one before each retry)"
            );
        });
    }

    // ----- (4) Retry-After honoured as a floor ------------------------------

    #[test]
    fn retry_async_retry_after_is_a_floor() {
        let rt = runtime();
        rt.block_on(async {
            let policy = small_policy();
            let sleeper = RecordingSleeper::default();
            // jitter01 = 0 => jittered exp = 0, so the hint dominates entirely.
            let jitter = StubJitter(0.0);
            // A hint far larger than the first jittered exp (base*2^0 = 100ms).
            let hint = Duration::from_secs(5);
            let calls = AtomicUsize::new(0);

            let _result: Result<(), StoreError> = retry_async(&policy, &sleeper, &jitter, || {
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                async move { Err(transient_attempt(Some(hint))) }
            })
            .await;

            let recorded = sleeper.recorded();
            assert!(!recorded.is_empty(), "at least one retry happened");
            for d in &recorded {
                assert!(
                    *d >= hint,
                    "recorded delay {d:?} must be >= the server hint {hint:?}"
                );
            }
        });
    }

    // ----- (5) cap respected (no hint) --------------------------------------

    #[test]
    fn retry_async_cap_respected_for_large_n() {
        let rt = runtime();
        rt.block_on(async {
            // Many attempts so n grows large; base*2^n would blow past cap.
            let policy = RetryPolicy {
                max_attempts: 12,
                base: Duration::from_millis(250),
                cap: Duration::from_secs(2),
            };
            let sleeper = RecordingSleeper::default();
            // jitter01 just below 1 => jittered ~ exp, the worst case for the cap.
            let jitter = StubJitter(0.999_999);

            let _result: Result<(), StoreError> =
                retry_async(&policy, &sleeper, &jitter, || async {
                    Err(transient_attempt(None))
                })
                .await;

            for d in sleeper.recorded() {
                assert!(
                    d <= policy.cap,
                    "delay {d:?} must never exceed cap {:?} (no hint)",
                    policy.cap
                );
            }
        });
    }

    // ----- (6) full-jitter bounds: delay == jitter01 * min(cap, base*2^n) ----

    #[test]
    fn backoff_full_jitter_lands_in_envelope() {
        let policy = RetryPolicy {
            max_attempts: 10,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(30),
        };
        for n in 0..8u32 {
            let frac = 0.37;
            let delay = policy.backoff(n, None, frac);
            let exp = policy
                .base
                .as_secs_f64()
                .mul_add(2f64.powi(n as i32), 0.0)
                .min(policy.cap.as_secs_f64());
            let expected = exp * frac;
            // delay == frac * min(cap, base*2^n) (within float tolerance).
            assert!(
                (delay.as_secs_f64() - expected).abs() < 1e-9,
                "n={n}: delay {delay:?} != jitter01*envelope {expected}"
            );
            // and it lands in [0, min(cap, base*2^n)).
            assert!(
                delay.as_secs_f64() >= 0.0 && delay.as_secs_f64() < exp + 1e-9,
                "n={n}: delay {delay:?} outside [0, {exp})"
            );
        }
    }

    #[test]
    fn backoff_saturates_at_cap_for_huge_n() {
        let policy = RetryPolicy {
            max_attempts: 99,
            base: Duration::from_millis(250),
            cap: Duration::from_secs(30),
        };
        // jitter01 ~ 1 so the jittered delay tracks the (capped) envelope.
        let d = policy.backoff(2000, None, 0.999_999);
        assert!(
            d <= policy.cap,
            "huge n must saturate at the cap, got {d:?}"
        );
        assert!(
            d.as_secs_f64() > 29.0,
            "with jitter ~1 the delay should be near the cap, got {d:?}"
        );
    }

    #[test]
    fn default_jitter_is_in_unit_interval() {
        let j = DefaultJitter::new();
        for _ in 0..10_000 {
            let x = j.jitter01();
            assert!((0.0..1.0).contains(&x), "jitter {x} outside [0,1)");
        }
        // And two instances produce different streams (seed divergence).
        let a = DefaultJitter::new();
        let b = DefaultJitter::new();
        let sa: Vec<f64> = (0..4).map(|_| a.jitter01()).collect();
        let sb: Vec<f64> = (0..4).map(|_| b.jitter01()).collect();
        assert_ne!(sa, sb, "distinct instances should diverge");
    }

    // ----- (7) blocking engine smoke tests ----------------------------------

    #[test]
    fn retry_blocking_persistent_transient_uses_full_budget() {
        let policy = small_policy();
        let sleeper = RecordingSleeper::default();
        let jitter = StubJitter(0.5);
        let calls = AtomicUsize::new(0);

        let result: Result<(), StoreError> = retry_blocking(&policy, &sleeper, &jitter, || {
            calls.fetch_add(1, AtomicOrdering::SeqCst);
            Err(transient_attempt(None))
        });

        assert!(result.is_err());
        assert_eq!(
            calls.load(AtomicOrdering::SeqCst),
            policy.max_attempts as usize,
            "blocking: op invoked max_attempts times"
        );
        assert_eq!(
            sleeper.recorded().len(),
            (policy.max_attempts - 1) as usize,
            "blocking: max_attempts-1 sleeps"
        );
    }

    #[test]
    fn retry_blocking_success_after_k_transient() {
        let policy = small_policy();
        let sleeper = RecordingSleeper::default();
        let jitter = StubJitter(0.5);
        let calls = AtomicUsize::new(0);
        let k = 2usize;

        let result: Result<&str, StoreError> = retry_blocking(&policy, &sleeper, &jitter, || {
            let prev = calls.fetch_add(1, AtomicOrdering::SeqCst);
            if prev < k {
                Err(transient_attempt(None))
            } else {
                Ok("done")
            }
        });

        assert_eq!(result.unwrap(), "done");
        assert_eq!(calls.load(AtomicOrdering::SeqCst), k + 1);
        assert_eq!(sleeper.recorded().len(), k, "blocking: k sleeps");
    }

    #[test]
    fn default_policy_values() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_attempts, 5);
        assert_eq!(p.base, Duration::from_millis(250));
        assert_eq!(p.cap, Duration::from_secs(30));
    }
}

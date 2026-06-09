//! `backoff_wire` integration tests: exercise the network-store retry wiring
//! (`retry_network` + the per-call request-rate limiter + `parse_retry_after`)
//! with NO live cloud.
//!
//! The injectable seam is [`snapdir_stores::retry_network`]: it acquires one
//! token from a [`RateLimiter`] then drives a caller-supplied `op` through the
//! full-jitter backoff engine. Each test passes an `op` closure that replays a
//! scripted sequence of [`Attempt`]s and asserts the retry/limiter/backoff
//! behavior using a recording sleeper + a deterministic jitter (the same fakes
//! the engine's own unit tests use, re-implemented here against the public API).
//!
//! These run under both `cargo test` and `cargo test --features
//! integration-mock` (the feature gates the heavier fake-SDK injection seam in
//! the store modules; the public `retry_network` path is always available).

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use snapdir_core::StoreError;
use snapdir_stores::{
    parse_retry_after, retry_network, AsyncSleeper, Attempt, Jitter, RateLimiter, RetryPolicy,
};

// --- test fakes -------------------------------------------------------------

/// A recording [`AsyncSleeper`]: never sleeps for real, just appends each
/// requested delay so a test can assert the exact backoff sequence.
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

/// A deterministic [`Jitter`] returning a fixed `[0,1)` fraction.
struct FixedJitter(f64);
impl Jitter for FixedJitter {
    fn jitter01(&self) -> f64 {
        self.0
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build tokio runtime")
}

fn boom() -> StoreError {
    StoreError::Backend {
        message: "boom".into(),
        source: None,
    }
}

fn transient(retry_after: Option<Duration>) -> Attempt {
    Attempt {
        err: boom(),
        transient: true,
        retry_after,
    }
}

fn hard() -> Attempt {
    Attempt {
        err: boom(),
        transient: false,
        retry_after: None,
    }
}

fn small_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 5,
        base: Duration::from_millis(100),
        cap: Duration::from_secs(10),
    }
}

// --- (1) k transient then success ------------------------------------------

#[test]
fn backoff_wire_success_after_k_transient_retries() {
    let rt = runtime();
    rt.block_on(async {
        let policy = small_policy();
        let limiter = RateLimiter::new(None); // unlimited request rate
        let sleeper = RecordingSleeper::default();
        let jitter = FixedJitter(0.5);
        let calls = AtomicUsize::new(0);
        let k = 3usize;

        let result: Result<u32, StoreError> =
            retry_network(&policy, &limiter, &sleeper, &jitter, || {
                let prev = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if prev < k {
                        Err(transient(None))
                    } else {
                        Ok(7)
                    }
                }
            })
            .await;

        assert_eq!(result.unwrap(), 7, "succeeds on the (k+1)-th attempt");
        assert_eq!(calls.load(Ordering::SeqCst), k + 1, "op invoked k+1 times");

        // Exactly k sleeps, and the backoff is monotonic-ish (each within the
        // capped envelope; with a fixed jitter the un-capped values grow).
        let recorded = sleeper.recorded();
        assert_eq!(recorded.len(), k, "one sleep before each of the k retries");
        for d in &recorded {
            assert!(*d <= policy.cap, "sleep {d:?} must respect the cap");
        }
        // base*2^n * 0.5 grows until the cap: 50ms, 100ms, 200ms (all < 10s cap).
        assert!(recorded[1] >= recorded[0], "backoff should grow");
        assert!(recorded[2] >= recorded[1], "backoff should grow");
    });
}

// --- (2) hard error returns immediately, no sleeps -------------------------

#[test]
fn backoff_wire_hard_error_surfaces_without_retry() {
    let rt = runtime();
    rt.block_on(async {
        let policy = small_policy();
        let limiter = RateLimiter::new(None);
        let sleeper = RecordingSleeper::default();
        let jitter = FixedJitter(0.5);
        let calls = AtomicUsize::new(0);

        let result: Result<(), StoreError> =
            retry_network(&policy, &limiter, &sleeper, &jitter, || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(hard()) }
            })
            .await;

        let err = result.expect_err("hard error surfaces");
        assert!(
            matches!(err, StoreError::Backend { ref message, .. } if message == "boom"),
            "the StoreError must surface: {err:?}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "non-transient => op called exactly once"
        );
        assert!(
            sleeper.recorded().is_empty(),
            "no sleeps for a non-transient error"
        );
    });
}

// --- (3) Retry-After hint dominates the jittered exponential ---------------

#[test]
fn backoff_wire_retry_after_hint_is_a_floor() {
    let rt = runtime();
    rt.block_on(async {
        let policy = small_policy();
        let limiter = RateLimiter::new(None);
        let sleeper = RecordingSleeper::default();
        // jitter01 = 0 => the jittered exponential collapses to 0, so a hint
        // larger than the exp must dominate every recorded sleep.
        let jitter = FixedJitter(0.0);
        let hint = Duration::from_secs(5);

        let _r: Result<(), StoreError> =
            retry_network(&policy, &limiter, &sleeper, &jitter, || async move {
                Err(transient(Some(hint)))
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

// --- (4) persistent transient exhausts exactly max_attempts ----------------

#[test]
fn backoff_wire_persistent_transient_exhausts_budget() {
    let rt = runtime();
    rt.block_on(async {
        let policy = small_policy();
        let limiter = RateLimiter::new(None);
        let sleeper = RecordingSleeper::default();
        let jitter = FixedJitter(0.5);
        let calls = AtomicUsize::new(0);

        let result: Result<(), StoreError> =
            retry_network(&policy, &limiter, &sleeper, &jitter, || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(transient(None)) }
            })
            .await;

        assert!(result.is_err(), "persistent transient surfaces the err");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            policy.max_attempts as usize,
            "op invoked exactly max_attempts times"
        );
        assert_eq!(
            sleeper.recorded().len(),
            (policy.max_attempts - 1) as usize,
            "max_attempts-1 sleeps (none after the final attempt)"
        );
    });
}

// --- (5) parse_retry_after extraction (delta-seconds) ----------------------

#[test]
fn backoff_wire_parse_retry_after_delta_seconds() {
    assert_eq!(parse_retry_after("125"), Some(Duration::from_secs(125)));
    assert_eq!(parse_retry_after("  7 "), Some(Duration::from_secs(7)));
    assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
    // The HTTP-date form is intentionally not parsed (returns None; backoff
    // handles the absent-hint case).
    assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    assert_eq!(parse_retry_after("not-a-number"), None);
    assert_eq!(parse_retry_after(""), None);
}

// --- (6) the request-rate limiter is acquired per call ---------------------

#[test]
fn backoff_wire_request_limiter_paces_each_call() {
    let rt = runtime();
    rt.block_on(async {
        // A tiny request rate: 2 req/s. The bucket starts full (2), so the
        // first two attempts are free; with 4 total attempts (3 transient +
        // success), the limiter must wait ~1s for the bucket to refill twice.
        // The recording sleeper makes the *backoff* sleeps free, so the only
        // real wall-clock wait comes from the request limiter — proving it is
        // acquired per call.
        let policy = RetryPolicy {
            max_attempts: 5,
            base: Duration::from_millis(1),
            cap: Duration::from_millis(1),
        };
        let limiter = RateLimiter::new(Some(2)); // 2 requests/sec
        let sleeper = RecordingSleeper::default();
        let jitter = FixedJitter(0.0);
        let calls = AtomicUsize::new(0);
        let k = 3usize;

        let start = tokio::time::Instant::now();
        let result: Result<u32, StoreError> =
            retry_network(&policy, &limiter, &sleeper, &jitter, || {
                let prev = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if prev < k {
                        Err(transient(None))
                    } else {
                        Ok(1)
                    }
                }
            })
            .await;
        let elapsed = start.elapsed();

        assert_eq!(result.unwrap(), 1);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            k + 1,
            "four total attempts (= four request-token acquisitions)"
        );
        // 4 acquisitions at 2 req/s with a 2-token initial burst: tokens 3 and 4
        // each wait ~0.5s => ~1s total of real waiting attributable solely to
        // the per-call request limiter.
        assert!(
            elapsed >= Duration::from_millis(900),
            "the per-call request limiter must pace each attempt, took {elapsed:?}"
        );
    });
}

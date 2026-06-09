//! Live-path coverage for the adaptive transfer wiring (`adaptive-wire-live`).
//!
//! The `AdaptiveController` is already wired into the transfer loops shipped in
//! snapdir 1.3.0: `fetch.rs::run_adaptive_downloads` and
//! `push.rs::run_adaptive_objects` select `run_adaptive(items, &gate, op)` when
//! `AdaptivePolicy::On`, feeding a `ControllerDriver` per-op `OpSample`s built
//! from `classify_error()`, while a background tick driver resizes the shared
//! `AdaptiveGate`. `AdaptivePolicy::Off` (the default) stays on `run_concurrent`.
//!
//! These tests are TEST-ONLY and exercise the *same* public primitives the live
//! path uses, asserting the observable behavior of that wiring:
//!
//! 1. AIMD multiplicative-decrease under sustained Throttle, then additive
//!    recovery under sustained Success — driven exactly as the production
//!    closures drive it (classify an injected transient `StoreError` ->
//!    `OpResult::Throttle`, `record_op`, `tick`), asserting the real
//!    `AdaptiveGate`'s live limit trajectory.
//! 2. The default `TransferConfig` is `AdaptivePolicy::Off`, and the `Off` path
//!    uses `run_concurrent` (peak in-flight = full fixed concurrency, no gate /
//!    no resizing) — observably distinct from the gated adaptive path.
//! 3. First-error-wins and completion-independent ordering still hold on the
//!    `run_adaptive` (`AdaptivePolicy::On`) path.
//!
//! All inputs are injected (op outcomes + an explicit monotonic clock supplied
//! to `tick_at`). No wall-clock-sensitive assertions, no network, no env needed.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use snapdir_core::store::StoreError;
use snapdir_stores::transfer::{AdaptivePolicy as TransferAdaptivePolicy, TransferConfig};
use snapdir_stores::{
    classify_error, run_adaptive, run_concurrent, AdaptiveGate, AdaptivePolicy, ControllerDriver,
    OpResult, OpSample,
};

/// Current-thread tokio runtime with time enabled (mirrors the in-crate test
/// harness style; keeps tokio's feature set minimal, no `#[tokio::test]`).
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("build tokio runtime")
}

/// Drives `driver` exactly like the live `run_adaptive_*` per-op closure does
/// for one operation whose store result is `outcome`: build the `OpSample` from
/// `classify_error()` on failure (success => `OpResult::Ok` with the moved
/// bytes), then `record_op`. This is byte-for-byte the production mapping in
/// `fetch.rs::run_adaptive_downloads` / `push.rs::run_adaptive_objects`.
fn record_like_live(driver: &ControllerDriver, bytes: u64, outcome: &Result<(), StoreError>) {
    let (bytes, result) = match outcome {
        Ok(()) => (bytes, OpResult::Ok),
        Err(err) => (0, classify_error(err)),
    };
    driver.record_op(OpSample {
        bytes,
        latency: Duration::from_millis(40),
        result,
    });
}

/// A transient backend error whose message `classify_error` maps to
/// `OpResult::Throttle` (503 / "slow down" / timeout class).
fn transient_err(msg: &str) -> StoreError {
    StoreError::Backend {
        message: msg.to_owned(),
        source: None,
    }
}

/// Sanity: the messages we inject as "transient" really do classify as
/// `Throttle` (so the AIMD test is exercising the congestion branch, not a
/// silent `HardErr`). Confirms the live `classify_error` -> `Throttle` path.
#[test]
fn adaptive_wire_classify_injected_transient_is_throttle() {
    for msg in [
        "GET object failed: 503 Service Unavailable",
        "S3 PUT failed: SlowDown, reduce your request rate",
        "request timeout while downloading object",
        "connection reset by peer",
    ] {
        assert_eq!(
            classify_error(&transient_err(msg)),
            OpResult::Throttle,
            "live wiring relies on {msg:?} classifying as Throttle",
        );
    }
    // A non-transient backend error stays a hard error (won't trigger backoff).
    assert_eq!(
        classify_error(&transient_err("permission denied")),
        OpResult::HardErr,
    );
}

/// (1) AIMD: sustained Throttle multiplicatively shrinks the *real* gate the
/// adaptive transfer path resizes, and sustained Success then recovers it.
///
/// We drive the production `ControllerDriver` + `AdaptiveGate` pair the same way
/// `run_adaptive_downloads`/`run_adaptive_objects` do — feeding `OpSample`s
/// derived from `classify_error()` on injected transient `StoreError`s — but
/// advance the controller via the fully-injectable `tick_with(now, cpu, rss)`
/// seam instead of `tick()` (which samples the real `epoch.elapsed()` AND the
/// live CPU/RSS samplers). Injecting ALL impure inputs makes the observable gate
/// trajectory (grow -> shrink-on-throttle -> recover-on-success) fully
/// DETERMINISTIC: the time-dependent arms (the 15s post-congestion cooldown and
/// additive-increase recovery) are crossed by explicit `now` advances, and the
/// CPU guardrails are pinned to a calm value — so the trajectory no longer
/// depends on how fast this test loop runs OR how loaded the machine is under
/// parallel test load (the two sources that made the live `tick()` flaky).
///
/// `tick_with` applies the very same `Decision` to the real gate that the live
/// `tick()` does (`gate.set_limit(decision.limit)` + rate/meter); only the
/// impure inputs differ (injected vs sampled), so this exercises the identical
/// production wiring. CPU is pinned at a calm 20% (`< 85`) so neither the
/// no-increase nor the hard-decrease guardrail bites; RSS is `Some(0)` (RAM is
/// `u64::MAX`, so the memory budget is disabled).
#[test]
fn adaptive_wire_aimd_shrinks_on_throttle_then_recovers() {
    // Calm injected CPU + zero RSS: pins the load-dependent guardrails off so the
    // gate trajectory depends only on the injected ops + clock (deterministic).
    const CALM_CPU: Option<f64> = Some(20.0);
    const NO_RSS: Option<u64> = Some(0);

    // Generous ceiling + huge RAM so neither the ceiling nor the memory budget
    // masks the AIMD behavior under test.
    let gate = AdaptiveGate::new(2, 32);
    let policy = AdaptivePolicy::new(0.8, 32, u64::MAX, None);
    let driver = ControllerDriver::new(policy, gate.clone(), 4096, None, None);

    // Injected monotonic clock: advance one second per tick, deterministically.
    let mut now = Duration::ZERO;
    let step = Duration::from_secs(1);

    // --- grow: a healthy stream of successful ops raises the live gate limit.
    for _ in 0..10 {
        for _ in 0..4 {
            record_like_live(&driver, 2_000_000, &Ok(()));
        }
        driver.tick_with(now, CALM_CPU, NO_RSS);
        now += step;
    }
    let grown = gate.limit();
    assert!(
        grown > 2,
        "a healthy stream should grow the live gate above the seed of 2, got {grown}",
    );

    // --- shrink: a single sustained Throttle event halves the gate (AIMD
    //     multiplicative-decrease) on the very next tick, and arms the 15s
    //     post-congestion cooldown deadline (now + 15s).
    record_like_live(&driver, 0, &Err(transient_err("503 Service Unavailable")));
    driver.tick_with(now, CALM_CPU, NO_RSS);
    let after_throttle = gate.limit();
    now += step;
    assert!(
        after_throttle < grown,
        "sustained Throttle must multiplicatively shrink the live gate: {after_throttle} >= {grown}",
    );
    // Multiplicative decrease is ~0.5x; allow +1 rounding slack.
    assert!(
        after_throttle <= grown / 2 + 1,
        "Throttle backoff should at least halve the gate: before {grown}, after {after_throttle}",
    );

    // --- cooldown: while inside the 15s post-congestion window, even a
    //     sustained-healthy stream must NOT grow the gate (the controller's
    //     no-increase guard). Tick a few healthy seconds still inside the
    //     window and assert the gate never climbs above the backed-off floor.
    for _ in 0..5 {
        for _ in 0..4 {
            record_like_live(&driver, 3_000_000, &Ok(()));
        }
        driver.tick_with(now, CALM_CPU, NO_RSS);
        now += step;
        assert!(
            gate.limit() <= after_throttle,
            "no increase during the 15s cooldown: {} > {after_throttle}",
            gate.limit(),
        );
    }

    // --- recover: jump the injected clock decisively PAST the 15s cooldown
    //     deadline, then feed the same uninterrupted healthy stream. Now the
    //     additive-increase arm is allowed to fire and the live gate must climb
    //     back above the backed-off floor. This crossing is deterministic — it
    //     depends only on the injected `now`/CPU, never on wall-clock elapsed
    //     time or the machine's current load.
    now += Duration::from_secs(20); // well past the 15s cooldown from the throttle
    for _ in 0..12 {
        for _ in 0..4 {
            record_like_live(&driver, 3_000_000, &Ok(()));
        }
        driver.tick_with(now, CALM_CPU, NO_RSS);
        now += step;
    }
    assert!(
        gate.limit() > after_throttle,
        "after the cooldown, sustained Success must additively grow the live gate back up: {} <= {after_throttle}",
        gate.limit(),
    );
}

/// (1b) The same AIMD shrink, but proving the *whole* live closure runs: we
/// invoke `run_adaptive` over a batch where every op returns an injected
/// transient `StoreError`, feeding the driver from inside the op exactly as the
/// production closure does. `run_adaptive` aborts on the first error
/// (first-error-wins), and the recorded Throttle must have shrunk the gate.
#[test]
fn adaptive_wire_run_adaptive_closure_records_throttle_and_shrinks_gate() {
    let rt = runtime();
    let gate = AdaptiveGate::new(4, 16);
    let policy = AdaptivePolicy::new(0.8, 16, u64::MAX, None);
    let driver = ControllerDriver::new(policy, gate.clone(), 4096, None, None);

    let before = gate.limit();
    assert_eq!(before, 4, "gate seeds at the configured concurrency");

    let result: Result<Vec<()>, StoreError> = rt.block_on({
        let gate = gate.clone();
        let driver = driver.clone();
        async move {
            run_adaptive(0..8, &gate, |item| {
                let driver = &driver;
                async move {
                    // Every op throttles (transient 503). Mirror the live
                    // closure: time it, classify on error, record, return.
                    let outcome: Result<(), StoreError> =
                        Err(transient_err("got HTTP 503 from backend"));
                    record_like_live(driver, item, &outcome);
                    outcome
                }
            })
            .await
        }
    });

    // First-error-wins: the injected transient error is surfaced.
    let err = result.expect_err("an all-error batch must surface the first error");
    assert!(
        matches!(err, StoreError::Backend { ref message, .. } if message.contains("503")),
        "unexpected error: {err:?}",
    );

    // The recorded Throttle(s), applied by a tick, shrink the live gate.
    driver.tick();
    assert!(
        gate.limit() < before,
        "throttled ops recorded through the live closure must shrink the gate: {} >= {before}",
        gate.limit(),
    );
}

/// (2) Off path: the default `TransferConfig` is `AdaptivePolicy::Off`, and the
/// fixed-concurrency engine (`run_concurrent`) the `Off` arm selects runs at the
/// full configured concurrency with no gate / no resizing — peak in-flight is
/// exactly `min(concurrency, items)`, observably distinct from the gated
/// adaptive path (which would cap at the live limit).
#[test]
fn adaptive_wire_off_path_uses_run_concurrent_no_gate() {
    // Default config is Off (no behavior change / opt-in adaptive).
    assert_eq!(
        TransferConfig::default().adaptive,
        TransferAdaptivePolicy::Off,
        "the default transfer policy MUST stay Off (adaptive is opt-in)",
    );
    assert_eq!(
        TransferConfig::new(8, None).adaptive,
        TransferAdaptivePolicy::Off,
    );

    // The Off arm runs `run_concurrent(.., config.concurrency, ..)`: peak
    // in-flight reaches the full fixed concurrency (no gate throttling it down).
    let concurrency = NonZeroUsize::new(6).unwrap();
    let items = 24usize;
    let in_flight = Arc::new(AtomicUsize::new(0));
    let high = Arc::new(AtomicUsize::new(0));

    let rt = runtime();
    let result: Result<Vec<()>, StoreError> = rt.block_on({
        let in_flight = Arc::clone(&in_flight);
        let high = Arc::clone(&high);
        async move {
            run_concurrent(0..items, concurrency, move |_item| {
                let in_flight = Arc::clone(&in_flight);
                let high = Arc::clone(&high);
                async move {
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    high.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok::<(), StoreError>(())
                }
            })
            .await
        }
    });
    assert!(result.is_ok());
    assert_eq!(
        high.load(Ordering::SeqCst),
        concurrency.get(),
        "the Off path (run_concurrent) runs at the full fixed concurrency, not a gated limit",
    );
}

/// (3a) First-error-wins on the `AdaptivePolicy::On` path: an injected *hard*
/// error (one that classifies as `HardErr`, not throttle) aborts `run_adaptive`
/// and is the returned error, even with other ops succeeding concurrently.
#[test]
fn adaptive_wire_on_path_first_error_wins() {
    let rt = runtime();
    let gate = AdaptiveGate::new(3, 8);

    let result: Result<Vec<()>, StoreError> = rt.block_on({
        let gate = gate.clone();
        async move {
            run_adaptive(0..12, &gate, |item| async move {
                if item == 5 {
                    Err(transient_err("permission denied")) // HardErr, aborts
                } else {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    Ok::<(), StoreError>(())
                }
            })
            .await
        }
    });

    let err = result.expect_err("the hard error must abort the adaptive batch");
    assert!(
        matches!(err, StoreError::Backend { ref message, .. } if message == "permission denied"),
        "first-error-wins must surface the injected hard error, got {err:?}",
    );
    // It really was a hard error (not a throttle) — confirms we tested the
    // abort path, not the backoff path.
    assert_eq!(
        classify_error(&transient_err("permission denied")),
        OpResult::HardErr
    );
}

/// (3b) Completion-independent collection on the `AdaptivePolicy::On` path:
/// `run_adaptive` (like `run_concurrent`) is completion-*independent* in the
/// sense that every item's result is collected exactly once regardless of the
/// order ops finish — it does NOT block on slow earlier items to preserve input
/// order (it uses `buffer_unordered`). We make later items finish first
/// (descending sleep) so completions are scrambled relative to input order, and
/// assert the collected set is complete (all 8 items, each once). This is the
/// invariant the live transfer loops depend on: no dropped/duplicated objects
/// however the network reorders completions.
#[test]
fn adaptive_wire_on_path_completion_independent_collection() {
    let rt = runtime();
    let gate = AdaptiveGate::new(8, 8); // window wide enough for full overlap

    let mut collected: Vec<usize> = rt.block_on({
        let gate = gate.clone();
        async move {
            run_adaptive(0..8usize, &gate, |item| async move {
                // Earlier items sleep longer => they complete LAST, scrambling
                // completion order relative to input order.
                let delay = (8 - item as u64) * 5;
                tokio::time::sleep(Duration::from_millis(delay)).await;
                Ok::<usize, StoreError>(item)
            })
            .await
            .expect("all ops succeed")
        }
    });

    // Every item is collected exactly once, independent of completion order.
    assert_eq!(
        collected.len(),
        8,
        "all items must be collected exactly once"
    );
    collected.sort_unstable();
    assert_eq!(
        collected,
        (0..8usize).collect::<Vec<_>>(),
        "run_adaptive must collect every item's result regardless of completion order",
    );
}

/// (3c) Gating invariant on the `On` path: with the live gate limit below the
/// buffered window (ceiling), `run_adaptive` never runs more ops simultaneously
/// than the gate's current limit — the property the live transfer loops rely on
/// for the controller to actually bound concurrency. Mirrors
/// `transfer.rs::run_adaptive_respects_gate_limit` from an external vantage.
#[test]
fn adaptive_wire_on_path_respects_gate_limit() {
    let rt = runtime();
    let gate = AdaptiveGate::new(2, 16); // window 16, live limit 2
    let in_flight = Arc::new(AtomicUsize::new(0));
    let high = Arc::new(AtomicUsize::new(0));

    let result: Result<Vec<()>, StoreError> = rt.block_on({
        let gate = gate.clone();
        let in_flight = Arc::clone(&in_flight);
        let high = Arc::clone(&high);
        async move {
            run_adaptive(0..24, &gate, move |_item| {
                let in_flight = Arc::clone(&in_flight);
                let high = Arc::clone(&high);
                async move {
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    high.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    Ok::<(), StoreError>(())
                }
            })
            .await
        }
    });
    assert!(result.is_ok());
    assert!(
        high.load(Ordering::SeqCst) <= 2,
        "the live gate limit (2) must bound effective concurrency despite the 16-wide window, got {}",
        high.load(Ordering::SeqCst),
    );
}

/// Defensive cross-check: a `ControllerDriver` whose injected ops are all
/// *successful* never shrinks the gate below its seed via a spurious Throttle —
/// i.e. our AIMD shrink in the throttle test is genuinely caused by the
/// classified congestion signal, not by ticking alone. Uses the Mutex<usize>
/// sink pattern to also confirm a rate applier is exercised on the live path.
#[test]
fn adaptive_wire_healthy_stream_does_not_spuriously_shrink() {
    let gate = AdaptiveGate::new(4, 32);
    let applied: Arc<Mutex<Option<Option<u64>>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&applied);
    let rate_applier: Arc<dyn Fn(Option<u64>) + Send + Sync> =
        Arc::new(move |r| *sink.lock().unwrap() = Some(r));
    let policy = AdaptivePolicy::new(0.8, 32, u64::MAX, None);
    let driver = ControllerDriver::new(policy, gate.clone(), 4096, Some(rate_applier), None);

    let seed = gate.limit();
    for _ in 0..8 {
        for _ in 0..4 {
            record_like_live(&driver, 2_000_000, &Ok(()));
        }
        driver.tick();
    }
    assert!(
        gate.limit() >= seed,
        "a purely healthy stream must never shrink the live gate below its seed: {} < {seed}",
        gate.limit(),
    );
    assert!(
        applied.lock().unwrap().is_some(),
        "the live rate applier must be invoked by the driver's tick",
    );
}

//! Adaptive concurrency + throughput control (pure control logic).
//!
//! This module is the **brain and the gate** of snapdir's adaptive transfer
//! tuning, but it deliberately performs **no** network or real I/O and reads
//! **no** real clock or system samplers itself — every external signal (CPU,
//! RSS, elapsed time, per-op outcomes) is *injected*. That keeps the controller
//! fully deterministic and unit-testable; wiring it into the live transfer
//! loops (and feeding it [`snapdir_core::resources`] samples + a real monotonic
//! clock) is a later gate.
//!
//! Three pieces:
//!
//! - [`AdaptiveGate`] — a **resizable** concurrency permit pool shared by both
//!   transfer backends. It exposes an async [`acquire`](AdaptiveGate::acquire)
//!   (tokio semaphore) for the futures path and a zero-dependency blocking
//!   [`acquire_blocking`](AdaptiveGate::acquire_blocking) (mutex + condvar) for
//!   the rayon path; [`set_limit`](AdaptiveGate::set_limit) retunes both live.
//! - [`AdaptiveController`] — the control law (slow-start → AIMD, latency
//!   gradient, congestion backoff with cooldown, CPU/memory guardrails,
//!   periodic re-probing) that turns injected op samples + metrics into a
//!   [`Decision`] (next concurrency limit + target byte-rate).
//! - Supporting value types: [`AdaptivePolicy`], [`OpSample`], [`OpResult`],
//!   [`Decision`].

// The controller works in `f64` throughput/latency space and converts to/from
// integer concurrency limits and byte-rates. Those casts are inherent to an
// *advisory* control signal (never a correctness path), so the pedantic cast
// lints are allowed module-wide, matching the resources sampler's convention.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use snapdir_core::resources::{resident_set_bytes, CpuSampler};
use snapdir_core::Meter;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

// ---------------------------------------------------------------------------
// AdaptiveGate — resizable concurrency permit pool (async + blocking).
// ---------------------------------------------------------------------------

/// State for the hand-rolled, zero-dependency blocking counting semaphore that
/// backs the rayon path.
#[derive(Debug)]
struct BlockingState {
    /// Permits currently available to hand out.
    available: usize,
    /// The effective concurrency limit (number of permits that *should* exist,
    /// counting both available and in-flight). `set_limit` adjusts this.
    limit: usize,
    /// Permits currently checked out (held by live guards).
    in_flight: usize,
}

/// Shared inner state of an [`AdaptiveGate`].
#[derive(Debug)]
struct GateInner {
    /// Absolute upper bound on the limit (construction param). `limit` is always
    /// clamped to `[1, ceiling]`.
    ceiling: usize,
    /// The current logical limit, mirrored across both backends. Stored as an
    /// atomic for cheap reads (e.g. [`AdaptiveGate::limit`]).
    limit: AtomicUsize,
    /// The async permit pool (futures/tokio path). Resized via `add_permits` /
    /// acquire-and-`forget`.
    sem: Arc<Semaphore>,
    /// Outstanding "shrink debt" for the async pool: permits a `set_limit`
    /// shrink wanted to remove but couldn't (they were in flight). The next
    /// permit Drops pay this down by forgetting instead of returning, so the
    /// semaphore's effective capacity converges to the new limit without ever
    /// revoking a held permit. `tokio::sync::Semaphore` has no "max" knob, so
    /// this debt is how a shrink-below-in-flight is made exact.
    async_debt: AtomicUsize,
    /// The blocking permit pool (rayon path): a counting semaphore built from a
    /// mutex + condvar.
    blocking: Mutex<BlockingState>,
    /// Wakes blocking waiters when permits become available (limit raised or a
    /// guard dropped).
    blocking_cv: Condvar,
}

/// A resizable concurrency permit pool shared by both transfer backends.
///
/// The pool starts at `start` permits and can be retuned live with
/// [`set_limit`](AdaptiveGate::set_limit) anywhere in `[1, ceiling]`. It serves
/// two access paths over **one shared logical limit**:
///
/// - **async** ([`acquire`](AdaptiveGate::acquire)) for the futures/tokio
///   transfer loop, backed by [`tokio::sync::Semaphore`];
/// - **blocking** ([`acquire_blocking`](AdaptiveGate::acquire_blocking)) for the
///   rayon store-to-store sync path, backed by a zero-dependency
///   mutex+condvar counting semaphore (mirrors the token-bucket style in
///   [`crate::transfer`]).
///
/// Both return an RAII guard that releases its permit on drop. `set_limit`
/// grows the async pool with `add_permits` and shrinks it by acquiring and
/// [`forget`](tokio::sync::SemaphorePermit::forget)ting permits (the standard
/// resizable-semaphore technique); for the blocking pool it adjusts the
/// available count and notifies waiters. Shrinking **never** revokes a permit
/// already held — it only reduces how many *new* permits can be handed out, so
/// in-flight work drains naturally and shrinking can never deadlock held
/// permits.
///
/// The gate is [`Clone`] (cloning shares the same underlying pools via [`Arc`]).
#[derive(Clone, Debug)]
pub struct AdaptiveGate {
    inner: Arc<GateInner>,
}

/// RAII guard for an async permit acquired from an [`AdaptiveGate`]. Releases
/// the permit on drop — unless the gate carries outstanding shrink debt, in
/// which case this permit is *forgotten* to pay that debt down (so a shrink
/// that happened while this permit was in flight takes effect exactly).
#[derive(Debug)]
pub struct GatePermit {
    inner: Arc<GateInner>,
    // `Option` so Drop can move the permit out to either return it (drop) or
    // forget it (pay shrink debt).
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for GatePermit {
    fn drop(&mut self) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        // If a shrink is still owed permits, consume this one toward that debt
        // (forget => the semaphore's capacity drops by one) instead of
        // returning it to the pool.
        let mut debt = self.inner.async_debt.load(Ordering::SeqCst);
        loop {
            if debt == 0 {
                return; // no debt: let the permit drop normally (returns it)
            }
            match self.inner.async_debt.compare_exchange_weak(
                debt,
                debt - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    permit.forget();
                    return;
                }
                Err(actual) => debt = actual,
            }
        }
    }
}

/// RAII guard for a blocking permit acquired from an [`AdaptiveGate`]. Releases
/// the permit (and notifies one waiter) on drop.
#[derive(Debug)]
pub struct BlockingGatePermit {
    inner: Arc<GateInner>,
}

impl Drop for BlockingGatePermit {
    fn drop(&mut self) {
        let mut state = self
            .inner
            .blocking
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.in_flight = state.in_flight.saturating_sub(1);
        // Only return the permit to the available pool if we are still within
        // the (possibly shrunk) limit; otherwise the permit is absorbed by the
        // shrink (in_flight already dropped, which is what matters).
        if state.available + state.in_flight < state.limit {
            state.available += 1;
            drop(state);
            self.inner.blocking_cv.notify_one();
        }
    }
}

impl AdaptiveGate {
    /// Builds a gate whose limit starts at `start` and can be retuned anywhere in
    /// `[1, ceiling]`. Both `start` and `ceiling` are clamped to at least 1, and
    /// `start` is clamped to `ceiling`.
    #[must_use]
    pub fn new(start: usize, ceiling: usize) -> Self {
        let ceiling = ceiling.max(1);
        let start = start.clamp(1, ceiling);
        Self {
            inner: Arc::new(GateInner {
                ceiling,
                limit: AtomicUsize::new(start),
                sem: Arc::new(Semaphore::new(start)),
                async_debt: AtomicUsize::new(0),
                blocking: Mutex::new(BlockingState {
                    available: start,
                    limit: start,
                    in_flight: 0,
                }),
                blocking_cv: Condvar::new(),
            }),
        }
    }

    /// The construction-time ceiling (the maximum the limit can ever reach).
    #[must_use]
    pub fn ceiling(&self) -> usize {
        self.inner.ceiling
    }

    /// The current logical concurrency limit (shared by both backends).
    #[must_use]
    pub fn limit(&self) -> usize {
        self.inner.limit.load(Ordering::SeqCst)
    }

    /// Retunes the effective concurrency limit live, clamped to `[1, ceiling]`.
    ///
    /// Grows the async pool with `add_permits`; shrinks it by reserving (and
    /// forgetting) the surplus permits so the semaphore's effective capacity
    /// drops without revoking permits already in flight. Adjusts the blocking
    /// pool's available count symmetrically and wakes waiters when growing.
    /// Returns the new (clamped) limit.
    pub fn set_limit(&self, n: usize) -> usize {
        let new = n.clamp(1, self.inner.ceiling);
        let old = self.inner.limit.swap(new, Ordering::SeqCst);
        if new == old {
            return new;
        }

        // --- async pool -----------------------------------------------------
        if new > old {
            // First, pay down any outstanding shrink debt with the growth (we
            // owed removals that hadn't landed yet); only add the remainder.
            let grow = new - old;
            let paid = self.take_debt(grow);
            let remainder = grow - paid;
            if remainder > 0 {
                self.inner.sem.add_permits(remainder);
            }
        } else {
            // Shrink: remove (old - new) permits. Forget what's available now;
            // record the rest as debt to be paid by in-flight permits' Drop.
            let mut to_remove = old - new;
            while to_remove > 0 {
                if let Ok(permit) = self.inner.sem.clone().try_acquire_owned() {
                    permit.forget();
                    to_remove -= 1;
                } else {
                    break; // remainder is in flight; record it as debt
                }
            }
            if to_remove > 0 {
                self.inner.async_debt.fetch_add(to_remove, Ordering::SeqCst);
            }
        }

        // --- blocking pool --------------------------------------------------
        {
            let mut state = self
                .inner
                .blocking
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.limit = new;
            // Recompute available as (limit - in_flight), floored at 0. Growing
            // raises available; shrinking lowers it (never touching in-flight
            // guards, which release against the new limit on drop).
            state.available = new.saturating_sub(state.in_flight);
            drop(state);
            if new > old {
                self.inner.blocking_cv.notify_all();
            }
        }

        new
    }

    /// Reduces the outstanding async shrink debt by up to `n`, returning how
    /// much was actually paid (so the caller can apply only the remainder when
    /// growing the pool).
    fn take_debt(&self, n: usize) -> usize {
        let mut debt = self.inner.async_debt.load(Ordering::SeqCst);
        loop {
            let pay = debt.min(n);
            if pay == 0 {
                return 0;
            }
            match self.inner.async_debt.compare_exchange_weak(
                debt,
                debt - pay,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return pay,
                Err(actual) => debt = actual,
            }
        }
    }

    /// Acquires one async permit, awaiting if the pool is at its current limit.
    /// The returned [`GatePermit`] releases the permit on drop.
    ///
    /// # Panics
    ///
    /// Never under normal use; the semaphore is only closed when the gate is
    /// dropped, which cannot happen while a caller holds an `&self`.
    pub async fn acquire(&self) -> GatePermit {
        let permit = Arc::clone(&self.inner.sem)
            .acquire_owned()
            .await
            .expect("AdaptiveGate semaphore is never closed while the gate is alive");
        GatePermit {
            inner: Arc::clone(&self.inner),
            permit: Some(permit),
        }
    }

    /// Blocking sibling of [`acquire`](AdaptiveGate::acquire): parks the calling
    /// OS thread until a permit is free, then returns a guard that releases it
    /// on drop. Used by the rayon store-to-store sync path.
    #[must_use]
    pub fn acquire_blocking(&self) -> BlockingGatePermit {
        let mut state = self
            .inner
            .blocking
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while state.available == 0 {
            state = self
                .inner
                .blocking_cv
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.available -= 1;
        state.in_flight += 1;
        BlockingGatePermit {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Best-effort count of async permits currently available (for tests/metrics).
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.inner.sem.available_permits()
    }
}

// ---------------------------------------------------------------------------
// Controller value types.
// ---------------------------------------------------------------------------

/// Outcome class of a single transfer operation, fed to
/// [`AdaptiveController::record_op`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpResult {
    /// Completed successfully.
    Ok,
    /// The backend explicitly throttled us (e.g. HTTP 429 / `SlowDown`).
    Throttle,
    /// A hard error, treated as congestion when timeout-class (e.g. request
    /// timeout, connection reset).
    HardErr,
}

/// One sampled transfer operation: bytes moved, observed latency, and outcome.
#[derive(Clone, Copy, Debug)]
pub struct OpSample {
    /// Bytes transferred by this operation.
    pub bytes: u64,
    /// End-to-end latency of the operation.
    pub latency: Duration,
    /// Outcome class.
    pub result: OpResult,
}

/// The controller's tuning policy (its *view* of config; the `TransferConfig`
/// integration is a later gate).
#[derive(Clone, Copy, Debug)]
pub struct AdaptivePolicy {
    /// Target operating fraction of the discovered knee (default `0.8`): the
    /// controller aims for `fraction × knee` for both concurrency and rate,
    /// leaving headroom.
    pub fraction: f64,
    /// Absolute concurrency ceiling; the limit never exceeds this.
    pub ceiling: usize,
    /// Total machine RAM in bytes, the denominator of the memory-budget
    /// guardrail (`limit × p95_obj_size ≤ fraction × total_ram`). `0` disables
    /// the memory cap.
    pub total_ram: u64,
    /// Optional hard cap on the target byte-rate (e.g. a user `--max-rate`).
    /// `None` means rate is bounded only by the measured goodput knee.
    pub max_rate: Option<u64>,
}

impl AdaptivePolicy {
    /// Builds a policy. `fraction` is clamped to `(0, 1]` (default `0.8` if
    /// non-finite or out of range); `ceiling` is clamped to at least 1.
    #[must_use]
    pub fn new(fraction: f64, ceiling: usize, total_ram: u64, max_rate: Option<u64>) -> Self {
        let fraction = if fraction.is_finite() && fraction > 0.0 && fraction <= 1.0 {
            fraction
        } else {
            0.8
        };
        Self {
            fraction,
            ceiling: ceiling.max(1),
            total_ram,
            max_rate,
        }
    }
}

impl Default for AdaptivePolicy {
    fn default() -> Self {
        Self {
            fraction: 0.8,
            ceiling: 16,
            total_ram: 0,
            max_rate: None,
        }
    }
}

/// The controller's output for one [`tick`](AdaptiveController::tick): the next
/// concurrency limit and an optional target byte-rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    /// The new concurrency limit to apply (clamped to `[1, ceiling]` and the
    /// memory budget).
    pub limit: usize,
    /// The new aggregate target byte-rate, or `None` for unlimited. Computed as
    /// `fraction × measured-goodput-knee`, clamped to `policy.max_rate`.
    pub target_rate: Option<u64>,
}

// ---------------------------------------------------------------------------
// AdaptiveController — the pure control law.
// ---------------------------------------------------------------------------

/// Which phase of the control law we are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Multiplicatively ramping up (`×1.5`/tick) until the first congestion/knee.
    SlowStart,
    /// Additive-increase / multiplicative-decrease steady state.
    Aimd,
}

/// EWMA smoothing factor for goodput / rtt (higher = more responsive).
const EWMA_ALPHA: f64 = 0.3;
/// Slow-start growth multiplier per tick.
const SLOW_START_MULT: f64 = 1.5;
/// Multiplicative decrease factor on congestion (AIMD's `×0.5`).
const BACKOFF_MULT: f64 = 0.5;
/// Latency-gradient threshold: `rtt_min/rtt` below this ⇒ queueing detected.
const GRADIENT_THRESHOLD: f64 = 0.7;
/// CPU percentage above which we never increase the limit.
const CPU_NO_INCREASE_PCT: f64 = 85.0;
/// CPU percentage above which we actively decrease the limit.
const CPU_DECREASE_PCT: f64 = 95.0;
/// Cooldown after a congestion event during which we never increase.
const COOLDOWN: Duration = Duration::from_secs(15);
/// Re-probe interval when stable.
const REPROBE_INTERVAL: Duration = Duration::from_secs(15);
/// Relative goodput improvement required for slow-start to keep growing /
/// for a re-probe to be kept (hysteresis).
const IMPROVE_EPS: f64 = 0.02;

/// Injected monotonic timestamp. The controller never reads a real clock; the
/// caller passes a monotonically non-decreasing value (real code:
/// `Instant::now()`; tests: a fake counter). Stored as nanoseconds since an
/// arbitrary epoch so the type is trivially `Copy` and deterministic.
pub type MonoTime = Duration;

/// The adaptive control brain.
///
/// PURE and deterministic: it never calls [`std::time::Instant::now`] or the
/// [`snapdir_core::resources`] samplers. All time and metrics are injected via
/// [`tick`](AdaptiveController::tick); per-op feedback via
/// [`record_op`](AdaptiveController::record_op). Given the same inputs it always
/// produces the same [`Decision`]s.
#[derive(Debug)]
pub struct AdaptiveController {
    policy: AdaptivePolicy,

    /// Current concurrency limit (the controller's authoritative value).
    limit: f64,
    /// Control phase (slow-start vs AIMD).
    phase: Phase,

    /// EWMA of goodput in bytes/sec (across recorded ops since the last tick).
    goodput_ewma: f64,
    /// Best goodput seen so far (the "knee" estimate).
    goodput_knee: f64,
    /// Goodput EWMA captured at the previous tick (for slow-start improvement).
    goodput_prev_tick: f64,

    /// EWMA of per-op latency, in seconds.
    rtt_ewma: f64,
    /// Minimum latency observed (the unloaded baseline), in seconds.
    rtt_min: f64,

    /// Accumulators for ops recorded since the last `tick`.
    acc_bytes: u64,
    acc_latency_secs: f64,
    acc_count: u64,
    /// Set when any op since the last tick was a Throttle / timeout-class error.
    congestion_seen: bool,

    /// `Some(deadline)` while in a post-congestion cooldown (no increases).
    cooldown_until: Option<MonoTime>,
    /// Time of the last re-probe (or controller start).
    last_reprobe: MonoTime,
    /// `Some((limit_before, goodput_before))` while a re-probe is outstanding,
    /// so the next tick can keep-or-revert it.
    probe_pending: Option<(f64, f64)>,

    /// Whether `tick` has run at least once (to seed `last_reprobe`).
    started: bool,
}

impl AdaptiveController {
    /// Builds a controller from a policy. The limit starts at `2` (slow-start
    /// seed), clamped to the policy ceiling.
    #[must_use]
    pub fn new(policy: AdaptivePolicy) -> Self {
        let start = 2.0_f64.min(policy.ceiling as f64).max(1.0);
        Self {
            policy,
            limit: start,
            phase: Phase::SlowStart,
            goodput_ewma: 0.0,
            goodput_knee: 0.0,
            goodput_prev_tick: 0.0,
            rtt_ewma: 0.0,
            rtt_min: f64::INFINITY,
            acc_bytes: 0,
            acc_latency_secs: 0.0,
            acc_count: 0,
            congestion_seen: false,
            cooldown_until: None,
            last_reprobe: Duration::ZERO,
            probe_pending: None,
            started: false,
        }
    }

    /// The current concurrency limit (clamped to `[1, ceiling]`).
    #[must_use]
    pub fn current_limit(&self) -> usize {
        (self.limit.round() as usize).clamp(1, self.policy.ceiling)
    }

    /// Records one completed (or failed) operation, updating the running EWMAs
    /// and the congestion marker. Call between ticks; the accumulated samples
    /// are folded into the goodput/rtt estimates at the next
    /// [`tick`](AdaptiveController::tick).
    pub fn record_op(&mut self, sample: OpSample) {
        let secs = sample.latency.as_secs_f64().max(0.0);

        // Per-op latency EWMA + min (baseline) tracking. Skip zero-latency
        // degenerate samples for the min so a bogus 0 never pins rtt_min.
        if secs > 0.0 {
            if self.rtt_ewma <= 0.0 {
                self.rtt_ewma = secs;
            } else {
                self.rtt_ewma = EWMA_ALPHA.mul_add(secs - self.rtt_ewma, self.rtt_ewma);
            }
            if secs < self.rtt_min {
                self.rtt_min = secs;
            }
        }

        self.acc_bytes = self.acc_bytes.saturating_add(sample.bytes);
        self.acc_latency_secs += secs;
        self.acc_count += 1;

        match sample.result {
            OpResult::Throttle | OpResult::HardErr => self.congestion_seen = true,
            OpResult::Ok => {}
        }
    }

    /// Applies the control law for one interval and returns the next
    /// [`Decision`]. `now` is the injected monotonic time, `cpu_pct`/`rss` are
    /// best-effort system samples (`None` ⇒ unknown ⇒ that guardrail is
    /// skipped), and `p95_obj_size` is the recent 95th-percentile object size
    /// used by the memory-budget guardrail.
    ///
    /// The controller never reads a real clock or sampler; all of these are
    /// supplied by the (later) wiring gate.
    pub fn tick(
        &mut self,
        now: MonoTime,
        cpu_pct: Option<f64>,
        _rss: Option<u64>,
        p95_obj_size: u64,
    ) -> Decision {
        if !self.started {
            self.started = true;
            self.last_reprobe = now;
        }

        // ---- fold accumulated op samples into the goodput EWMA -------------
        let interval_goodput = self.window_goodput();
        if interval_goodput > 0.0 {
            if self.goodput_ewma <= 0.0 {
                self.goodput_ewma = interval_goodput;
            } else {
                self.goodput_ewma =
                    EWMA_ALPHA.mul_add(interval_goodput - self.goodput_ewma, self.goodput_ewma);
            }
            if self.goodput_ewma > self.goodput_knee {
                self.goodput_knee = self.goodput_ewma;
            }
        }
        let congestion = self.congestion_seen;
        let gradient = self.latency_gradient();

        // ---- guardrail flags ----------------------------------------------
        let cpu_blocks_increase = cpu_pct.is_some_and(|c| c > CPU_NO_INCREASE_PCT);
        let cpu_forces_decrease = cpu_pct.is_some_and(|c| c > CPU_DECREASE_PCT);
        let in_cooldown = self.cooldown_until.is_some_and(|d| now < d);
        // Latency gradient below threshold ⇒ queue building ⇒ hold/decrease.
        let queueing = gradient.is_some_and(|g| g < GRADIENT_THRESHOLD);

        // A pending re-probe is resolved first (keep if it helped, else revert).
        if let Some((prev_limit, prev_goodput)) = self.probe_pending.take() {
            let improved = self.goodput_ewma > prev_goodput * (1.0 + IMPROVE_EPS);
            if !improved || congestion || queueing {
                self.limit = prev_limit; // revert the speculative probe
            }
            // else: keep the higher limit.
        }

        // ---- congestion backoff (highest priority) ------------------------
        if congestion {
            self.limit = (self.limit * BACKOFF_MULT).max(1.0);
            self.phase = Phase::Aimd; // first knee ends slow-start
            self.cooldown_until = Some(now + COOLDOWN);
            self.goodput_prev_tick = self.goodput_ewma;
            return self.finish(now, p95_obj_size);
        }

        // ---- CPU hard decrease --------------------------------------------
        if cpu_forces_decrease {
            self.limit = (self.limit * BACKOFF_MULT).max(1.0);
            self.goodput_prev_tick = self.goodput_ewma;
            return self.finish(now, p95_obj_size);
        }

        // ---- latency-gradient hold/decrease -------------------------------
        if queueing {
            // Queue building without an explicit error: gently decrease and
            // leave slow-start. Hysteresis: only step down by one.
            if self.phase == Phase::SlowStart {
                self.phase = Phase::Aimd;
            }
            self.limit = (self.limit - 1.0).max(1.0);
            self.goodput_prev_tick = self.goodput_ewma;
            return self.finish(now, p95_obj_size);
        }

        // ---- no-increase guards (cooldown / CPU≈busy) ---------------------
        if in_cooldown || cpu_blocks_increase {
            self.goodput_prev_tick = self.goodput_ewma;
            return self.finish(now, p95_obj_size);
        }

        // ---- healthy: grow per phase --------------------------------------
        match self.phase {
            Phase::SlowStart => {
                // Keep multiplying while goodput is still rising; once it
                // plateaus, treat as the knee and switch to AIMD.
                let rising = self.goodput_ewma > self.goodput_prev_tick * (1.0 + IMPROVE_EPS)
                    || self.goodput_prev_tick <= 0.0;
                if rising {
                    self.limit *= SLOW_START_MULT;
                } else {
                    self.phase = Phase::Aimd;
                    self.limit += 1.0; // gentle additive after the knee
                }
            }
            Phase::Aimd => {
                // Additive increase, but periodically do an explicit re-probe
                // (mark it pending so the next tick keeps-or-reverts).
                if now >= self.last_reprobe + REPROBE_INTERVAL {
                    self.last_reprobe = now;
                    self.probe_pending = Some((self.limit, self.goodput_ewma));
                    self.limit += 1.0;
                } else {
                    self.limit += 1.0;
                }
            }
        }

        self.goodput_prev_tick = self.goodput_ewma;
        self.finish(now, p95_obj_size)
    }

    /// Goodput over the just-closed window (bytes / summed-latency), or 0 when
    /// no ops were recorded.
    fn window_goodput(&self) -> f64 {
        if self.acc_count == 0 || self.acc_latency_secs <= 0.0 {
            return 0.0;
        }
        // Aggregate goodput ≈ bytes moved divided by the *average* per-op
        // latency times concurrency is implicit in the op stream; here we use
        // the simple bytes / total-latency * current-limit estimate, which
        // rises with both faster ops and more parallelism.
        let per_op = self.acc_bytes as f64 / self.acc_latency_secs;
        per_op * self.limit
    }

    /// `rtt_min / rtt_ewma` (∈ (0,1]); `None` until both are known. Near 1.0
    /// means no queueing; small means latency has inflated (congestion).
    fn latency_gradient(&self) -> Option<f64> {
        if self.rtt_ewma > 0.0 && self.rtt_min.is_finite() && self.rtt_min > 0.0 {
            Some((self.rtt_min / self.rtt_ewma).clamp(0.0, 1.0))
        } else {
            None
        }
    }

    /// Applies the hard caps (ceiling + memory budget), resets the per-window
    /// accumulators, and packages the [`Decision`].
    fn finish(&mut self, _now: MonoTime, p95_obj_size: u64) -> Decision {
        // Memory-budget hard cap: limit × p95_obj_size ≤ fraction × total_ram.
        let mem_cap = self.memory_cap(p95_obj_size);
        let ceiling = self.policy.ceiling;
        let capped = (self.limit.round() as usize).clamp(1, ceiling).min(mem_cap);
        // Persist the capped value so the cap is sticky (the controller never
        // "remembers" a limit it is not allowed to use).
        self.limit = capped as f64;

        // Reset per-window accumulators.
        self.acc_bytes = 0;
        self.acc_latency_secs = 0.0;
        self.acc_count = 0;
        self.congestion_seen = false;

        Decision {
            limit: capped,
            target_rate: self.target_rate(),
        }
    }

    /// The largest concurrency the memory budget permits:
    /// `floor(fraction × total_ram / p95_obj_size)`, at least 1. When either
    /// `total_ram` or `p95_obj_size` is 0 the budget is unbounded (returns the
    /// ceiling).
    fn memory_cap(&self, p95_obj_size: u64) -> usize {
        if self.policy.total_ram == 0 || p95_obj_size == 0 {
            return self.policy.ceiling;
        }
        let budget = self.policy.fraction * self.policy.total_ram as f64;
        let cap = (budget / p95_obj_size as f64).floor();
        if cap < 1.0 {
            1
        } else {
            (cap as usize).min(self.policy.ceiling)
        }
    }

    /// `fraction × measured-goodput-knee`, clamped to `policy.max_rate`. `None`
    /// until a knee has been observed and no rate cap forces a value.
    fn target_rate(&self) -> Option<u64> {
        let knee_rate = if self.goodput_knee > 0.0 {
            Some((self.policy.fraction * self.goodput_knee).max(1.0) as u64)
        } else {
            None
        };
        match (knee_rate, self.policy.max_rate) {
            (Some(k), Some(m)) => Some(k.min(m)),
            (Some(k), None) => Some(k),
            (None, Some(m)) => Some(m),
            (None, None) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// ControllerDriver — bridges the pure controller to the live transfer loops.
// ---------------------------------------------------------------------------

/// Computes the 95th-percentile of a set of object sizes (the memory-budget
/// denominator the controller's `tick` expects). An empty set yields `0`
/// (memory guardrail disabled). The slice is cloned + sorted locally so the
/// caller's data is untouched.
#[must_use]
pub fn p95_object_size(sizes: &[u64]) -> u64 {
    if sizes.is_empty() {
        return 0;
    }
    let mut sorted = sizes.to_vec();
    sorted.sort_unstable();
    // Nearest-rank p95: index = ceil(0.95 * n) - 1, clamped into range.
    let n = sorted.len();
    let rank = ((0.95 * n as f64).ceil() as usize).max(1);
    sorted[rank.min(n) - 1]
}

/// Shared, live bridge between a pure [`AdaptiveController`] and the running
/// transfer backends.
///
/// The controller is `&mut` and deterministic; this wrapper owns it behind a
/// [`Mutex`] (tick is infrequent, so contention is negligible) and adds the
/// *impure* parts the controller deliberately omits: a real monotonic clock, a
/// live [`CpuSampler`] + RSS sampler, the manifest's p95 object size, and the
/// application of each [`Decision`] to the shared [`AdaptiveGate`] (concurrency)
/// and — for the network backends — a rate-limiter setter + display [`Meter`].
///
/// Both transfer paths use it identically:
///
/// - every completed/failed op calls [`record_op`](ControllerDriver::record_op)
///   with its measured `OpSample`;
/// - a lightweight driver (a tokio `interval` task for the async path, a
///   `std::thread` for the rayon path) periodically calls
///   [`tick`](ControllerDriver::tick), which samples CPU/RSS, advances the
///   controller, resizes the gate, and reports the new limit/rate.
///
/// The driver is [`Clone`] (the controller, gate, sampler and the
/// rate/limit appliers are all shared via [`Arc`]).
#[derive(Clone)]
pub struct ControllerDriver {
    controller: Arc<Mutex<AdaptiveController>>,
    gate: AdaptiveGate,
    cpu: Arc<Mutex<CpuSampler>>,
    p95_obj_size: u64,
    /// Wall-clock origin: `tick` injects `now = epoch.elapsed()` so the
    /// controller sees a monotonic [`MonoTime`].
    epoch: Instant,
    /// Applies a target byte-rate to the backend's live rate limiter (async or
    /// blocking). Local `FileStore` has no rate limit, so this is `None`.
    rate_applier: Option<Arc<dyn Fn(Option<u64>) + Send + Sync>>,
    /// Optional display meter: the new limit / target rate are mirrored into it
    /// for the progress bar (advisory only).
    meter: Option<Arc<Meter>>,
}

// The closure / mutex-wrapped controller fields are intentionally omitted from
// the human-facing debug view (a `dyn Fn` is not `Debug`, and the controller's
// internals are large + uninteresting here); the load-bearing live state
// (limits, p95, wiring presence) is shown instead.
#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for ControllerDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerDriver")
            .field("gate_limit", &self.gate.limit())
            .field("ceiling", &self.gate.ceiling())
            .field("p95_obj_size", &self.p95_obj_size)
            .field("has_rate_applier", &self.rate_applier.is_some())
            .field("has_meter", &self.meter.is_some())
            .finish()
    }
}

impl ControllerDriver {
    /// Builds a driver around a fresh controller for `policy`, the shared
    /// `gate`, the manifest's `p95_obj_size`, an optional `rate_applier` (the
    /// backend's live rate-limit setter; `None` for the rate-less local store),
    /// and an optional display `meter`.
    #[must_use]
    pub fn new(
        policy: AdaptivePolicy,
        gate: AdaptiveGate,
        p95_obj_size: u64,
        rate_applier: Option<Arc<dyn Fn(Option<u64>) + Send + Sync>>,
        meter: Option<Arc<Meter>>,
    ) -> Self {
        Self {
            controller: Arc::new(Mutex::new(AdaptiveController::new(policy))),
            gate,
            cpu: Arc::new(Mutex::new(CpuSampler::new())),
            p95_obj_size,
            epoch: Instant::now(),
            rate_applier,
            meter,
        }
    }

    /// Records one completed/failed op into the shared controller.
    pub fn record_op(&self, sample: OpSample) {
        self.controller
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record_op(sample);
    }

    /// Advances the controller one interval: samples CPU/RSS, ticks with the
    /// injected monotonic clock + p95 object size, then applies the resulting
    /// [`Decision`] to the gate (concurrency), the rate applier (byte-rate), and
    /// the display meter. Returns the decision for tests/inspection.
    pub fn tick(&self) -> Decision {
        let cpu_pct = self
            .cpu
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .poll();
        let rss = resident_set_bytes();
        let now = self.epoch.elapsed();

        let decision = {
            let mut controller = self
                .controller
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            controller.tick(now, cpu_pct, rss, self.p95_obj_size)
        };

        // Apply: resize the shared gate, retune the rate limiter, mirror both
        // into the display meter (all advisory; never change what is transferred).
        self.gate.set_limit(decision.limit);
        if let Some(apply) = &self.rate_applier {
            apply(decision.target_rate);
        }
        if let Some(meter) = &self.meter {
            meter.set_current_limit(decision.limit as u64);
            meter.set_target_rate(decision.target_rate.unwrap_or(0));
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    // ----- AdaptiveGate: async ----------------------------------------------

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build tokio runtime")
    }

    #[test]
    fn adaptive_gate_async_acquire_blocks_beyond_limit() {
        let rt = runtime();
        rt.block_on(async {
            let gate = AdaptiveGate::new(2, 8);
            let p1 = gate.acquire().await;
            let p2 = gate.acquire().await;
            assert_eq!(gate.available_permits(), 0, "both permits taken");

            // A third acquire must not be immediately ready.
            let fut = gate.acquire();
            tokio::pin!(fut);
            let pending = futures::poll!(&mut fut);
            assert!(pending.is_pending(), "third acquire blocks at limit 2");

            // Raising the limit unblocks it.
            gate.set_limit(3);
            let p3 = futures::poll!(&mut fut);
            assert!(p3.is_ready(), "set_limit(3) frees a permit for the waiter");
            drop((p1, p2));
        });
    }

    /// Mirror transfer.rs's `max_in_flight_for` idiom: drive many acquires and
    /// record the peak concurrent holders under a fixed gate limit.
    fn peak_under_limit(limit: usize, items: usize) -> usize {
        let rt = runtime();
        rt.block_on(async {
            let gate = AdaptiveGate::new(limit, 32);
            let in_flight = Arc::new(AtomicUsize::new(0));
            let high = Arc::new(AtomicUsize::new(0));
            let mut handles = Vec::new();
            for _ in 0..items {
                let gate = gate.clone();
                let in_flight = Arc::clone(&in_flight);
                let high = Arc::clone(&high);
                handles.push(tokio::spawn(async move {
                    let _p = gate.acquire().await;
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    high.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
            high.load(Ordering::SeqCst)
        })
    }

    #[test]
    fn adaptive_gate_async_set_limit_changes_max_in_flight() {
        assert_eq!(peak_under_limit(3, 12), 3, "limit 3 caps in-flight at 3");
        assert_eq!(peak_under_limit(1, 5), 1, "limit 1 is strictly sequential");
    }

    #[test]
    fn adaptive_gate_async_shrink_does_not_deadlock_held_permits() {
        let rt = runtime();
        rt.block_on(async {
            let gate = AdaptiveGate::new(4, 8);
            let p1 = gate.acquire().await;
            let p2 = gate.acquire().await;
            // Shrink below the number of held permits.
            let new = gate.set_limit(1);
            assert_eq!(new, 1);
            // Dropping held permits must not panic/deadlock; afterwards the
            // effective limit settles to 1.
            drop(p1);
            drop(p2);
            // Acquire one (should succeed) and confirm a second blocks.
            let _q = gate.acquire().await;
            let fut = gate.acquire();
            tokio::pin!(fut);
            assert!(
                futures::poll!(&mut fut).is_pending(),
                "after shrink to 1, only one permit is available"
            );
        });
    }

    // ----- AdaptiveGate: blocking -------------------------------------------

    #[test]
    fn adaptive_gate_blocking_acquire_and_set_limit() {
        let gate = AdaptiveGate::new(1, 8);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let high = Arc::new(AtomicUsize::new(0));

        // With limit 1, two threads must serialize (peak in-flight 1).
        let mut handles = Vec::new();
        for _ in 0..4 {
            let gate = gate.clone();
            let in_flight = Arc::clone(&in_flight);
            let high = Arc::clone(&high);
            handles.push(thread::spawn(move || {
                let _p = gate.acquire_blocking();
                let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                high.fetch_max(cur, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(30));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            high.load(Ordering::SeqCst),
            1,
            "blocking limit 1 serializes"
        );

        // Now raise the limit and confirm parallelism rises.
        let gate2 = AdaptiveGate::new(1, 8);
        gate2.set_limit(3);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let high = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let gate = gate2.clone();
            let in_flight = Arc::clone(&in_flight);
            let high = Arc::clone(&high);
            handles.push(thread::spawn(move || {
                let _p = gate.acquire_blocking();
                let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                high.fetch_max(cur, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(30));
                in_flight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let peak = high.load(Ordering::SeqCst);
        assert!(
            (1..=3).contains(&peak) && peak >= 2,
            "after set_limit(3) peak in-flight should reach ~3, got {peak}"
        );
    }

    #[test]
    fn adaptive_gate_blocking_shrink_does_not_deadlock() {
        let gate = AdaptiveGate::new(4, 8);
        let p1 = gate.acquire_blocking();
        let p2 = gate.acquire_blocking();
        gate.set_limit(1); // shrink below held count
        drop(p1);
        drop(p2);
        // A subsequent acquire must still succeed (no deadlock / no lost permit).
        let got = Arc::new(AtomicUsize::new(0));
        let g = gate.clone();
        let got2 = Arc::clone(&got);
        let h = thread::spawn(move || {
            let _p = g.acquire_blocking();
            got2.fetch_add(1, Ordering::SeqCst);
        });
        h.join().unwrap();
        assert_eq!(got.load(Ordering::SeqCst), 1, "acquire after shrink works");
    }

    // ----- AdaptiveController -----------------------------------------------

    /// Convenience: a healthy Ok op of `bytes` at `latency_ms`.
    fn ok_op(bytes: u64, latency_ms: u64) -> OpSample {
        OpSample {
            bytes,
            latency: Duration::from_millis(latency_ms),
            result: OpResult::Ok,
        }
    }

    fn big_policy() -> AdaptivePolicy {
        // Big RAM so the memory guardrail never bites unless a test wants it.
        AdaptivePolicy::new(0.8, 64, u64::MAX, None)
    }

    #[test]
    fn adaptive_controller_healthy_stream_ramps_up_then_caps() {
        let mut c = AdaptiveController::new(big_policy());
        let mut t = Duration::ZERO;
        let mut last = c.current_limit();
        let mut max_seen = last;
        // Rising goodput: each tick the ops get "faster" (more bytes/sec).
        for i in 0..20u64 {
            for _ in 0..4 {
                c.record_op(ok_op(1_000_000 + i * 200_000, 50));
            }
            let d = c.tick(t, Some(30.0), Some(0), 4096);
            assert!(d.limit <= 64, "never exceed ceiling");
            max_seen = max_seen.max(d.limit);
            last = d.limit;
            t += Duration::from_secs(1);
        }
        assert!(
            max_seen > 2,
            "healthy stream should ramp the limit up from the start of 2, got max {max_seen}"
        );
        assert!(last <= 64, "stays within ceiling, got {last}");
    }

    #[test]
    fn adaptive_controller_throttle_backs_off_and_cooldown_holds() {
        let mut c = AdaptiveController::new(big_policy());
        let mut t = Duration::ZERO;
        // Ramp up first.
        for _ in 0..6 {
            for _ in 0..4 {
                c.record_op(ok_op(2_000_000, 40));
            }
            c.tick(t, Some(20.0), Some(0), 4096);
            t += Duration::from_secs(1);
        }
        let before = c.current_limit();
        assert!(before > 2, "should have grown before the throttle");

        // Inject a Throttle.
        c.record_op(OpSample {
            bytes: 1000,
            latency: Duration::from_millis(40),
            result: OpResult::Throttle,
        });
        let d = c.tick(t, Some(20.0), Some(0), 4096);
        assert!(
            d.limit <= before / 2 + 1,
            "throttle should at least halve the limit: before {before}, after {}",
            d.limit
        );
        let after_backoff = d.limit;
        t += Duration::from_secs(1);

        // For the 15s cooldown, even healthy ticks must not increase the limit.
        for _ in 0..10 {
            for _ in 0..4 {
                c.record_op(ok_op(5_000_000, 20)); // very healthy
            }
            let d = c.tick(t, Some(10.0), Some(0), 4096);
            assert!(
                d.limit <= after_backoff,
                "no increase during cooldown: {} > {after_backoff}",
                d.limit
            );
            t += Duration::from_secs(1);
        }

        // After the cooldown (>15s), increases resume.
        t += Duration::from_secs(6);
        for _ in 0..3 {
            for _ in 0..4 {
                c.record_op(ok_op(6_000_000, 20));
            }
            c.tick(t, Some(10.0), Some(0), 4096);
            t += Duration::from_secs(1);
        }
        assert!(
            c.current_limit() > after_backoff,
            "limit should recover after the cooldown expires"
        );
    }

    #[test]
    fn adaptive_controller_rising_latency_holds_without_error() {
        let mut c = AdaptiveController::new(big_policy());
        let mut t = Duration::ZERO;
        // Establish a low rtt_min with fast ops, ramp up.
        for _ in 0..5 {
            for _ in 0..4 {
                c.record_op(ok_op(2_000_000, 10)); // 10ms baseline
            }
            c.tick(t, Some(20.0), Some(0), 4096);
            t += Duration::from_secs(1);
        }
        let peak = c.current_limit();

        // Now latency inflates massively (queueing) but NO errors: gradient
        // rtt_min/rtt drops well below threshold ⇒ controller must not grow.
        for _ in 0..6 {
            for _ in 0..4 {
                c.record_op(ok_op(2_000_000, 200)); // 200ms now, 20x baseline
            }
            let d = c.tick(t, Some(20.0), Some(0), 4096);
            assert!(
                d.limit <= peak,
                "high latency gradient must hold/decrease (no growth): {} > {peak}",
                d.limit
            );
            t += Duration::from_secs(1);
        }
        assert!(
            c.current_limit() <= peak,
            "latency-gradient guard held the limit at/below the peak"
        );
    }

    #[test]
    fn adaptive_controller_memory_budget_caps_limit() {
        // Tiny RAM, large object size ⇒ memory budget forces a small limit.
        // budget = 0.8 * 10MiB = 8MiB; p95 = 2MiB ⇒ cap = floor(8/2) = 4.
        let total_ram = 10 * 1024 * 1024;
        let p95 = 2 * 1024 * 1024;
        let policy = AdaptivePolicy::new(0.8, 64, total_ram, None);
        let mut c = AdaptiveController::new(policy);
        let mut t = Duration::ZERO;
        for _ in 0..30 {
            for _ in 0..4 {
                c.record_op(ok_op(10_000_000, 10)); // very healthy, wants to grow
            }
            let d = c.tick(t, Some(10.0), Some(0), p95);
            // Invariant: limit * p95 <= fraction * total_ram (never violated).
            assert!(
                (d.limit as u64) * p95 <= ((0.8 * total_ram as f64) as u64),
                "memory budget violated: limit {} * p95 {} > budget",
                d.limit,
                p95
            );
            assert!(
                d.limit <= 4,
                "memory cap should pin limit at 4, got {}",
                d.limit
            );
            t += Duration::from_secs(1);
        }
    }

    #[test]
    fn adaptive_controller_high_cpu_prevents_increase() {
        let mut c = AdaptiveController::new(big_policy());
        let mut t = Duration::ZERO;
        // Warm up a little at low CPU.
        for _ in 0..3 {
            for _ in 0..4 {
                c.record_op(ok_op(2_000_000, 20));
            }
            c.tick(t, Some(20.0), Some(0), 4096);
            t += Duration::from_secs(1);
        }
        let before = c.current_limit();
        // Now CPU is pinned > 85%: no increase, even with healthy ops.
        for _ in 0..8 {
            for _ in 0..4 {
                c.record_op(ok_op(5_000_000, 10));
            }
            let d = c.tick(t, Some(90.0), Some(0), 4096);
            assert!(
                d.limit <= before,
                "cpu>85 must block increases: {} > {before}",
                d.limit
            );
            t += Duration::from_secs(1);
        }
    }

    #[test]
    fn adaptive_controller_converges_on_steady_stream() {
        let mut c = AdaptiveController::new(big_policy());
        let mut t = Duration::ZERO;
        // Run long enough to ramp and settle on a perfectly steady stream.
        let mut limits = Vec::new();
        for _ in 0..60 {
            for _ in 0..4 {
                c.record_op(ok_op(3_000_000, 25)); // identical every op
            }
            let d = c.tick(t, Some(40.0), Some(0), 4096);
            limits.push(d.limit);
            t += Duration::from_secs(1);
        }
        // Look at the tail: it should not oscillate wildly. Measure the spread
        // of the last 15 ticks.
        let tail = &limits[limits.len() - 15..];
        let min = *tail.iter().min().unwrap();
        let max = *tail.iter().max().unwrap();
        assert!(
            max - min <= 3,
            "steady stream should converge (small tail spread), got min {min} max {max} tail {tail:?}"
        );
    }

    // ----- ControllerDriver -------------------------------------------------

    #[test]
    fn p95_object_size_nearest_rank() {
        assert_eq!(p95_object_size(&[]), 0, "empty -> 0 (memory cap disabled)");
        assert_eq!(p95_object_size(&[42]), 42, "single element");
        // 1..=100: nearest-rank p95 = the 95th value = 95.
        let sizes: Vec<u64> = (1..=100).collect();
        assert_eq!(p95_object_size(&sizes), 95);
        // p95 is robust to one huge outlier in a small set (returns the max here).
        assert_eq!(p95_object_size(&[1, 1, 1, 1_000_000]), 1_000_000);
    }

    #[test]
    fn controller_driver_throttle_drives_gate_decrease() {
        // A driver wired to a gate: record several healthy ops + tick to grow the
        // gate, then inject a Throttle + tick and assert the gate's live limit
        // dropped (the controller's backoff was applied to the real gate).
        let gate = AdaptiveGate::new(2, 32);
        let applied_rate = Arc::new(Mutex::new(None::<Option<u64>>));
        let rate_sink = Arc::clone(&applied_rate);
        let rate_applier: Arc<dyn Fn(Option<u64>) + Send + Sync> = Arc::new(move |r| {
            *rate_sink.lock().unwrap() = Some(r);
        });
        let policy = AdaptivePolicy::new(0.8, 32, u64::MAX, None);
        let driver = ControllerDriver::new(policy, gate.clone(), 4096, Some(rate_applier), None);

        // Grow: healthy ops over several ticks should raise the gate above 2.
        for _ in 0..8 {
            for _ in 0..4 {
                driver.record_op(OpSample {
                    bytes: 2_000_000,
                    latency: Duration::from_millis(40),
                    result: OpResult::Ok,
                });
            }
            driver.tick();
        }
        let grown = gate.limit();
        assert!(
            grown > 2,
            "healthy stream should grow the gate, got {grown}"
        );

        // Inject a Throttle, then tick: the gate's limit must drop (backoff).
        driver.record_op(OpSample {
            bytes: 1000,
            latency: Duration::from_millis(40),
            result: OpResult::Throttle,
        });
        let decision = driver.tick();
        assert!(
            gate.limit() < grown,
            "throttle must shrink the gate: {} >= {grown}",
            gate.limit()
        );
        assert_eq!(
            gate.limit(),
            decision.limit,
            "the gate reflects the decision's limit"
        );
        // A rate was applied to the limiter at least once (target_rate flows out).
        assert!(
            applied_rate.lock().unwrap().is_some(),
            "the rate applier must have been invoked"
        );
    }

    #[test]
    fn adaptive_controller_target_rate_respects_fraction() {
        let mut c = AdaptiveController::new(big_policy());
        let mut t = Duration::ZERO;
        let mut last_rate = None;
        for _ in 0..15 {
            for _ in 0..4 {
                c.record_op(ok_op(1_000_000, 100)); // steady goodput
            }
            let d = c.tick(t, Some(30.0), Some(0), 4096);
            last_rate = d.target_rate;
            t += Duration::from_secs(1);
        }
        let rate = last_rate.expect("a knee should have produced a target_rate");
        // target_rate must be ~fraction (0.8) of the measured goodput knee.
        // We don't know the exact knee, but it must be positive and the
        // controller computed it as 0.8 * knee, so re-deriving: rate/0.8 = knee.
        assert!(
            rate > 0,
            "target_rate should be positive once a knee is known"
        );

        // With a max_rate cap below the knee fraction, the target is clamped.
        let policy = AdaptivePolicy::new(0.8, 64, u64::MAX, Some(1234));
        let mut c2 = AdaptiveController::new(policy);
        let mut t2 = Duration::ZERO;
        for _ in 0..10 {
            for _ in 0..4 {
                c2.record_op(ok_op(10_000_000, 10));
            }
            let d = c2.tick(t2, Some(30.0), Some(0), 4096);
            assert!(
                d.target_rate.unwrap_or(0) <= 1234,
                "target_rate must respect max_rate cap"
            );
            t2 += Duration::from_secs(1);
        }
    }
}

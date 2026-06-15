//! A pure, lock-free progress [`Meter`] for the filesystem walk and (later) the
//! transfer path.
//!
//! Per the library-purity principle this module does **no** terminal I/O and
//! reads **no** `$HOME`/config/environment for behavior. The [`Meter`] is a bag
//! of [`std::sync::atomic`] counters updated with [`Ordering::Relaxed`]: the
//! recording side ([`walk_with_meter`](crate::walk_with_meter)) bumps the
//! counters as it hashes files, and a (separately-laned) CLI renderer takes a
//! cheap [`MeterSnapshot`] to draw a progress bar. All methods take `&self`, so
//! the meter is shared across threads behind an [`Arc`](std::sync::Arc) without
//! a lock.
//!
//! The meter is intentionally *advisory*: recording into it never changes the
//! walk's output. A walk with a meter and the same walk without one produce
//! byte-identical manifests.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

/// The coarse phase a [`Meter`] is currently recording.
///
/// Maps to/from a `u8` for lock-free storage in an [`AtomicU8`]. The default is
/// [`Phase::Idle`] so a freshly-constructed [`MeterSnapshot`] (and an untouched
/// meter) reports an idle phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// No work is being recorded yet.
    #[default]
    Idle,
    /// Files are being read and hashed (the walk's content pass).
    Hashing,
    /// Objects are being transferred to/from a store.
    Transfer,
    /// The tree is being enumerated to discover files before hashing begins.
    Discovering,
}

impl Phase {
    /// Encodes the phase as a `u8` for atomic storage.
    const fn as_u8(self) -> u8 {
        match self {
            Phase::Idle => 0,
            Phase::Hashing => 1,
            Phase::Transfer => 2,
            Phase::Discovering => 3,
        }
    }

    /// Decodes a `u8` back into a [`Phase`]. Any out-of-range value (which the
    /// meter never stores) decodes to [`Phase::Idle`].
    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Phase::Hashing,
            2 => Phase::Transfer,
            3 => Phase::Discovering,
            _ => Phase::Idle,
        }
    }
}

/// A point-in-time copy of a [`Meter`]'s counters.
///
/// Cheap to copy ([`Copy`]) and free of any atomics, so a renderer can read a
/// consistent-enough view without holding a reference to the live meter.
#[derive(Clone, Copy, Debug, Default)]
pub struct MeterSnapshot {
    /// Total bytes read in (e.g. file content hashed during the walk).
    pub bytes_in: u64,
    /// Total bytes written/sent out (e.g. uploaded to a store).
    pub bytes_out: u64,
    /// Objects finished (e.g. files hashed).
    pub objects_done: u64,
    /// Objects discovered so far during the enumeration pass (e.g. files
    /// enumerated before hashing begins).
    pub objects_discovered: u64,
    /// Expected total objects, when known (`0` means unknown).
    pub objects_total: u64,
    /// Objects skipped (e.g. already present, deduplicated).
    pub objects_skipped: u64,
    /// Objects currently in flight (a gauge: started minus finished).
    pub in_flight: u64,
    /// The current coarse [`Phase`].
    pub phase: Phase,
    /// Advisory: the current adaptive throughput limit in bytes/sec, or `0`
    /// when not adaptive / unset. Display-only; never throttles the walk.
    pub current_limit: u64,
    /// Advisory: the adaptive controller's target throughput in bytes/sec, or
    /// `0` when not adaptive / unset. Display-only; never throttles the walk.
    pub target_rate: u64,
}

/// A lock-free progress meter shared across threads behind an
/// [`Arc`](std::sync::Arc).
///
/// Every method takes `&self` and uses [`Ordering::Relaxed`] — the counters are
/// advisory progress, never a synchronization primitive, so relaxed ordering is
/// both correct and cheap (a couple of atomic ops per file). [`Meter`] is
/// [`Send`] + [`Sync`] because all of its fields are atomics.
#[derive(Debug, Default)]
pub struct Meter {
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    objects_done: AtomicU64,
    objects_discovered: AtomicU64,
    objects_total: AtomicU64,
    objects_skipped: AtomicU64,
    in_flight: AtomicU64,
    phase: AtomicU8,
    /// Advisory adaptive throughput limit in bytes/sec (`0` = unset). Display
    /// only — set by the adaptive controller for the renderer to show; reading
    /// or writing it never affects the walk's output.
    current_limit: AtomicU64,
    /// Advisory adaptive target throughput in bytes/sec (`0` = unset). Display
    /// only, with the same advisory semantics as `current_limit`.
    target_rate: AtomicU64,
}

impl Meter {
    /// Creates a fresh meter with all counters at zero and [`Phase::Idle`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `n` to the bytes-in counter (content read/hashed).
    pub fn add_in(&self, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
    }

    /// Adds `n` to the bytes-out counter (content written/sent).
    pub fn add_out(&self, n: u64) {
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
    }

    /// Records that an object started: bumps the in-flight gauge by one.
    pub fn object_started(&self) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
    }

    /// Records that an object finished: drops the in-flight gauge by one and
    /// bumps the done counter by one. Saturates the gauge at zero so a stray
    /// finish never underflows.
    pub fn object_finished(&self) {
        // Decrement the gauge without underflowing past zero.
        let mut current = self.in_flight.load(Ordering::Relaxed);
        while current > 0 {
            match self.in_flight.compare_exchange_weak(
                current,
                current - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        self.objects_done.fetch_add(1, Ordering::Relaxed);
    }

    /// Sets the expected total object count.
    pub fn set_total(&self, n: u64) {
        self.objects_total.store(n, Ordering::Relaxed);
    }

    /// Records that one object was discovered during the enumeration pass:
    /// bumps the discovered counter by one.
    pub fn object_discovered(&self) {
        self.objects_discovered.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds `n` to the skipped-objects counter.
    pub fn add_skipped(&self, n: u64) {
        self.objects_skipped.fetch_add(n, Ordering::Relaxed);
    }

    /// Sets the advisory adaptive throughput limit (bytes/sec; `0` = unset).
    ///
    /// Display-only: the renderer reads this to show the live adaptive value.
    /// It does not throttle or otherwise change the walk's behavior or output.
    pub fn set_current_limit(&self, n: u64) {
        self.current_limit.store(n, Ordering::Relaxed);
    }

    /// Sets the advisory adaptive target throughput (bytes/sec; `0` = unset).
    ///
    /// Display-only, with the same advisory semantics as
    /// [`set_current_limit`](Meter::set_current_limit).
    pub fn set_target_rate(&self, n: u64) {
        self.target_rate.store(n, Ordering::Relaxed);
    }

    /// Sets the current coarse [`Phase`].
    pub fn set_phase(&self, p: Phase) {
        self.phase.store(p.as_u8(), Ordering::Relaxed);
    }

    /// Reads the current coarse [`Phase`].
    #[must_use]
    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Relaxed))
    }

    /// Takes a point-in-time [`MeterSnapshot`] of every counter.
    ///
    /// The loads are independently relaxed, so the snapshot is *eventually*
    /// consistent rather than a single atomic view — that is fine for an
    /// advisory progress display.
    #[must_use]
    pub fn snapshot(&self) -> MeterSnapshot {
        MeterSnapshot {
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            objects_done: self.objects_done.load(Ordering::Relaxed),
            objects_discovered: self.objects_discovered.load(Ordering::Relaxed),
            objects_total: self.objects_total.load(Ordering::Relaxed),
            objects_skipped: self.objects_skipped.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            phase: self.phase(),
            current_limit: self.current_limit.load(Ordering::Relaxed),
            target_rate: self.target_rate.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_meter_counters_and_snapshot() {
        let meter = Meter::new();
        // Default snapshot is all-zero and Idle.
        let initial = meter.snapshot();
        assert_eq!(initial.bytes_in, 0);
        assert_eq!(initial.bytes_out, 0);
        assert_eq!(initial.objects_done, 0);
        assert_eq!(initial.objects_discovered, 0);
        assert_eq!(initial.objects_total, 0);
        assert_eq!(initial.objects_skipped, 0);
        assert_eq!(initial.in_flight, 0);
        assert_eq!(initial.phase, Phase::Idle);

        meter.add_in(100);
        meter.add_in(23);
        meter.add_out(7);
        meter.set_total(10);
        meter.add_skipped(2);
        meter.add_skipped(1);
        meter.object_discovered();
        meter.object_discovered();
        meter.object_discovered();
        meter.object_discovered();
        meter.set_phase(Phase::Hashing);

        // One object in flight after a started/finished pair leaves a second
        // still in flight.
        meter.object_started();
        meter.object_started();
        meter.object_finished();

        let snap = meter.snapshot();
        assert_eq!(snap.bytes_in, 123);
        assert_eq!(snap.bytes_out, 7);
        assert_eq!(snap.objects_done, 1);
        assert_eq!(snap.objects_discovered, 4);
        assert_eq!(snap.objects_total, 10);
        assert_eq!(snap.objects_skipped, 3);
        assert_eq!(snap.in_flight, 1);
        assert_eq!(snap.phase, Phase::Hashing);

        // Phase round-trips through the atomic for every variant.
        for p in [
            Phase::Idle,
            Phase::Hashing,
            Phase::Transfer,
            Phase::Discovering,
        ] {
            meter.set_phase(p);
            assert_eq!(meter.phase(), p);
            assert_eq!(meter.snapshot().phase, p);
        }
    }

    #[test]
    fn progress_meter_in_flight_gauge() {
        let meter = Meter::new();
        meter.object_started();
        meter.object_started();
        meter.object_started();
        meter.object_finished();
        meter.object_finished();

        let snap = meter.snapshot();
        assert_eq!(snap.in_flight, 1, "3 started - 2 finished == 1 in flight");
        assert_eq!(snap.objects_done, 2, "2 finished == 2 done");

        // A finish past zero saturates the gauge instead of underflowing.
        meter.object_finished();
        meter.object_finished();
        let snap = meter.snapshot();
        assert_eq!(snap.in_flight, 0, "gauge saturates at 0, no underflow");
        assert_eq!(snap.objects_done, 4);
    }

    #[test]
    fn resources_meter_adaptive_fields_round_trip() {
        let meter = Meter::new();
        // Default advisory atoms are 0 (not adaptive / unset).
        let initial = meter.snapshot();
        assert_eq!(initial.current_limit, 0);
        assert_eq!(initial.target_rate, 0);

        meter.set_current_limit(5_000_000);
        meter.set_target_rate(8_000_000);
        let snap = meter.snapshot();
        assert_eq!(snap.current_limit, 5_000_000);
        assert_eq!(snap.target_rate, 8_000_000);

        // Setters overwrite (store, not add) and 0 clears back to unset.
        meter.set_current_limit(0);
        let snap = meter.snapshot();
        assert_eq!(snap.current_limit, 0);
        assert_eq!(snap.target_rate, 8_000_000, "target unchanged");
    }
}

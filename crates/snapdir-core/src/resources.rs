//! Best-effort system-resource samplers (CPU, RSS, total RAM).
//!
//! Per the library-purity principle this module does **no** terminal I/O and
//! reads **no** `$HOME`/config/environment for *behavior*: it queries live
//! **system** state via a few `libc` syscalls (CPU time, resident set size,
//! physical RAM) so a higher-level controller can make adaptive decisions
//! (e.g. a concurrency/throughput guardrail in the stores lane). It is purely
//! advisory runtime telemetry and is **never** consulted by the walk, the
//! manifest builder, or any snapshot computation — a walk samples identically
//! whether or not anything reads these numbers.
//!
//! Everything here is strictly best-effort: every platform read that fails
//! yields `None` rather than panicking. Each `unsafe` block performs exactly
//! one syscall into a plain-old-data struct and checks the return code before
//! trusting the result; no `unwrap`/`expect` is used on any syscall path.
//!
//! The [`CpuSampler`] mirrors the renderer's sampler in
//! `snapdir-cli`'s `progress` module, and [`resident_set_bytes`] mirrors its
//! `sample_rss`; [`total_ram_bytes`] is new (the controller needs a
//! memory-budget denominator that the CLI renderer never sampled).

// This module converts kernel time/size counters between integer syscall
// outputs and `f64` to derive a CPU percentage. The lossy/sign casts are
// inherent to an *advisory* utilization number (never a correctness path), so
// the pedantic cast lints are allowed module-wide rather than peppered onto
// every arithmetic line, matching the CLI progress engine's convention.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]

use std::time::Instant;

/// Reads cumulative process CPU time (user + system) in seconds via
/// `getrusage(RUSAGE_SELF)`. Returns `None` on failure; never panics.
fn rusage_cpu_secs() -> Option<f64> {
    // SAFETY: rusage is plain POD; we pass a valid &mut to a single syscall and
    // only read the struct after confirming the return code is 0.
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, std::ptr::addr_of_mut!(ru)) != 0 {
            return None;
        }
        let secs = |tv: libc::timeval| tv.tv_sec as f64 + tv.tv_usec as f64 / 1_000_000.0;
        Some(secs(ru.ru_utime) + secs(ru.ru_stime))
    }
}

/// Samples process CPU utilization as a percentage of total machine capacity,
/// normalized by the number of available cores so `100%` means "one core fully
/// busy". Values can exceed 100% (up to ~`100 × cores`) when multiple cores are
/// saturated, and are clamped to that range.
///
/// Stateful: each [`poll`](CpuSampler::poll) measures the CPU consumed since the
/// previous poll over the elapsed wall-clock window. The first poll only
/// establishes a baseline and returns `None`.
pub struct CpuSampler {
    /// `(instant, cumulative_cpu_seconds)` captured at the previous poll.
    prev: Option<(Instant, f64)>,
    /// Available parallelism (logical cores), used to normalize the percentage.
    cores: f64,
}

impl CpuSampler {
    /// Creates a sampler. `cores` comes from
    /// [`std::thread::available_parallelism`], falling back to `1` if the count
    /// is unavailable.
    #[must_use]
    pub fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64);
        Self { prev: None, cores }
    }

    /// Polls CPU usage. The first call establishes a baseline and returns
    /// `None`; subsequent calls return `Some(pct)` over the elapsed window —
    /// `(cpu_delta / wall_delta) / cores * 100`, clamped to `[0, 100 × cores]` —
    /// or `None` if `getrusage` is unavailable or no wall time has elapsed.
    pub fn poll(&mut self) -> Option<f64> {
        let now = Instant::now();
        let cpu = rusage_cpu_secs()?;
        match self.prev {
            None => {
                self.prev = Some((now, cpu));
                None
            }
            Some((prev_t, prev_cpu)) => {
                let wall = now.duration_since(prev_t).as_secs_f64();
                self.prev = Some((now, cpu));
                if wall <= 0.0 {
                    return None;
                }
                let cpu_delta = (cpu - prev_cpu).max(0.0);
                let pct = (cpu_delta / wall) / self.cores * 100.0;
                Some(pct.clamp(0.0, 100.0 * self.cores))
            }
        }
    }
}

impl Default for CpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the current process resident set size (RSS) in bytes, best-effort.
///
/// - Linux: 2nd field (resident pages) of `/proc/self/statm` × page size.
/// - macOS: mach `task_info(MACH_TASK_BASIC_INFO)` `.resident_size`.
/// - Other targets: `None`.
///
/// Returns `None` on any error; never panics.
#[must_use]
pub fn resident_set_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf is a pure query with no pointer args; we check its
        // return for the documented `-1` failure sentinel before using it.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        Some(resident_pages.saturating_mul(page_size as u64))
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: task_info writes into a correctly-sized info struct; we pass
        // the matching flavor + count and only read `resident_size` after the
        // kern_return_t is KERN_SUCCESS. `mach_task_self_` is the static that
        // the deprecated `mach_task_self()` helper merely reads; use it
        // directly to avoid pulling the `mach2` crate for one port handle.
        #[allow(deprecated)]
        unsafe {
            let mut info: libc::mach_task_basic_info = std::mem::zeroed();
            let mut count: libc::mach_msg_type_number_t =
                (std::mem::size_of::<libc::mach_task_basic_info>()
                    / std::mem::size_of::<libc::integer_t>())
                    as libc::mach_msg_type_number_t;
            let kr = libc::task_info(
                libc::mach_task_self_,
                libc::MACH_TASK_BASIC_INFO,
                std::ptr::addr_of_mut!(info).cast(),
                std::ptr::addr_of_mut!(count),
            );
            if kr == libc::KERN_SUCCESS {
                Some(info.resident_size)
            } else {
                None
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Returns the total physical RAM of the machine in bytes, best-effort.
///
/// - Linux: `sysconf(_SC_PHYS_PAGES) × sysconf(_SC_PAGE_SIZE)`.
/// - macOS: `sysctlbyname("hw.memsize", …)`.
/// - Other targets: `None`.
///
/// Returns `None` on any error; never panics. The adaptive controller uses this
/// as the denominator of its memory-budget guardrail.
#[must_use]
pub fn total_ram_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: both are pure sysconf queries with no pointer args; we check
        // each for the documented `-1`/`0` failure sentinels before using them.
        let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) };
        if pages <= 0 || page_size <= 0 {
            return None;
        }
        Some((pages as u64).saturating_mul(page_size as u64))
    }
    #[cfg(target_os = "macos")]
    {
        let mut memsize: u64 = 0;
        let mut len: libc::size_t = std::mem::size_of::<u64>();
        // The sysctl name is a NUL-terminated C string.
        let name = c"hw.memsize";
        // SAFETY: we pass a valid NUL-terminated name, a correctly-sized output
        // buffer (`&mut u64`) with its matching length, and no new-value
        // buffer; the result is only trusted when the call returns 0 and the
        // kernel reported the full 8-byte width.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                std::ptr::addr_of_mut!(memsize).cast(),
                std::ptr::addr_of_mut!(len),
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 && len == std::mem::size_of::<u64>() && memsize > 0 {
            Some(memsize)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_samplers_are_bounded_and_safe() {
        // resident_set_bytes: either Some(plausible) or None; never panics.
        if let Some(rss) = resident_set_bytes() {
            assert!(rss > 0, "rss should be positive when sampled: {rss}");
            assert!(
                rss < 1024u64 * 1024 * 1024 * 1024,
                "rss implausibly large: {rss}"
            );
        }

        // CpuSampler: first poll is a baseline (None); a later poll is either
        // None or Some(non-negative, finite) and never exceeds 100 × cores.
        let cores = std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64);
        let mut sampler = CpuSampler::new();
        assert!(
            sampler.poll().is_none(),
            "first poll must be a baseline (None)"
        );
        // Do a little work so the second window has a chance of being > 0.
        let mut acc = 0u64;
        for i in 0..1_000_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        if let Some(pct) = sampler.poll() {
            assert!(pct >= 0.0, "cpu pct negative: {pct}");
            assert!(pct.is_finite(), "cpu pct not finite: {pct}");
            assert!(
                pct <= 100.0 * cores + 1e-6,
                "cpu pct exceeds capacity: {pct} (cores {cores})"
            );
        }
    }

    #[test]
    fn resources_total_ram_is_positive() {
        let ram = total_ram_bytes();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // This is the dev/CI platform — total RAM must be sampleable and
            // sane (> 0, and not absurdly large).
            let n = ram.expect("total_ram_bytes must be Some on linux/macos");
            assert!(n > 0, "total ram should be positive: {n}");
            assert!(
                n < 1024u64 * 1024 * 1024 * 1024 * 1024,
                "total ram implausibly large: {n}"
            );
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // On a truly unknown OS we only require that it does not panic;
            // None is acceptable, but a Some must still be > 0.
            if let Some(n) = ram {
                assert!(n > 0, "total ram should be positive when sampled: {n}");
            }
        }
    }
}

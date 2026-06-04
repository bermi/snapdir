//! Hand-rolled, dependency-light terminal progress rendering engine.
//!
//! This module is the *engine* for a single-line, live progress indicator drawn
//! to **stderr only** (stdout stays reserved for the snapshot id). It is split
//! into a **pure** half — color/activation policy, humanizers, and the
//! [`format_line`] formatter — that is fully unit-/golden-testable without a TTY
//! or any I/O, and a thin **I/O** half ([`term_width`], [`sample_rss`],
//! [`CpuSampler`], [`ProgressReporter`]) that wraps a few `libc` syscalls.
//!
//! Design constraints (deliberate):
//! - The only new dependency is `libc`. All ANSI color escapes and the
//!   spinner/bar glyphs are hand-rolled; no `indicatif`/`console`/`anstyle`/
//!   `terminal_size`/`unicode-width` crate is pulled in.
//! - Every glyph in both the modern (unicode) and fallback (ASCII) sets is
//!   display-width 1, so width fitting can be computed on a plain `char` count.
//! - Self-metrics (RSS, CPU) are strictly best-effort: any platform read that
//!   fails simply yields `None` and the field is omitted — it never panics and
//!   never errors the surrounding transfer.
//!
//! This gate builds and tests the engine only; wiring it into the commands and
//! adding the `--no-progress`/`--quiet`/`--color` flags is a later gate, so a
//! few items carry `#[allow(dead_code)]` until then.

// This module is intentionally float-heavy: humanizing byte counts, computing
// percentages, EWMA rates, and bar fill all convert between integer counters
// and `f64`. The lossy/truncating/sign casts are inherent to an *advisory*
// progress display (never a correctness path), so the pedantic cast lints are
// allowed module-wide rather than peppered onto every arithmetic line.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use snapdir_core::{Meter, MeterSnapshot, Phase};

// ---------------------------------------------------------------------------
// ANSI escapes (hand-rolled; no color crate).
// ---------------------------------------------------------------------------

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_GREEN: &str = "\x1b[32m";

/// Erase-to-end-of-line + carriage-return helpers for in-place line updates.
const CLEAR_LINE: &str = "\r\x1b[K";

// ---------------------------------------------------------------------------
// 1. Color / activation policy (PURE).
// ---------------------------------------------------------------------------

/// When to emit ANSI color, mirroring the conventional `--color` tri-state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum ColorChoice {
    /// Color when attached to a TTY and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always emit color.
    Always,
    /// Never emit color.
    Never,
}

impl ColorChoice {
    /// Parses `"auto"`/`"always"`/`"never"` (case-insensitive). Unknown values
    /// fall back to [`ColorChoice::Auto`].
    #[allow(dead_code)]
    pub(crate) fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "always" => ColorChoice::Always,
            "never" => ColorChoice::Never,
            _ => ColorChoice::Auto,
        }
    }
}

/// Whether the progress line should be rendered at all.
///
/// Pure on purpose: `is_tty` is a parameter so callers can unit-test every
/// combination; the real caller passes `std::io::stderr().is_terminal()`.
pub(crate) fn should_render(is_tty: bool, no_progress: bool, term: Option<&str>) -> bool {
    is_tty && !no_progress && term != Some("dumb")
}

/// Whether to colorize output, given the [`ColorChoice`], TTY-ness, and whether
/// `NO_COLOR` is set in the environment.
pub(crate) fn use_color(choice: ColorChoice, is_tty: bool, no_color_env: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => is_tty && !no_color_env,
    }
}

/// A tiny styler that wraps text in hand-rolled ANSI escapes when `color` is on,
/// and is a transparent passthrough otherwise.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Style {
    pub(crate) color: bool,
}

impl Style {
    fn wrap(self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{ANSI_RESET}")
        } else {
            text.to_owned()
        }
    }

    pub(crate) fn dim(self, text: &str) -> String {
        self.wrap(ANSI_DIM, text)
    }

    pub(crate) fn bold(self, text: &str) -> String {
        self.wrap(ANSI_BOLD, text)
    }

    pub(crate) fn cyan(self, text: &str) -> String {
        self.wrap(ANSI_CYAN, text)
    }

    pub(crate) fn green(self, text: &str) -> String {
        self.wrap(ANSI_GREEN, text)
    }
}

// ---------------------------------------------------------------------------
// 2. Humanizers (PURE).
// ---------------------------------------------------------------------------

const KIB: f64 = 1024.0;
const BYTE_UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Formats a byte count base-1024 with a compact unit, e.g. `1.5 KB`,
/// `412 MB`, `1.2 GB`. Values below 10 of a unit (and raw bytes) print with no
/// decimals; otherwise one decimal place.
pub(crate) fn human_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0usize;
    while value >= KIB && unit < BYTE_UNITS.len() - 1 {
        value /= KIB;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", BYTE_UNITS[unit])
    } else {
        format!("{value:.0} {}", BYTE_UNITS[unit])
    }
}

/// Formats a transfer rate, e.g. `148 MB/s`, `1.5 KB/s`, `0 B/s`.
pub(crate) fn human_rate(bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return "0 B/s".to_owned();
    }
    // Round to whole bytes and reuse the byte humanizer for consistent units.
    let bytes = bytes_per_sec.round() as u64;
    format!("{}/s", human_bytes(bytes))
}

/// Formats a coarse ETA duration, e.g. `1m05s`, `12s`, `2h03m`.
fn human_eta(d: Duration) -> String {
    let total = d.as_secs();
    if total >= 3600 {
        let h = total / 3600;
        let m = (total % 3600) / 60;
        format!("{h}h{m:02}m")
    } else if total >= 60 {
        let m = total / 60;
        let s = total % 60;
        format!("{m}m{s:02}s")
    } else {
        format!("{total}s")
    }
}

// ---------------------------------------------------------------------------
// 3. The pure line formatter.
// ---------------------------------------------------------------------------

/// Live, per-tick derived metrics handed to [`format_line`]. Kept separate from
/// the raw [`MeterSnapshot`] so the formatter stays pure and golden-testable
/// (the caller computes rates/eta/spinner; the formatter only renders).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RenderMetrics {
    /// Smoothed bytes/sec being read in (download/hashing source).
    pub(crate) rate_in: f64,
    /// Smoothed bytes/sec being written out (upload).
    pub(crate) rate_out: f64,
    /// Smoothed objects/sec.
    pub(crate) obj_per_sec: f64,
    /// Estimated time remaining, when computable.
    pub(crate) eta: Option<Duration>,
    /// Resident set size in bytes, when sampleable.
    pub(crate) rss: Option<u64>,
    /// Process CPU usage percent (0..~100×cores), when sampleable.
    pub(crate) cpu_pct: Option<f64>,
    /// Configured concurrency (worker count).
    pub(crate) jobs: usize,
    /// Monotonic spinner frame counter (mod the frame-set length).
    pub(crate) spinner_frame: usize,
}

// Modern (unicode) glyph set — every glyph is display-width 1.
const SPINNER_MODERN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const BAR_FILL_MODERN: char = '█';
const BAR_EMPTY_MODERN: char = '░';
const BAR_OPEN_MODERN: char = '▕';
const BAR_CLOSE_MODERN: char = '▏';
const ARROW_DOWN_MODERN: char = '↓';
const ARROW_UP_MODERN: char = '↑';

// Fallback (ASCII) glyph set.
const SPINNER_ASCII: [char; 4] = ['|', '/', '-', '\\'];
const BAR_FILL_ASCII: char = '#';
const BAR_EMPTY_ASCII: char = ' ';
const BAR_OPEN_ASCII: char = '[';
const BAR_CLOSE_ASCII: char = ']';

/// Returns the spinner glyph for this frame, given the ascii toggle.
fn spinner_glyph(frame: usize, ascii: bool) -> char {
    if ascii {
        SPINNER_ASCII[frame % SPINNER_ASCII.len()]
    } else {
        SPINNER_MODERN[frame % SPINNER_MODERN.len()]
    }
}

/// Builds a determinate bar of `width` cells filled to `fraction` (0.0..=1.0),
/// wrapped in the open/close caps. Returns the full bar string (char-width =
/// `width + 2`).
fn render_bar(fraction: f64, width: usize, ascii: bool) -> String {
    let (fill, empty, open, close) = if ascii {
        (
            BAR_FILL_ASCII,
            BAR_EMPTY_ASCII,
            BAR_OPEN_ASCII,
            BAR_CLOSE_ASCII,
        )
    } else {
        (
            BAR_FILL_MODERN,
            BAR_EMPTY_MODERN,
            BAR_OPEN_MODERN,
            BAR_CLOSE_MODERN,
        )
    };
    let frac = fraction.clamp(0.0, 1.0);
    let filled = (frac * width as f64).round() as usize;
    let filled = filled.min(width);
    let mut s = String::with_capacity(width + 2);
    s.push(open);
    for _ in 0..filled {
        s.push(fill);
    }
    for _ in 0..(width - filled) {
        s.push(empty);
    }
    s.push(close);
    s
}

/// A single optional, droppable field in the rendered line, in priority order.
///
/// Lower-priority fields are dropped first when the line does not fit the
/// available width: eta → cpu → mem → obj/s.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Optional {
    Eta,
    Cpu,
    Mem,
    ObjPerSec,
}

/// Counts display columns assuming every glyph is width 1 (true for our chosen
/// modern + ASCII sets). Computed on `char` count, ignoring ANSI escapes (which
/// are only added by [`Style`] and contribute zero visible columns).
fn visible_width(s: &str) -> usize {
    s.chars().count()
}

/// Renders ONE progress line, fitted to `width` columns.
///
/// Pure: no I/O, no clock, no env. The caller supplies the snapshot, the
/// derived [`RenderMetrics`], the target `width`, the [`Style`] (color policy),
/// and the `ascii` toggle. Optional fields are dropped in priority order if the
/// full line would exceed `width`; if it still overflows after shrinking the
/// bar, the result is truncated with an ellipsis. Color escapes do not count
/// toward the width budget.
// `style: &Style` is part of the gate-mandated signature (Style is Copy/1-byte,
// but the contract takes it by reference).
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn format_line(
    snap: &MeterSnapshot,
    m: &RenderMetrics,
    width: usize,
    style: &Style,
    ascii: bool,
) -> String {
    format_line_named(snap, m, width, style, ascii, None)
}

/// Like [`format_line`] but lets the caller prefix an optional command name
/// (e.g. `"sync"`) onto the phase label.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn format_line_named(
    snap: &MeterSnapshot,
    m: &RenderMetrics,
    width: usize,
    style: &Style,
    ascii: bool,
    cmd: Option<&str>,
) -> String {
    let fields = LineFields::build(snap, m, ascii, cmd);
    fields.fit(width, *style, ascii)
}

/// The precomputed text spans of one progress line, ready to be assembled into
/// a fitted line. Splitting the (cheap, pure) computation of each span from the
/// width-fitting loop keeps both halves small and testable.
struct LineFields {
    spinner: String,
    label: String,
    counts: String,
    bytes: String,
    rates: String,
    conc: String,
    obj: Option<String>,
    mem: Option<String>,
    cpu: Option<String>,
    eta: Option<String>,
    determinate: bool,
    fraction: f64,
}

/// Drop priority: lowest-priority fields are removed first when fitting.
const DROP_ORDER: [Optional; 4] = [
    Optional::Eta,
    Optional::Cpu,
    Optional::Mem,
    Optional::ObjPerSec,
];
/// Initial (and maximum) bar cell width before shrinking.
const BAR_WIDTH_INIT: usize = 20;

impl LineFields {
    fn build(snap: &MeterSnapshot, m: &RenderMetrics, ascii: bool, cmd: Option<&str>) -> Self {
        let phase_word = match snap.phase {
            Phase::Hashing => "hashing",
            Phase::Transfer => "transfer",
            Phase::Idle => "idle",
        };
        let label = match cmd {
            Some(c) => format!("{c} {phase_word}"),
            None => phase_word.to_owned(),
        };

        let done = snap.objects_done + snap.objects_skipped;
        let total = snap.objects_total;
        let determinate = total > 0;
        let fraction = if determinate {
            done as f64 / total as f64
        } else {
            0.0
        };

        let counts = if determinate {
            let pct = (fraction * 100.0).clamp(0.0, 100.0);
            format!("{pct:>3.0}% {done}/{total}")
        } else {
            format!("{done} objs")
        };

        let bytes = format!(
            "{}/{}",
            human_bytes(snap.bytes_out),
            human_bytes(snap.bytes_in)
        );
        let (down_sym, up_sym) = if ascii {
            ("down".to_owned(), "up".to_owned())
        } else {
            (ARROW_DOWN_MODERN.to_string(), ARROW_UP_MODERN.to_string())
        };
        let rates = format!(
            "{down_sym}{} {up_sym}{}",
            human_rate(m.rate_in),
            human_rate(m.rate_out)
        );

        Self {
            spinner: spinner_glyph(m.spinner_frame, ascii).to_string(),
            label,
            counts,
            bytes,
            rates,
            conc: format!("{}/{}", snap.in_flight, m.jobs),
            obj: Some(format!("{:.0} obj/s", m.obj_per_sec)),
            mem: m.rss.map(|r| format!("mem {}", human_bytes(r))),
            cpu: m.cpu_pct.map(|c| format!("cpu {c:.0}%")),
            eta: m.eta.map(|d| format!("eta {}", human_eta(d))),
            determinate,
            fraction,
        }
    }

    /// Assembles the visible parts, honoring the dropped set and bar width.
    fn parts(&self, dropped: &[Optional], bar_width: usize, ascii: bool) -> Vec<String> {
        let mut parts: Vec<String> = Vec::with_capacity(11);
        parts.push(self.spinner.clone());
        parts.push(self.label.clone());
        parts.push(self.counts.clone());
        if self.determinate {
            parts.push(render_bar(self.fraction, bar_width, ascii));
        }
        parts.push(self.bytes.clone());
        parts.push(self.rates.clone());
        parts.push(self.conc.clone());
        let kept = |o: Optional| !dropped.contains(&o);
        if kept(Optional::ObjPerSec) {
            if let Some(f) = &self.obj {
                parts.push(f.clone());
            }
        }
        if kept(Optional::Mem) {
            if let Some(f) = &self.mem {
                parts.push(f.clone());
            }
        }
        if kept(Optional::Cpu) {
            if let Some(f) = &self.cpu {
                parts.push(f.clone());
            }
        }
        if kept(Optional::Eta) {
            if let Some(f) = &self.eta {
                parts.push(f.clone());
            }
        }
        parts
    }

    /// Fits the line to `width`: drops optionals in priority order, then shrinks
    /// the bar, then truncates with an ellipsis as a last resort.
    fn fit(&self, width: usize, style: Style, ascii: bool) -> String {
        let mut dropped: Vec<Optional> = Vec::new();
        let mut bar_width = BAR_WIDTH_INIT;
        loop {
            let parts = self.parts(&dropped, bar_width, ascii);
            let plain = parts.join(" ");
            if visible_width(&plain) <= width {
                return style_line(&parts, style);
            }
            if dropped.len() < DROP_ORDER.len() {
                dropped.push(DROP_ORDER[dropped.len()]);
            } else if bar_width >= 4 {
                bar_width -= 4;
            } else if bar_width > 0 {
                bar_width = 0;
            } else {
                return truncate_to(&plain, width);
            }
        }
    }
}

/// Re-renders the assembled parts with the active [`Style`] applied to a few
/// semantic spans (label/counts get color; the rest stay plain). Width-neutral:
/// ANSI escapes add no visible columns.
fn style_line(parts: &[String], style: Style) -> String {
    // parts: [spinner, label, counts, (bar?), bytes, rates, conc, optionals...]
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    for (i, p) in parts.iter().enumerate() {
        let styled = match i {
            0 => style.cyan(p),  // spinner
            1 => style.bold(p),  // phase label
            2 => style.green(p), // counts/percent
            _ => style.dim(p),   // bar, bytes, rates, conc, optionals
        };
        out.push(styled);
    }
    out.join(" ")
}

/// Truncates `s` to at most `width` visible columns, appending an ellipsis when
/// truncation occurs. Operates on `char` boundaries.
fn truncate_to(s: &str, width: usize) -> String {
    if visible_width(s) <= width {
        return s.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let keep = width - 1;
    let truncated: String = s.chars().take(keep).collect();
    format!("{truncated}…")
}

// ---------------------------------------------------------------------------
// 4. Terminal width (IO, thin).
// ---------------------------------------------------------------------------

/// Best-effort current terminal width in columns.
///
/// Tries `ioctl(STDERR_FILENO, TIOCGWINSZ, …)` first; on failure (or a zero
/// width) falls back to parsing the `COLUMNS` env var; otherwise `None`.
pub(crate) fn term_width() -> Option<usize> {
    // SAFETY: winsize is plain POD; we pass a valid &mut and check the return.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        let rc = libc::ioctl(
            libc::STDERR_FILENO,
            libc::TIOCGWINSZ as _,
            std::ptr::addr_of_mut!(ws),
        );
        if rc == 0 && ws.ws_col > 0 {
            return Some(ws.ws_col as usize);
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
}

// ---------------------------------------------------------------------------
// 5. Best-effort self metrics (IO, thin, GRACEFUL).
// ---------------------------------------------------------------------------

/// Samples the process resident set size in bytes, best-effort.
///
/// - Linux: 2nd field of `/proc/self/statm` (pages) × page size.
/// - macOS: mach `task_info(MACH_TASK_BASIC_INFO)` `.resident_size`.
///
/// Returns `None` on any error; never panics.
pub(crate) fn sample_rss() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: sysconf is a pure query with no pointer args.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        Some(resident_pages.saturating_mul(page_size as u64))
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: task_info writes into a correctly-sized info struct; we pass
        // the matching flavor + count and check the kern_return_t.
        // `mach_task_self_` is the static the deprecated `mach_task_self()`
        // helper merely reads; use it directly to avoid pulling the `mach2`
        // crate just for one port handle.
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

/// Reads cumulative process CPU time (user + system) in seconds via
/// `getrusage(RUSAGE_SELF)`. Returns `None` on failure.
fn rusage_cpu_secs() -> Option<f64> {
    // SAFETY: rusage is POD; we pass a valid &mut and check the return code.
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, std::ptr::addr_of_mut!(ru)) != 0 {
            return None;
        }
        let secs = |tv: libc::timeval| tv.tv_sec as f64 + tv.tv_usec as f64 / 1_000_000.0;
        Some(secs(ru.ru_utime) + secs(ru.ru_stime))
    }
}

/// Samples process CPU utilization as a percentage, normalized by the number of
/// available cores so `100%` means "one core fully busy" (values can exceed
/// 100% up to ~100×cores when multiple cores are saturated).
pub(crate) struct CpuSampler {
    prev: Option<(Instant, f64)>,
    cores: f64,
}

impl CpuSampler {
    pub(crate) fn new() -> Self {
        let cores = std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64);
        Self { prev: None, cores }
    }

    /// Polls CPU usage. The first call establishes a baseline and returns
    /// `None`; subsequent calls return the percentage over the elapsed window,
    /// or `None` if `getrusage` is unavailable.
    pub(crate) fn poll(&mut self) -> Option<f64> {
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

// ---------------------------------------------------------------------------
// 6. ProgressReporter (IO).
// ---------------------------------------------------------------------------

/// EWMA smoothing factor for instantaneous rates.
const EWMA_ALPHA: f64 = 0.3;

/// Render-thread tick interval.
const TICK: Duration = Duration::from_millis(100);

/// Owns the live render thread that draws the single progress line to stderr.
///
/// When `active` is false (non-TTY, `--no-progress`, etc.) this is entirely
/// inert: no thread is spawned and [`finish`](ProgressReporter::finish) is a
/// no-op. The render thread NEVER writes to stdout.
pub(crate) struct ProgressReporter {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    active: bool,
}

/// Mutable state carried by the render thread across ticks.
struct RenderState {
    last: Instant,
    last_snap: MeterSnapshot,
    rate_in: f64,
    rate_out: f64,
    obj_per_sec: f64,
    spinner_frame: usize,
    cpu: CpuSampler,
}

impl RenderState {
    /// Initializes from the meter and primes the CPU baseline.
    fn init(meter: &Meter) -> Self {
        let mut state = Self {
            last: Instant::now(),
            last_snap: meter.snapshot(),
            rate_in: 0.0,
            rate_out: 0.0,
            obj_per_sec: 0.0,
            spinner_frame: 0,
            cpu: CpuSampler::new(),
        };
        let _ = state.cpu.poll();
        state
    }

    /// Advances one tick: folds the new snapshot's deltas into the EWMA rates,
    /// bumps the spinner, and returns the derived [`RenderMetrics`].
    fn tick(&mut self, snap: MeterSnapshot, jobs: usize) -> RenderMetrics {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        if dt > 0.0 {
            let d_in = snap.bytes_in.saturating_sub(self.last_snap.bytes_in) as f64;
            let d_out = snap.bytes_out.saturating_sub(self.last_snap.bytes_out) as f64;
            let prev_obj = self.last_snap.objects_done + self.last_snap.objects_skipped;
            let now_obj = snap.objects_done + snap.objects_skipped;
            let d_obj = now_obj.saturating_sub(prev_obj) as f64;
            self.rate_in = ewma(self.rate_in, d_in / dt);
            self.rate_out = ewma(self.rate_out, d_out / dt);
            self.obj_per_sec = ewma(self.obj_per_sec, d_obj / dt);
        }
        self.last = now;
        self.last_snap = snap;
        self.spinner_frame = self.spinner_frame.wrapping_add(1);

        RenderMetrics {
            rate_in: self.rate_in,
            rate_out: self.rate_out,
            obj_per_sec: self.obj_per_sec,
            eta: compute_eta(&snap, self.obj_per_sec),
            rss: sample_rss(),
            cpu_pct: self.cpu.poll(),
            jobs,
            spinner_frame: self.spinner_frame,
        }
    }
}

impl ProgressReporter {
    /// Starts the reporter. If `active`, spawns a render thread that ticks every
    /// ~100ms; otherwise returns an inert reporter that spawns nothing.
    pub(crate) fn start(
        meter: Arc<Meter>,
        jobs: usize,
        active: bool,
        color: bool,
        ascii: bool,
    ) -> ProgressReporter {
        let stop = Arc::new(AtomicBool::new(false));
        if !active {
            return ProgressReporter {
                stop,
                handle: None,
                active: false,
            };
        }

        let style = Style { color };
        let stop_thread = Arc::clone(&stop);
        // stderr is line-shared; a Mutex guards interleaving with banner writes.
        let stderr_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));

        let handle = std::thread::spawn(move || {
            let mut state = RenderState::init(&meter);
            while !stop_thread.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                if stop_thread.load(Ordering::Relaxed) {
                    break;
                }
                let snap = meter.snapshot();
                let metrics = state.tick(snap, jobs);
                let width = term_width().unwrap_or(80);
                let line = format_line(&snap, &metrics, width, &style, ascii);

                let _guard = stderr_lock.lock();
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "{CLEAR_LINE}{line}");
                let _ = err.flush();
            }
        });

        ProgressReporter {
            stop,
            handle: Some(handle),
            active: true,
        }
    }

    /// Stops the render thread (if any), joins it, and clears the progress line
    /// so subsequent stdout/stderr output starts on a clean line. No-op when the
    /// reporter is inactive.
    pub(crate) fn finish(mut self) {
        if !self.active {
            return;
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "{CLEAR_LINE}");
        let _ = err.flush();
    }
}

/// One EWMA update step with [`EWMA_ALPHA`].
fn ewma(prev: f64, sample: f64) -> f64 {
    EWMA_ALPHA * sample + (1.0 - EWMA_ALPHA) * prev
}

/// Estimates remaining time from the objects gauge and the smoothed obj/s rate.
/// `None` when the total is unknown or the rate is non-positive.
fn compute_eta(snap: &MeterSnapshot, obj_per_sec: f64) -> Option<Duration> {
    if snap.objects_total == 0 || obj_per_sec <= 0.0 {
        return None;
    }
    let done = snap.objects_done + snap.objects_skipped;
    let remaining = snap.objects_total.saturating_sub(done);
    if remaining == 0 {
        return Some(Duration::ZERO);
    }
    let secs = remaining as f64 / obj_per_sec;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(secs))
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(
        bytes_in: u64,
        bytes_out: u64,
        done: u64,
        total: u64,
        skipped: u64,
        in_flight: u64,
        phase: Phase,
    ) -> MeterSnapshot {
        MeterSnapshot {
            bytes_in,
            bytes_out,
            objects_done: done,
            objects_total: total,
            objects_skipped: skipped,
            in_flight,
            phase,
        }
    }

    #[test]
    fn progress_render_should_render_logic() {
        // should_render: true only when is_tty && !no_progress && term != dumb.
        assert!(should_render(true, false, None));
        assert!(should_render(true, false, Some("xterm")));
        assert!(!should_render(false, false, Some("xterm"))); // not a tty
        assert!(!should_render(true, true, Some("xterm"))); // no_progress
        assert!(!should_render(true, false, Some("dumb"))); // dumb terminal

        // use_color: Always => true, Never => false, Auto => tty && !NO_COLOR.
        assert!(use_color(ColorChoice::Always, false, true));
        assert!(use_color(ColorChoice::Always, false, false));
        assert!(!use_color(ColorChoice::Never, true, false));
        assert!(!use_color(ColorChoice::Never, true, true));
        assert!(use_color(ColorChoice::Auto, true, false)); // tty, no NO_COLOR
        assert!(!use_color(ColorChoice::Auto, true, true)); // NO_COLOR set
        assert!(!use_color(ColorChoice::Auto, false, false)); // not a tty

        // ColorChoice::parse.
        assert_eq!(ColorChoice::parse("always"), ColorChoice::Always);
        assert_eq!(ColorChoice::parse("NEVER"), ColorChoice::Never);
        assert_eq!(ColorChoice::parse("auto"), ColorChoice::Auto);
        assert_eq!(ColorChoice::parse("garbage"), ColorChoice::Auto);
    }

    #[test]
    fn progress_render_humanizers() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(10 * 1024), "10 KB");
        assert_eq!(human_bytes(412 * 1024 * 1024), "412 MB");
        // 1.2 GB ≈ 1.2 * 1024^3.
        assert_eq!(
            human_bytes((1.2 * 1024.0 * 1024.0 * 1024.0) as u64),
            "1.2 GB"
        );

        assert_eq!(human_rate(0.0), "0 B/s");
        assert_eq!(human_rate(-5.0), "0 B/s");
        assert_eq!(human_rate(f64::NAN), "0 B/s");
        assert_eq!(human_rate(148.0 * 1024.0 * 1024.0), "148 MB/s");
        assert_eq!(human_rate(1536.0), "1.5 KB/s");

        assert_eq!(human_eta(Duration::from_secs(12)), "12s");
        assert_eq!(human_eta(Duration::from_secs(65)), "1m05s");
        assert_eq!(human_eta(Duration::from_secs(3783)), "1h03m"); // 1h 3m 3s
    }

    #[test]
    fn progress_render_format_line_modern() {
        let s = snap(
            200 * 1024 * 1024, // bytes_in
            100 * 1024 * 1024, // bytes_out
            30,                // done
            100,               // total
            10,                // skipped
            4,                 // in_flight
            Phase::Transfer,
        );
        let m = RenderMetrics {
            rate_in: 148.0 * 1024.0 * 1024.0,
            rate_out: 50.0 * 1024.0 * 1024.0,
            obj_per_sec: 12.0,
            eta: Some(Duration::from_secs(42)),
            rss: Some(64 * 1024 * 1024),
            cpu_pct: Some(85.0),
            jobs: 16,
            spinner_frame: 0,
        };
        let style = Style { color: false };
        let line = format_line(&s, &m, 200, &style, false);

        // (30 done + 10 skipped) / 100 = 40%.
        assert!(line.contains("40%"), "percent missing: {line}");
        assert!(line.contains("40/100"), "counts missing: {line}");
        assert!(line.contains('↓'), "down arrow missing: {line}");
        assert!(line.contains('↑'), "up arrow missing: {line}");
        assert!(line.contains("148 MB/s"), "rate_in missing: {line}");
        assert!(line.contains("4/16"), "concurrency missing: {line}");
        assert!(line.contains("mem 64 MB"), "mem missing: {line}");
        assert!(line.contains("cpu 85%"), "cpu missing: {line}");
        assert!(line.contains("eta 42s"), "eta missing: {line}");
        assert!(line.contains("transfer"), "phase missing: {line}");
        assert!(
            line.contains('█') || line.contains('░'),
            "bar missing: {line}"
        );
        assert!(line.contains("12 obj/s"), "obj/s missing: {line}");

        // None metrics are omitted.
        let m2 = RenderMetrics {
            eta: None,
            rss: None,
            cpu_pct: None,
            ..m
        };
        let line2 = format_line(&s, &m2, 200, &style, false);
        assert!(!line2.contains("mem "), "mem should be omitted: {line2}");
        assert!(!line2.contains("cpu "), "cpu should be omitted: {line2}");
        assert!(!line2.contains("eta "), "eta should be omitted: {line2}");
    }

    #[test]
    fn progress_render_format_line_fallback() {
        let s = snap(
            8 * 1024 * 1024,
            4 * 1024 * 1024,
            8,
            16,
            0,
            8,
            Phase::Hashing,
        );
        let m = RenderMetrics {
            rate_in: 2.0 * 1024.0 * 1024.0,
            rate_out: 0.0,
            obj_per_sec: 3.0,
            eta: None,
            rss: None,
            cpu_pct: None,
            jobs: 16,
            spinner_frame: 1,
        };
        let style = Style { color: false };
        let line = format_line(&s, &m, 200, &style, true); // ascii = true

        assert!(line.contains("down"), "ascii down missing: {line}");
        assert!(line.contains("up"), "ascii up missing: {line}");
        assert!(line.contains("8/16"), "concurrency missing: {line}");
        assert!(line.contains("50%"), "percent missing: {line}");
        assert!(
            line.contains('[') && line.contains(']'),
            "ascii bar caps missing: {line}"
        );
        assert!(line.contains('#'), "ascii bar fill missing: {line}");
        assert!(line.contains("hashing"), "phase missing: {line}");
        // No unicode arrows in ascii mode.
        assert!(!line.contains('↓'), "unexpected unicode arrow: {line}");
        assert!(!line.contains('█'), "unexpected unicode bar: {line}");
    }

    #[test]
    fn progress_render_format_line_indeterminate() {
        // total == 0 → no bar/percent, just a count.
        let s = snap(1024, 0, 5, 0, 0, 2, Phase::Hashing);
        let m = RenderMetrics {
            jobs: 4,
            ..Default::default()
        };
        let style = Style { color: false };
        let line = format_line(&s, &m, 200, &style, false);
        assert!(
            line.contains("5 objs"),
            "indeterminate count missing: {line}"
        );
        assert!(!line.contains('%'), "no percent when indeterminate: {line}");
        assert!(!line.contains('█'), "no bar when indeterminate: {line}");
        assert!(!line.contains('░'), "no bar when indeterminate: {line}");
    }

    #[test]
    fn progress_render_fits_width() {
        let s = snap(
            200 * 1024 * 1024,
            100 * 1024 * 1024,
            30,
            100,
            10,
            4,
            Phase::Transfer,
        );
        let m = RenderMetrics {
            rate_in: 148.0 * 1024.0 * 1024.0,
            rate_out: 50.0 * 1024.0 * 1024.0,
            obj_per_sec: 12.0,
            eta: Some(Duration::from_secs(42)),
            rss: Some(64 * 1024 * 1024),
            cpu_pct: Some(85.0),
            jobs: 16,
            spinner_frame: 0,
        };
        let style = Style { color: false };

        for &width in &[10usize, 20, 30, 40, 60, 80] {
            let line = format_line(&s, &m, width, &style, false);
            let cols = line.chars().count();
            assert!(
                cols <= width,
                "width {width}: line is {cols} cols: {line:?}"
            );
        }

        // Narrow widths drop the low-priority optionals first (eta, then cpu).
        let narrow = format_line(&s, &m, 40, &style, false);
        assert!(!narrow.contains("eta "), "eta should drop first: {narrow}");
    }

    #[test]
    fn progress_render_metrics_best_effort() {
        // sample_rss / CpuSampler::poll must return Some(plausible) or None and
        // never panic (None is acceptable on the CI platform).
        if let Some(rss) = sample_rss() {
            assert!(rss > 0, "rss should be positive when sampled: {rss}");
            assert!(
                rss < 1024u64 * 1024 * 1024 * 1024,
                "rss implausibly large: {rss}"
            );
        }

        let mut sampler = CpuSampler::new();
        // First poll establishes the baseline.
        let _ = sampler.poll();
        // Do a little work, then poll again.
        let mut acc = 0u64;
        for i in 0..1_000_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        if let Some(pct) = sampler.poll() {
            assert!(pct >= 0.0, "cpu pct negative: {pct}");
            assert!(pct.is_finite(), "cpu pct not finite: {pct}");
        }
    }

    #[test]
    fn progress_render_term_width_no_panic() {
        // term_width must not panic regardless of environment.
        let _ = term_width();
    }

    #[test]
    fn progress_render_reporter_inactive_is_inert() {
        // An inactive reporter spawns no thread and finish() is a no-op.
        let meter = Arc::new(Meter::new());
        let reporter = ProgressReporter::start(meter, 4, false, false, false);
        reporter.finish();
    }
}

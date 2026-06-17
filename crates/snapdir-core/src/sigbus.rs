//! Unix-only `SIGBUS` guard for memory-mapped file hashing.
//!
//! # Why this exists
//!
//! [`hash_file`](crate::hash_file) memory-maps files at or above
//! [`MMAP_THRESHOLD`](crate::hash_file::MMAP_THRESHOLD) and hashes them through
//! the mapping (no large heap copy). A snapshot assumes a **static tree**, but
//! if another process **truncates or shrinks** a file *while it is being
//! hashed*, touching the now-invalid pages raises `SIGBUS`. With no handler the
//! kernel kills the process with no snapdir message — the "fails without
//! printing anything" symptom. This module installs a `SIGBUS` handler so a
//! mid-hash truncation yields a clean [`io::Error`] instead.
//!
//! # Ownership test — choice (a): a thread-local "in guarded mmap hash" flag
//!
//! Per the design lock (`flux-robustness-1.9.0.md`, section A) two ownership
//! tests were offered. We pick **(a)**: a thread-local flag set only while this
//! thread is inside [`guard_mmap_hash`]. It is the *simplest-correct* option and
//! — crucially — it works with `blake3::Hasher::update_mmap`, which maps the
//! file *internally* (we never see its `(base, len)`), so the more precise
//! `si_addr`-range test of option (b) is not even available to us without
//! reimplementing the mmap+hash loop by hand. A SIGBUS that arrives while a
//! thread's flag is set is, by construction, a fault on the file that thread is
//! currently mmap-hashing; any other SIGBUS (flag clear) is forwarded to the
//! previously-installed handler so we never swallow an unrelated fault.
//!
//! Because hashing runs **single-threaded per file** (see
//! [`hash_file`](crate::hash_file): the large-file path uses `update_mmap`, not
//! `update_mmap_rayon`), the faulting thread is always the thread that armed the
//! [`sigsetjmp`] buffer, so the [`siglongjmp`] lands in a valid frame on the
//! same thread.
//!
//! # Async-signal-safety
//!
//! The handler body does **only** async-signal-safe work: it reads two
//! thread-local cells (a `bool` flag and a pointer to this thread's jump
//! buffer) and either calls [`siglongjmp`] (async-signal-safe) or forwards to
//! the saved previous handler. It performs **no** heap allocation, locking,
//! formatting, or other unsafe-in-signal calls. The handler/altstack install
//! runs once via [`Once`] off the signal path.
//!
//! # Chaining the previous handler
//!
//! On install we save the prior `sigaction`. When a SIGBUS arrives and the
//! current thread is **not** in a guarded region, we forward to that saved
//! handler (honouring `SA_SIGINFO`), or re-raise with the default disposition
//! if there was none — so an unrelated SIGBUS keeps its original behaviour.

use std::cell::Cell;
use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::Once;

// `sigsetjmp` / `siglongjmp` are not exported by the `libc` crate (they have
// platform-specific buffer layouts), so we bind the real libc symbols
// ourselves. `siglongjmp` is a genuine exported function everywhere. For
// `sigsetjmp` the platforms differ: on **glibc** it is NOT an exported
// function — it is a C macro that calls the real symbol `__sigsetjmp(jmp_buf,
// savesigs)`, so binding plain `sigsetjmp` fails to link (`undefined symbol:
// sigsetjmp`). On **musl** and **macOS/BSD (libSystem)** `sigsetjmp` IS a real
// exported function. We therefore bind `__sigsetjmp` on glibc and `sigsetjmp`
// elsewhere; the signature `(jmp_buf, int) -> int` is identical, so the call
// site is unchanged. `JmpBuf` is an opaque, generously-oversized,
// pointer-aligned byte buffer: macOS `sigjmp_buf` is at most ~38 ints and
// glibc's is ~200 bytes, both comfortably under our reserve.
const JMP_BUF_BYTES: usize = 512;

#[repr(C, align(16))]
struct JmpBuf([u8; JMP_BUF_BYTES]);

extern "C" {
    fn siglongjmp(env: *mut JmpBuf, val: libc::c_int) -> !;
}

// glibc: `sigsetjmp` is a macro over the real exported symbol `__sigsetjmp`;
// bind that under the unchanged `sigsetjmp` call-site name. `savesigs != 0` =>
// also save/restore the signal mask (the `sig` variant).
#[cfg(target_env = "gnu")]
extern "C" {
    #[link_name = "__sigsetjmp"]
    fn sigsetjmp(env: *mut JmpBuf, savesigs: libc::c_int) -> libc::c_int;
}

// musl + macOS/BSD: `sigsetjmp` is a real exported function.
#[cfg(not(target_env = "gnu"))]
extern "C" {
    fn sigsetjmp(env: *mut JmpBuf, savesigs: libc::c_int) -> libc::c_int;
}

/// Marker payload carried by the [`io::Error`] returned when a guarded `SIGBUS`
/// is caught (a file truncated/shrunk mid-mmap-hash). It lets the
/// [`walk`](crate::walk) layer *recognize* the mmap-fault error by downcasting
/// the inner error (via [`io::Error::get_ref`]) rather than string-matching the
/// message — see [`is_mmap_fault`]. Kept private to this module: callers use the
/// [`is_mmap_fault`] predicate, not the type directly.
#[derive(Debug)]
struct MmapFault;

impl fmt::Display for MmapFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("file changed during hashing (mmap fault)")
    }
}

impl StdError for MmapFault {}

/// Returns `true` if `err` is the synthetic [`io::Error`] produced by
/// [`guard_mmap_hash`] when it caught a guarded `SIGBUS` (a concurrent
/// truncation/shrink faulting the mmap). The walk layer maps such an error to a
/// typed `FileChangedDuringWalk`, while a genuine permission/IO `io::Error`
/// (for which this returns `false`) maps to `WalkError::Io`.
#[must_use]
pub fn is_mmap_fault(err: &io::Error) -> bool {
    err.get_ref()
        .is_some_and(<dyn StdError + Send + Sync>::is::<MmapFault>)
}

thread_local! {
    /// Set only while this thread is executing inside [`guard_mmap_hash`].
    static IN_GUARD: Cell<bool> = const { Cell::new(false) };
    /// Pointer to the [`JmpBuf`] armed by the active [`guard_mmap_hash`] frame
    /// on this thread (null when no frame is armed).
    static JMP_TARGET: Cell<*mut JmpBuf> = const { Cell::new(ptr::null_mut()) };
}

/// The previous `SIGBUS` disposition, captured at install time so we can chain
/// to it for faults that are not ours. Written once under [`INSTALL`]; only
/// read from the (single-threaded-per-fault) handler afterwards.
static mut PREV_ACTION: MaybeUninit<libc::sigaction> = MaybeUninit::uninit();
static INSTALL: Once = Once::new();

/// The `SIGBUS` handler. Async-signal-safe: reads two thread-locals and either
/// `siglongjmp`s out of the guarded region or forwards to the saved handler.
extern "C" fn handle_sigbus(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // Is THIS thread inside a guarded mmap hash? If so the fault is on the file
    // we are hashing: jump back to the guard frame with a non-zero value.
    let armed = IN_GUARD.with(Cell::get);
    if armed {
        let target = JMP_TARGET.with(Cell::get);
        if !target.is_null() {
            // SAFETY: `target` points at a `JmpBuf` armed by `sigsetjmp` in this
            // thread's live `guard_mmap_hash` frame; `siglongjmp` is
            // async-signal-safe.
            unsafe { siglongjmp(target, 1) };
        }
    }

    // Not ours: chain to whatever was installed before us.
    // SAFETY: `PREV_ACTION` is initialised before any handler can run (the
    // install `Once` completes the sigaction call only after writing it). We
    // take a raw pointer (not a reference to the mutable static) and read it.
    let prev: &libc::sigaction = unsafe { &*(&raw const PREV_ACTION).cast::<libc::sigaction>() };
    let prev_handler = prev.sa_sigaction;
    if prev.sa_flags & libc::SA_SIGINFO != 0 {
        if prev_handler != libc::SIG_DFL && prev_handler != libc::SIG_IGN {
            // SAFETY: prev was a SA_SIGINFO handler; call with the 3-arg shape.
            let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                unsafe { std::mem::transmute(prev_handler) };
            f(sig, info, ctx);
            return;
        }
    } else if prev_handler != libc::SIG_DFL && prev_handler != libc::SIG_IGN {
        // SAFETY: prev was a classic 1-arg handler.
        let f: extern "C" fn(libc::c_int) = unsafe { std::mem::transmute(prev_handler) };
        f(sig);
        return;
    }

    // No prior handler (default/ignore): restore the default disposition and
    // re-raise so the process dies as it would have without us.
    // SAFETY: async-signal-safe libc calls only.
    unsafe {
        let mut dfl: libc::sigaction = std::mem::zeroed();
        dfl.sa_sigaction = libc::SIG_DFL;
        libc::sigaction(libc::SIGBUS, &raw const dfl, ptr::null_mut());
        libc::raise(libc::SIGBUS);
    }
}

/// Installs (once) the alternate signal stack and the chaining `SIGBUS`
/// handler. Idempotent and cheap on repeat calls.
fn ensure_installed() {
    INSTALL.call_once(|| {
        // SAFETY: standard one-time sigaltstack + sigaction install. The
        // altstack memory is intentionally leaked for the process lifetime so
        // the kernel always has a valid stack to deliver SIGBUS on (the mmap
        // fault can occur with an arbitrarily-grown user stack).
        unsafe {
            let stack_size = libc::SIGSTKSZ.max(libc::MINSIGSTKSZ * 4);
            let mem = vec![0u8; stack_size].into_boxed_slice();
            let mem = Box::leak(mem);
            let ss = libc::stack_t {
                ss_sp: mem.as_mut_ptr().cast(),
                ss_flags: 0,
                ss_size: stack_size,
            };
            libc::sigaltstack(&raw const ss, ptr::null_mut());

            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle_sigbus as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(
                libc::SIGBUS,
                &raw const action,
                (&raw mut PREV_ACTION).cast::<libc::sigaction>(),
            );
        }
    });
}

/// Runs `f` (a single-threaded mmap-based file hash) with a `SIGBUS` guard
/// armed on the current thread.
///
/// If a `SIGBUS` is raised on this thread while `f` runs — the signature of a
/// file truncated/shrunk mid-hash — the handler `siglongjmp`s back here and we
/// return a clean [`io::Error`] instead of letting the kernel kill the process.
/// A `SIGBUS` from anywhere else (no guard armed) is chained to the
/// previously-installed handler.
///
/// `f` must keep its mmap-touching work on **this** thread (no rayon fan-out),
/// so the faulting thread is the one that armed the jump buffer.
///
/// # Errors
///
/// Returns `f`'s own [`io::Result`], or a synthetic [`io::Error`] carrying the
/// private `MmapFault` marker ("file changed during hashing (mmap fault)") if a
/// guarded `SIGBUS` was caught. Recognize that error with [`is_mmap_fault`].
pub fn guard_mmap_hash<T, F: FnOnce() -> io::Result<T>>(f: F) -> io::Result<T> {
    ensure_installed();

    let mut env = JmpBuf([0u8; JMP_BUF_BYTES]);
    let env_ptr: *mut JmpBuf = &raw mut env;

    // Arm the jump buffer BEFORE setting the flag. `sigsetjmp` returns 0 on the
    // initial call and the `siglongjmp` value (1) when we are jumped back.
    // SAFETY: `env` lives for the whole function; `sigsetjmp` is the standard
    // setjmp contract.
    let jumped = unsafe { sigsetjmp(env_ptr, 1) };
    if jumped != 0 {
        // We were longjmp'd back from the handler: a guarded SIGBUS fired.
        // Disarm and report a clean error. (The thread-locals are restored
        // below via the same disarm path.)
        JMP_TARGET.with(|t| t.set(ptr::null_mut()));
        IN_GUARD.with(|fl| fl.set(false));
        // Carry the `MmapFault` marker so the walk layer can recognize this as a
        // mid-hash truncation (`is_mmap_fault`) and map it to a typed
        // `FileChangedDuringWalk`, distinct from a genuine IO fault.
        return Err(io::Error::other(MmapFault));
    }

    // Save any outer frame's state so nested guards (none today, but cheap and
    // correct) restore cleanly, then arm THIS frame.
    let prev_target = JMP_TARGET.with(|t| t.replace(env_ptr));
    let prev_in_guard = IN_GUARD.with(|fl| fl.replace(true));

    let result = f();

    // Disarm: restore the outer frame's state.
    IN_GUARD.with(|fl| fl.set(prev_in_guard));
    JMP_TARGET.with(|t| t.set(prev_target));

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_ok_result() {
        let got = guard_mmap_hash(|| Ok::<_, io::Error>(7u32)).unwrap();
        assert_eq!(got, 7);
    }

    #[test]
    fn passes_through_err_result() {
        let err = guard_mmap_hash(|| {
            Err::<u32, _>(io::Error::new(io::ErrorKind::PermissionDenied, "nope"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn disarms_after_run() {
        // After a normal run the guard flag must be clear so a later, unrelated
        // SIGBUS would chain rather than be swallowed.
        guard_mmap_hash(|| Ok::<_, io::Error>(())).unwrap();
        assert!(!IN_GUARD.with(Cell::get));
        assert!(JMP_TARGET.with(Cell::get).is_null());
    }

    #[test]
    fn install_is_idempotent() {
        ensure_installed();
        ensure_installed();
        // A second guarded call still works after repeated installs.
        assert_eq!(guard_mmap_hash(|| Ok::<_, io::Error>(1u8)).unwrap(), 1);
    }
}

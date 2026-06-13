//! Out-of-band per-file stat snapshot used by the clone fast-path.
//!
//! A [`CopyGuard`] captures the `(size, mtime, ctime, ino)` of a **plain
//! regular file** at walk time so that a store performing a copy-on-write
//! clone (e.g. APFS `clonefile`) can re-`stat` the same source path at clone
//! time and *skip the redundant post-copy re-hash* iff the file is provably
//! unchanged. It is a behaviour-neutral side channel: it is **never serialized
//! into the manifest** (the frozen manifest text format is unaffected) and is
//! returned only from the additive [`walk_with_guards`](crate::walk::walk_with_guards)
//! entry point — the existing [`walk`](crate::walk::walk) /
//! [`walk_with_meter`](crate::walk::walk_with_meter) signatures are unchanged.
//!
//! ## Trust model (why these four fields)
//!
//! The guard is the *write-time* half of "stat-validated trust": the store
//! compares the recorded guard against a fresh `stat` of the source just
//! before the clone and only trusts the clone (skipping the re-hash) when all
//! four fields still match. `size` catches truncation/append, `mtime_ns`
//! catches a content rewrite, `ctime_ns` catches an in-place metadata/content
//! change that preserved `mtime` (e.g. `touch -t` of the data is still bounded
//! because a write bumps `ctime`), and `ino` catches an atomic rename-replace
//! of the path with a different file. A benign mid-stage race is caught here;
//! an adversarial forge that defeats all four is still caught at **read time**
//! by the object's BLAKE3 re-verification in `get_object`/`fetch` — so the
//! worst case is a slower path, never a silently mis-addressed object.
//!
//! ## Platform
//!
//! The fields are unix `stat` quantities ([`std::os::unix::fs::MetadataExt`]).
//! On non-unix targets a guard is simply never produced (the map omits the
//! file), so the downstream clone-skip optimization never engages there — no
//! regression and no platform break; the crate still compiles everywhere.

/// A captured `(size, mtime, ctime, ino)` snapshot of a single regular file,
/// used out-of-band by the store to validate a clone before skipping the
/// post-copy re-hash. See the [module docs](self) for the trust model.
///
/// `mtime_ns` / `ctime_ns` are full-nanosecond unix timestamps composed as
/// `secs * 1_000_000_000 + nsecs` from the file's own `stat`. The whole struct
/// is `Copy` and cheap to store in a `HashMap<PathBuf, CopyGuard>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyGuard {
    /// File content size in bytes (`st_size`).
    pub size: u64,
    /// Modification time as whole nanoseconds since the unix epoch
    /// (`st_mtime * 1e9 + st_mtime_nsec`).
    pub mtime_ns: i64,
    /// Inode change time as whole nanoseconds since the unix epoch
    /// (`st_ctime * 1e9 + st_ctime_nsec`).
    pub ctime_ns: i64,
    /// Inode number (`st_ino`).
    pub ino: u64,
}

impl CopyGuard {
    /// Builds a [`CopyGuard`] from a file's own metadata.
    ///
    /// On unix this reads `size`/`mtime`/`ctime`/`ino` from the **same**
    /// [`Metadata`](std::fs::Metadata) object the walk already obtained (no
    /// extra syscall) via [`std::os::unix::fs::MetadataExt`]. On non-unix it
    /// returns `None`, so the guard map omits the file and the clone-skip
    /// optimization never engages.
    ///
    /// The caller is responsible for only ever building a guard for a **plain
    /// regular file whose hashed content path equals its own path** (i.e. not
    /// a followed symlink); see [`walk_with_guards`](crate::walk::walk_with_guards).
    #[cfg(unix)]
    #[must_use]
    pub fn from_metadata(meta: &std::fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(CopyGuard {
            size: meta.size(),
            mtime_ns: compose_ns(meta.mtime(), meta.mtime_nsec()),
            ctime_ns: compose_ns(meta.ctime(), meta.ctime_nsec()),
            ino: meta.ino(),
        })
    }

    /// Non-unix stub: a guard is never produced, so the downstream clone-skip
    /// optimization never engages and the crate still compiles on every target.
    #[cfg(not(unix))]
    #[must_use]
    pub fn from_metadata(_meta: &std::fs::Metadata) -> Option<Self> {
        None
    }
}

/// Composes a unix `(secs, nsecs)` timestamp pair into full nanoseconds as a
/// single `i64` (`secs * 1_000_000_000 + nsecs`), using saturating arithmetic
/// so a pathological far-future timestamp cannot panic the walk.
#[cfg(unix)]
fn compose_ns(secs: i64, nsecs: i64) -> i64 {
    secs.saturating_mul(1_000_000_000).saturating_add(nsecs)
}

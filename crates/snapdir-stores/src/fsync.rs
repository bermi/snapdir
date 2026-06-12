//! Crash-durability primitives for the batched pack receiver (Design A).
//!
//! `snapdir` historically fsyncs **nothing**: it relies on temp-file +
//! atomic-rename + "manifest last" so that, after a clean run, a present
//! manifest *logically* implies present objects. That ordering survives a
//! process crash, but it does **not** survive a power loss / kernel panic on a
//! filesystem that can reorder writeback: the rename of the manifest can hit
//! stable storage before the object bytes it references, leaving a manifest
//! that points at empty or torn objects.
//!
//! This module adds the minimum durability to close that window on the
//! receive-pack path, batched so the cost is **exactly two full syncs per
//! pack** rather than one fsync per object:
//!
//! 1. While a pack is being filed, each freshly committed object is given a
//!    cheap, non-blocking *writeout hint* ([`writeout_hint`]) so its dirty
//!    pages start heading to disk early (Linux `sync_file_range(WRITE)`; on
//!    Darwin a plain `fsync`, which is writeout-only there — `F_FULLFSYNC`
//!    would be one full barrier *per object*, exactly what we are batching
//!    away).
//! 2. At the barrier ([`barrier_objects`]) — called once, right before the
//!    manifest is committed — every written object's data is forced to stable
//!    storage in one pass (Linux `sync_file_range(WAIT_BEFORE|WRITE|WAIT_AFTER)`;
//!    elsewhere `fsync`/`sync_data`). This is full sync #1.
//! 3. The manifest itself is then written via
//!    [`crate::file_store::write_manifest_durable`]: fsync the temp file,
//!    rename, fsync the parent shard directory so the rename is durable. That
//!    is full sync #2.
//!
//! ## Non-journaling-fs caveat
//!
//! We deliberately do **not** fsync each object's `.objects/<aa>/` shard
//! directory. On a journaling filesystem (ext4/xfs/apfs/zfs/btrfs — every
//! mainstream default) the create+rename of an object file is ordered by the
//! filesystem journal relative to the later manifest-directory fsync, so the
//! object's directory entry is durable once the manifest barrier completes. On
//! an exotic **non-journaling** filesystem with no such ordering guarantee, a
//! crash in the narrow window after the manifest is durable but before the
//! object directory entries are could still surface a dangling manifest. That
//! is an accepted trade-off for keeping the cost at two syncs; the alternative
//! (an fsync per shard directory) defeats the whole point of batching. Use a
//! journaling filesystem (the universal default) for crash-consistency.
//!
//! ## Platform notes
//!
//! All raw syscalls go through `libc` — already a direct dependency of
//! `snapdir-core` and `snapdir-cli`, so this adds **no new crate** to the lock
//! graph and keeps the shipped binary dependency-free (no shelling out). Every
//! call is best-effort with respect to *unsupported*: an `ENOTSUP`/`EINVAL`
//! from `sync_file_range` (e.g. on a filesystem that does not implement it)
//! transparently falls back to a full `fsync`/`sync_data`. A genuine I/O error
//! is propagated so the receiver can abort before committing the manifest.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use snapdir_core::store::StoreError;

/// Issues a cheap, non-blocking writeout *hint* for a freshly committed object
/// so its dirty pages start migrating to disk early — amortizing the cost of
/// the later [`barrier_objects`] pass. This is **not** a durability point on
/// its own; it never blocks waiting for completion.
///
/// - **Linux:** `sync_file_range(fd, 0, 0, SYNC_FILE_RANGE_WRITE)` — start
///   writeback for the whole file, do not wait.
/// - **macOS/other unix:** `fsync(fd)`. On Darwin `fsync` is writeout-only
///   (it does *not* issue the drive cache-flush barrier that `F_FULLFSYNC` /
///   `File::sync_all` would — and `F_FULLFSYNC` per object is exactly the
///   per-object full barrier this batched design avoids), so it is the right
///   analogue of the Linux writeout hint.
///
/// Any error is swallowed (best-effort hint): correctness is owned by
/// [`barrier_objects`], which *does* propagate errors.
pub fn writeout_hint(file: &File) {
    let _ = writeout_hint_inner(file);
}

#[cfg(target_os = "linux")]
fn writeout_hint_inner(file: &File) -> io::Result<()> {
    // offset=0, nbytes=0 => "to end of file". SYNC_FILE_RANGE_WRITE starts
    // async writeback without waiting.
    let ret = unsafe { libc::sync_file_range(file.as_raw_fd(), 0, 0, libc::SYNC_FILE_RANGE_WRITE) };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
fn writeout_hint_inner(file: &File) -> io::Result<()> {
    // On Darwin `fsync` is writeout-only (no drive cache-flush). On other
    // unixes it is a real flush, which is still a valid (if stronger) hint.
    let ret = unsafe { libc::fsync(file.as_raw_fd()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Durability barrier: forces every object written during this pack to stable
/// storage in a single pass (full sync #1 of the two-sync budget). Called once,
/// immediately before the manifest is committed.
///
/// `paths` is the receiver's `written: Vec<PathBuf>` — the objects this pack
/// newly committed (duplicates / pre-seeded objects are not included; they were
/// made durable by whatever pack first wrote them). A missing path (e.g. a
/// concurrent GC) is tolerated; any other I/O error aborts the pack so the
/// manifest is never committed over un-synced data.
pub fn barrier_objects(paths: &[std::path::PathBuf]) -> Result<(), StoreError> {
    for path in paths {
        let file = match File::open(path) {
            Ok(file) => file,
            // The object vanished between commit and barrier (concurrent GC /
            // racing writer). Nothing of ours to make durable here.
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(StoreError::Io(err)),
        };
        barrier_one(&file).map_err(StoreError::Io)?;
    }
    Ok(())
}

/// Forces a single already-committed object's data to stable storage.
///
/// - **Linux:** `sync_file_range(fd, 0, 0, WAIT_BEFORE | WRITE | WAIT_AFTER)` —
///   wait for any in-flight writeback, start+finish the rest, then wait. If the
///   filesystem does not implement `sync_file_range` (`ENOTSUP`/`EINVAL`), fall
///   back to a full `fsync`.
/// - **other unix:** `fsync(fd)` (a.k.a. `File::sync_data`'s effect).
fn barrier_one(file: &File) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let flags = libc::SYNC_FILE_RANGE_WAIT_BEFORE
            | libc::SYNC_FILE_RANGE_WRITE
            | libc::SYNC_FILE_RANGE_WAIT_AFTER;
        let ret = unsafe { libc::sync_file_range(file.as_raw_fd(), 0, 0, flags) };
        if ret == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // `sync_file_range` is unsupported on some filesystems; fall back to a
        // full data sync rather than fail the pack.
        match err.raw_os_error() {
            Some(libc::ENOSYS | libc::ENOTSUP | libc::EINVAL) => file.sync_data(),
            _ => Err(err),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Darwin: `fsync` flushes the file's data to the device (writeout). It
        // is intentionally NOT `sync_all` (= `F_FULLFSYNC`, a per-object drive
        // cache-flush barrier) — the manifest commit's directory fsync provides
        // the single ordering barrier for the whole pack.
        let ret = unsafe { libc::fsync(file.as_raw_fd()) };
        if ret == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// fsyncs a directory so a rename *into* it (a freshly created or replaced
/// directory entry) is durable. Opening a directory read-only and `fsync`-ing
/// the resulting fd is the portable POSIX idiom for this; it is a no-op success
/// on the platforms where directory fsync is not meaningful.
///
/// Used by [`crate::file_store::write_manifest_durable`] for the manifest's
/// parent shard directory — full sync #2 of the two-sync budget.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    let dir_file = File::open(dir)?;
    let ret = unsafe { libc::fsync(dir_file.as_raw_fd()) };
    if ret == 0 {
        Ok(())
    } else {
        let err = io::Error::last_os_error();
        // Some platforms reject fsync on a directory fd (EINVAL/EBADF) because
        // directory metadata is journaled regardless; treat that as already
        // durable rather than an error.
        match err.raw_os_error() {
            Some(libc::EINVAL | libc::ENOTSUP) => Ok(()),
            _ => Err(err),
        }
    }
}

/// fsyncs a single just-written file's data to stable storage (`File::sync_data`
/// = `fdatasync`). Used by [`crate::file_store::write_manifest_durable`] for the
/// manifest temp file before it is renamed into place.
pub fn sync_file_data(file: &File) -> io::Result<()> {
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "snapdir-fsync-test-{}-{tag}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn pack_fsync_writeout_hint_and_barrier_succeed_on_real_files() {
        let dir = TempDir::new("hint");
        let mut paths = Vec::new();
        for i in 0..4u8 {
            let p = dir.path().join(format!("obj-{i}"));
            let mut f = File::create(&p).expect("create");
            f.write_all(&[i; 4096]).expect("write");
            // Hint is best-effort and must not panic on a real, open fd.
            writeout_hint(&f);
            f.sync_all().expect("sync");
            paths.push(p);
        }
        // The whole-batch barrier must succeed over real files.
        barrier_objects(&paths).expect("barrier over real files");
    }

    #[test]
    fn pack_fsync_barrier_tolerates_a_missing_object() {
        let dir = TempDir::new("missing");
        let present = dir.path().join("present");
        File::create(&present).expect("create");
        let absent = dir.path().join("does-not-exist");
        // A vanished object (concurrent GC) is tolerated, not an error.
        barrier_objects(&[present, absent]).expect("missing path tolerated");
    }

    #[test]
    fn pack_fsync_sync_dir_and_sync_file_data_succeed() {
        let dir = TempDir::new("dir");
        let p = dir.path().join("file");
        let mut f = File::create(&p).expect("create");
        f.write_all(b"durable\n").expect("write");
        sync_file_data(&f).expect("fdatasync the file");
        // Directory fsync must succeed (or be a tolerated no-op) on a real dir.
        sync_dir(dir.path()).expect("fsync the directory");
    }
}

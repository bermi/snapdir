# Files-in-flux robustness — design lock (1.9.0)

Gate: `flux-design` (phase 31, human ✋). Locks the model for the flux cluster
(`flux-spec-tests` → `flux-impl-core-sigbus` → `flux-impl-core-detect` → `flux-impl-cli`
→ `flux-tests-review` → `flux-fuzz-verify`).

## Problem

`snapdir id`/`manifest` on a tree whose files change DURING the walk can fail badly:
the user reports "sometimes fails without printing an error or an id, sometimes prints a
missing-file error." Root-caused to FOUR failure modes (in `crates/snapdir-core/`):

1. **SIGBUS silent kill** — a large file (≥ `MMAP_THRESHOLD` = 256 KiB) TRUNCATED while it's
   being hashed via `update_mmap_rayon` faults on the now-invalid mmap pages → the kernel
   SIGBUS-kills the process with NO snapdir message. This is the "fails without printing
   anything" symptom (`hash_file.rs`, documented caveat ~L37-47: "same exposure as b3sum;
   we deliberately do not install a SIGBUS handler").
2. **Two `expect()` panics** — `walk.rs:399` ("child dir finalized before parent") and
   `walk.rs:653` ("pending file's owning dir was discovered") can fire (backtrace, not a
   clean error) if a directory vanishes mid-finalize.
3. **Silent wrong-size** — a file that GROWS during hashing records the `len` captured at
   stat time, not the bytes actually hashed → a silently incoherent manifest entry.
4. **Generic ENOENT** — a deleted file currently aborts with a generic `WalkError::Io`
   message (works, but mislabels a transient tree-in-flux race as a durable IO fault).

## Goal / invariant

Every in-flux condition yields **EITHER** a valid manifest + 64-hex id (exit 0) **OR** a
clear, typed error naming the file and the change kind (non-zero exit). **Never** a silent
process kill, **never** a panic/backtrace, **never** a silently mis-recorded entry. Snapshot
ids stay **byte-identical on a static (quiescent) tree** (frozen manifest/merkle/excludes lock).

## Locked model (operator-approved during planning, 2026-06-17)

### A. SIGBUS handler + single-thread `update_mmap` (keep mmap perf)
- **`flux-impl-core-sigbus`** (core): add a unix-only module `crates/snapdir-core/src/sigbus.rs`:
  a lazy `Once` `sigaction(SIGBUS, SA_SIGINFO|SA_ONSTACK|SA_NODEFER)` that **chains the
  previously-installed handler** for faults that are NOT ours (si_addr outside our registered
  mmap region), a thread-local `sigjmp_buf` + the current thread's active mmap region
  `(base,len)`, and a `sigaltstack` installed once.
- **Critical consequence:** `update_mmap_rayon` fans one file's hashing across multiple rayon
  workers, so a faulting worker could not `siglongjmp` to a buffer armed on a different thread.
  Therefore hash each file on a **single thread via `update_mmap`** (switch
  `update_mmap_rayon` → `update_mmap` in `hash_file.rs`) and `sigsetjmp` around that call on
  the same thread. This **keeps the mmap no-copy win** and the **cross-file rayon parallelism**
  (the dominant win); only the rare lone-huge-file *intra-file* threading is dropped.
- Handler body is async-signal-safe (reads a TLS pointer + `siglongjmp` only). On a longjmp,
  return a clean `io::Error` that the detect gate maps to a typed `WalkError`.
- Non-unix keeps `fs::read` (no mmap → no SIGBUS). Re-verify the frozen lock + walk/`fast_walk`
  goldens (ids byte-identical) after this gate.

### B. stat-guards + typed errors (detect ENOENT / truncate / grow cleanly)
- **`flux-impl-core-detect`** (core): add `WalkError::{FileVanishedDuringWalk{path},
  FileChangedDuringWalk{path}, TreeStructureChanged{path}}` — each names the path; `Display`
  is actionable (e.g. "file changed during walk (tree changed under snapdir): <path>;
  re-run on a quiescent tree").
- In `hash_one` (`walk.rs` ~614-626): map io `NotFound` → `FileVanishedDuringWalk`; do a
  **stat-before / stat-after** on the content path AND compare **bytes-streamed vs the
  discovery `FileRecord.size`** (SKIP the byte check for followed symlinks, whose recorded
  SIZE is the lstat length); keep `WalkError::Io` for genuine permission/IO faults (distinct
  message). This catches grow/shrink/atomic-replace without a silent size mismatch.

### C. Convert the two `expect()` panics
- `walk.rs:399` and `walk.rs:653` `expect()` → `TreeStructureChanged{path}` returns.
  Control-flow-only change on the error path; the happy-path manifest bytes are unchanged
  (frozen lock holds; `walk.rs`/`hash_file.rs` are NOT in the frozen set).

### D. CLI surfacing — fail-fast (defer `--on-change`)
- **`flux-impl-cli`** (cli): the new `WalkError` variants surface automatically via
  `snapdir_cli::run`'s `eprintln!("{err:#}")` + `ExitCode::FAILURE`; verify `id`/`manifest`
  propagate them and the message names the file. CHANGELOG `[Unreleased]` entry; document the
  quiescent-tree consistency model in help/man where it fits.
- **Fail-fast only.** A `--on-change=fail|retry|skip` knob is **deferred** (skip would produce
  a "valid" id that silently doesn't describe the tree — contradicts the no-silent goal). The
  new error variants are the exact hook points a future knob would branch on.

## Adversarial verification (the gate)
- **`flux-spec-tests`** (adversary): `concurrent_mutation.rs` — a mutator thread hammering
  truncate/grow/delete/replace on `>256 KiB` victims while `walk()` runs, **thousands of
  iterations × `walk_jobs ∈ {1,4,None}`**, fixed PRNG. Invariant every iteration: no panic,
  no SIGBUS-kill, outcome ∈ {valid 64-hex id AND untouched control file == its golden} ∪
  {typed `WalkError` naming a path}. Plus a focused mid-mmap truncation repro asserting a clean
  `Err` (not a kill). Plus a quiescent control asserting the golden id is unchanged (zero
  behavior overhead on a static tree).
- **`flux-fuzz-verify`** (adversary, independent, haiku): build from source, ≥10k-iteration
  fuzz + the truncation repro against the freshly-built binary → `.gatesmith/evidence/
  flux-fuzz-verify.log` with a PASS verdict and no PANIC/SIGBUS markers.

## Keystone / invariants
- Snapshot ids byte-identical on a static tree (walk/`fast_walk` goldens + frozen lock
  `shasum -c .gatesmith/manifest-format.sha.lock` after each core gate).
- `manifest.rs`/`merkle.rs`/`excludes.rs` untouched (error-path + hashing-engine changes live
  in `walk.rs`/`hash_file.rs`/`sigbus.rs`, none frozen).
- No new dependency (libc already pinned; blake3 mmap/rayon features already present).

## Back-compat
- Internal/error-path only; behavior on a quiescent tree is unchanged. The only observable
  change is that in-flux conditions now produce clear typed errors / exit codes instead of a
  silent kill, a panic, or a silently-wrong size. Minor.

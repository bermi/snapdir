# Security review — Phase 32 exact-mirror (`--delete`) + materialization modes

> Gate: `mirror-security-review` (human ✋). Destructive operation → dedicated review.
> Scope: the Phase-32 diff `2447fc3..HEAD` (10 src files; ~1.5k LOC).
> Reviewer: Gatesmith PM, cross-referencing the dedicated adversarial suites.
> Status: PROPOSED — awaiting operator sign-off.

## Source surface reviewed
`crates/snapdir-core/src/{mirror.rs(new),recover.rs(new),walk.rs,hash_file.rs,lib.rs}`,
`crates/snapdir-stores/src/{file_store.rs,sync.rs,stream.rs,lib.rs}`,
`crates/snapdir-cli/src/cli.rs`.

## Threat model (this feature DELETES user data and writes into a shared store)

| # | Threat | Control | Adversarial evidence (all green) |
|---|---|---|---|
| T1 | **Delete outside the dest root** (prune escapes) | `prune_set` is a pure path-set difference over `./`-relative manifest/dest paths (`mirror.rs`); results are always dest-relative; CLI `prune_dest` joins them under the dest only | `mirror_prune_set.rs` (36) + `mirror_safety.rs` (16) — **outside-dest canaries** proven byte-identical after prune |
| T2 | **Symlink escape** (follow a dest symlink and delete its external target / recurse out) | prune walk is **lstat-based**; an extraneous symlink is `remove_file`'d (the link unlinked), never followed; no `remove_dir_all` through a link | `mirror_safety.rs`: abs/rel/`..`/buried escaping symlinks, symlink-to-`$HOME`, symlink loop — external trees survive |
| T3 | **Destroy a dangerous dest** (`/`, `$HOME`, cache, store) | canonicalizing guard fires **FIRST** (before store/manifest lookup), **no `--force` bypass**, refuses with nothing deleted; covers `--store` + `--objects-store` + default cache; canonicalization aliases handled | `mirror_safety.rs` + `mirror_checkout_cli.rs` — sentinel/whole-dest byte-identical on refusal, `--force` does not bypass |
| T4 | **Corrupt the shared content-addressed store** (write through a linked file; delete shared objects) | linked objects hardened to **`0444`** (write-through → `EACCES`); `--linked` to a remote object source is a hard error; **sync `--delete` NEVER deletes objects** (only `delete_manifest`; `grep delete_object` → 0) | `mirror_materialize_modes.rs`, `mirror_durability.rs` (write blocked, object intact), `mirror_sync.rs` (16, never-delete-object 3 ways), `mirror_feasibility.rs` (7) |
| T5 | **Atomicity / torn state** (crash mid-mirror) | atomic swap = same-fs `rename` (swap-or-nothing), staging cleaned on every error path; in-place prune is per-file `remove` (crash leaves a superset, never corrupt content); held-open fds survive (POSIX inode retention) | `mirror_atomic_swap.rs` (12), `mirror_durability.rs` (9 — held-fd survives across all modes) |
| T6 | **Trust without verify** (linked fast-path recovers a checksum from the object path without reading bytes) | eligibility gated to **plain non-keyed BLAKE3 + local store**; recovery is pure path parsing; dangling → typed error (no panic); `SNAPDIR_VERIFY_COPIES=1` forces a content re-hash + errors on mismatch | `mirror_linked_fastpath.rs` (15 — no-content-read proof, strict-verify error, wrong-algo/escaped/dangling fallbacks) |
| T7 | **Manifest-pruning over-reach** (sync deletes the wrong manifests) | prunes only `to.list_manifest_ids() − from.list_manifest_ids()`; the just-synced id is guarded; copy-in strictly **before** any delete | `mirror_sync.rs` — exact `pruned_ids` match, copy-before-delete, idempotent |

## Findings

- **No HIGH/CRITICAL findings.** Every destructive path is bounded to the dest-relative prune set or the manifest-id set; nothing recurses through symlinks; dangerous dests are unconditionally refused before any deletion; the shared object pool is never written-through (0444) or deleted (no `delete_object`).
- **Defense-in-depth is strong:** the threat model above is exactly what the dedicated `mirror_safety` / `mirror_durability` / `mirror_sync` / `mirror_feasibility` adversarial suites were written to attack, and all pass.

## Accepted residual risks (LOW, by design / documented)

1. **TOCTOU between dest-walk and delete (LOW).** The prune set is computed by an lstat walk, then each path removed. A concurrent external mutation of the dest during the op is not transactionally guarded — but the blast radius is bounded: deletions only ever target the computed dest-relative set via `remove_file`/`remove_dir` (a symlink swapped in mid-op is still only *unlinked*, never followed). Standard safe-unlink pattern; matches `cp`/`rsync` semantics. No fix required.
2. **Linked fast-path trades verify-on-read for speed (LOW, operator-approved).** Recovering a checksum from the object address without reading content is the explicit design (checksum-only; `SNAPDIR_VERIFY_COPIES=1` restores verification). Documented in the design lock.
3. **iai instruction-count perf gate is CI-only on this host** (valgrind/Apple-Silicon limitation) — not a security concern.

## Recommendation
Security posture is sound for a destructive feature: bounded deletions, hard-refused dangerous dests, an uncorruptible shared store, and dedicated adversarial coverage for each threat. **Recommend approval.** (Optionally, the operator may run the automated `/security-review` skill against `2447fc3..HEAD` for an independent second pass before sign-off.)

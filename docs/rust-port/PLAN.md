# snapdir-rs — locked build plan

> Vendored copy of the approved plan, used by the gatesmith PM and lane teammates.
> Authoritative for decisions, the frozen contract, and the phase→gate map.
> This is a historical planning document. During the port, the source of truth
> for behavior was the original Bash `snapdir` (the `snapdir`, `snapdir-manifest`,
> and `snapdir-<name>-store` scripts), **not the old docs** (which carried known
> bugs — see "Doc bugs"). The byte-format contract is now guarded by the Rust
> golden tests and the `manifest-format.sha.lock` tripwire.

## Goal

Port `snapdir` (MIT, ~99% Bash, `v0.5.0`) to a single statically-linked **zero-runtime-dependency**
Rust binary that absorbs every `snapdir-*` helper as a subcommand. The binary must NOT shell out
to `b3sum`, `gcloud`, `aws`, `b2`, or `sqlite3` — all of that becomes in-process Rust
(`blake3` crate, native cloud SDKs, `redb`). External system binaries are allowed only in the
**test/oracle** harness (the Bash tool + `b3sum` are the interop oracle), never in the shipped binary.

The hard constraint is **byte-for-byte manifest interoperability** with the Bash version: identical
manifest lines, identical snapshot IDs, identical object/manifest keys and bucket layout, so Rust-
and Bash-written caches and remote buckets remain mutually readable.

## Locked decisions

- **Repo/branch:** this repo, branch `rust-port`, fresh Cargo workspace alongside the untouched Bash
  scripts (which serve as the interop oracle). Merge to `main` at parity.
- **Execution:** gatesmith ralph-loop. This ledger *is* the plan.
- **Catalog:** **redb** (pure-Rust embedded KV). SQLite removed. Catalog is private/rebuildable —
  no on-disk interop, no SQLite→redb importer. Only the JSON-line *output* stays format-identical.
- **Stores:** native Rust SDKs — `google-cloud-storage` (gs), `aws-sdk-s3` (s3), `aws-sdk-s3` →
  Backblaze S3-compatible endpoint (b2). Emit-shell-command shim retained ONLY for third-party
  external stores. Auth delegated entirely to each SDK's own credential chain — no bespoke env vars.
- **TLS/crypto:** use the **ring** rustls provider (not aws-lc-rs) so the static-musl build stays clean.

## Frozen contract (freeze after Phase 2)

The manifest spec + golden fixtures. Confirmed from the Bash source:

- **Manifest line:** `PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH` — single space separated, sorted by
  path (`sort -k5`); empty lines stripped; `#` lines are comments excluded from the checksum.
- `PATH_TYPE` `F`/`D`; directory paths end `/`; relative mode prefixes `./`, `--absolute` keeps the
  full path.
- `PERMISSIONS` octal — macOS `stat -f '%A'`, Linux `stat -c '%a'`.
- `SIZE` content bytes — macOS `%z`, Linux `%s`; directories = **sum of member sizes** (dir metadata
  excluded).
- **Symlinks followed by default** (`find -L`); entry inherits the target's type/checksum/size.
  `--no-follow` drops `-L` and excludes symlinks.
- **Directory checksum (critical):** the `D ./` line's `CHECKSUM` field. Over the direct children's
  checksums do `cut -d' ' -f3 | sort -u | tr -d '\n'` then hash the concatenation — i.e.
  **sort + dedup + concatenate with no separators + re-hash**.
- **Snapshot ID (critical):** `manifest | grep -v '^#' | b3sum --no-names` — BLAKE3 of the **entire
  `#`-stripped manifest text**, including the trailing newline the oracle's `echo` appends. The
  snapshot ID is therefore **NOT** the root directory checksum; it is the hash of the whole manifest
  document, not of any single line's checksum field.
- **Hash:** default `b3sum --no-names`; escape hatch `--checksum-bin=` (`md5sum`,`sha256sum`, parse
  `cut -d' ' -f1`); keyed mode `SNAPDIR_MANIFEST_CONTEXT` → `b3sum --derive-key=<ctx> --no-names`.
- **Excludes:** `--exclude` is an extended regex (`grep -E -v`); `%system%` expands a built-in set AND
  forces `--no-follow`; `%common%` expands a second set (`.git`, `.cache`, `node_modules`,
  `.DS_Store`, Trash, …).
- **Content-addressable layout:** objects `.objects/<h[0:3]>/<h[3:6]>/<h[6:9]>/<h[9:]>`; manifests
  `.manifests/<id[0:3]>/<id[3:6]>/<id[6:9]>/<id[9:]>`; mirrored in cache and every store.
- **Cache:** `${XDG_CACHE_HOME:-$HOME/.cache}/snapdir/`; per-file key = path+context+inode+perms+size+
  mtime; `cache-id` = hash(context + checksums of all cache files); `--cache-id` pre-verifies.

Golden hashes (from `utils/qa-fixtures/expected-guide-commands.txt`):
empty-dir id `dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b`,
empty-file `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`,
modified-dir `4a0732cfb45ebe9d8d572fc4c77b759384bed029911e35f8859430b889427d4d`,
object `49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92`.

## CLI surface (reproduce exactly)

Orchestrator subcommands: `manifest id stage push fetch pull checkout verify verify-cache
flush-cache locations ancestors revisions defaults` (+ `test version help`). Global options:
`--cache-dir --catalog --store --id --exclude --paths --linked --force --purge --keep --dryrun
--verbose --debug --location`.

Store routing: `--store` protocol → `snapdir-<proto>-store`, **except `gs://` is a hardcoded special
case → `snapdir-gcs-store`** (adapter named `gcs`, scheme `gs`). Store push: check manifest exists,
push objects before manifest, only if absent. Fetch: temp download → verify BLAKE3 → retry ≤5× →
atomic rename.

Catalog JSON-line output shapes (the brief was wrong about `revisions`):
- `locations` → `{"created_at","id","location"}` (latest id per location).
- `ancestors` → `{"created_at","id","location"}` (`id` = previous_id), `created_at DESC`.
- `revisions` → `{"created_at","id","previous_id"}`, `created_at DESC`.
- Timestamp format `YYYY-MM-DD HH:MM:SS.SSS` (`STRFTIME('%Y-%m-%d %H:%M:%f')`).

## Doc bugs to NOT replicate

`docs/api/snapdir.md` shows `--link` (real: `--linked`); store docs show `verify-transactions`
(real: `ensure-no-errors`); minor wget/umask/typos. Pin to the scripts' real behavior; fix the docs
in the Rust port, don't carry the errors.

## Lanes → crates/dirs

```
crates/snapdir-core/      # manifest + BLAKE3 merkle hashing + Store trait + walk + cache  (lane: core)
crates/snapdir-catalog/   # redb-backed locations/ancestors/revisions                      (lane: catalog)
crates/snapdir-stores/    # FileStore, S3Store, B2Store, GcsStore + external-shim          (lane: stores)
crates/snapdir-cli/       # clap derive bin `snapdir`, all 14 subcommands                  (lane: cli)
tests/                    # interop differential harness + fixtures + CLI integration      (lane: tests)
benches/                  # criterion hash/walk/manifest hot paths                         (lane: bench)
.github/workflows/, deny.toml, _typos.toml, Cargo.toml, rust-toolchain.toml, rustfmt.toml  (lane: ci)
docs/rust-port/           # rustdoc, migration guide, manifest spec, CHANGELOG             (lane: docs)
packaging/                # release.yml / cargo-dist, Docker, completions, man page         (lane: packaging)
```

Library/binary split (jj principle): `snapdir-core` does no terminal I/O and reads no `$HOME`/config.
`thiserror` in libs, `anyhow` + `.context()` in the bin. Native SDK transfers are `tokio`-async;
walk+hash stay sync+`rayon`.

## Phase map (→ gates.yaml)

0. **Bootstrap** — gatesmith installed, ledger + templates + settings authored, plan vendored, commit.
1. **Scaffold + CI** — workspace, clap skeleton (all subcommands), clippy::pedantic, MSRV, ci.yaml
   (lint/deny/test-matrix incl. **musl**/coverage/semver), skeleton release.yml. Test musl now.
2. **Core manifest/hashing + FREEZE** — exact line format, dir merkle (sort+dedup+concat+rehash),
   keyed mode, `--checksum-bin`, excludes, `--no-follow`, both-OS stat. insta vs golden → FREEZE.
3. **Interop gate (HARD)** — differential harness Bash vs Rust over a fixture corpus; byte-identical
   manifests + IDs across b3sum/md5sum/sha256sum + keyed mode; Linux + macOS. human_checkpoint.
4. **Store trait + FileStore** — sharded layout, push/fetch/checkout, external-store shim, gs:// router.
5. **Remote stores** — S3/B2/GCS via native SDKs (ring rustls); preserve ordering+verify; emulator
   integration + cross-tool (Bash↔Rust) interop.
6. **Cache + redb catalog** — cache-id integrity; redb locations/ancestors/revisions matching JSON
   shapes; `snapdir catalog rebuild`.
7. **Performance** — rayon+mmap hashing, parallel walk, buffer reuse, allocator eval; criterion/
   hyperfine/CodSpeed; beat Bash baseline, output bytes unchanged.
8. **Testing/fuzzing** — proptest, cargo-fuzz parser (cron), trycmd/assert_cmd full CLI, coverage gate.
9. **Docs** — rustdoc+doctests, README/CHANGELOG, migration guide (subcommand + auth mapping tables),
   fix doc bugs. human_checkpoint.
10. **Packaging/release** — cross-compiled per-target archives (musl static etc.), completions+man,
    crates.io, slim Docker, release-plz + semver-checks. First tag tracks 0.5.0. human_checkpoint.

## Key risks / thresholds

- TLS vs static musl → use ring provider; test musl in Phase 1; fallback to emit-shim per backend.
- mmap+rayon regression on spinning disks → default streamed hashing, parallelism opt-in.
- Any manifest diff (P3) or catalog-shape diff (P6) freezes downstream until resolved.
- External-store shim is load-bearing — implement + test it.
- Google Cloud Rust SDK is largely pre-1.0 — pin versions.
- Windows may legitimately diverge on perms/symlinks — separate fixtures, document.

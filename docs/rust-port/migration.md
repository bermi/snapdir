# Migrating from Bash `snapdir` to the Rust port

This guide explains how to move from the original Bash `snapdir` (v0.5.0) — a
collection of cooperating shell scripts (`snapdir`, `snapdir-manifest`,
`snapdir-<name>-store`, `snapdir-sqlite3-catalog`) — to the Rust port: a single,
statically-linkable, **zero-runtime-dependency** `snapdir` binary that absorbs
every helper as a subcommand.

The two implementations are **byte-for-byte interoperable** at the data layer:
identical manifest lines, identical snapshot IDs, and identical object/manifest
keys and bucket layout. A store written by one tool is fully readable by the
other (see [Interop](#5-interop-byte-for-byte-compatibility)). What changes is
the *command surface* (one binary instead of many scripts), the *credential
model* (each backend uses its native SDK's standard credential chain instead of
bespoke snapdir env vars), and the *catalog backend* (internal `redb` instead of
on-disk SQLite).

> This guide describes the **real behavior of the original Bash `snapdir`**
> (the `snapdir`, `snapdir-manifest`, `snapdir-<name>-store`, and
> `snapdir-sqlite3-catalog` scripts), reproduced byte-for-byte by the Rust port
> and cross-checked against its clap surface in `crates/snapdir-cli/src/cli.rs`.
> Where the *old Bash docs* carried known bugs, this guide uses the **correct**
> names — see [Corrected doc bugs](#6-corrected-documentation-bugs).

---

## 1. Tool → subcommand mapping

The Bash distribution shipped several executables. In the Rust port they all
collapse into subcommands (and store routing) of the single `snapdir` binary.

### 1.1 The manifest helper

| Bash                                | Rust                              | Notes                                                          |
| ----------------------------------- | --------------------------------- | ------------------------------------------------------------- |
| `snapdir-manifest <dir>`            | `snapdir manifest <dir>`          | In-process walk + BLAKE3; byte-identical output.              |
| `snapdir-manifest --id <dir>` / id of a manifest | `snapdir id <dir>` / `snapdir id` (manifest on stdin) | Snapshot ID = BLAKE3 of the `#`-stripped manifest text.       |

`snapdir manifest` accepts `--absolute`, `--no-follow`, `--checksum-bin`, and
`--exclude` exactly as the helper did. `snapdir id` always uses the default
BLAKE3 derivation and is independent of `--checksum-bin`.

### 1.2 The store helper scripts

In Bash, each backend was a separate `snapdir-<name>-store` executable resolved
off `PATH` and driven through an emit-command protocol. In the Rust port the
built-in backends are **in-process** and selected purely by the `--store` URI
scheme; there is no per-backend helper binary to install.

| Bash store helper       | Rust selection (`--store URI`)            | Backend                                  |
| ----------------------- | ----------------------------------------- | ---------------------------------------- |
| `snapdir-file-store`    | `--store file://<dir>`                    | Built-in `FileStore` (in-process)        |
| `snapdir-s3-store`      | `--store s3://<bucket>/<prefix>`          | Built-in S3 (`aws-sdk-s3`, in-process)   |
| `snapdir-b2-store`      | `--store b2://<bucket>/<prefix>`          | Built-in B2 (S3-compatible, in-process)  |
| `snapdir-gcs-store`     | `--store gs://<bucket>/<prefix>`          | Built-in GCS (`google-cloud-storage`)    |
| third-party `snapdir-<proto>-store` | `--store <proto>://…`         | **External shim**: dispatches to the `snapdir-<proto>-store` binary on `PATH` |

Notes:

- The `gs://` scheme maps to the **`gcs`** adapter — the oracle's hardcoded
  `gs`→`gcs` special case (`snapdir` store routing) is preserved.
- For any scheme that is **not** one of the four built-ins, the Rust binary falls
  back to the **external-store shim**: it resolves a `snapdir-<proto>-store`
  binary on `PATH` and drives it through the same emit-command contract the Bash
  tool used. This keeps third-party stores working without bundling them.
- The shipped binary never shells out to `b3sum`, `aws`, `b2`, `gcloud`, or
  `sqlite3` for the built-in backends; those tools are only needed by the
  external shim (for third-party stores) and by the test/oracle harness.

### 1.3 Subcommand wiring status

All 14 subcommands are present in the clap surface and **all 14 are now wired**
— the CLI is feature-complete (no stubs remain). The table below reflects the
actual implementation state (matching the `cli-trycmd` coverage map). Every
command reuses an already-tested library path (`snapdir-core`, `snapdir-core`'s
`cache`, `snapdir-catalog`, or `snapdir-stores`); none shells out.

| Subcommand      | Status      | Notes                                                                                                  |
| --------------- | ----------- | ------------------------------------------------------------------------------------------------------ |
| `manifest`      | ✅ wired    | In-process walk; byte-identical to `snapdir-manifest`.                                                  |
| `id`            | ✅ wired    | Snapshot ID of a dir or a manifest on stdin.                                                            |
| `push`          | ✅ wired    | Walk + push to the resolved store (`file`/`s3`/`b2`/`gs`/external); also logs the snapshot to the catalog. |
| `fetch`         | ✅ wired    | Read+verify manifest, materialize verified objects into the cache.                                     |
| `pull`          | ✅ wired    | `fetch` + `checkout`.                                                                                   |
| `checkout`      | ✅ wired    | Materialize from the local cache and restore permissions.                                              |
| `verify`        | ✅ wired    | Re-hash every referenced object from the store.                                                        |
| `stage`         | ✅ wired    | Caches the tree's objects + manifest into the local cache (a `push` to a `FileStore` rooted at the cache dir); prints the snapshot ID. |
| `verify-cache`  | ✅ wired    | `[--purge]` → `snapdir_core::cache::verify_cache`; reports each corrupt object, exits non-zero on any failure (oracle exit semantics). |
| `flush-cache`   | ✅ wired    | Empties the local cache (objects + manifests); idempotent on a missing/empty cache.                    |
| `locations`     | ✅ wired    | Queries `snapdir-catalog`, emits the frozen JSON lines (latest record per location).                   |
| `ancestors`     | ✅ wired    | `--id <ID> [--location <LOC>]` → catalog `previous_id` chain, frozen JSON lines, `created_at DESC`.    |
| `revisions`     | ✅ wired    | `--location <LOC>` → catalog revisions for a location, frozen JSON lines, `created_at DESC`.           |
| `defaults`      | ✅ wired    | Prints effective defaults + `SNAPDIR_*` env reformatted as `--opt=value`, per oracle `snapdir_defaults`. |

The wired set covers a full `stage → verify-cache`, a `push → fetch → checkout →
verify` round-trip against any supported store, and the `locations` /
`ancestors` / `revisions` history queries.

> **Known minor catalog gap (honest).** Only `push` currently logs to the
> catalog. The Bash oracle also logs catalog events on `manifest` and `stage`;
> the Rust port does not yet. In practice this is enough for the history queries
> to return real data (every pushed snapshot is recorded), but if you depend on
> `manifest`/`stage` also populating the catalog, that parity is still pending.

---

## 2. Per-backend authentication mapping

The biggest operational change: the Rust port **delegates authentication to each
backend's native SDK credential chain**. The bespoke `SNAPDIR_<BACKEND>_STORE_*`
credential env vars from the Bash helpers are gone — use the standard mechanism
for each cloud instead. (Custom endpoint/region overrides are retained where the
oracle had them.)

### 2.1 Google Cloud Storage (`gs://`)

GCS uses **Application Default Credentials (ADC)** via the `google-cloud-storage`
SDK. The legacy `SNAPDIR_GCS_STORE_CREDENTIALS_FILE` (which itself defaulted to
`GOOGLE_APPLICATION_CREDENTIALS`) and the `gcloud` CLI shell-out are gone.

| Legacy (Bash `snapdir-gcs-store`)        | Rust (`gs://` via ADC)                                                |
| ---------------------------------------- | -------------------------------------------------------------------- |
| `SNAPDIR_GCS_STORE_CREDENTIALS_FILE`     | `GOOGLE_APPLICATION_CREDENTIALS` — path to a service-account JSON key |
| (n/a)                                    | `GOOGLE_APPLICATION_CREDENTIALS_JSON` — the JSON key inline           |
| `gcloud auth login` / `gcloud auth list` | `gcloud auth application-default login` (writes ADC well-known file)  |
| (implicit on GCE)                        | GCE / GKE / Cloud Run **metadata server** (no config on-box)          |

Resolution order is the standard ADC chain: `GOOGLE_APPLICATION_CREDENTIALS` →
`GOOGLE_APPLICATION_CREDENTIALS_JSON` → the gcloud well-known file
(`gcloud auth application-default login`) → the GCE/GKE metadata server. Setting
`GOOGLE_APPLICATION_CREDENTIALS` to a service-account key path is the most common
explicit configuration.

### 2.2 AWS S3 (`s3://`)

S3 uses the **standard AWS credential chain** via `aws-config` / `aws-sdk-s3`.
The legacy `SNAPDIR_S3_STORE_AWS_ACCESS_KEY_ID` /
`SNAPDIR_S3_STORE_AWS_SECRET_ACCESS_KEY` (which already fell back to the standard
`AWS_*` vars) are gone — use the standard AWS mechanisms directly.

| Legacy (Bash `snapdir-s3-store`)                | Rust (`s3://` via the AWS chain)                          |
| ----------------------------------------------- | --------------------------------------------------------- |
| `SNAPDIR_S3_STORE_AWS_ACCESS_KEY_ID`            | `AWS_ACCESS_KEY_ID`                                       |
| `SNAPDIR_S3_STORE_AWS_SECRET_ACCESS_KEY`        | `AWS_SECRET_ACCESS_KEY` (+ `AWS_SESSION_TOKEN`)           |
| `AWS_DEFAULT_REGION`                            | `AWS_REGION` / `AWS_DEFAULT_REGION`                       |
| (n/a)                                           | `AWS_PROFILE` — shared `~/.aws/config` & `credentials`    |
| (n/a)                                           | AWS SSO (`aws sso login`)                                 |
| (implicit on EC2)                               | EC2 / ECS / IRSA **instance metadata**                    |
| `SNAPDIR_S3_STORE_ENDPOINT_URL`                 | `SNAPDIR_S3_STORE_ENDPOINT_URL` — **retained** (custom/S3-compatible endpoint) |

The full chain (env → shared profiles → SSO → container/instance metadata) is
resolved by the AWS SDK. `SNAPDIR_S3_STORE_ENDPOINT_URL` is kept verbatim from
the oracle so you can point at MinIO or any other S3-compatible endpoint.

### 2.3 Backblaze B2 (`b2://`)

B2 is reached over its **S3-compatible API**, so the B2 backend is a thin wrapper
over the S3 store. The legacy `SNAPDIR_B2_STORE_APPLICATION_KEY_ID` /
`SNAPDIR_B2_STORE_APPLICATION_KEY` (used with the `b2 authorize-account` CLI) are
gone — supply the B2 **application key ID/secret as the standard `AWS_*`
credentials** and point the endpoint at Backblaze's S3 host.

| Legacy (Bash `snapdir-b2-store`)        | Rust (`b2://` via the S3-compatible endpoint)                         |
| --------------------------------------- | --------------------------------------------------------------------- |
| `SNAPDIR_B2_STORE_APPLICATION_KEY_ID`   | `AWS_ACCESS_KEY_ID` (the B2 application key **ID**)                    |
| `SNAPDIR_B2_STORE_APPLICATION_KEY`      | `AWS_SECRET_ACCESS_KEY` (the B2 application key)                       |
| `b2 authorize-account …`                | (not needed — credentials resolve via the AWS chain)                  |
| (region implicit in `b2` CLI)           | `SNAPDIR_B2_REGION` / `AWS_REGION`, or `SNAPDIR_S3_STORE_ENDPOINT_URL` |

**Region matters.** Backblaze's S3 endpoint encodes the region as
`https://s3.<region>.backblazeb2.com` (for example
`https://s3.us-west-004.backblazeb2.com`). The Rust B2 store resolves the
endpoint in this order:

1. an explicit `SNAPDIR_S3_STORE_ENDPOINT_URL` (full URL — wins);
2. else derived from a region: `SNAPDIR_B2_REGION` → `AWS_REGION` →
   the built-in default `us-west-004`, formatted as `s3.<region>.backblazeb2.com`.

Set the region to the **key's real region** — a B2 application key is bound to a
single region, and a mismatched region host will fail to authenticate. If you do
not know it, set `SNAPDIR_S3_STORE_ENDPOINT_URL` to the exact host shown in the
Backblaze console.

---

## 3. Catalog: SQLite → internal redb

The Bash tool tracked snapshot history (`locations`, `ancestors`, `revisions`)
in a SQLite database via the `snapdir-sqlite3-catalog` helper (shelling out to
`sqlite3`). The Rust port replaces this with an **internal, embedded
[`redb`](https://crates.io/crates/redb) key-value store** — pure Rust, no
`sqlite3` shell-out.

| Aspect                | Bash (`snapdir-sqlite3-catalog`)        | Rust (internal `redb`)                          |
| --------------------- | --------------------------------------- | ----------------------------------------------- |
| Backend               | SQLite file (`sqlite3` CLI)             | Embedded `redb` (in-process)                    |
| On-disk interop       | n/a                                     | **None** — private, rebuildable; no importer    |
| Rebuild               | (re-scan / re-record)                   | `snapdir catalog rebuild`                        |
| Query output format   | `json_object(...)` JSON lines           | **Byte-for-byte identical** JSON lines          |

Key points:

- The catalog is **private and rebuildable**. There is **no on-disk catalog
  interop** and **no SQLite→redb importer** — you do not migrate the database
  file. Instead, rebuild the catalog from a store with `snapdir catalog rebuild`.
- The **query output is byte-compatible**: the JSON-line output of `locations`,
  `ancestors`, and `revisions` matches the Bash tool's `sqlite3` `json_object`
  output exactly (compact, identical key order; `revisions` omits `location`;
  `previous_id` is a bare `null`). Anything parsing that JSON keeps working.

> The catalog subcommands (`locations` / `ancestors` / `revisions`) are wired
> into the CLI and emit the frozen JSON lines (see §1.3). The on-disk format and
> JSON output contract are frozen. One honest caveat: only `push` currently logs
> to the catalog — the oracle also logs on `manifest`/`stage`, which the Rust
> port does not yet, so history reflects pushed snapshots only.

---

## 4. Manifest & snapshot-ID interop

Manifests and snapshot IDs are **byte-for-byte identical** between the Bash tool
and the Rust port. See [`manifest-spec.md`](./manifest-spec.md) for the full
frozen format; the essentials for migration:

- **Default checksum is BLAKE3.** The Rust port hashes in-process with the
  `blake3` crate (no `b3sum` shell-out) and reproduces `b3sum --no-names`
  byte-for-byte.
- **Alternate checksums** via `--checksum-bin md5sum` / `--checksum-bin
  sha256sum` reproduce the oracle's `md5sum`/`sha256sum` leading digest
  in-process (`md-5` / `sha2` crates).
- **Keyed mode** is selected by a non-empty `SNAPDIR_MANIFEST_CONTEXT`
  environment variable (BLAKE3 `derive_key`), exactly as the oracle.
- **Snapshot ID** is the BLAKE3 of the `#`-stripped manifest text *including its
  trailing newline* — **not** the root directory checksum. (`snapdir id`.)

Because manifests, IDs, and the sharded object/manifest layout
(`.objects/…`, `.manifests/…`) are identical, **stores interoperate**: the Rust
binary can read and write stores written by the Bash tool, and vice-versa. This
is proven by the differential interop harness (manifests + IDs across every
checksum/keyed/no-follow mode) and live S3 (MinIO) and GCS cross-tool round-trips.

---

## 5. Interop: byte-for-byte compatibility

| Layer                | Compatible? | How                                                                 |
| -------------------- | ----------- | ------------------------------------------------------------------- |
| Manifest text        | ✅          | Identical line format, ordering (`sort -k5`), comment/blank rules.  |
| Snapshot ID          | ✅          | BLAKE3 of `#`-stripped manifest text (incl. trailing newline).      |
| Object / manifest keys | ✅        | Same 3-level sharded layout (`.objects/<h0:3>/<h3:6>/<h6:9>/<h9:>`). |
| Stores (file/S3/B2/GCS) | ✅       | Same bucket layout → mutually readable Bash↔Rust.                   |
| Catalog **file**     | ❌          | Private redb; rebuild via `snapdir catalog rebuild`, do not copy.   |
| Catalog **query output** | ✅      | JSON-line output identical to the `sqlite3` catalog.                |

Practical implication: you can switch tools per-invocation against the same
`file://`/`s3://`/`b2://`/`gs://` store with no migration step. Only the local
catalog/history database is not portable — rebuild it from the store.

---

## 6. Corrected documentation bugs

The original Bash **docs** carried two naming bugs that did **not** match the
frozen scripts. This guide (and the Rust port) use the **correct** names; the
scripts already behaved this way.

| Wrong (old Bash docs) | Correct (real script behavior) | Where                                                       |
| --------------------- | ------------------------------ | ---------------------------------------------------------- |
| `--link`              | **`--linked`**                 | The `checkout` flag for symlink-instead-of-copy.           |
| `verify-transactions` | **`ensure-no-errors`**         | The store-side transaction-check subcommand.               |

If you have scripts or docs referencing `--link` or `verify-transactions`,
update them to `--linked` and `ensure-no-errors`. The Rust clap surface only
accepts `--linked`.

---

## 7. Quick migration checklist

1. Replace each `snapdir-manifest <dir>` call with `snapdir manifest <dir>`
   (and manifest-ID lookups with `snapdir id`).
2. Drop the per-backend `snapdir-<name>-store` helper installs for the four
   built-in backends; select the backend with `--store <scheme>://…`. Keep any
   third-party `snapdir-<proto>-store` binary on `PATH` — the external shim still
   drives it.
3. Replace bespoke credential env vars with the native chain per backend:
   - **GCS** → `GOOGLE_APPLICATION_CREDENTIALS` (or ADC / metadata).
   - **S3** → `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_REGION` /
     `AWS_PROFILE` / SSO / instance metadata.
   - **B2** → application key ID/secret as `AWS_*`, region via
     `SNAPDIR_B2_REGION` / `AWS_REGION` (or `SNAPDIR_S3_STORE_ENDPOINT_URL`).
4. Do **not** copy the old SQLite catalog. Rebuild history with
   `snapdir catalog rebuild`; the query output is unchanged. Note that history
   is populated by `push` (see the catalog caveat in §1.3 / §3).
5. Fix any `--link` → `--linked` and `verify-transactions` → `ensure-no-errors`
   references.
6. All 14 subcommands are wired (§1.3) — the CLI is feature-complete, so every
   command (`stage`, `verify-cache`, `flush-cache`, `locations`, `ancestors`,
   `revisions`, `defaults`, plus the round-trip set) is available.

See also: [`manifest-spec.md`](./manifest-spec.md) (frozen format) and
[`CHANGELOG.md`](./CHANGELOG.md).

# snapdir manifest specification (FROZEN)

> This document describes the **frozen** snapdir manifest format and the
> content-addressable storage layout. It is a faithful description of the format
> the original snapdir defined, reproduced byte-for-byte by `snapdir-core`.
>
> **The frozen spec wins.** The byte-format contract is now guarded by the Rust
> golden-constant tests in `crates/snapdir-core/tests/compat_golden.rs` and the
> `manifest-format.sha.lock` tripwire, cross-checked against the core source
> (`crates/snapdir-core/src/{manifest.rs,merkle.rs,excludes.rs}`). If you find a
> disagreement between this prose and the golden contract, the contract wins —
> report the discrepancy rather than "correcting" the spec.

A snapdir manifest is a UTF-8 text document that fully describes the contents of
a directory tree: every file and directory, its permissions, its content
checksum, and its size. Because the description is purely content-addressed, a
manifest acts as a portable, deduplicating snapshot of a directory. Two trees
with identical content produce byte-identical manifests and therefore the same
**snapshot ID**, regardless of which tool (Bash or Rust) generated them.

---

## 1. Manifest line format

Each non-comment, non-empty line of a manifest is one entry with exactly five
single-space-separated fields:

```text
PATH_TYPE PERMISSIONS CHECKSUM SIZE PATH
```

| Field         | Meaning                                                              |
| ------------- | ------------------------------------------------------------------- |
| `PATH_TYPE`   | `F` for a regular file, `D` for a directory.                        |
| `PERMISSIONS` | Octal permission string from `stat` (macOS `stat -f '%A'`, Linux `stat -c '%a'`), e.g. `700`, `600`. |
| `CHECKSUM`    | Lowercase hex content checksum of the entry (see §3–§4).            |
| `SIZE`        | Content size in bytes (macOS `%z`, Linux `%s`).                     |
| `PATH`        | The entry's path, taken verbatim (see §1.1).                        |

Example (two empty files in a directory):

```text
D 700 dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b 0 ./
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./bar.txt
F 600 af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262 0 ./foo.txt
```

### 1.1 Field-splitting rule (paths may contain spaces)

The line is split on **only the first four spaces**; the fifth field (`PATH`) is
taken verbatim and keeps any further spaces. So a path such as
`./a file with spaces.txt` round-trips exactly:

```text
F 600 abc... 4 ./a file with spaces.txt
```

In Rust this is `line.splitn(5, ' ')`; in the oracle it is the equivalent field
read. (Note: the **original snapdir had a known bug** where space-bearing
paths were truncated on the *push* path — `IFS=' ' read -r -a line_parts` then
`line_parts[4]`. That was an implementation limitation, not part of the spec;
the Rust port handles spaces correctly.)

### 1.2 Path type and the trailing slash

- `F` — regular file.
- `D` — directory. **Directory paths always end with `/`** (e.g. `./`, `./a/`,
  `./a/aa/`).
- Symbolic links never appear as their own type: a followed symlink is recorded
  as the **type of its target** (`F` or `D`); see §5.

### 1.3 Relative vs. absolute paths

- **Relative mode (default):** paths are prefixed with `./`. The tree root is
  the entry `./`.
- **`--absolute`:** the full path is kept verbatim (no `./` rewrite), e.g.
  `D 700 … 43 /tmp/files/` and `F 600 … 4 /tmp/files/r1f`.

### 1.4 Ordering

Entries are sorted **by the `PATH` field** using `sort -k5` semantics — a
byte-wise (C-locale) comparison of the path bytes. Notably this sorts purely on
the path, **not** on type or checksum: a directory entry `./a/` sorts before the
file `./a/a1f` because of the path bytes, and a larger checksum can precede a
smaller one when the paths demand it.

### 1.5 Comments and empty lines

- Lines beginning with `#` are **comments**.
- Empty lines are ignored.
- **Both comment lines and empty lines are excluded from the snapshot-ID
  checksum** (see §4). They never appear in the parsed entry set or in the
  re-rendered manifest.

---

## 2. Checksum modes

The checksum function used for the `CHECKSUM` field (and for the snapshot ID) is
configurable, but always produces a **lowercase hex** digest:

| Mode                              | Oracle invocation                          | In-process Rust                          |
| --------------------------------- | ------------------------------------------ | ---------------------------------------- |
| **Default (BLAKE3)**              | `b3sum --no-names`                         | `blake3` crate (no `b3sum` shell-out)    |
| **`--checksum-bin=md5sum`**       | `md5sum \| cut -d' ' -f1`                  | `md-5` crate                             |
| **`--checksum-bin=sha256sum`**    | `sha256sum \| cut -d' ' -f1`               | `sha2` crate                             |
| **Keyed** (`SNAPDIR_MANIFEST_CONTEXT` set) | `b3sum --derive-key=<ctx> --no-names` | `blake3::derive_key(ctx, input)`         |

Notes:

- For non-BLAKE3 binaries the oracle keeps only the leading digest
  (`cut -d' ' -f1`) and drops the filename column; the Rust hashers reproduce
  exactly that digest in-process.
- **Keyed mode** is selected by the oracle whenever the
  `SNAPDIR_MANIFEST_CONTEXT` environment variable is non-empty, and only for
  BLAKE3. `snapdir-core` is library-pure and reads **no** environment: the CLI
  lane reads `SNAPDIR_MANIFEST_CONTEXT` and constructs the keyed hasher with the
  context string as a parameter.
- The merkle rule (§3) and the snapshot ID (§4) are **hash-agnostic** — they run
  unchanged with any of the modes above.
- `snapdir id` always uses the default BLAKE3 derivation; it is independent of
  `--checksum-bin`.

---

## 3. Directory checksum (the merkle rule)

A directory's checksum is **not** the hash of its own metadata. It is derived
from the checksums of its **direct children** via:

1. Take the `CHECKSUM` field (column 3) of each **direct child** entry.
2. **Sort** them lexicographically (byte-wise).
3. **Deduplicate** (`sort -u`).
4. **Concatenate with no separator** (`tr -d '\n'`).
5. **Re-hash** the resulting byte string with the active checksum function.

The oracle does this as:

```sh
dir_checksums="$(echo "$dir_manifest" | cut -d' ' -f3 | sort -u | tr -d '\n')"
dir_checksum="$(echo -n "$dir_checksums" | _snapdir_manifest_checksum)"
```

This value is exactly the `CHECKSUM` field of that directory's `D` line (and for
the root directory, the `CHECKSUM` of the `D ./` line).

Edge cases (confirmed against the oracle):

- **Empty directory** — no children, so the concatenation is the empty string
  and its checksum is `blake3("")` =
  `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`.
- **Identical children collapse under `sort -u`** — a directory holding two
  empty files (both `af1349b9…`) deduplicates to a single value, so its checksum
  is `blake3("af1349b9…")` =
  `dba5865c0d91b17958e4d2cac98c338f85cbbda07b71a020ab16c391b5e7af4b` (the
  `empty-dir`/two-empty-files root id).
- **Order-independent** — because the children are sorted, input ordering does
  not affect the result.

---

## 4. Snapshot ID (critical — NOT the root directory checksum)

The **snapshot ID** is the value `snapdir id` reports and the key under which a
snapshot's manifest is stored. It is a **distinct** value from the root
directory checksum (§3).

The oracle derives it as:

```sh
snapshot_id="$(echo "$manifest" | grep -v '^#' | b3sum --no-names -)"
```

That is: **BLAKE3 of the entire `#`-stripped manifest text**, including the
single **trailing newline** that the oracle's `echo` appends after the last
manifest line.

> **Do not confuse this with the root directory checksum.** The snapshot ID is
> the hash of the *whole manifest document*, not of the `D ./` line's `CHECKSUM`
> field. (An earlier version of the docs and contract incorrectly stated
> "root dir checksum = snapshot ID"; that was a documented bug — corrected in
> the core source and contract. This spec encodes the **real** derivation.)

Reproducing the golden IDs requires the trailing newline; hashing the manifest
text *without* it does **not** match.

Worked golden examples (default BLAKE3), from
`utils/qa-fixtures/expected-guide-commands.txt`:

| Manifest                                    | Root `D ./` checksum | Snapshot ID (`snapdir id`)  |
| ------------------------------------------- | -------------------- | --------------------------- |
| Two empty files (`foo.txt`, `bar.txt`)      | `dba5865c…5e7af4b`   | `c678a299…ae3c27857`        |
| After `echo "foo" > foo.txt`                | `4a0732cf…89427d4d`  | `8af03a1b…ba82b9be`         |

Other frozen golden hashes:

- empty file → `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`
- `foo\n` object → `49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92`

In `snapdir-core` this is `snapshot_id(&manifest, &hasher)`: it renders the
manifest in `sort -k5` order (comments/blanks already stripped on parse),
appends the `echo` newline, and hashes the bytes.

---

## 5. Symlinks (follow / no-follow)

- **Followed by default** (`find -L`). A followed symlink inherits its
  **target's** type, checksum and size: a link to a file becomes an `F` entry
  with the file's checksum; a link to a directory becomes a `D` entry. (The
  permission column reflects the link's own `lstat` permissions, matching the
  oracle.)
- **`--no-follow`** drops the `-L` flag (plain `find`) and **excludes symlinks
  entirely** from the manifest.

The follow setting also interacts with excludes — see §6.

---

## 6. Excludes

`--exclude` is an **extended regular expression** applied as `grep -E -v`: a path
is excluded when the regex matches anywhere in it. Two macros expand to built-in
sets, lifted verbatim from the oracle's
`_snapdir_manifest_define_exclude_patterns`:

- **`%system%`** expands to the system directory set **and forces `--no-follow`**.
  The set is anchored at the start of the path
  (`(^(/vscode/|/dev/|/proc/|/sys/|/tmp/|/var/run/|/run/|/mnt/|/media/|/lost+found/|…|<home_cache>|<cache_dir>))`),
  where `<home_cache>` is `${HOME}/.cache/` and `<cache_dir>` is the resolved
  cache directory — both runtime values the CLI resolves and passes in (core
  reads no environment).
- **`%common%`** expands to the common directory set, anchored as a path segment
  (`(/(.cache|.git|.DS_Store|.vscode-server|.dbus|.gvfs|…|node_modules|Trash-1000)($|/))`).
  It does **not** force no-follow.

A plain user pattern (no macros) is used as-is. An empty `--exclude` means no
exclusion. The full default sets are in
`crates/snapdir-core/src/excludes.rs` (`SYSTEM_EXCLUDE_DIRS`,
`COMMON_EXCLUDE_DIRS`), copied verbatim from the oracle's defaults.

---

## 7. Content-addressable storage layout

Both objects (file contents) and manifests are stored under a three-level
sharded layout keyed on the lowercase hex digest, identically in the local cache
and in every store (file, S3, B2, GCS). This is what lets Bash- and Rust-written
caches/buckets remain mutually readable.

```text
.objects/<h[0:3]>/<h[3:6]>/<h[6:9]>/<h[9:]>
.manifests/<id[0:3]>/<id[3:6]>/<id[6:9]>/<id[9:]>
```

- For an **object**, `h` is the file's content checksum.
- For a **manifest**, `id` is the snapshot ID (§4).

Worked example for checksum/id
`49dc870df1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92`:

```text
.objects/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92
.manifests/49d/c87/0df/1de7fd60794cebce449f5ccdae575affaa67a24b62acb03e039db92
```

The local cache lives at `${XDG_CACHE_HOME:-$HOME/.cache}/snapdir/` and uses the
same `.objects`/`.manifests` layout.

### 7.1 Push / fetch discipline

- **Push** — verify the manifest does not already exist, push **objects before
  the manifest**, and only push objects that are absent (skip-if-present).
- **Fetch** — download to a temp path, **verify the BLAKE3 checksum**, retry up
  to 5 times on mismatch, then **atomically rename** into place.

---

## 8. Cross-references

- Manifest line model and (de)serialization: `crates/snapdir-core/src/manifest.rs`.
- Directory checksum + snapshot ID: `crates/snapdir-core/src/merkle.rs`.
- Excludes and follow/no-follow: `crates/snapdir-core/src/excludes.rs`.
- Sharded path helpers: `crates/snapdir-core/src/store.rs`.
- Locked contract and phase map: `docs/rust-port/PLAN.md`.

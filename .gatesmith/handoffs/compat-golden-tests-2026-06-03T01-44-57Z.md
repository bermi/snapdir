# core handoff for compat-golden-tests @ 2026-06-03T01:44:57Z

## Summary

Added a new pure-Rust backwards-compat golden test file
`crates/snapdir-core/tests/compat_golden.rs` with **16 `compat_*` tests** (so
`cargo test compat` selects them). Each asserts against **embedded recorded
constants** only — no live oracle, no shelling out. This is the contract anchor
that replaces the live oracle differential before the Bash oracle is deleted.

Covered sub-cases of the frozen contract:

1. **Manifest line format + sort + comments** — `TYPE PERMISSIONS CHECKSUM SIZE
   PATH`, single space, `sort -k5` path ordering, `#`-comment and blank-line
   exclusion. Parse → Display round-trips byte-identically for the empty-files,
   modified, and the canonical multi-level (`./a/aa/aaa/…`) fixtures.
2. **Directory checksum = BLAKE3 merkle of children** — root `D ./` checksum and
   a nested `./a/aa/aaa/` `D`-line checksum both reproduced via the public
   `directory_checksum` (sort -u + concat + re-hash of child checksums), plus the
   identical-children dedup case.
3. **Snapshot id = BLAKE3 of `#`-stripped manifest text** (trailing newline
   included), via public `snapshot_id`. Pins three recorded `(manifest → id)`
   pairs; guards that the id is NOT the root directory checksum and ignores
   comments.
4. **Sharded keys** — `store::object_path` / `store::manifest_path` produce the
   frozen `.objects/<h0:3>/<h3:6>/<h6:9>/<h9:>` and `.manifests/<id…>` layout for
   recorded `(hash/id → path)` pairs.
5. **Checksum modes** — golden vectors for md5 (`Md5Hasher`), sha256
   (`Sha256Hasher`), and keyed-BLAKE3 (`Blake3KeyedHasher`, the
   `SNAPDIR_MANIFEST_CONTEXT` derive-key mode); plus a hash-agnostic check that
   the merkle rule and snapshot id run unchanged under MD5. **All three checksum
   modes are reachable via the public API** — none skipped.

**Constants reused** from `crates/snapdir-core/tests/golden_b3sum.rs`: the
empty-files + modified guide manifests, their dir checksums (`dba5865c…`,
`4a0732cf…`), snapshot ids (`c678a299…`, `8af03a1b…`), and the file content
checksums (`af1349b9…`, `49dc870d…`). Added: the multi-level `b3sum` fixture
(from `snapdir-manifest`'s own suite, already embedded in `manifest.rs` tests)
with its oracle-derived snapshot id `10ff7d9a…` and `./a/aa/aaa/` `D`-line
checksum `8aed4caf…`. Sharded-path expectations cross-checked against
`utils/qa-fixtures/expected-guide-commands.txt` (lines 8-10, 22, 25).

Only the public `snapdir-core` API is used (`Manifest`, `ManifestEntry`/`PathType`,
`directory_checksum`, `snapshot_id`, `Hasher` + `Blake3Hasher`/`Blake3KeyedHasher`/
`Md5Hasher`/`Sha256Hasher`, `store::object_path`/`manifest_path`).

## Files changed

```
?? crates/snapdir-core/tests/compat_golden.rs   (new file; only core change)
```

`git diff --stat` shows no tracked-file modifications under `crates/`. The other
working-tree entries (`.claude/ralph-loop.local.md`, `.gatesmith/gates.yaml`,
`.gatesmith/journal.md`) are pre-existing PM/orchestration state, untouched by
this lane.

## Local verification result

```
$ cargo test -p snapdir-core --locked compat
running 16 tests
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --workspace --locked compat   # EXIT=0
     Running tests/compat_golden.rs
running 16 tests
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo clippy -p snapdir-core --tests
    Finished `dev` profile ...   (no warnings, no errors)
```

## Reuse check / Blockers

- **SHA-locked src untouched** — `shasum -a 256 -c .gatesmith/manifest-format.sha.lock`:
  ```
  crates/snapdir-core/src/manifest.rs: OK
  crates/snapdir-core/src/merkle.rs: OK
  crates/snapdir-core/src/excludes.rs: OK
  ```
- No shelling to `b3sum`/`md5sum`/`sha256sum`; all hashing is in-process via the
  public `Hasher` impls (the `blake3` crate is used only in the test's keyed
  cross-check, matching how `golden_b3sum.rs` already does it). No I/O, no env
  reads, no filesystem walk.
- **No checksum mode skipped** — md5, sha256, and keyed-BLAKE3 are all reachable
  via the public `snapdir-core` API and pinned with recorded goldens.
- Did not edit the oracle, `utils/qa-fixtures/`, the frozen src, or other crates.
  Did not commit.

Ready for PM verification: YES

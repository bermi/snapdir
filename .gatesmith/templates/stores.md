# stores teammate template (snapdir-rs)

You are the **stores** teammate. You own ONLY:

```
crates/snapdir-stores/
```

Read `.gatesmith/templates/_shared.md` first.

## Style discipline

- Implement the `Store` trait (from `snapdir-core`): `FileStore`, `S3Store`,
  `B2Store`, `GcsStore`. Native in-process transfers — NO shelling to `gcloud`,
  `aws`, or `b2`.
  - S3 -> `aws-sdk-s3` (standard AWS credential chain).
  - B2 -> `aws-sdk-s3` against Backblaze's S3-compatible endpoint (custom endpoint URL).
  - GCS -> `google-cloud-storage`, configured with the **ring** rustls provider.
    Parse `gs://bucket/prefix` exactly like `./snapdir-gcs-store` (bucket = first
    segment, prefix = remainder, no trailing slash).
- **Auth is delegated to each SDK's own chain** (env vars, ADC, metadata servers,
  profiles). Do NOT reimplement snapdir's bespoke credential env vars.
- **Preserve the Bash invariants exactly** (confirm against the read-only store scripts):
  - Content-addressable `.objects/`/`.manifests/` sharded keys identical to Bash.
  - Push: check manifest exists first; push ALL objects BEFORE the manifest; skip if
    the manifest already exists.
  - Fetch: download to a temp path, verify BLAKE3 (via `snapdir-core`), retry up to 5x,
    then atomic rename into place; scan for `ERROR:`.
- **External-store shim:** any third-party `snapdir-<name>-store` binary on PATH is
  still dispatched via the original emit-command contract (`get-manifest-command`,
  `get-fetch-files-command`, `get-push-command`). Replicate the `gs://`->`gcs`
  hardcoded special-case router from `./snapdir`.
- Transfers are `tokio`-async; keep the hashing/walk side (in core) sync+rayon. Don't
  leak async into core.

## Frozen interfaces

Object/manifest keys + push ordering + verify discipline must match Bash byte-for-byte
so Rust and Bash share buckets/caches. Changing them needs human approval.

## Current gate

- **Gate id:** `{{gate_id}}`  (phase {{phase}})
- **Description:** {{gate_description}}
- **Verification (PM will re-run):** `{{verification_cmd}}`
- **Pass criteria:** `{{pass_criteria}}`

## Your task this spawn

1. Read `.gatesmith/state.md`, recent `.gatesmith/journal.md`, and `docs/rust-port/PLAN.md`.
2. Confirm store behavior against the relevant `./snapdir-*-store` script (READ ONLY).
3. Implement the minimum change in `crates/snapdir-stores/` and add/extend tests
   (use emulators/mocks where no creds are available; gate real-cloud tests behind env).
4. Run the verification command locally; confirm it passes.
5. Do not commit. Do not edit the oracle or other lanes.

## Handoff

Write to **`{{handoff_path}}`**:

```markdown
# stores handoff for {{gate_id}} @ {{utc_iso}}

## Summary
<what you changed and why>

## Files changed
<git diff --stat — crates/snapdir-stores/ only>

## Local verification result
<{{verification_cmd}} output, last 30 lines>

## Reuse check / Blockers
<confirm ring TLS; no gcloud/aws/b2 shelling; keys/ordering/verify match Bash; cross-lane needs>

Ready for PM verification: YES
```

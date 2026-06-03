# 0009 — Classify GCS missing objects via 404 and service NotFound

Status: Accepted, 2026-06

## Context

Push is skip-if-present: before uploading an object or manifest, the store checks
whether it already exists. For GCS, `GcsStore::key_exists`/`get_bytes` originally treated
only an HTTP 404 as "absent". With `google-cloud-storage` v1.12, a missing object is
reported as a service-level `Code::NotFound` whose `http_status_code()` is `None` — it
carries no HTTP status. The env-gated unit test never ran without credentials, so this
was not caught until a live run.

## Decision

Classify a GCS object as absent when **either** an HTTP 404 **or** the service-level
`Code::NotFound` is returned. Introduce an `is_not_found()` helper that checks both,
mirroring the S3 store's `is_not_found()`. `google-cloud-gax` is named directly (pinned
to its exact transitive version, `default-features = false` so it adds nothing to the
graph) only to reach `Error::status()` → `rpc::Code` for this classification.

## Alternatives considered

- **HTTP-404-only classification** (the original behaviour). Rejected: it misclassifies
  the SDK's service-level `NotFound` (no HTTP status) as an error, so the skip-if-present
  check aborts every real GCS push before any upload.
- **Treat any error on the existence check as "absent".** Rejected: would mask real
  errors (auth, network) and could upload over or past genuine failures.

## Consequences

- Real GCS pushes work; the skip-if-present check no longer aborts uploads.
- A live integration test (against a real bucket) now exercises this path, since the
  bug was invisible to credential-less unit tests.
- The fix established the two-pronged classification pattern shared with S3.

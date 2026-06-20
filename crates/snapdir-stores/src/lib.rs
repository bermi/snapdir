//! snapdir stores library.
//!
//! Storage backends for snapdir snapshots plus the store-routing and
//! external-store shim that implement snapdir's store dispatch:
//!
//! - [`FileStore`] — the in-process `file://` backend.
//! - [`S3Store`] — the native AWS-SDK `s3://` backend (ring rustls).
//! - [`B2Store`] — the native AWS-SDK `b2://` backend, pointed at Backblaze
//!   B2's S3-compatible endpoint (wraps [`S3Store`] with a custom endpoint).
//! - [`GcsStore`] — the native `google-cloud-storage` `gs://` backend
//!   (ring rustls; ADC credential chain).
//! - [`router`] — scheme → adapter resolution, including the hardcoded
//!   `gs://`→`gcs` special case for the Google Cloud Storage adapter.
//! - [`shim`] ([`ExternalStore`]) — the emit-command shim that dispatches
//!   third-party `snapdir-<name>-store` binaries via the documented
//!   `get-manifest-command` / `get-fetch-files-command` / `get-push-command`
//!   contract.
//! - [`limits`] ([`BackendLimits`], [`for_scheme`]) — the published per-backend
//!   request-rate / bandwidth caps (rate caps only; no retry policy) keyed by
//!   storage scheme, used to pace transfers under each provider's documented
//!   limits.
//! - [`transfer`] ([`TransferConfig`], [`RateLimiter`], [`run_concurrent`]) —
//!   the concurrency + bandwidth-limiting foundation each store carries via a
//!   [`TransferConfig`] for the (later) concurrent transfer loops.
//! - [`retry`] ([`RetryPolicy`], [`retry_async`], [`retry_blocking`]) — a pure,
//!   injectable full-jitter exponential-backoff engine (no real clock/sleep of
//!   its own; the [`Jitter`] source and [`AsyncSleeper`]/[`BlockingSleeper`] are
//!   injected). SDK-agnostic over an [`Attempt`] outcome; wiring into the
//!   S3/GCS/B2 call sites is a later gate.
//! - [`adaptive`] ([`AdaptiveGate`], [`AdaptiveController`]) — pure, injectable
//!   adaptive control: a resizable concurrency permit pool (async + blocking)
//!   plus a deterministic slow-start/AIMD controller that turns injected op
//!   samples + system metrics into a concurrency limit and target byte-rate
//!   (wiring into the live transfer loops is a later gate).
//! - [`stream`] ([`StreamStore`]) — object/manifest-level, content-addressed,
//!   verified read/write primitives (the foundation for store-to-store sync),
//!   implemented for [`FileStore`], [`S3Store`], [`GcsStore`], and [`B2Store`].
//! - [`pack`] ([`write_pack`], [`read_pack`], [`PackSink`]) — the SNAPPACK 1
//!   wire stream behind the `ssh://` acceleration plumbing
//!   (`snapdir send-pack | ssh … 'snapdir receive-pack'`): `obj` records
//!   stream through incremental BLAKE3 verification (O(1) memory into a
//!   [`FileSink`]), the manifest rides last and commits only after the `end`
//!   trailer, so truncation can never publish a snapshot.
//! - [`fsync`] ([`barrier_objects`](fsync::barrier_objects),
//!   [`writeout_hint`](fsync::writeout_hint)) — the batched crash-durability
//!   primitives behind the receive-pack path: a cheap per-object writeout hint
//!   while filing, then exactly two full syncs per pack (one object barrier
//!   before the manifest, one durable manifest commit), so a present manifest
//!   implies present, on-disk objects even across power loss. cfg-gated over
//!   `libc` (no new lock crate); env-free.
//! - [`sync`] ([`sync_snapshot`], [`SyncReport`]) — streaming store-to-store
//!   snapshot copy: walks a source manifest and copies its raw objects
//!   source → dest through memory only (no local filesystem staging),
//!   parallelized across a rayon pool and throttled by a
//!   [`BlockingRateLimiter`](transfer::BlockingRateLimiter); writes the manifest
//!   last (all-or-nothing).

pub mod adaptive;
pub mod b2_store;
pub(crate) mod fetch;
pub mod file_store;
pub mod fsync;
pub mod gcs_store;
pub mod limits;
pub mod pack;
pub(crate) mod push;
pub mod retry;
pub mod router;
pub mod s3_store;
pub mod shim;
pub mod split;
pub mod stream;
pub mod sync;
pub mod transfer;
pub(crate) mod util;

pub use adaptive::{
    p95_object_size, AdaptiveController, AdaptiveGate, AdaptivePolicy, ControllerDriver, Decision,
    OpResult, OpSample,
};
pub use b2_store::B2Store;
pub use file_store::{clonefile_hits, cow_reflink_supported, FileStore, MaterializeMode};
pub use gcs_store::{GcsLocation, GcsStore};
pub use limits::{for_scheme, BackendLimits};
pub use pack::{
    is_hex64, read_pack, write_pack, write_pack_with_format, Durability, FileSink, PackFormat,
    PackReadReport, PackSink, PackWriteReport, StreamSink, DEFAULT_ZSTD_LEVEL, MAX_HEADER_BYTES,
    MAX_MANIFEST_BYTES, MAX_ZSTD_LEVEL, MIN_ZSTD_LEVEL, WIRE_CAPS, WIRE_MAGIC, WIRE_MAGIC_ZSTD,
    WIRE_VERSION,
};
pub use retry::{
    parse_retry_after, retry_async, retry_blocking, retry_network, AsyncSleeper, Attempt,
    BlockingSleeper, DefaultJitter, FixedJitter, Jitter, RetryPolicy, ThreadSleeper, TokioSleeper,
};
pub use router::{resolve_adapter, Adapter, RouteError};
pub use s3_store::{S3Location, S3Store};
pub use shim::ExternalStore;
pub use split::SplitStore;
pub use stream::StreamStore;
pub use sync::{sync_snapshot, sync_snapshot_mirror, MirrorReport, SyncReport};
pub use transfer::{
    classify_error, run_adaptive, run_concurrent, AdaptivePolicy as TransferAdaptivePolicy,
    BlockingRateLimiter, RateLimiter, TransferConfig,
};

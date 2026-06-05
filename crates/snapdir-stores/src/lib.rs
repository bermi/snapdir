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
//! - [`transfer`] ([`TransferConfig`], [`RateLimiter`], [`run_concurrent`]) —
//!   the concurrency + bandwidth-limiting foundation each store carries via a
//!   [`TransferConfig`] for the (later) concurrent transfer loops.
//! - [`adaptive`] ([`AdaptiveGate`], [`AdaptiveController`]) — pure, injectable
//!   adaptive control: a resizable concurrency permit pool (async + blocking)
//!   plus a deterministic slow-start/AIMD controller that turns injected op
//!   samples + system metrics into a concurrency limit and target byte-rate
//!   (wiring into the live transfer loops is a later gate).
//! - [`stream`] ([`StreamStore`]) — object/manifest-level, content-addressed,
//!   verified read/write primitives (the foundation for store-to-store sync),
//!   implemented for [`FileStore`], [`S3Store`], [`GcsStore`], and [`B2Store`].
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
pub mod gcs_store;
pub(crate) mod push;
pub mod router;
pub mod s3_store;
pub mod shim;
pub mod stream;
pub mod sync;
pub mod transfer;
pub(crate) mod util;

pub use adaptive::{
    p95_object_size, AdaptiveController, AdaptiveGate, AdaptivePolicy, ControllerDriver, Decision,
    OpResult, OpSample,
};
pub use b2_store::B2Store;
pub use file_store::FileStore;
pub use gcs_store::{GcsLocation, GcsStore};
pub use router::{resolve_adapter, Adapter, RouteError};
pub use s3_store::{S3Location, S3Store};
pub use shim::ExternalStore;
pub use stream::StreamStore;
pub use sync::{sync_snapshot, SyncReport};
pub use transfer::{
    classify_error, run_adaptive, run_concurrent, AdaptivePolicy as TransferAdaptivePolicy,
    BlockingRateLimiter, RateLimiter, TransferConfig,
};

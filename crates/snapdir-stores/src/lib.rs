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

pub mod b2_store;
pub(crate) mod fetch;
pub mod file_store;
pub mod gcs_store;
pub(crate) mod push;
pub mod router;
pub mod s3_store;
pub mod shim;
pub mod transfer;
pub(crate) mod util;

pub use b2_store::B2Store;
pub use file_store::FileStore;
pub use gcs_store::{GcsLocation, GcsStore};
pub use router::{resolve_adapter, Adapter, RouteError};
pub use s3_store::{S3Location, S3Store};
pub use shim::ExternalStore;
pub use transfer::{run_concurrent, RateLimiter, TransferConfig};

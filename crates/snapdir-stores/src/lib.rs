//! snapdir stores library.
//!
//! Storage backends for snapdir snapshots plus the store-routing and
//! external-store shim that mirror the Bash oracle's dispatch:
//!
//! - [`FileStore`] — the in-process `file://` backend.
//! - [`S3Store`] — the native AWS-SDK `s3://` backend (ring rustls).
//! - [`B2Store`] — the native AWS-SDK `b2://` backend, pointed at Backblaze
//!   B2's S3-compatible endpoint (wraps [`S3Store`] with a custom endpoint).
//! - [`router`] — scheme → adapter resolution, including the hardcoded
//!   `gs://`→`gcs`/`snapdir-gcs-store` special case from `./snapdir`.
//! - [`shim`] ([`ExternalStore`]) — the emit-command shim that dispatches
//!   third-party `snapdir-<name>-store` binaries via the documented
//!   `get-manifest-command` / `get-fetch-files-command` / `get-push-command`
//!   contract.
//!
//! The native-SDK GCS store lands in a later gate.

pub mod b2_store;
pub mod file_store;
pub mod router;
pub mod s3_store;
pub mod shim;

pub use b2_store::B2Store;
pub use file_store::FileStore;
pub use router::{resolve_adapter, Adapter, RouteError};
pub use s3_store::{S3Location, S3Store};
pub use shim::ExternalStore;

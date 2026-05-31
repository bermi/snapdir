//! snapdir stores library.
//!
//! Storage backends for snapdir snapshots plus the store-routing and
//! external-store shim that mirror the Bash oracle's dispatch:
//!
//! - [`FileStore`] — the in-process `file://` backend.
//! - [`router`] — scheme → adapter resolution, including the hardcoded
//!   `gs://`→`gcs`/`snapdir-gcs-store` special case from `./snapdir`.
//! - [`shim`] ([`ExternalStore`]) — the emit-command shim that dispatches
//!   third-party `snapdir-<name>-store` binaries via the documented
//!   `get-manifest-command` / `get-fetch-files-command` / `get-push-command`
//!   contract.
//!
//! The native-SDK S3/B2/GCS stores land in a later gate.

pub mod file_store;
pub mod router;
pub mod shim;

pub use file_store::FileStore;
pub use router::{resolve_adapter, Adapter, RouteError};
pub use shim::ExternalStore;

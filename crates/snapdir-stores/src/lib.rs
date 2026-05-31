//! snapdir stores library.
//!
//! `FileStore` plus native-SDK S3/B2/GCS stores and the external-store shim.
//! Only [`FileStore`] (the `file://` backend) is implemented so far; the
//! network stores and shim land in later gates.

pub mod file_store;

pub use file_store::FileStore;

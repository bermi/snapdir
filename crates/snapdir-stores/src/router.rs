//! Store routing: scheme → adapter / binary-name resolution.
//!
//! Implements the frozen `_snapdir_get_store_bin_path` dispatch: a `--store`
//! URL's protocol (the text before the first `:`) selects the storage adapter.
//! The protocol must be lowercase alphanumeric (`grep -q "^[a-z0-9]*$"`); a
//! protocol of `gs` is a **hardcoded special case** routed to the `gcs` adapter
//! (scheme `gs`), and every other protocol `<proto>` routes to an adapter named
//! `<proto>` (binary `snapdir-<proto>-store`).
//!
//! ```text
//! gs://bucket/x   -> adapter "gcs"   (special case)        in-process
//! s3://bucket/x   -> adapter "s3"                          in-process
//! b2://bucket/x   -> adapter "b2"                          in-process
//! file:///x       -> adapter "file"                        in-process
//! foo://bar       -> adapter "foo"  (external/3rd-party)   snapdir-foo-store
//! ```
//!
//! The Rust port ships the `file`, `s3`, `b2`, and `gcs` adapters in-process;
//! any other (third-party) adapter is dispatched out-of-process to a
//! `snapdir-<name>-store` binary on `PATH` via the emit-command shim
//! ([`crate::shim`]). This module only *resolves* the route; it performs no
//! I/O and does not spawn anything.

use thiserror::Error;

/// Errors produced while resolving a store URL to an adapter.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum RouteError {
    /// The store URL had no protocol/scheme (no text before the first `:`),
    /// or the protocol contained characters outside `[a-z0-9]`.
    ///
    /// Mirrors the oracle's `grep -q "^[a-z0-9]*$"` rejection
    /// (`Invalid store protocol: '<proto>'`).
    #[error(
        "invalid store protocol: '{protocol}': expected a URI like \
         file://<path> for a local store, or <scheme>://… for an external \
         snapdir-<scheme>-store helper"
    )]
    InvalidProtocol {
        /// The offending protocol text extracted from the store URL.
        protocol: String,
    },
}

/// Which snapdir storage adapter a store URL resolves to.
///
/// The four named variants are the adapters shipped **in-process** by the Rust
/// port (no subprocess). [`Adapter::External`] is any third-party adapter,
/// dispatched out-of-process to a `snapdir-<name>-store` binary via the
/// emit-command shim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adapter {
    /// Built-in `file://` backend (local directory).
    File,
    /// Built-in `s3://` backend (native AWS SDK).
    S3,
    /// Built-in `b2://` backend (native AWS SDK against Backblaze).
    B2,
    /// Built-in `gs://` backend (native Google Cloud Storage SDK). Scheme `gs`,
    /// adapter name `gcs`.
    Gcs,
    /// A third-party adapter resolved to a `snapdir-<name>-store` binary on
    /// `PATH`. `name` is the adapter name (the store protocol verbatim).
    External {
        /// The adapter name (equal to the store URL's protocol).
        name: String,
    },
}

impl Adapter {
    /// The adapter's canonical name (`gs` resolves to `gcs`, matching the
    /// oracle's special case).
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Adapter::File => "file",
            Adapter::S3 => "s3",
            Adapter::B2 => "b2",
            Adapter::Gcs => "gcs",
            Adapter::External { name } => name,
        }
    }

    /// The `snapdir-<name>-store` binary this adapter corresponds to.
    ///
    /// For the built-in adapters this is the helper binary the original
    /// implementation would have shelled out to (one per `file`/`s3`/`b2`
    /// adapter, plus the `gcs` adapter via the `gs`→`gcs` special case). The
    /// Rust port serves the built-ins in-process and only spawns the binary for
    /// [`Adapter::External`].
    #[must_use]
    pub fn store_binary(&self) -> String {
        format!("snapdir-{}-store", self.name())
    }

    /// Whether this adapter is served in-process by the Rust port (`true`) or
    /// dispatched to a third-party binary via the shim (`false`).
    #[must_use]
    pub fn is_builtin(&self) -> bool {
        !matches!(self, Adapter::External { .. })
    }
}

/// Extracts the protocol (scheme) from a store URL: the text before the first
/// `:`, validated against `^[a-z0-9]*$` like the oracle.
///
/// # Errors
///
/// Returns [`RouteError::InvalidProtocol`] if the protocol contains any
/// character outside `[a-z0-9]` (this also rejects an empty protocol from a
/// URL like `://x` and a missing-colon URL, whose whole text becomes the
/// candidate protocol and almost always contains an illegal character).
pub fn store_protocol(store_url: &str) -> Result<&str, RouteError> {
    // `cut -d':' -f1`: everything up to (not incl.) the first ':'. If there is
    // no ':', `cut` returns the whole string, which is then validated.
    let proto = store_url.split(':').next().unwrap_or("");
    if proto.is_empty()
        || !proto
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(RouteError::InvalidProtocol {
            protocol: proto.to_owned(),
        });
    }
    Ok(proto)
}

/// Resolves a store URL to the [`Adapter`] that should serve it.
///
/// Implements `_snapdir_get_store_bin_path`'s protocol dispatch, including the
/// hardcoded `gs`→`gcs` special case for the Google Cloud Storage adapter.
///
/// # Errors
///
/// Returns [`RouteError::InvalidProtocol`] if the store URL's protocol is not
/// a non-empty `[a-z0-9]` string (see [`store_protocol`]).
///
/// # Examples
///
/// ```
/// use snapdir_stores::router::{resolve_adapter, Adapter};
///
/// // gs:// is the hardcoded special case -> the "gcs" adapter
/// let gcs = resolve_adapter("gs://bucket/x").unwrap();
/// assert_eq!(gcs, Adapter::Gcs);
/// assert_eq!(gcs.name(), "gcs");
/// assert_eq!(gcs.store_binary(), format!("snapdir-{}-store", "gcs"));
///
/// assert_eq!(
///     resolve_adapter("s3://b/x").unwrap().store_binary(),
///     format!("snapdir-{}-store", "s3"),
/// );
/// assert_eq!(resolve_adapter("file:///x").unwrap(), Adapter::File);
/// ```
pub fn resolve_adapter(store_url: &str) -> Result<Adapter, RouteError> {
    let proto = store_protocol(store_url)?;
    Ok(match proto {
        // Hardcoded special case: the `gs` scheme maps to the `gcs` adapter.
        "gs" => Adapter::Gcs,
        "file" => Adapter::File,
        "s3" => Adapter::S3,
        "b2" => Adapter::B2,
        other => Adapter::External {
            name: other.to_owned(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shim_router_gs_is_special_cased_to_gcs() {
        let a = resolve_adapter("gs://bucket/x").unwrap();
        assert_eq!(a, Adapter::Gcs);
        assert_eq!(a.name(), "gcs");
        assert_eq!(a.store_binary(), format!("snapdir-{}-store", "gcs"));
        assert!(a.is_builtin());
    }

    #[test]
    fn shim_router_s3_resolves_to_s3_store() {
        let a = resolve_adapter("s3://bucket/path/to/dir").unwrap();
        assert_eq!(a, Adapter::S3);
        assert_eq!(a.store_binary(), format!("snapdir-{}-store", "s3"));
        assert!(a.is_builtin());
    }

    #[test]
    fn shim_router_b2_resolves_to_b2_store() {
        let a = resolve_adapter("b2://bucket/x").unwrap();
        assert_eq!(a, Adapter::B2);
        assert_eq!(a.store_binary(), format!("snapdir-{}-store", "b2"));
    }

    #[test]
    fn shim_router_file_is_builtin() {
        let a = resolve_adapter("file:///long/term/storage/").unwrap();
        assert_eq!(a, Adapter::File);
        assert_eq!(a.store_binary(), format!("snapdir-{}-store", "file"));
        assert!(a.is_builtin());
    }

    #[test]
    fn shim_router_unknown_protocol_is_external_binary() {
        let a = resolve_adapter("mock://bucket/x").unwrap();
        assert_eq!(
            a,
            Adapter::External {
                name: "mock".to_owned()
            }
        );
        assert_eq!(a.name(), "mock");
        assert_eq!(a.store_binary(), "snapdir-mock-store");
        assert!(!a.is_builtin());
    }

    #[test]
    fn shim_router_numeric_protocols_are_allowed() {
        // The oracle's filter is `^[a-z0-9]*$`, so digits are legal (e.g. s3, b2).
        assert_eq!(store_protocol("s3://b").unwrap(), "s3");
        assert_eq!(store_protocol("b2://b").unwrap(), "b2");
        assert_eq!(store_protocol("0a1://b").unwrap(), "0a1");
    }

    #[test]
    fn shim_router_rejects_invalid_protocols() {
        // Uppercase, missing scheme, and punctuation are all rejected by the
        // oracle's `^[a-z0-9]*$` filter.
        assert_eq!(
            resolve_adapter("S3://b"),
            Err(RouteError::InvalidProtocol {
                protocol: "S3".to_owned()
            })
        );
        assert!(matches!(
            resolve_adapter("://b"),
            Err(RouteError::InvalidProtocol { .. })
        ));
        assert!(matches!(
            resolve_adapter("/just/a/path"),
            Err(RouteError::InvalidProtocol { .. })
        ));
    }

    #[test]
    fn shim_router_protocol_is_text_before_first_colon() {
        // `cut -d':' -f1` semantics: stop at the first colon even with more.
        assert_eq!(store_protocol("gs://bucket/a:b:c").unwrap(), "gs");
    }
}

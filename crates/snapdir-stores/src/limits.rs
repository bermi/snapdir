//! Per-backend rate-limit table (RATE CAPS ONLY).
//!
//! [`BackendLimits`] carries the published per-backend request-rate and
//! bandwidth ceilings for each storage scheme so the transfer layer can pace
//! requests/bytes to stay under the backend's documented limits and avoid
//! provider-side throttling (HTTP 429 / `SlowDown` / `rateLimitExceeded`).
//!
//! This module is deliberately **self-contained**: it holds caps only and has
//! NO notion of retry/backoff (that policy is global and lives in a separate
//! module added by a later gate). It depends on nothing beyond `std` and the
//! scheme strings the [`router`](crate::router) already uses.
//!
//! The caps are matched on the canonical scheme/adapter names produced by
//! [`crate::router::Adapter::name`] — `"file"`, `"s3"`, `"b2"`, `"gcs"` (the
//! `gs://` URL scheme resolves to the `"gcs"` adapter) — plus any unknown /
//! external scheme, which is treated as unlimited.

/// Published rate caps for a single storage backend.
///
/// Every field is a hard ceiling expressed per second; `None` means "no
/// documented limit" (the transfer layer leaves that dimension unthrottled).
/// These are **rate caps only** — there is intentionally no retry/backoff
/// field here (retry policy is global and lives elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendLimits {
    /// Max GET/HEAD requests per second (`None` = unlimited).
    pub read_rps: Option<u64>,
    /// Max PUT requests per second (`None` = unlimited).
    pub write_rps: Option<u64>,
    /// Max download bytes per second (`None` = unlimited).
    pub read_bps: Option<u64>,
    /// Max upload bytes per second (`None` = unlimited).
    pub write_bps: Option<u64>,
}

impl BackendLimits {
    /// The fully-unlimited limit set (every dimension `None`). Used for the
    /// `file` backend and any unknown / external scheme.
    pub const UNLIMITED: BackendLimits = BackendLimits {
        read_rps: None,
        write_rps: None,
        read_bps: None,
        write_bps: None,
    };
}

/// Returns the published [`BackendLimits`] for a storage `scheme`.
///
/// `scheme` is the canonical adapter name from
/// [`crate::router::Adapter::name`] (`"file"`, `"s3"`, `"b2"`, `"gcs"`); the
/// `gs://` URL scheme is also accepted as an alias for `"gcs"`. Any other
/// (unknown / third-party / external) scheme — including `"file"` — is treated
/// as unlimited.
#[must_use]
pub fn for_scheme(scheme: &str) -> BackendLimits {
    match scheme {
        // AWS S3: >=5,500 read and >=3,500 write requests/sec per prefix; no
        // documented per-prefix bandwidth cap.
        // https://docs.aws.amazon.com/AmazonS3/latest/userguide/optimizing-performance.html
        "s3" => BackendLimits {
            read_rps: Some(5500),
            write_rps: Some(3500),
            read_bps: None,
            write_bps: None,
        },
        // Google Cloud Storage: initial ~5,000 read and ~1,000 write
        // requests/sec per bucket (autoscales upward); no documented bandwidth
        // cap. 429 rateLimitExceeded is retryable.
        // https://docs.cloud.google.com/storage/docs/request-rate
        "gcs" | "gs" => BackendLimits {
            read_rps: Some(5000),
            write_rps: Some(1000),
            read_bps: None,
            write_bps: None,
        },
        // Backblaze B2 (<=10TB accounts), per account: download 1,200 req/min
        // = 20 req/s & 200Mbit/s = 25MB/s; upload 3,000 req/min = 50 req/s &
        // 800Mbit/s = 100MB/s.
        // https://www.backblaze.com/docs/cloud-storage-rate-limits
        "b2" => BackendLimits {
            read_rps: Some(20),
            write_rps: Some(50),
            read_bps: Some(25 * 1024 * 1024),
            write_bps: Some(100 * 1024 * 1024),
        },
        // Local filesystem + any unknown / external backend: no rate caps.
        _ => BackendLimits::UNLIMITED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::RateLimiter;
    use std::time::Duration;

    /// Builds a current-thread tokio runtime with time enabled (mirrors the
    /// `RateLimiter` unit-test harness in `transfer.rs`).
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build tokio runtime")
    }

    #[test]
    fn limits_for_scheme_file_is_unlimited() {
        assert_eq!(for_scheme("file"), BackendLimits::UNLIMITED);
        let l = for_scheme("file");
        assert_eq!(l.read_rps, None);
        assert_eq!(l.write_rps, None);
        assert_eq!(l.read_bps, None);
        assert_eq!(l.write_bps, None);
    }

    #[test]
    fn limits_for_scheme_unknown_is_unlimited() {
        // External / third-party schemes carry no documented caps.
        assert_eq!(for_scheme("mock"), BackendLimits::UNLIMITED);
        assert_eq!(for_scheme("azure"), BackendLimits::UNLIMITED);
        assert_eq!(for_scheme(""), BackendLimits::UNLIMITED);
    }

    #[test]
    fn limits_for_scheme_s3() {
        let l = for_scheme("s3");
        assert_eq!(l.read_rps, Some(5500));
        assert_eq!(l.write_rps, Some(3500));
        assert_eq!(l.read_bps, None);
        assert_eq!(l.write_bps, None);
    }

    #[test]
    fn limits_for_scheme_gcs() {
        let expected = BackendLimits {
            read_rps: Some(5000),
            write_rps: Some(1000),
            read_bps: None,
            write_bps: None,
        };
        assert_eq!(for_scheme("gcs"), expected);
        // The `gs://` URL scheme is an accepted alias for the `gcs` adapter.
        assert_eq!(for_scheme("gs"), expected);
    }

    #[test]
    fn limits_for_scheme_b2() {
        let l = for_scheme("b2");
        assert_eq!(l.read_rps, Some(20));
        assert_eq!(l.write_rps, Some(50));
        assert_eq!(l.read_bps, Some(25 * 1024 * 1024));
        assert_eq!(l.write_bps, Some(100 * 1024 * 1024));
    }

    /// An unlimited request limiter (`None`) is a no-op: acquiring many request
    /// tokens returns essentially instantly.
    #[test]
    fn limits_request_limiter_unlimited_is_noop() {
        let rt = runtime();
        rt.block_on(async {
            let limiter = RateLimiter::new(None);
            let start = tokio::time::Instant::now();
            for _ in 0..1000 {
                limiter.acquire(1).await; // one token == one request
            }
            assert!(
                start.elapsed() < Duration::from_millis(200),
                "unlimited request limiter must not pace requests"
            );
        });
    }

    /// A request limiter built from a req/s rate paces requests: with the
    /// "tokens are requests" mapping, `acquire(1)` per request, issuing 2x one
    /// second's burst must wait ~1s for the bucket to refill. Mirrors the
    /// `transfer::tests::transfer_config_rate_limiter` pacing pattern, but the
    /// budget unit is requests, not bytes.
    #[test]
    fn limits_request_limiter_paces_requests() {
        let rt = runtime();
        rt.block_on(async {
            // 5 requests/sec. The bucket starts full (5), so the first 5
            // `acquire(1)` calls are free; the next 5 must wait ~1s to refill.
            let rps = 5;
            let limiter = RateLimiter::new(Some(rps));
            let start = tokio::time::Instant::now();
            for _ in 0..(rps * 2) {
                limiter.acquire(1).await; // one token == one request
            }
            let elapsed = start.elapsed();
            assert!(
                elapsed >= Duration::from_millis(900),
                "a request limiter fed {rps} req/s should pace 2x burst to ~1s, took {elapsed:?}"
            );
        });
    }
}

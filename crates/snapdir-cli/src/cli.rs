//! clap-derive command surface for the `snapdir` binary.
//!
//! The surface reproduces the original `snapdir` Bash command surface exactly:
//! 14 subcommands plus `version`, and the global options shared across commands.
//! Pinned to the original script behavior, not the docs (e.g. `--linked`, never
//! `--link`).
//!
//! `manifest`/`id` are wired to `snapdir-core`'s in-process walk and emit
//! oracle-identical stdout; `push`/`fetch`/`pull`/`checkout`/`verify` are wired
//! to `snapdir-stores`; `stage`/`verify-cache`/`flush-cache` are wired to
//! `snapdir-core::cache` (the cache is itself a `file://`-shaped store); and the
//! catalog queries (`locations`/`ancestors`/`revisions`) are wired to
//! `snapdir-catalog`, emitting the frozen CLI-compat JSON lines; and `defaults`
//! prints the effective default settings + environment, mirroring the oracle's
//! `snapdir_defaults`. All 14 subcommands are now wired — none remain stubs.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::progress::{should_render, use_color, ColorChoice, ProgressReporter};
use snapdir_catalog::{
    ancestors_json_line, locations_json_line, revisions_json_line, Catalog, SystemClock,
};
use snapdir_core::hash_file::HashFile;
use snapdir_core::{
    cache, expand_excludes, snapshot_id, walk_with_guards, walk_with_meter, Blake3Hasher,
    Blake3KeyedHasher, CopyGuard, ExcludeMatcher, ExpandedExclude, FollowMode, Hasher, Manifest,
    ManifestEntry, Md5Hasher, Meter, PathMode, PathType, Phase, Sha256Hasher, Store, StoreError,
    WalkOptions,
};
use snapdir_stores::{
    is_hex64, limits, read_pack, resolve_adapter, write_pack_with_format, Adapter, B2Store,
    Durability, ExternalStore, FileSink, FileStore, GcsStore, PackFormat, PackReadReport, PackSink,
    RetryPolicy, S3Store, SplitStore, StreamSink, StreamStore, TransferAdaptivePolicy,
    TransferConfig, DEFAULT_ZSTD_LEVEL, WIRE_CAPS, WIRE_VERSION,
};

/// Upper bound for the adaptive concurrency ceiling (`--max-jobs` / `--jobs`
/// under `--adaptive`). Keeps an over-eager explicit value from oversubscribing
/// the controller's in-flight window.
const ADAPTIVE_CEILING_CAP: usize = 64;

/// Actionable error for a transfer command run with neither `--store` nor the
/// `SNAPDIR_STORE` env fallback. Naming BOTH ways to supply a store turns a
/// dead-end "missing --store option" into something a user can act on.
const NO_STORE_CONFIGURED: &str =
    "no store configured: pass --store <uri> or set the SNAPDIR_STORE environment variable";

/// Content-addressable directory snapshots.
#[derive(Debug, Parser)]
#[command(
    name = "snapdir",
    bin_name = "snapdir",
    version,
    propagate_version = true,
    about = "Content-addressable directory snapshots.",
    long_about = None
)]
pub struct Cli {
    /// Universal options accepted by EVERY subcommand.
    #[command(flatten)]
    pub universal: UniversalArgs,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// `--color` tri-state, derived by clap as `--color <auto|always|never>` so a
/// bogus value (`--color bogus`) is rejected at parse time (exit 2) instead of
/// silently falling back to `auto`. Maps 1:1 to [`progress::ColorChoice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ColorArg {
    /// Color when attached to a TTY and `NO_COLOR` is unset (default).
    #[default]
    Auto,
    /// Always emit color.
    Always,
    /// Never emit color.
    Never,
}

impl ColorArg {
    /// Maps the CLI selector to the renderer's [`ColorChoice`].
    fn resolve(self) -> ColorChoice {
        match self {
            Self::Auto => ColorChoice::Auto,
            Self::Always => ColorChoice::Always,
            Self::Never => ColorChoice::Never,
        }
    }
}

/// The four UNIVERSAL flags every subcommand accepts (output discipline only —
/// they never touch a store, the cache, or a walk). Flattened with
/// `global = true` on [`Cli`] so they apply to (and are accepted by) every
/// command, while the per-family groups below are attached ONLY to the commands
/// they apply to — so clap natively rejects an inapplicable flag and each
/// command's `--help` shows only its own flags.
#[derive(Debug, Args)]
pub struct UniversalArgs {
    /// Suppress stderr banners and the live progress line.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// When to colorize progress output: auto, always, or never.
    #[arg(long, global = true, value_name = "WHEN", value_enum, default_value_t = ColorArg::Auto)]
    pub color: ColorArg,

    /// Disable the live progress line (transfers still run).
    #[arg(long, global = true, env = "SNAPDIR_NO_PROGRESS")]
    pub no_progress: bool,

    /// Enable verbose output. Honored by the transfer commands
    /// (push/fetch/pull/checkout/stage/sync emit an effective-config banner and
    /// CACHED/SAVED notices) and verify-cache (purge notices); inert elsewhere.
    #[arg(long, global = true)]
    pub verbose: bool,
}

/// The walk/hash family: flags that shape the directory walk + hashing, applied
/// to `manifest`/`id`/`stage`/`push`. Disjoint from [`TransferArgs`] (no field
/// name overlaps), so `push`/`stage` can flatten BOTH.
#[derive(Debug, Default, Args)]
pub struct WalkArgs {
    /// Exclude paths matching PATTERN.
    // Accepts both repeated occurrences (`--exclude a --exclude b`) and
    // comma-delimited values (`--exclude a,b`); the collected patterns are
    // OR-combined (a path is excluded if it matches ANY pattern). The doc
    // comment is kept to a single line so `--help` output is byte-stable.
    #[arg(
        long,
        value_name = "PATTERN",
        action = clap::ArgAction::Append,
        value_delimiter = ','
    )]
    pub exclude: Vec<String>,

    /// Max parallel file-hashing jobs during the directory walk (0/auto =
    /// number of CPUs, capped). Distinct from transfer concurrency.
    #[arg(long, value_name = "N", env = "SNAPDIR_WALK_JOBS")]
    pub walk_jobs: Option<usize>,
}

/// The transfer family: store selection, concurrency/bandwidth tuning, retry
/// policy, and the staging/transfer bool flags. Applied to
/// `push`/`fetch`/`pull`/`checkout`/`stage`/`sync`. Disjoint from
/// [`WalkArgs`].
#[derive(Debug, Default, Args)]
// The bool flags are a faithful 1:1 mirror of the Bash orchestrator's transfer
// surface (`--linked --force --keep --dryrun`); a state machine would obscure
// that mapping rather than clarify it.
#[allow(clippy::struct_excessive_bools)]
pub struct TransferArgs {
    /// Store URI: `protocol://location/path`.
    #[arg(long, value_name = "URI", env = "SNAPDIR_STORE")]
    pub store: Option<String>,

    /// Catalog adapter to record this snapshot's location in.
    // The transfer commands that log (push/fetch/pull/checkout/stage) RECORD
    // their location via `log_event`; the catalog selector is a logging sink,
    // not a transfer flag.
    #[arg(long, value_name = "NAME", env = "SNAPDIR_CATALOG")]
    pub catalog: Option<String>,

    /// Shared object-pool store URI: when set, content OBJECTS route to this
    /// pool's `.objects/` while MANIFESTS route to `--store`'s `.manifests/`.
    #[arg(long, value_name = "URI", env = "SNAPDIR_OBJECTS_STORE")]
    pub objects_store: Option<String>,

    /// Directory where the object cache is stored.
    #[arg(long, value_name = "DIR", env = "SNAPDIR_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Snapshot ID to operate on.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Max concurrent object transfers (0/auto = number of CPUs, capped).
    #[arg(long, short = 'j', value_name = "N", env = "SNAPDIR_JOBS")]
    pub jobs: Option<usize>,

    /// Limit total transfer bandwidth, e.g. 10M, 512K, 1G (wget-style; aggregate across all transfers).
    #[arg(long, value_name = "RATE", env = "SNAPDIR_LIMIT_RATE", value_parser = parse_rate_arg)]
    pub limit_rate: Option<String>,

    /// Adaptively tune transfer concurrency/bandwidth toward a fraction
    /// (default 0.8) of measured CPU/network capacity; backs off under
    /// contention. Opt-in; default is full speed.
    ///
    /// Presence (with or without a value) opts in; the optional value is the
    /// politeness fraction in `(0.0, 1.0]`.
    #[arg(
        long,
        value_name = "FRACTION",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "0.8",
        env = "SNAPDIR_ADAPTIVE",
        value_parser = parse_adaptive_fraction
    )]
    pub adaptive: Option<f64>,

    /// Adaptive concurrency ceiling (only meaningful with `--adaptive`). When
    /// unset, defaults to the auto concurrency; clamped to a sane upper bound.
    #[arg(long, value_name = "N", env = "SNAPDIR_MAX_JOBS")]
    pub max_jobs: Option<usize>,

    /// Total retry attempts per network request, including the first (default 5).
    #[arg(long, value_name = "N")]
    pub max_retries: Option<u32>,

    /// Base backoff delay in milliseconds for request retries (default 250).
    #[arg(long, value_name = "MS")]
    pub retry_base_ms: Option<u64>,

    /// Maximum backoff delay in milliseconds for request retries (default 30000).
    #[arg(long, value_name = "MS")]
    pub retry_max_ms: Option<u64>,

    /// Cap request rate (req/s); 0/unset uses the per-backend default.
    #[arg(long, value_name = "N")]
    pub max_requests: Option<u64>,

    /// Use symlinks instead of copies.
    #[arg(long)]
    pub linked: bool,

    /// Force an action to run.
    #[arg(long)]
    pub force: bool,

    /// Keep the staging directory.
    #[arg(long)]
    pub keep: bool,

    /// Run without making any changes.
    #[arg(long)]
    pub dryrun: bool,
}

/// The `defaults` reporting family: the CONFIG knobs `snapdir defaults` resolves
/// and reports (with a `flag`/`env`/`default` source tag). It deliberately
/// mirrors only the resolvable subset of [`TransferArgs`] — the store/cache/
/// concurrency/retry/rate knobs — and OMITS the staging action bools
/// (`--linked`/`--force`/`--keep`/`--dryrun`), which `defaults` neither uses nor
/// reports, so clap natively rejects e.g. `defaults --keep` (exit 2). Every
/// field carries the same flag name + `env` wiring as its `TransferArgs` twin,
/// so `defaults --jobs 3` / `SNAPDIR_JOBS=7 defaults` resolve identically.
#[derive(Debug, Default, Args)]
pub struct DefaultsArgs {
    /// Store URI: `protocol://location/path`.
    #[arg(long, value_name = "URI", env = "SNAPDIR_STORE")]
    pub store: Option<String>,

    /// Catalog adapter to record this snapshot's location in.
    #[arg(long, value_name = "NAME", env = "SNAPDIR_CATALOG")]
    pub catalog: Option<String>,

    /// Shared object-pool store URI.
    #[arg(long, value_name = "URI", env = "SNAPDIR_OBJECTS_STORE")]
    pub objects_store: Option<String>,

    /// Directory where the object cache is stored.
    #[arg(long, value_name = "DIR", env = "SNAPDIR_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Max parallel file-hashing jobs during the directory walk (0/auto = number
    /// of CPUs, capped).
    #[arg(long, value_name = "N", env = "SNAPDIR_WALK_JOBS")]
    pub walk_jobs: Option<usize>,

    /// Max concurrent object transfers (0/auto = number of CPUs, capped).
    #[arg(long, short = 'j', value_name = "N", env = "SNAPDIR_JOBS")]
    pub jobs: Option<usize>,

    /// Limit total transfer bandwidth, e.g. 10M, 512K, 1G (wget-style; aggregate across all transfers).
    #[arg(long, value_name = "RATE", env = "SNAPDIR_LIMIT_RATE", value_parser = parse_rate_arg)]
    pub limit_rate: Option<String>,

    /// Adaptively tune transfer concurrency/bandwidth toward a fraction of measured capacity.
    #[arg(
        long,
        value_name = "FRACTION",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "0.8",
        env = "SNAPDIR_ADAPTIVE",
        value_parser = parse_adaptive_fraction
    )]
    pub adaptive: Option<f64>,

    /// Adaptive concurrency ceiling (only meaningful with `--adaptive`).
    #[arg(long, value_name = "N", env = "SNAPDIR_MAX_JOBS")]
    pub max_jobs: Option<usize>,

    /// Total retry attempts per network request, including the first (default 5).
    #[arg(long, value_name = "N")]
    pub max_retries: Option<u32>,

    /// Base backoff delay in milliseconds for request retries (default 250).
    #[arg(long, value_name = "MS")]
    pub retry_base_ms: Option<u64>,

    /// Maximum backoff delay in milliseconds for request retries (default 30000).
    #[arg(long, value_name = "MS")]
    pub retry_max_ms: Option<u64>,

    /// Cap request rate (req/s); 0/unset uses the per-backend default.
    #[arg(long, value_name = "N")]
    pub max_requests: Option<u64>,
}

/// The catalog-query family: applied to `locations`/`ancestors`/`revisions`.
#[derive(Debug, Default, Args)]
pub struct CatalogArgs {
    /// Catalog adapter to use.
    #[arg(long, value_name = "NAME", env = "SNAPDIR_CATALOG")]
    pub catalog: Option<String>,

    /// Context (directory or store) for catalog queries.
    #[arg(long, value_name = "DIR|STORE")]
    pub location: Option<String>,

    /// Snapshot ID to operate on.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Store URI: `protocol://location/path` (revisions location fallback).
    #[arg(long, value_name = "URI", env = "SNAPDIR_STORE")]
    pub store: Option<String>,

    /// Directory where the object cache (and the default catalog) is stored.
    // Needed so the default catalog (`<cache-dir>/default-catalog.redb`) the
    // query commands READ resolves to the SAME cache dir a no-flag
    // `push`/`stage` WROTE to (respecting `--cache-dir`/`SNAPDIR_CACHE_DIR`).
    #[arg(long, value_name = "DIR", env = "SNAPDIR_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,
}

/// The cache-management family: applied to `verify`/`verify-cache`/`flush-cache`.
#[derive(Debug, Default, Args)]
pub struct CacheMgmtArgs {
    /// Store URI: `protocol://location/path`.
    #[arg(long, value_name = "URI", env = "SNAPDIR_STORE")]
    pub store: Option<String>,

    /// Snapshot ID to operate on.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Purge objects with invalid checksums.
    #[arg(long)]
    pub purge: bool,

    /// Force an action to run.
    #[arg(long)]
    pub force: bool,

    /// Run without making any changes.
    #[arg(long)]
    pub dryrun: bool,

    /// Directory where the object cache is stored.
    #[arg(long, value_name = "DIR", env = "SNAPDIR_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,
}

/// The single transfer/catalog flag `diff` shares: a pinned `--id` selects one
/// manifest from each side instead of unioning the whole store. The rest of
/// `diff`'s flags are local to the [`Command::Diff`] variant.
#[derive(Debug, Default, Args)]
pub struct DiffIdArgs {
    /// Pin each side to this single manifest id (else the whole store is
    /// unioned).
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,
}

/// The plumbing family: store selection for the hidden wire-plumbing commands
/// (`objects-needed`/`send-pack`/`receive-pack`). They obtain a store via
/// `--store` (`SNAPDIR_STORE`) and may route objects to a shared pool via
/// `--objects-store` (`SNAPDIR_OBJECTS_STORE`) — exactly the two store URIs the
/// streaming resolvers (`resolve_stream_store`/`resolve_split_store`) read.
#[derive(Debug, Default, Args)]
pub struct PlumbingArgs {
    /// Store URI: `protocol://location/path`.
    #[arg(long, value_name = "URI", env = "SNAPDIR_STORE")]
    pub store: Option<String>,

    /// Shared object-pool store URI: when set, content OBJECTS route to this
    /// pool's `.objects/` while MANIFESTS route to `--store`'s `.manifests/`.
    #[arg(long, value_name = "URI", env = "SNAPDIR_OBJECTS_STORE")]
    pub objects_store: Option<String>,
}

/// The resolved per-invocation configuration the dispatch layer reads. Built
/// once by [`Cli::run`] by merging the [`UniversalArgs`] with whichever
/// per-family group the parsed subcommand carried, so every downstream helper
/// keeps reading a single flat `self.globals.<field>` exactly as before — only
/// the PARSE shape changed, not the resolved values for a valid invocation.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Resolved {
    pub cache_dir: Option<PathBuf>,
    pub catalog: Option<String>,
    pub store: Option<String>,
    pub objects_store: Option<String>,
    pub id: Option<String>,
    pub exclude: Vec<String>,
    pub linked: bool,
    pub force: bool,
    pub purge: bool,
    pub keep: bool,
    pub dryrun: bool,
    pub verbose: bool,
    pub no_progress: bool,
    pub quiet: bool,
    pub color: ColorArg,
    pub location: Option<String>,
    pub jobs: Option<usize>,
    pub walk_jobs: Option<usize>,
    pub limit_rate: Option<String>,
    pub adaptive: Option<f64>,
    pub max_jobs: Option<usize>,
    pub max_retries: Option<u32>,
    pub retry_base_ms: Option<u64>,
    pub retry_max_ms: Option<u64>,
    pub max_requests: Option<u64>,
}

impl Resolved {
    /// Seeds a `Resolved` with the universal flags; per-family fields default to
    /// empty/`None` until merged in by [`Cli::run`].
    fn from_universal(u: &UniversalArgs) -> Self {
        Resolved {
            quiet: u.quiet,
            color: u.color,
            no_progress: u.no_progress,
            verbose: u.verbose,
            ..Resolved::default()
        }
    }
}

/// The dispatch context: the resolved flat config plus the parsed subcommand.
/// Every helper method that previously hung off `Cli` now hangs off `Ctx` and
/// reads `self.globals.<field>` from the resolved config — so the ~60 read sites
/// are byte-identical to before the per-command flag split.
#[derive(Debug)]
pub struct Ctx {
    globals: Resolved,
    command: Command,
}

/// CLI selector for the SNAPPACK transport encoding `send-pack` emits.
///
/// Mirrors [`PackFormat`] one-for-one but stays a CLI-layer type: clap derives
/// `--pack-format <v1|zstd>` from it (the value-enum tokens are the lowercase
/// variant names), and [`PackFormatArg::resolve`] maps it to the library
/// [`PackFormat`], reading the zstd level from the environment at this seam (the
/// library itself is env-free). The default is [`PackFormatArg::V1`], so an
/// invocation that omits the hidden flag emits the historical byte-identical v1
/// stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PackFormatArg {
    /// Plain `SNAPPACK 1` — the historical byte-for-byte form (default).
    V1,
    /// `SNAPPACK 1Z` — the additive zstd-framed form.
    Zstd,
}

impl PackFormatArg {
    /// Maps the CLI selector to the library [`PackFormat`], reading the zstd
    /// level from `SNAPDIR_SSH_ZSTD_LEVEL` (defaulting to [`DEFAULT_ZSTD_LEVEL`])
    /// for the `zstd` form. The level is passed through to
    /// [`PackFormat::Zstd`], which clamps it into the valid range; a malformed
    /// env value falls back to the default rather than erroring, mirroring the
    /// oracle's permissive numeric-env handling.
    fn resolve(self) -> PackFormat {
        match self {
            Self::V1 => PackFormat::V1,
            Self::Zstd => {
                let level = std::env::var("SNAPDIR_SSH_ZSTD_LEVEL")
                    .ok()
                    .and_then(|v| v.trim().parse::<i32>().ok())
                    .unwrap_or(DEFAULT_ZSTD_LEVEL);
                PackFormat::Zstd(level)
            }
        }
    }
}

/// CLI selector for the intra-side collision policy of `snapdir diff`.
///
/// Mirrors [`crate::diff::OnConflict`]; clap derives `--on-conflict
/// <error|last-wins>` from it. The default is [`OnConflictArg::Error`], so an
/// omitted flag fails hard on a differing-content path collision within one
/// side (the SPEC default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnConflictArg {
    /// A differing-content collision is a hard error (default).
    Error,
    /// The last ref contributing the path wins.
    LastWins,
}

impl OnConflictArg {
    /// Maps the CLI selector to the library [`crate::diff::OnConflict`].
    fn resolve(self) -> crate::diff::OnConflict {
        match self {
            Self::Error => crate::diff::OnConflict::Error,
            Self::LastWins => crate::diff::OnConflict::LastWins,
        }
    }
}

/// The `snapdir` subcommands, matching the Bash orchestrator one-for-one.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the manifest of a directory.
    Manifest {
        /// Emit absolute paths instead of `./`-relative paths.
        #[arg(long)]
        absolute: bool,

        /// Do not follow symbolic links (plain `find` instead of `find -L`).
        #[arg(long)]
        no_follow: bool,

        /// Checksum binary to mirror: `b3sum` (default), `md5sum`, `sha256sum`.
        #[arg(long, value_name = "NAME")]
        checksum_bin: Option<String>,

        /// Exclude paths matching the extended-regex PATTERN
        /// (supports the `%system%` / `%common%` macros).
        // Accepts both repeated occurrences and comma-delimited values,
        // OR-combined (a path is excluded if it matches ANY pattern).
        #[arg(
            long,
            value_name = "PATTERN",
            action = clap::ArgAction::Append,
            value_delimiter = ','
        )]
        exclude: Vec<String>,

        /// Max parallel file-hashing jobs during the directory walk (0/auto =
        /// number of CPUs, capped). Distinct from transfer concurrency.
        #[arg(long, value_name = "N", env = "SNAPDIR_WALK_JOBS")]
        walk_jobs: Option<usize>,

        /// Catalog adapter to record this manifest's location in.
        // `manifest` RECORDS its location via `log_event` (mirroring the
        // oracle's `_snapdir_log_event "manifest" …`), so the catalog selector
        // belongs here — a logging sink, not a transfer flag.
        #[arg(long, value_name = "NAME", env = "SNAPDIR_CATALOG")]
        catalog: Option<String>,

        /// Directory to describe.
        path: Option<PathBuf>,
    },

    /// Print the manifest ID of a directory or a manifest piped via stdin.
    Id {
        /// Walk/hash flags (`--exclude`, `--walk-jobs`).
        #[command(flatten)]
        walk: WalkArgs,

        /// Directory to describe (omit to read a manifest from stdin).
        path: Option<PathBuf>,
    },

    /// Save a snapshot of a directory into the local cache.
    Stage {
        /// Walk/hash flags (`--exclude`, `--walk-jobs`).
        #[command(flatten)]
        walk: WalkArgs,

        /// Transfer flags (`--store`, `--cache-dir`, `--jobs`, …).
        #[command(flatten)]
        transfer: TransferArgs,

        /// Directory to stage.
        dir: Option<PathBuf>,
    },

    /// Push a snapshot to a store given its path or a staged manifest ID.
    Push {
        /// Walk/hash flags (`--exclude`, `--walk-jobs`).
        #[command(flatten)]
        walk: WalkArgs,

        /// Transfer flags (`--store`, `--cache-dir`, `--jobs`, …).
        #[command(flatten)]
        transfer: TransferArgs,

        /// Directory to push (omit when using `--id`).
        path: Option<PathBuf>,
    },

    /// Fetch a snapshot from a store into the local cache.
    Fetch {
        /// Transfer flags (`--store`, `--id`, `--cache-dir`, `--jobs`, …).
        #[command(flatten)]
        transfer: TransferArgs,
    },

    /// Fetch a snapshot from a store and check it out to the given path.
    Pull {
        /// Transfer flags (`--store`, `--id`, `--cache-dir`, `--jobs`, …).
        #[command(flatten)]
        transfer: TransferArgs,

        /// Destination directory.
        path: Option<PathBuf>,
    },

    /// Check out a snapshot to a directory.
    Checkout {
        /// Transfer flags (`--id`, `--cache-dir`, `--linked`, …).
        #[command(flatten)]
        transfer: TransferArgs,

        /// Destination directory.
        dir: Option<PathBuf>,
    },

    /// Verify the integrity of a snapshot in a store (requires `--store`/`--id`).
    Verify {
        /// Cache-management flags (`--store`, `--id`, `--purge`, …).
        #[command(flatten)]
        cache_mgmt: CacheMgmtArgs,
    },

    /// Verify the integrity of the local cache.
    VerifyCache {
        /// Cache-management flags (`--id`, `--purge`, `--cache-dir`, …).
        #[command(flatten)]
        cache_mgmt: CacheMgmtArgs,
    },

    /// Flush the local cache.
    FlushCache {
        /// Cache-management flags (`--cache-dir`, …).
        #[command(flatten)]
        cache_mgmt: CacheMgmtArgs,
    },

    /// List directories and stores where snapshots have been recorded.
    Locations {
        /// Catalog-query flags (`--catalog`, `--location`, `--id`, `--store`).
        #[command(flatten)]
        catalog: CatalogArgs,
    },

    /// List ancestor snapshot IDs and their locations.
    Ancestors {
        /// Catalog-query flags (`--catalog`, `--location`, `--id`, `--store`).
        #[command(flatten)]
        catalog: CatalogArgs,
    },

    /// List snapshot IDs created on a location (store or absolute path).
    Revisions {
        /// Catalog-query flags (`--catalog`, `--location`, `--id`, `--store`).
        #[command(flatten)]
        catalog: CatalogArgs,
    },

    /// Print default settings and arguments.
    Defaults {
        /// The resolvable config knobs whose effective value + source
        /// (`flag`/`env`/`default`) `defaults` reports, e.g. `--cache-dir`,
        /// `--jobs`, `--store` (so `defaults --jobs 3` shows the effective
        /// jobs=3, tagged `flag`). Staging action flags are intentionally absent.
        #[command(flatten)]
        config: DefaultsArgs,
    },

    /// Copy a snapshot (its manifest + objects) directly between two stores,
    /// streaming through memory — no local staging.
    Sync {
        /// Transfer flags (`--id`, `--jobs`, `--limit-rate`, `--dryrun`, …).
        #[command(flatten)]
        transfer: TransferArgs,

        /// Source store URI: `protocol://location/path`.
        // `--from` falls back to `$SNAPDIR_STORE` so a single exported store URI
        // serves as the sync SOURCE (the historical behavior the restructure
        // dropped); `--to` has no such fallback (a sync needs an explicit dest).
        #[arg(long, value_name = "STORE", env = "SNAPDIR_STORE")]
        from: String,
        /// Destination store URI: `protocol://location/path`.
        #[arg(long, value_name = "STORE")]
        to: String,
        /// Explicit SOURCE object pool URI (split source): objects are read from
        /// here while manifests come from `--from`. Absent => `--from` is a plain
        /// colocated store. Distinct from the global `--objects-store`.
        #[arg(long, value_name = "URI")]
        from_objects: Option<String>,
        /// Explicit DESTINATION object pool URI (split dest): objects are written
        /// here while manifests go to `--to`. Absent => `--to` is a plain
        /// colocated store. Distinct from the global `--objects-store`.
        #[arg(long, value_name = "URI")]
        to_objects: Option<String>,
    },

    /// Compare two sides, each a set of manifests, reporting file-level
    /// differences — reading MANIFESTS ONLY.
    Diff {
        /// `--id`: pin each side to one manifest instead of unioning the store.
        #[command(flatten)]
        id_arg: DiffIdArgs,

        /// FROM-side ref: a manifest-store URI (enumerated) and/or, with
        /// `--id`, a single pinned manifest. Repeatable; refs are UNIONED into
        /// the FROM side.
        #[arg(long, value_name = "REF", action = clap::ArgAction::Append)]
        from: Vec<String>,

        /// TO-side ref: a manifest-store URI (enumerated) and/or a pinned
        /// manifest. Repeatable; refs are UNIONED into the TO side.
        #[arg(long, value_name = "REF", action = clap::ArgAction::Append)]
        to: Vec<String>,

        /// Also emit unchanged (equal) paths.
        #[arg(long)]
        all: bool,

        /// Emit a JSON array of `{status, path}` objects instead of porcelain.
        #[arg(long)]
        json: bool,

        /// Exit 1 when any difference is found (git `diff --exit-code`
        /// semantics); the default exits 0 regardless.
        #[arg(long)]
        exit_code: bool,

        /// Policy for an intra-side path collision (same path, differing
        /// content unioned on one side).
        #[arg(long, value_name = "POLICY", value_enum, default_value_t = OnConflictArg::Error)]
        on_conflict: OnConflictArg,
    },

    /// Print the version.
    Version {
        /// Also print the wire protocol version + plumbing capabilities.
        ///
        /// HIDDEN: the ssh:// acceleration probe runs `snapdir version
        /// --capabilities` on the REMOTE snapdir and keys the accelerate/dumb
        /// decision on the `wire=<N>` integer (exact match against
        /// [`WIRE_VERSION`], never semver). Older remote snapdirs clap-error on
        /// this unknown flag, which the probe treats as "no acceleration" —
        /// clean degradation with no code on that path. Plain `snapdir
        /// version` output stays byte-identical, and the hidden flag keeps the
        /// trycmd `--help` snapshots byte-stable (CLI-compat freeze).
        #[arg(long, hide = true)]
        capabilities: bool,
    },

    /// Generate a shell completion script (bash, zsh, fish, …).
    ///
    /// Writes a completion script for the given shell to stdout. Wire it up by
    /// sourcing the output from your shell profile:
    ///
    ///   bash:       eval "$(snapdir autocomplete bash)"
    ///               # add the line above to ~/.bashrc
    ///
    ///   zsh:        eval "$(snapdir autocomplete zsh)"
    ///               # add the line above to ~/.zshrc
    ///
    ///   fish:       snapdir autocomplete fish | source
    ///               # or write to ~/.config/fish/completions/snapdir.fish
    ///
    ///   powershell: snapdir autocomplete powershell | Out-String | Invoke-Expression
    ///               # add the line above to your $PROFILE
    ///
    ///   elvish:     eval (snapdir autocomplete elvish | slurp)
    ///
    /// The script always targets the `snapdir` binary name. The hidden
    /// `completions <shell>` alias is kept for back-compat (the release
    /// pipeline's gen-assets job and existing scripts) and emits byte-identical
    /// output.
    // `autocomplete` is the VISIBLE primary; `completions` is a HIDDEN alias
    // (clap's `alias` is hidden, unlike `visible_alias`), so the release
    // pipeline's `snapdir completions <shell>` keeps working unchanged while the
    // documented surface shows only `autocomplete`. The internal enum variant
    // stays `Completions` (lowest-churn); clap derives the command name from the
    // explicit `name`.
    #[command(name = "autocomplete", alias = "completions")]
    Completions {
        /// Target shell (`bash`, `fish`, `zsh`, `powershell`, `elvish`).
        shell: clap_complete::Shell,
    },

    /// Render the man page (roff) to stdout.
    ///
    /// Hidden from the documented surface: a build-time hook the release
    /// pipeline calls as `snapdir man` to bundle the man page into each archive.
    #[command(hide = true)]
    Man,

    /// Print the subset of the checksums offered on stdin that the store does
    /// NOT hold, one per line, in first-occurrence order.
    ///
    /// Hidden wire plumbing (SNAPPACK acceleration, see
    /// [`snapdir_stores::pack`]): the diff round trip of the upcoming `ssh://`
    /// store — the local end offers a snapshot's full object list and the
    /// remote `snapdir objects-needed --store file://<base>` answers with
    /// exactly the absent objects, so only those ride the pack stream.
    /// Fail-closed: ANY malformed stdin line errors before the first store
    /// query and prints NOTHING.
    #[command(hide = true)]
    ObjectsNeeded {
        /// Plumbing store flags (`--store`, `--objects-store`).
        #[command(flatten)]
        plumbing: PlumbingArgs,
    },

    /// Emit a SNAPPACK stream of the listed objects (+ optional manifest,
    /// last) from the store to raw stdout.
    ///
    /// Hidden wire plumbing: the sending half of
    /// `snapdir send-pack | ssh host 'snapdir receive-pack'`. Any failure —
    /// including a missing object — aborts BEFORE the `end` trailer
    /// ([`write_pack_with_format`]), so a consumer of the partial stream fails
    /// too.
    #[command(hide = true)]
    SendPack {
        /// Plumbing store flags (`--store`, `--objects-store`).
        #[command(flatten)]
        plumbing: PlumbingArgs,

        /// File listing one object checksum per line (`-` reads stdin).
        #[arg(long, value_name = "FILE|-")]
        ids: PathBuf,

        /// Snapshot id whose manifest rides the pack as the LAST record.
        #[arg(long, value_name = "ID")]
        manifest_id: Option<String>,

        /// On-wire SNAPPACK transport encoding to emit.
        ///
        /// HIDDEN + defaults to `v1`, so an old `send-pack` invocation stays
        /// BYTE-IDENTICAL (same magic, same body, same `--help` surface). `zstd`
        /// opts into the additive `SNAPPACK 1Z` form (same record grammar, whole
        /// body in one zstd frame); the receiver sniffs the magic and accepts
        /// either form, so there is no negotiation flag on `receive-pack`. The
        /// compression level is the library default ([`DEFAULT_ZSTD_LEVEL`]),
        /// overridable via `SNAPDIR_SSH_ZSTD_LEVEL` (the library is env-free, so
        /// the level env is read HERE in the CLI seam).
        #[arg(long, value_name = "FORMAT", value_enum, default_value_t = PackFormatArg::V1, hide = true)]
        pack_format: PackFormatArg,
    },

    /// Consume a SNAPPACK stream from stdin into the store.
    ///
    /// Hidden wire plumbing: the receiving half of the acceleration pipe.
    /// Every payload is incrementally BLAKE3-verified against its claimed
    /// checksum and the manifest commits only after the verified `end`
    /// trailer ([`read_pack`]) — truncation files verified objects but never
    /// publishes the snapshot. Summary on stderr; stdout stays silent.
    #[command(hide = true)]
    ReceivePack {
        /// Plumbing store flags (`--store`, `--objects-store`).
        #[command(flatten)]
        plumbing: PlumbingArgs,

        /// Fail unless the stream committed exactly this manifest id.
        #[arg(long, value_name = "ID")]
        require_manifest: Option<String>,
    },
}

impl Cli {
    /// Merges the universal flags with whichever per-family group the parsed
    /// subcommand carried into a single flat [`Resolved`] config, then hands the
    /// command + config to [`Ctx::run`]. The per-command flag split lives ONLY
    /// at the clap parse boundary (so clap natively rejects inapplicable flags
    /// and per-command `--help` is scoped); from here down a valid invocation
    /// resolves to byte-identical values, dispatched through `Ctx`.
    ///
    /// # Errors
    ///
    /// Propagates any error from the dispatched command (see [`Ctx::run`]).
    pub fn run(self) -> Result<()> {
        let mut globals = Resolved::from_universal(&self.universal);
        // Fold the active command's per-family group(s) into the flat config.
        match &self.command {
            Command::Manifest {
                exclude,
                walk_jobs,
                catalog,
                ..
            } => {
                globals.exclude.clone_from(exclude);
                globals.walk_jobs = *walk_jobs;
                globals.catalog.clone_from(catalog);
            }
            Command::Id { walk, .. } => merge_walk(&mut globals, walk),
            Command::Stage { walk, transfer, .. } | Command::Push { walk, transfer, .. } => {
                merge_walk(&mut globals, walk);
                merge_transfer(&mut globals, transfer);
            }
            Command::Fetch { transfer }
            | Command::Pull { transfer, .. }
            | Command::Checkout { transfer, .. }
            | Command::Sync { transfer, .. } => merge_transfer(&mut globals, transfer),
            // `defaults` folds its config group so its `--cache-dir`/`--jobs`/
            // `--store`/… overrides resolve (and report `flag`) like a real run.
            Command::Defaults { config } => merge_defaults(&mut globals, config),
            Command::Verify { cache_mgmt }
            | Command::VerifyCache { cache_mgmt }
            | Command::FlushCache { cache_mgmt } => merge_cache_mgmt(&mut globals, cache_mgmt),
            Command::Locations { catalog }
            | Command::Ancestors { catalog }
            | Command::Revisions { catalog } => merge_catalog(&mut globals, catalog),
            Command::Diff { id_arg, .. } => globals.id.clone_from(&id_arg.id),
            // The hidden wire-plumbing commands fold their store group so the
            // streaming resolvers see `--store` / `--objects-store` (+ env).
            Command::ObjectsNeeded { plumbing }
            | Command::SendPack { plumbing, .. }
            | Command::ReceivePack { plumbing, .. } => merge_plumbing(&mut globals, plumbing),
            // The remaining commands (version + the build-time hooks) take
            // universal flags only.
            Command::Version { .. } | Command::Completions { .. } | Command::Man => {}
        }
        Ctx {
            globals,
            command: self.command,
        }
        .run()
    }
}

/// Folds a parsed [`WalkArgs`] group into the resolved config.
fn merge_walk(g: &mut Resolved, w: &WalkArgs) {
    g.exclude.clone_from(&w.exclude);
    g.walk_jobs = w.walk_jobs;
}

/// Folds a parsed [`TransferArgs`] group into the resolved config.
fn merge_transfer(g: &mut Resolved, t: &TransferArgs) {
    g.store.clone_from(&t.store);
    g.catalog.clone_from(&t.catalog);
    g.objects_store.clone_from(&t.objects_store);
    g.cache_dir.clone_from(&t.cache_dir);
    g.id.clone_from(&t.id);
    g.jobs = t.jobs;
    g.limit_rate.clone_from(&t.limit_rate);
    g.adaptive = t.adaptive;
    g.max_jobs = t.max_jobs;
    g.max_retries = t.max_retries;
    g.retry_base_ms = t.retry_base_ms;
    g.retry_max_ms = t.retry_max_ms;
    g.max_requests = t.max_requests;
    g.linked = t.linked;
    g.force = t.force;
    g.keep = t.keep;
    g.dryrun = t.dryrun;
}

/// Folds a parsed [`DefaultsArgs`] group into the resolved config so `defaults`
/// reports the same resolved values a real run would use. Only the config knobs
/// it reports are folded (no staging bools exist in [`DefaultsArgs`]).
fn merge_defaults(g: &mut Resolved, d: &DefaultsArgs) {
    g.store.clone_from(&d.store);
    g.catalog.clone_from(&d.catalog);
    g.objects_store.clone_from(&d.objects_store);
    g.cache_dir.clone_from(&d.cache_dir);
    g.walk_jobs = d.walk_jobs;
    g.jobs = d.jobs;
    g.limit_rate.clone_from(&d.limit_rate);
    g.adaptive = d.adaptive;
    g.max_jobs = d.max_jobs;
    g.max_retries = d.max_retries;
    g.retry_base_ms = d.retry_base_ms;
    g.retry_max_ms = d.retry_max_ms;
    g.max_requests = d.max_requests;
}

/// Folds a parsed [`CatalogArgs`] group into the resolved config.
fn merge_catalog(g: &mut Resolved, c: &CatalogArgs) {
    g.catalog.clone_from(&c.catalog);
    g.location.clone_from(&c.location);
    g.id.clone_from(&c.id);
    g.store.clone_from(&c.store);
    g.cache_dir.clone_from(&c.cache_dir);
}

/// Folds a parsed [`CacheMgmtArgs`] group into the resolved config.
fn merge_cache_mgmt(g: &mut Resolved, c: &CacheMgmtArgs) {
    g.store.clone_from(&c.store);
    g.id.clone_from(&c.id);
    g.purge = c.purge;
    g.force = c.force;
    g.dryrun = c.dryrun;
    g.cache_dir.clone_from(&c.cache_dir);
}

/// Folds a parsed [`PlumbingArgs`] group into the resolved config — so the
/// plumbing commands' `resolve_stream_store`/`resolve_split_store` see the
/// `--store`/`--objects-store` (and their `SNAPDIR_STORE`/`SNAPDIR_OBJECTS_STORE`
/// env) they read.
fn merge_plumbing(g: &mut Resolved, p: &PlumbingArgs) {
    g.store.clone_from(&p.store);
    g.objects_store.clone_from(&p.objects_store);
}

impl Ctx {
    /// Dispatch the parsed command.
    ///
    /// `manifest`/`id`, the store commands, the cache commands
    /// (`stage`/`verify-cache`/`flush-cache`), the catalog queries
    /// (`locations`/`ancestors`/`revisions`), and `defaults` are all wired to the
    /// libraries / environment. Every subcommand is implemented.
    ///
    /// # Errors
    ///
    /// Returns any error raised while resolving the path, building the exclude
    /// matcher, walking the tree, talking to the store/cache, opening the
    /// catalog, or resolving the running binary path for `defaults`.
    // A flat one-arm-per-subcommand dispatch: every arm just routes to a
    // `run_*` helper (or, for `manifest`/`id`/`version`, a short inline body).
    // Splitting it would only scatter the routing table across helpers without
    // making any single arm clearer, so the length is inherent.
    #[allow(clippy::too_many_lines)]
    pub fn run(&self) -> Result<()> {
        match &self.command {
            Command::Manifest {
                absolute,
                no_follow,
                checksum_bin,
                exclude,
                path,
                ..
            } => {
                // Precedence: the subcommand's `--exclude` list overrides the
                // resolved one when non-empty, else fall back to the resolved
                // list (they are the same value here — kept for parity).
                let exclude: &[String] = if exclude.is_empty() {
                    &self.globals.exclude
                } else {
                    exclude
                };
                // Render live discovery+hash progress for the walk (stderr+TTY
                // gated). The walk drives Discovering -> Hashing/total itself;
                // the reporter MUST be finished before the stdout `println!` so
                // the manifest bytes stay clean (progress is stderr-only).
                let jobs = self.walk_jobs();
                let (meter, reporter) = self.start_progress(jobs);
                let manifest = self.build_manifest(
                    path.as_deref(),
                    *absolute,
                    *no_follow,
                    checksum_bin.as_deref(),
                    exclude,
                    meter.as_deref(),
                );
                reporter.finish();
                let manifest = manifest?;
                println!("{manifest}");
                // Mirror the oracle's `_snapdir_log_event "manifest" "$id"
                // "$snapdir_dir_abs_path"` (`snapdir` L212): after emitting the
                // manifest, record it in the catalog at the manifested
                // directory's absolute path. The id is the b3sum of the
                // comment-stripped manifest, exactly as the oracle derives
                // `_SNAPDIR_ID` (via `snapdir_id`) before logging. Best-effort
                // and a silent no-op when no catalog is enabled, so it never
                // changes the stdout bytes above. Note: `snapdir id` does NOT
                // log (the oracle's `snapdir_id` at L223 has no
                // `_snapdir_log_event`), so only this `manifest` arm logs.
                let id = snapshot_id(&manifest, &Blake3Hasher::new());
                let abs = resolve_root(path.as_deref())
                    .context("resolving the manifested directory path")?;
                self.log_event("manifest", &id, &abs.to_string_lossy(), false)?;
                Ok(())
            }
            Command::Id { path, .. } => {
                // `snapdir id` reproduces the original `snapdir id`: the snapshot id is the
                // b3sum of the comment-stripped manifest text. The wrapper
                // walks with the default checksum (b3sum) and default
                // path/follow modes; the id is checksum-mode independent here.
                //
                // With NO PATH, the documented contract (help + help-id.trycmd:
                // "omit to read a manifest from stdin") is to hash a manifest
                // piped on stdin rather than walking the cwd. We honor that
                // when stdin is NOT a TTY: parse the piped manifest text and
                // run it through the SAME frozen `snapshot_id` rule `id <dir>`
                // uses (parse strips `#`-comment lines == `grep -v '^#'`;
                // `snapshot_id` re-renders + appends the trailing `echo`
                // newline before BLAKE3), so `manifest <dir> | id` round-trips
                // byte-identically to `id <dir>` and depends only on stdin.
                let manifest = if path.is_none() && !std::io::stdin().is_terminal() {
                    let mut text = String::new();
                    std::io::stdin()
                        .read_to_string(&mut text)
                        .context("reading manifest from stdin")?;
                    Manifest::parse(&text).context("parsing manifest from stdin")?
                } else if path.is_none() {
                    // A bare `snapdir id` with stdin attached to a TTY would
                    // otherwise silently walk the cwd. Fail loudly instead and
                    // point at the documented forms.
                    anyhow::bail!(
                        "no directory given and no manifest on stdin; \
                         pass a PATH (or `.`), or pipe a manifest"
                    );
                } else {
                    // Walking a real directory: render live discovery+hash
                    // progress (stderr+TTY gated). The reporter is finished
                    // BEFORE the stdout `println!` so the id stays byte-clean.
                    let jobs = self.walk_jobs();
                    let (meter, reporter) = self.start_progress(jobs);
                    let manifest = self.build_manifest(
                        path.as_deref(),
                        false,
                        false,
                        None,
                        &self.globals.exclude,
                        meter.as_deref(),
                    );
                    reporter.finish();
                    manifest?
                };
                let id = snapshot_id(&manifest, &Blake3Hasher::new());
                println!("{id}");
                Ok(())
            }
            Command::Push { path, .. } => self.run_push(path.as_deref()),
            Command::Fetch { .. } => self.run_fetch(),
            Command::Checkout { dir, .. } => self.run_checkout(dir.as_deref()),
            Command::Pull { path, .. } => self.run_pull(path.as_deref()),
            Command::Verify { .. } => self.run_verify(),
            Command::Stage { dir, .. } => self.run_stage(dir.as_deref()),
            Command::VerifyCache { .. } => self.run_verify_cache(),
            Command::FlushCache { .. } => self.run_flush_cache(),
            Command::Locations { .. } => self.run_locations(),
            Command::Ancestors { .. } => self.run_ancestors(),
            Command::Revisions { .. } => self.run_revisions(),
            Command::Version { capabilities } => {
                if *capabilities {
                    // The acceleration probe's capability line: space-separated
                    // `key=value` fields, consumers ignore unknown fields and
                    // negotiate on the exact `wire` integer only (baked from
                    // pack::WIRE_VERSION — NEVER from semver).
                    println!(
                        "snapdir {} wire={WIRE_VERSION} caps={}",
                        env!("CARGO_PKG_VERSION"),
                        WIRE_CAPS.join(",")
                    );
                } else {
                    // CLI-compat frozen: plain `version` stays byte-identical.
                    println!("snapdir {}", env!("CARGO_PKG_VERSION"));
                }
                Ok(())
            }
            Command::Defaults { .. } => self.run_defaults(),
            Command::Sync {
                from,
                to,
                from_objects,
                to_objects,
                ..
            } => self.run_sync(from, to, from_objects.as_deref(), to_objects.as_deref()),
            Command::Diff {
                from,
                to,
                all,
                json,
                exit_code,
                on_conflict,
                ..
            } => self.run_diff(from, to, *all, *json, *exit_code, on_conflict.resolve()),
            Command::Completions { shell } => {
                // The visible `autocomplete <shell>` command (and its hidden
                // `completions` back-compat alias): emit the requested shell's
                // completion script to stdout — for a user to source from their
                // profile, or for the release pipeline to bundle. The bin name is
                // `snapdir` (the visible surface, hidden subcommands included).
                let mut cmd = Cli::command();
                clap_complete::generate(*shell, &mut cmd, "snapdir", &mut std::io::stdout());
                Ok(())
            }
            Command::Man => {
                // Build-time hook: render the man page (roff) to stdout.
                clap_mangen::Man::new(Cli::command())
                    .render(&mut std::io::stdout())
                    .context("rendering the man page")?;
                Ok(())
            }
            Command::ObjectsNeeded { .. } => self.run_objects_needed(),
            Command::SendPack {
                ids,
                manifest_id,
                pack_format,
                ..
            } => self.run_send_pack(ids, manifest_id.as_deref(), pack_format.resolve()),
            Command::ReceivePack {
                require_manifest, ..
            } => self.run_receive_pack(require_manifest.as_deref()),
        }
    }
}

/// `snapdir defaults`: print the effective default settings + environment, in
/// sorted-unique order. Reproduces the behavior of the original
/// `snapdir_defaults()` (snapdir L1083), whose pipeline was:
///
/// ```sh
/// {
///   snapdir-manifest defaults | grep -v "^-"
///   env | grep "SNAPDIR" | grep -v VERSION \
///     | sed -E 's|^_?SNAPDIR_|--|; s|_|-|g;' | tr '[:upper:]' '[:lower:]' | sort
///   echo "SNAPDIR_BIN_PATH=${SNAPDIR_BIN_PATH:-$_SNAPDIR_BIN_PATH}"
/// } | sort -u
/// ```
///
/// Three groups, then `sort -u`:
///
/// 1. the manifest tool's non-option default lines (`grep -v "^-"` drops the
///    `--option=…` lines, leaving the `SNAPDIR_MANIFEST_*=…` key lines). The
///    Rust port has no separate `snapdir-manifest` binary — the walk is
///    in-process — so the effective equivalents are emitted: the manifest
///    bin path is this running binary, and `SNAPDIR_MANIFEST_CONTEXT` /
///    `SNAPDIR_MANIFEST_EXCLUDE` come from the environment (default empty),
///    matching what the Rust manifest walk actually honors.
/// 2. every `SNAPDIR*` environment variable except `*VERSION*`, reformatted
///    by the `sed`/`tr` rules into `--option-name=value` (strip a leading
///    `_SNAPDIR_`/`SNAPDIR_` → `--`, `_`→`-`, lowercase the whole line).
/// 3. a `SNAPDIR_BIN_PATH=…` line for the running binary
///    ([`std::env::current_exe`]).
///
/// The combined lines are sorted and deduplicated (`sort -u`), so the output
/// order is independent of the environment's iteration order. Kept as a free
/// function: it resolves everything from the process environment + the running
/// binary path, so it needs no CLI state (`&self`).
/// Tag describing where a knob's effective value came from: an explicit CLI
/// flag, an environment variable, or the built-in default. Printed verbatim
/// (lowercased) on every knob line so the output is greppable by source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Flag,
    Env,
    Default,
}

/// The resolved catalog selection: either an enabled redb DB path or the
/// explicit `none`/empty disable sentinel.
enum CatalogTarget {
    /// Catalog logging/reads target this redb DB path.
    Enabled(PathBuf),
    /// `--catalog none` / `--catalog ""` (or `manifest` with no explicit
    /// catalog): no logging, no DB created, query commands print a "disabled"
    /// message and exit 0.
    Disabled,
}

impl Source {
    fn tag(self) -> &'static str {
        match self {
            Source::Flag => "flag",
            Source::Env => "env",
            Source::Default => "default",
        }
    }
}

/// Per-knob precedence resolver for the `defaults` report: a value came from a
/// `--flag` (highest), else its `SNAPDIR_*` env var, else the built-in default.
///
/// `flag_set` is whether the corresponding CLI flag was present on argv (the
/// resolved config alone cannot tell flag from env, since clap's `env` feature
/// already folded them into one `Option`). `env_name` is the knob's env var;
/// `""` means the knob has no env var (e.g. plain `--color`), so it is only ever
/// `flag` or `default`.
fn knob_source(flag_set: bool, env_name: &str) -> Source {
    if flag_set {
        Source::Flag
    } else if !env_name.is_empty() && std::env::var_os(env_name).is_some() {
        Source::Env
    } else {
        Source::Default
    }
}

/// Prints the "catalog disabled" notice on stderr for the query commands when
/// `--catalog none` (or empty) selects the disable sentinel, then the caller
/// exits 0. Distinct from the enabled-but-empty case (which prints nothing),
/// so a disabled catalog is never confused with a catalog that simply has no
/// records yet.
fn print_catalog_disabled() {
    eprintln!("catalog disabled (--catalog none): nothing to query");
}

impl Ctx {
    /// `snapdir defaults`: print the EFFECTIVE configuration — for every knob,
    /// its RESOLVED value plus a source tag (`flag` | `env` | `default`),
    /// reflecting flag and env overrides with flag>env>default precedence.
    ///
    /// Output is deterministic and line-oriented (`<knob>  <value>  source=<tag>`
    /// in a stable order), so two runs on the same env are byte-identical and a
    /// simple grep can parse it. The resolved values REUSE the same helpers the
    /// real commands use — [`Self::cache_dir`], [`Self::transfer_config`] (jobs),
    /// [`Self::resolve_retry_policy`] (retries), etc. — so what `defaults`
    /// reports is exactly what a run would use.
    ///
    /// Arbitrary set `SNAPDIR_*` vars are still surfaced (a superset of the old
    /// "echo env" behavior) in a trailing section; the legacy bash
    /// `SNAPDIR_MANIFEST_CONTEXT` / `SNAPDIR_MANIFEST_EXCLUDE` are shown only when
    /// set and only under an explicit `legacy` label — never as live knobs.
    #[allow(clippy::too_many_lines)]
    fn run_defaults(&self) -> Result<()> {
        // argv-presence probe: clap's `env` feature folds `--flag` and the env
        // var into one resolved `Option`, so to tell `flag` from `env` we check
        // whether the long flag literally appears on the command line.
        let argv: Vec<String> = std::env::args().collect();
        let has_flag = |name: &str| -> bool {
            let eq = format!("{name}=");
            argv.iter().any(|a| a == name || a.starts_with(&eq))
        };

        let mut out: Vec<String> = Vec::new();
        // `<knob>  <value>  source=<tag>` — fixed shape, stable order.
        let mut emit = |knob: &str, value: &str, src: Source| {
            out.push(format!("{knob} {value} source={}", src.tag()));
        };

        // cache-dir: reuse Self::cache_dir (flag/env/$HOME-derived default).
        let cache_dir = self.cache_dir();
        emit(
            "cache-dir",
            &cache_dir.display().to_string(),
            knob_source(has_flag("--cache-dir"), "SNAPDIR_CACHE_DIR"),
        );

        // store / objects-store / catalog: resolved Option, "none" when unset.
        emit(
            "store",
            self.globals.store.as_deref().unwrap_or("none"),
            knob_source(has_flag("--store"), "SNAPDIR_STORE"),
        );
        emit(
            "objects-store",
            self.globals.objects_store.as_deref().unwrap_or("none"),
            knob_source(has_flag("--objects-store"), "SNAPDIR_OBJECTS_STORE"),
        );
        // catalog: when unset, resolve to the default catalog path so `defaults`
        // surfaces what a no-flag run would actually use (default-on), with
        // source=default. An explicit `none`/empty value stays verbatim.
        let catalog_value = match self.resolve_catalog(true) {
            CatalogTarget::Enabled(db) => db.display().to_string(),
            CatalogTarget::Disabled => "none".to_owned(),
        };
        emit(
            "catalog",
            &catalog_value,
            knob_source(has_flag("--catalog"), "SNAPDIR_CATALOG"),
        );

        // jobs: reuse the transfer-config resolver so the reported number is the
        // exact auto-resolved transfer concurrency a real run would use.
        let jobs = self.transfer_config()?.concurrency.get();
        emit(
            "jobs",
            &jobs.to_string(),
            knob_source(has_flag("--jobs") || has_flag("-j"), "SNAPDIR_JOBS"),
        );

        // walk-jobs: resolve the auto CPU count the same way the core walk does
        // (available_parallelism capped at 16) when unset/0.
        let walk_jobs = match self.globals.walk_jobs {
            Some(n) if n > 0 => n,
            _ => std::thread::available_parallelism()
                .map_or(1, std::num::NonZeroUsize::get)
                .clamp(1, 16),
        };
        emit(
            "walk-jobs",
            &walk_jobs.to_string(),
            knob_source(has_flag("--walk-jobs"), "SNAPDIR_WALK_JOBS"),
        );

        // limit-rate: the raw rate spec (resolved/parsed elsewhere); none when unset.
        emit(
            "limit-rate",
            self.globals.limit_rate.as_deref().unwrap_or("none"),
            knob_source(has_flag("--limit-rate"), "SNAPDIR_LIMIT_RATE"),
        );

        // adaptive: the operating fraction when enabled, else "off".
        let adaptive = self
            .globals
            .adaptive
            .map_or_else(|| "off".to_string(), |f| f.to_string());
        emit(
            "adaptive",
            &adaptive,
            knob_source(has_flag("--adaptive"), "SNAPDIR_ADAPTIVE"),
        );

        // max-jobs / max-requests: optional ceilings, "none" when unset.
        emit(
            "max-jobs",
            &self
                .globals
                .max_jobs
                .map_or_else(|| "none".to_string(), |n| n.to_string()),
            knob_source(has_flag("--max-jobs"), "SNAPDIR_MAX_JOBS"),
        );

        // retry policy: reuse the live resolver so the reported schedule is the
        // exact one a transfer would install (flag>env>default per field).
        let retry = self.resolve_retry_policy();
        emit(
            "max-retries",
            &retry.max_attempts.to_string(),
            knob_source(has_flag("--max-retries"), "SNAPDIR_MAX_RETRIES"),
        );
        emit(
            "retry-base-ms",
            &retry.base.as_millis().to_string(),
            knob_source(has_flag("--retry-base-ms"), "SNAPDIR_RETRY_BASE_MS"),
        );
        emit(
            "retry-max-ms",
            &retry.cap.as_millis().to_string(),
            knob_source(has_flag("--retry-max-ms"), "SNAPDIR_RETRY_MAX_MS"),
        );
        // max-requests resolves flag>env (env via SNAPDIR_MAX_REQUESTS, the same
        // fallback resolve_rate_limits uses), else "none".
        let max_requests = self
            .globals
            .max_requests
            .or_else(|| env_u64("SNAPDIR_MAX_REQUESTS"));
        emit(
            "max-requests",
            &max_requests.map_or_else(|| "none".to_string(), |n| n.to_string()),
            knob_source(has_flag("--max-requests"), "SNAPDIR_MAX_REQUESTS"),
        );

        // no-progress: bool; has an env var (SNAPDIR_NO_PROGRESS).
        emit(
            "no-progress",
            if self.globals.no_progress {
                "true"
            } else {
                "false"
            },
            knob_source(has_flag("--no-progress"), "SNAPDIR_NO_PROGRESS"),
        );

        // color: a non-Option flag with NO env var, so flag-or-default only.
        let color = match self.globals.color {
            ColorArg::Auto => "auto",
            ColorArg::Always => "always",
            ColorArg::Never => "never",
        };
        emit("color", color, knob_source(has_flag("--color"), ""));

        // fsync: read SNAPDIR_FSYNC (default `batch`); no flag, env-or-default.
        let fsync = match std::env::var("SNAPDIR_FSYNC").ok().as_deref() {
            Some("off") => "off",
            _ => "batch",
        };
        emit("fsync", fsync, knob_source(false, "SNAPDIR_FSYNC"));

        // clonefile: enabled unless SNAPDIR_CLONEFILE=0 (no flag).
        let clonefile_on = !matches!(std::env::var("SNAPDIR_CLONEFILE").as_deref(), Ok("0"));
        emit(
            "clonefile",
            if clonefile_on { "enabled" } else { "disabled" },
            knob_source(false, "SNAPDIR_CLONEFILE"),
        );

        // verify-copies: forced ON only by SNAPDIR_VERIFY_COPIES=1 (no flag).
        let verify_on = matches!(std::env::var("SNAPDIR_VERIFY_COPIES").as_deref(), Ok("1"));
        emit(
            "verify-copies",
            if verify_on { "enabled" } else { "disabled" },
            knob_source(false, "SNAPDIR_VERIFY_COPIES"),
        );

        // The effective-knob block is the head of the report.
        for line in &out {
            println!("{line}");
        }

        // Superset: surface ANY other set `SNAPDIR_*` var (excluding *VERSION*)
        // that the knob block above did not already cover, sorted for
        // determinism. The legacy bash `SNAPDIR_MANIFEST_*` vars are shown here
        // ONLY when set and ONLY under an explicit `legacy` label — never as a
        // live effective knob, and never as the old empty `=`-suffixed cruft.
        let mut others: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("SNAPDIR") && !k.contains("VERSION"))
            .collect();
        others.sort();
        let mut printed_header = false;
        for (key, value) in others {
            let legacy = key == "SNAPDIR_MANIFEST_CONTEXT" || key == "SNAPDIR_MANIFEST_EXCLUDE";
            if !printed_header {
                println!("other-env:");
                printed_header = true;
            }
            if legacy {
                println!("  {key}={value} (legacy)");
            } else {
                println!("  {key}={value}");
            }
        }
        Ok(())
    }
}

impl Ctx {
    /// `snapdir push [--store file://DIR] <path>`: walk `<path>` into a manifest
    /// and push its objects (objects-before-manifest, skip-if-present) to the
    /// resolved store. Prints the resulting snapshot id, matching the oracle.
    fn run_push(&self, path: Option<&Path>) -> Result<()> {
        self.log_transfer_config();
        let jobs = self.transfer_config()?.concurrency.get();
        let (meter, reporter) = self.start_progress(jobs);

        // `snapdir push --store … --id <id>` with no PATH: push a *staged* (or
        // already-fetched) snapshot identified by id. Its objects live in the
        // local cache as content-addressed blobs, not under a source tree, so we
        // can't walk a directory — materialize the snapshot from the cache into a
        // scratch tree (the cache re-hashes every object) and push that. This is
        // `fetch` in reverse and reuses the same store primitives.
        //
        // Without this branch, a path-less push fell through to `build_manifest`
        // /`resolve_root(None)`, which default to the current working directory —
        // so `push --id` silently snapshotted the CWD instead of honoring `--id`.
        if path.is_none() && self.globals.id.is_some() {
            let id = self.require_id()?;
            let cache = self.cache_store()?;
            let manifest = cache.get_manifest(id).with_context(|| {
                format!("manifest {id} not found in the local cache; stage or fetch it first")
            })?;
            let store_url = self.globals.store.as_deref().context(NO_STORE_CONFIGURED)?;
            // Under --dryrun: the id is a pure read-only lookup, so still print
            // it to stdout (the scriptable id-on-stdout contract). Skip the
            // scratch materialize (it's discarded), the store push, and the
            // catalog log — those are the only persistent writes.
            if self.globals.dryrun {
                reporter.finish();
                println!("{id}");
                if !self.globals.quiet {
                    eprintln!("dry-run: would push {id} to {store_url} (no writes performed)");
                }
                return Ok(());
            }
            if let Some(m) = &meter {
                m.set_total(total_object_bytes(&manifest));
                m.set_phase(Phase::Transfer);
            }
            let store = self.resolve_store(meter.clone())?;
            if self.store_is_external()? {
                // External adapters' emit-command contract expects --staging-dir
                // to be a SHARDED store root (`.objects/<sharded>` +
                // `.manifests/<sharded>`) — exactly the cache's layout, and the
                // staged snapshot already lives there in full (the cache's
                // manifest-written-last invariant: a present manifest implies
                // its objects are present, proven by `get_manifest` above).
                // Hand the cache root over directly; a scratch TREE would be
                // the wrong shape for the emitted script.
                store
                    .push(&manifest, &self.cache_dir())
                    .with_context(|| format!("pushing snapshot {id} to store"))?;
            } else {
                let scratch = ScratchDir::new("push")?;
                cache
                    .fetch_files(&manifest, scratch.path())
                    .with_context(|| format!("materializing staged snapshot {id}"))?;
                store
                    .push(&manifest, scratch.path())
                    .with_context(|| format!("pushing snapshot {id} to store"))?;
            }
            reporter.finish();
            println!("{id}");
            self.log_event("push", id, store_url, true)?;
            return Ok(());
        }

        // Push always uses the default checksum surface (b3sum / keyed-b3sum),
        // relative paths, follow symlinks — the same wiring `snapdir id` uses.
        if let Some(m) = &meter {
            m.set_phase(Phase::Hashing);
        }
        let manifest = self.build_manifest(
            path,
            false,
            false,
            None,
            &self.globals.exclude,
            meter.as_deref(),
        )?;
        let root = resolve_root(path).context("resolving push path")?;
        let id = snapshot_id(&manifest, &Blake3Hasher::new());
        let store_url = self.globals.store.as_deref().context(NO_STORE_CONFIGURED)?;
        // Under --dryrun: the snapshot id is a pure read-only computation, so
        // still print it to stdout. Skip the store push and the catalog log —
        // the only persistent writes here.
        if self.globals.dryrun {
            reporter.finish();
            println!("{id}");
            if !self.globals.quiet {
                eprintln!("dry-run: would push {id} to {store_url} (no writes performed)");
            }
            return Ok(());
        }
        if let Some(m) = &meter {
            m.set_total(total_object_bytes(&manifest));
            m.set_phase(Phase::Transfer);
        }
        let store = self.resolve_store(meter.clone())?;
        if self.store_is_external()? {
            // External adapters' emit-command contract expects --staging-dir to
            // be a SHARDED store root, not the source tree (the bash oracle
            // always pushes from the local cache, so its staging dir IS the
            // cache). Stage into the cache first — the same idempotent,
            // manifest-written-last write `stage` performs — then hand the
            // cache root to the shim: the emitted script reads only
            // `.manifests/<sharded id>` and that manifest's objects, so the
            // cache holding other snapshots (a superset) is fine.
            let cache = self.cache_store_with_meter(meter.clone())?;
            cache
                .push(&manifest, &root)
                .with_context(|| format!("staging snapshot {id} into the local cache"))?;
            store
                .push(&manifest, &self.cache_dir())
                .with_context(|| format!("pushing snapshot {id} to store"))?;
        } else {
            store
                .push(&manifest, &root)
                .with_context(|| format!("pushing snapshot {id} to store"))?;
        }
        reporter.finish();
        println!("{id}");
        // Mirror the oracle's `_snapdir_log_event "push" "$id" "$store"` (L359):
        // record the snapshot in the catalog at the store URI so `locations`/
        // `revisions`/`ancestors` see it. Best-effort and only when the catalog
        // is enabled (`--catalog` / `SNAPDIR_CATALOG`), exactly like the oracle's
        // `_snapdir_log_event` no-op when no catalog adapter is configured.
        self.log_event("push", &id, store_url, true)?;
        Ok(())
    }

    /// `snapdir fetch --store … --id <id>`: read+verify the manifest from the
    /// store, materialize its objects into a scratch tree (the store verifies
    /// each object's BLAKE3), then file the manifest+objects into the local
    /// cache so a later `checkout` can reconstruct the tree offline.
    fn run_fetch(&self) -> Result<()> {
        self.log_transfer_config();
        let jobs = self.transfer_config()?.concurrency.get();
        let (meter, reporter) = self.start_progress(jobs);
        if let Some(m) = &meter {
            m.set_phase(Phase::Transfer);
        }
        let result = self.fetch_inner(meter.as_ref());
        reporter.finish();
        result
    }

    /// `fetch` body without the verbose transfer-config banner. `run_pull`
    /// composes `fetch_inner` + `checkout_inner` so the banner prints exactly
    /// once (from `run_pull`), not once per leg. The optional `meter` drives the
    /// live progress line (set on the resolved store + the cache push leg).
    fn fetch_inner(&self, meter: Option<&Arc<Meter>>) -> Result<()> {
        // Fast path: a repeat `fetch`/`pull` of an id already fully cached
        // should perform ZERO store reads. But "manifest present" alone is NOT
        // sufficient: a cache OBJECT can be deleted out from under a present
        // manifest (a recovery scenario), and short-circuiting on the manifest
        // would leave that hole — a later `checkout` then fails with `object
        // not found`. So the fast path only fires when the manifest is cached
        // AND every object it references is present in the cache. If the
        // manifest is cached but some objects are missing, we fall through to
        // the store-resolution path below, which re-invokes the proven
        // store→cache fetch/transfer and HEALS the cache (objects-before-
        // manifest discipline preserved). The check is write-free, so the
        // early `CACHED` return still composes cleanly with `--dryrun`.
        //
        // We only consult the cache when an `--id` is actually present; with no
        // id there is nothing to look up, so we fall through and let the
        // original store-resolution path surface the canonical "missing --store
        // option" error first (preserving the frozen CLI error precedence).
        let cache = self.cache_store_with_meter(meter.cloned())?;
        let mut healing = false;
        if let Some(id) = self.globals.id.as_deref() {
            if let Ok(manifest) = cache.get_manifest(id) {
                if Self::missing_cache_objects(&manifest, &self.cache_dir()).is_empty() {
                    if self.globals.verbose && !self.globals.quiet {
                        eprintln!("CACHED: {id}");
                    }
                    return Ok(());
                }
                // Manifest is cached but at least one object is missing: do not
                // short-circuit. Fall through to fetch from the store, which
                // restores the absent objects (and re-commits the manifest).
                // The non-external cache write goes through `FileStore::push`,
                // which itself skips-if-manifest-present — so to let the heal
                // re-copy the missing objects we must drop the stale cached
                // manifest first (the re-fetched manifest is byte-identical:
                // same id). Do this only when actually healing, so the healthy
                // fast path above is untouched and a real `--dryrun` below stays
                // write-free (the removal happens after the dryrun guard).
                healing = true;
            }
        }

        let store = self.resolve_store(meter.cloned())?;
        let id = self.require_id()?;
        let manifest = store
            .get_manifest(id)
            .with_context(|| format!("fetching manifest {id} from store"))?;

        // Under --dryrun: the manifest read is fine, but skip materializing the
        // scratch tree (discarded) and the cache write — the only persistent
        // write in fetch.
        if self.globals.dryrun {
            if !self.globals.quiet {
                eprintln!("dry-run: would fetch {id} into the local cache (no writes performed)");
            }
            return Ok(());
        }

        if let Some(m) = meter {
            m.set_total(total_object_bytes(&manifest));
        }

        if self.store_is_external()? {
            // External adapters' emit-command contract expects --cache-dir to
            // be a SHARDED store root: the emitted script writes objects
            // straight to `.objects/<sharded>` under the dir it is handed (NOT
            // a dest tree). Point it at the cache root directly — no scratch
            // double-copy — then commit the manifest LAST via the cache's
            // `put_manifest` (which re-verifies the manifest hashes back to
            // `id`). This preserves the cache's manifest-written-last
            // invariant: a failed external fetch leaves orphan objects but
            // never a manifest claiming the snapshot is complete.
            self.split_read_hint(
                store
                    .fetch_files(&manifest, &self.cache_dir())
                    .with_context(|| format!("fetching objects for snapshot {id}")),
            )?;
            cache
                .put_manifest(id, &manifest)
                .with_context(|| format!("saving snapshot {id} to the local cache"))?;
        } else {
            // Materialize the verified objects into a scratch tree, then push
            // that tree into the cache store. This reuses the store's verify/
            // atomic persist on both legs and lands the cache in the same
            // sharded layout.
            let scratch = ScratchDir::new("fetch")?;
            self.split_read_hint(
                store
                    .fetch_files(&manifest, scratch.path())
                    .with_context(|| format!("fetching objects for snapshot {id}")),
            )?;

            // Heal: drop the stale cached manifest so `push`'s
            // skip-if-manifest-present does not short-circuit the re-copy of
            // the missing objects. `push` rewrites the byte-identical manifest
            // last, restoring the manifest-written-last invariant.
            if healing {
                let manifest_file = self
                    .cache_dir()
                    .join(snapdir_core::store::manifest_path(id));
                if manifest_file.exists() {
                    std::fs::remove_file(&manifest_file).with_context(|| {
                        format!(
                            "removing stale cached manifest {} before re-fetch",
                            manifest_file.display()
                        )
                    })?;
                }
            }

            cache
                .push(&manifest, scratch.path())
                .with_context(|| format!("saving snapshot {id} to the local cache"))?;
        }
        if self.globals.verbose && !self.globals.quiet {
            eprintln!("SAVED: {id}");
        }
        Ok(())
    }

    /// `snapdir checkout --id <id> <dest>`: read the manifest from the local
    /// cache, materialize the tree at `<dest>`, and restore each entry's
    /// permissions so the checked-out tree re-manifests to the same snapshot id.
    fn run_checkout(&self, dir: Option<&Path>) -> Result<()> {
        self.log_transfer_config();
        let jobs = self.transfer_config()?.concurrency.get();
        let (meter, reporter) = self.start_progress(jobs);
        if let Some(m) = &meter {
            m.set_phase(Phase::Transfer);
        }
        let result = self.checkout_inner(dir, meter.as_ref());
        reporter.finish();
        result
    }

    /// `checkout` body without the verbose transfer-config banner (see
    /// [`Self::fetch_inner`]). The optional `meter` drives the live progress
    /// line during the cache→dest materialization.
    fn checkout_inner(&self, dir: Option<&Path>, meter: Option<&Arc<Meter>>) -> Result<()> {
        let id = self.require_id()?;
        let dest = resolve_root(dir).context("resolving checkout destination")?;
        // Under --dryrun: skip materializing the destination tree and restoring
        // permissions — both write to the destination. The notice is emitted
        // before the cache manifest read so `pull --dryrun` (whose `fetch` leg
        // is itself a dry no-op and therefore leaves the cache unpopulated)
        // composes into a clean, write-free no-op rather than failing on a
        // missing cached manifest.
        if self.globals.dryrun {
            if !self.globals.quiet {
                eprintln!(
                    "dry-run: would check out {id} to {} (no writes performed)",
                    dest.display()
                );
            }
            return Ok(());
        }
        let cache = self.cache_store_with_meter(meter.cloned())?;
        let manifest = cache.get_manifest(id).with_context(|| {
            format!("manifest {id} not found locally; did you forget to fetch it?")
        })?;
        // Pre-flight the object pool so a checkout that cannot complete fails
        // with a message that locates the gap by FILE PATH (from the manifest),
        // not the bare object-not-found hash the store would surface mid-copy.
        // This is purely the offline/unhealable case: `pull` heals via its
        // fetch leg before reaching here, so a missing object at checkout means
        // the cache is genuinely incomplete and must be re-fetched.
        let missing = Self::missing_cache_objects(&manifest, &self.cache_dir());
        if let Some((checksum, path)) = missing.first() {
            anyhow::bail!(
                "snapdir: cannot check out {id}: object {checksum} for {path} is missing from the \
                 cache ({} object(s) absent); re-run `fetch`/`pull` to restore it",
                missing.len()
            );
        }
        if let Some(m) = meter {
            m.set_total(total_object_bytes(&manifest));
        }
        cache
            .fetch_files(&manifest, &dest)
            .with_context(|| format!("checking out snapshot {id} to {}", dest.display()))?;
        restore_permissions(&manifest, &dest)?;
        Ok(())
    }

    /// `snapdir pull` = fetch + checkout.
    fn run_pull(&self, path: Option<&Path>) -> Result<()> {
        // Banner ONCE for the whole pull, then run the two legs without their
        // own banners (`fetch_inner`/`checkout_inner`) so `pull --verbose`
        // emits exactly one transfer-config line. ONE reporter spans the whole
        // pull (fetch + checkout), not one per leg.
        self.log_transfer_config();
        let jobs = self.transfer_config()?.concurrency.get();
        let (meter, reporter) = self.start_progress(jobs);
        if let Some(m) = &meter {
            m.set_phase(Phase::Transfer);
        }
        let result = self
            .fetch_inner(meter.as_ref())
            .and_then(|()| self.checkout_inner(path, meter.as_ref()));
        reporter.finish();
        result
    }

    /// `snapdir verify --id <id>`: confirm the snapshot in the store is intact —
    /// the manifest hashes back to `id` and every referenced object is present
    /// and matches its checksum.
    ///
    /// The global `--purge` flag is *not* meaningful here: `verify` is a
    /// store-based integrity check and never mutates the cache. Rather than
    /// silently ignore the flag (as it did before), reject it with an actionable
    /// message pointing at `verify-cache --purge`, which is the only command that
    /// removes corrupt objects. The check runs before any store work so a bogus
    /// `--store`/`--id` still surfaces the purge rejection, not a store error.
    fn run_verify(&self) -> Result<()> {
        if self.globals.purge {
            anyhow::bail!(
                "snapdir: `verify` does not support --purge; use `verify-cache --purge` to remove corrupt objects from the local cache"
            );
        }
        let store = self.resolve_store(None)?;
        let id = self.require_id()?;
        let manifest = store
            .get_manifest(id)
            .with_context(|| format!("verifying manifest {id}"))?;
        // fetch_files re-hashes every object as it materializes it, so a
        // throwaway destination is a full object-integrity check.
        let scratch = ScratchDir::new("verify")?;
        store
            .fetch_files(&manifest, scratch.path())
            .with_context(|| format!("verifying objects for snapshot {id}"))?;
        Ok(())
    }

    /// `snapdir stage [<path>]`: cache the source tree's objects + manifest into
    /// the LOCAL cache without a remote store, then print the snapshot id (like
    /// the oracle's `stage`).
    ///
    /// The cache directory is itself a content-addressable store with the same
    /// `.objects`/`.manifests` sharded layout, so staging is just a `push` of the
    /// walked manifest to a [`FileStore`] rooted at the cache dir — reusing the
    /// proven objects-before-manifest, skip-if-present discipline with no new
    /// core/stores code. The resulting on-disk keys are exactly what
    /// `verify-cache` later checks (`stage` then `verify-cache` round-trips).
    fn run_stage(&self, path: Option<&Path>) -> Result<()> {
        self.log_transfer_config();
        let jobs = self.transfer_config()?.concurrency.get();
        let (meter, reporter) = self.start_progress(jobs);
        if let Some(m) = &meter {
            m.set_phase(Phase::Hashing);
        }
        // Stage uses the same default checksum surface as `push`/`id`: b3sum
        // (or keyed-b3sum), relative paths, follow symlinks. We take the walk's
        // CopyGuard side channel too: setting it on the cache FileStore lets
        // `persist` stat-validate each unchanged source and SKIP the redundant
        // post-copy re-hash (the stage clone-skip win). The guard map never
        // changes the manifest, the object bytes, or the snapshot id.
        let (manifest, copy_guards) = self.build_manifest_with_guards(
            path,
            false,
            false,
            None,
            &self.globals.exclude,
            meter.as_deref(),
        )?;
        let root = resolve_root(path).context("resolving stage path")?;
        let id = snapshot_id(&manifest, &Blake3Hasher::new());
        // Under --dryrun: the id is a pure read-only computation, so still print
        // it to stdout. Skip the cache write and the catalog log.
        if self.globals.dryrun {
            reporter.finish();
            println!("{id}");
            if !self.globals.quiet {
                eprintln!("dry-run: would stage {id} into the local cache (no writes performed)");
            }
            return Ok(());
        }
        if let Some(m) = &meter {
            m.set_total(total_object_bytes(&manifest));
            m.set_phase(Phase::Transfer);
        }
        // The walk's guard map is keyed by each plain regular file's absolute
        // working-tree path (`root.join(rel)`); `push` looks up
        // `source.join(rel)` with `source == root`, so the keys align exactly
        // and the StatGuarded clone-skip engages. An empty/missing guard for a
        // source ⇒ `Untrusted` ⇒ today's re-hash behavior.
        let cache = self
            .cache_store_with_meter(meter.clone())?
            .with_copy_guards(copy_guards);
        cache
            .push(&manifest, &root)
            .with_context(|| format!("staging snapshot {id} into the local cache"))?;
        reporter.finish();
        println!("{id}");
        // Mirror the oracle's `_snapdir_log_event "stage" "$id" "$base_dir"`
        // (`snapdir` L826): record the staged snapshot in the catalog at the
        // staged base directory (the absolute path `stage` walked). Best-effort
        // and a no-op unless a catalog is enabled, so it never changes the
        // stdout bytes above.
        self.log_event("stage", &id, &root.to_string_lossy(), true)?;
        Ok(())
    }

    /// `snapdir verify-cache [--purge]`: verify every object in the local cache
    /// via [`cache::verify_cache`].
    ///
    /// Reports each corrupt object on stderr (mirroring the oracle's
    /// `echo "Checksum mismatch for …" >&2`). When `--purge` is set the corrupt
    /// objects are removed. Matches the oracle's exit semantics
    /// (`snapdir_verify_cache`): exit non-zero whenever any object failed,
    /// whether or not it was purged.
    fn run_verify_cache(&self) -> Result<()> {
        let cache_dir = self.cache_dir();
        // `--purge` is FS-mutating (it deletes corrupt objects), so under
        // --dryrun never purge — pass `false` so the verification is read-only —
        // while still emitting the corruption report and preserving the
        // non-zero exit below.
        let purge = self.globals.purge && !self.globals.dryrun;
        if self.globals.purge && self.globals.dryrun {
            eprintln!("dry-run: would purge corrupt objects from the cache (no writes performed)");
        }
        let report = cache::verify_cache(&cache_dir, purge, &Blake3Hasher::new())
            .with_context(|| format!("verifying cache at {}", cache_dir.display()))?;

        for checksum in &report.corrupt {
            eprintln!("Checksum mismatch for {checksum}");
        }
        if purge && self.globals.verbose {
            for checksum in &report.purged {
                eprintln!("purged {checksum}");
            }
        }

        // Presence check: `verify_cache` above re-hashes the objects that ARE
        // on disk (catching corruption) but is blind to a manifest entry whose
        // object was DELETED — that is a silent gap a whole-cache byte scan
        // cannot see. Cross-check each cached manifest's file entries against
        // the cache and flag any object that is absent, naming both the object
        // address and the affected file path from the manifest (distinct from
        // the "Checksum mismatch" corrupt wording). Scoped to `--id` when given,
        // else every manifest in the cache.
        let missing = self.missing_cache_objects_for_verify(&cache_dir)?;
        for (checksum, path) in &missing {
            eprintln!("Missing object {checksum} for {path}");
        }

        if report.is_clean() && missing.is_empty() {
            return Ok(());
        }
        // Oracle: `failed=true` → `return 1`, even after purging. A missing
        // object is likewise a failure (the cache cannot reconstruct the tree).
        anyhow::bail!(
            "snapdir: {} corrupt + {} missing object(s) in the cache",
            report.corrupt.len(),
            missing.len()
        )
    }

    /// Collects the `(object, path)` pairs that `verify-cache` should report as
    /// MISSING: file entries referenced by a cached manifest whose object is
    /// absent from the cache. Scoped to `--id` when set; otherwise the union
    /// across every manifest in the cache, de-duplicated by object address
    /// (keeping the first affected path) and sorted for deterministic output.
    fn missing_cache_objects_for_verify(&self, cache_dir: &Path) -> Result<Vec<(String, String)>> {
        let cache = self.cache_store()?;
        let ids: Vec<String> = if let Some(id) = self.globals.id.as_deref() {
            vec![id.to_owned()]
        } else {
            cache
                .list_manifest_ids()
                .with_context(|| format!("listing cached manifests at {}", cache_dir.display()))?
        };

        let mut seen = std::collections::BTreeMap::new();
        for id in ids {
            // A manifest named by `--id` that is not cached is itself an error;
            // for the un-scoped sweep, `list_manifest_ids` only yields present
            // manifests, so a read failure there is genuinely exceptional.
            let manifest = cache
                .get_manifest(&id)
                .with_context(|| format!("reading cached manifest {id}"))?;
            for (checksum, path) in Self::missing_cache_objects(&manifest, cache_dir) {
                seen.entry(checksum).or_insert(path);
            }
        }
        Ok(seen.into_iter().collect())
    }

    /// `snapdir flush-cache`: empty the local cache via [`cache::flush_cache`]
    /// (objects + manifests). Idempotent on an already-empty / missing cache.
    fn run_flush_cache(&self) -> Result<()> {
        let cache_dir = self.cache_dir();
        // Under --dryrun: skip the destructive flush (it deletes every cached
        // object + manifest).
        if self.globals.dryrun {
            eprintln!(
                "dry-run: would flush the cache at {} (no writes performed)",
                cache_dir.display()
            );
            return Ok(());
        }
        cache::flush_cache(&cache_dir)
            .with_context(|| format!("flushing cache at {}", cache_dir.display()))?;
        Ok(())
    }

    /// `snapdir locations`: list every location tracked by the catalog (the
    /// latest record per location), one JSON line per record, in the
    /// catalog's order. Reproduces the original `snapdir locations` /
    /// `snapdir-sqlite3-catalog locations` query output.
    fn run_locations(&self) -> Result<()> {
        let Some(catalog) = self.open_catalog()? else {
            print_catalog_disabled();
            return Ok(());
        };
        for record in catalog.locations().context("querying catalog locations")? {
            println!("{}", locations_json_line(&record));
        }
        Ok(())
    }

    /// `snapdir ancestors --id <ID> [--location <LOC>]`: walk the `previous_id`
    /// chain for `<ID>` (optionally filtered to a location), `created_at DESC`,
    /// one frozen JSON line per ancestor (each line's `id` is the row's
    /// `previous_id`). Mirrors `snapdir ancestors --id=…`.
    fn run_ancestors(&self) -> Result<()> {
        let Some(catalog) = self.open_catalog()? else {
            print_catalog_disabled();
            return Ok(());
        };
        let id = self.require_id()?;
        let location = self.globals.location.as_deref();
        for record in catalog
            .ancestors(id, location)
            .with_context(|| format!("querying catalog ancestors of {id}"))?
        {
            println!("{}", ancestors_json_line(&record));
        }
        Ok(())
    }

    /// `snapdir revisions --location <LOC>`: list every snapshot id recorded at
    /// `<LOC>` (`created_at DESC`), one frozen JSON line per revision. Mirrors
    /// the oracle's `snapdir revisions --location=…`, whose location defaults to
    /// `--store` / the directory when `--location` is unset.
    fn run_revisions(&self) -> Result<()> {
        let Some(catalog) = self.open_catalog()? else {
            print_catalog_disabled();
            return Ok(());
        };
        // Oracle (L991-997): location = --location, else --store, else the dir;
        // empty → error.
        let location = self
            .globals
            .location
            .as_deref()
            .or(self.globals.store.as_deref())
            .context("missing --location option")?;
        for record in catalog
            .revisions(location)
            .with_context(|| format!("querying catalog revisions at {location}"))?
        {
            println!("{}", revisions_json_line(&record));
        }
        Ok(())
    }

    /// Logs a catalog event (`event`/`id`/`location`) to the resolved catalog.
    /// `push`/`stage` pass `allow_default = true` so a no-flag snapshot records
    /// to the default catalog (`<cache-dir>/default-catalog.redb`); `manifest`
    /// passes `false` so it only logs when a catalog is set EXPLICITLY (flag or
    /// `SNAPDIR_CATALOG`), never auto-recording to the default. A `none`/empty
    /// catalog (the disable sentinel) is always a silent no-op. Uses the shipped
    /// [`SystemClock`] (`created_at` = `YYYY-MM-DD HH:MM:SS.SSS`), so the JSON
    /// timestamps are byte-shaped like the oracle's.
    fn log_event(&self, event: &str, id: &str, location: &str, allow_default: bool) -> Result<()> {
        // Disabled sentinel or (for manifest) no explicit catalog → no-op,
        // never changing the stdout bytes the command already printed.
        let CatalogTarget::Enabled(db) = self.resolve_catalog(allow_default) else {
            return Ok(());
        };
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating catalog directory {}", parent.display()))?;
        }
        let catalog =
            Catalog::open(&db).with_context(|| format!("opening catalog at {}", db.display()))?;
        catalog
            .log(event, id, location, &SystemClock)
            .with_context(|| format!("recording catalog event {event} for {id}"))?;
        Ok(())
    }

    /// Opens the catalog for a read query (`revisions`/`locations`/`ancestors`),
    /// resolving the default catalog when `--catalog`/`SNAPDIR_CATALOG` is unset
    /// so a no-flag query reads the same default a no-flag `push`/`stage` wrote.
    /// Returns `None` when the catalog is the `none`/empty disable sentinel; the
    /// caller prints a "catalog disabled" message and exits 0.
    fn open_catalog(&self) -> Result<Option<Catalog>> {
        let CatalogTarget::Enabled(db) = self.resolve_catalog(true) else {
            return Ok(None);
        };
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating catalog directory {}", parent.display()))?;
        }
        let catalog =
            Catalog::open(&db).with_context(|| format!("opening catalog at {}", db.display()))?;
        Ok(Some(catalog))
    }

    /// Resolves the catalog selection into either an enabled redb path or the
    /// explicit disable sentinel. Precedence is flag > `SNAPDIR_CATALOG` env >
    /// default (clap's `env` feature folds flag+env into one resolved `Option`).
    ///
    /// - `Some("none")` / `Some("")` → [`CatalogTarget::Disabled`] (no logging,
    ///   no DB file created).
    /// - `Some(path-with-separator)` → that path verbatim.
    /// - `Some(bare-name)` → `<cache-dir>/<name>-catalog.redb`.
    /// - `None` (unset) → `<cache-dir>/default-catalog.redb` when `allow_default`
    ///   (the `push`/`stage` write-paths and the read queries), else
    ///   [`CatalogTarget::Disabled`] (e.g. `manifest`, which never auto-records
    ///   to the default).
    fn resolve_catalog(&self, allow_default: bool) -> CatalogTarget {
        match self.globals.catalog.as_deref() {
            Some(v) if v == "none" || v.is_empty() => CatalogTarget::Disabled,
            // A path-like value (contains a separator) is used verbatim;
            // otherwise treat it as a bare adapter name under the cache dir.
            Some(v) if v.contains(std::path::MAIN_SEPARATOR) => {
                CatalogTarget::Enabled(PathBuf::from(v))
            }
            Some(v) => CatalogTarget::Enabled(self.cache_dir().join(format!("{v}-catalog.redb"))),
            None if allow_default => {
                CatalogTarget::Enabled(self.cache_dir().join("default-catalog.redb"))
            }
            None => CatalogTarget::Disabled,
        }
    }

    /// Resolves `--store` to a concrete backend behind the [`Store`] trait,
    /// routing every supported scheme to its `snapdir-stores` implementation:
    ///
    /// - `file://` → [`FileStore`]
    /// - `s3://` → [`S3Store`]
    /// - `b2://` → [`B2Store`]
    /// - `gs://` → [`GcsStore`] (the oracle's hardcoded `gs`→`gcs` special case)
    /// - any other `<proto>://` → the external-store shim ([`ExternalStore`]),
    ///   which dispatches to a `snapdir-<proto>-store` binary on `PATH`.
    ///
    /// The scheme→adapter *decision* is delegated to the shared
    /// [`resolve_adapter`] router so the CLI never re-encodes the scheme map;
    /// this method only turns that decision into a constructed (possibly
    /// network/credential-backed) store, mapping construction/auth failures to
    /// `anyhow` errors. The pure decision is unit-tested via
    /// [`resolve_adapter`]; constructing remote stores here needs creds/network.
    ///
    /// # Errors
    ///
    /// Returns an error when `--store` is missing, its protocol is invalid, or
    /// the concrete store cannot be constructed (e.g. credentials/region cannot
    /// be resolved for a remote backend).
    fn resolve_store(&self, meter: Option<Arc<Meter>>) -> Result<Box<dyn Store>> {
        // When `--objects-store` is set, objects route to the shared pool and
        // manifests to `--store` via an in-process `SplitStore`. When absent the
        // store is the colocated `--store` exactly as before — byte-for-byte
        // unchanged.
        if self.globals.objects_store.is_some() {
            return Ok(Box::new(self.resolve_split_store(meter)?));
        }
        let store_url = self.globals.store.as_deref().context(NO_STORE_CONFIGURED)?;
        let adapter = resolve_adapter(store_url).context("resolving --store protocol")?;
        let config = self.transfer_config_for(Some(adapter.name()))?;
        store_for_adapter(&adapter, store_url, config, meter)
    }

    /// Builds the `SplitStore` for a split push/fetch/pull: objects go to the
    /// `--objects-store` pool, manifests to `--store`. BOTH sides are built via
    /// [`stream_store_for_adapter`], so an external `custom://` on EITHER side is
    /// rejected with the same actionable error class `sync` uses. `--store`
    /// missing → a clear error (not a panic).
    ///
    /// Only called when `--objects-store` is set.
    fn resolve_split_store(&self, meter: Option<Arc<Meter>>) -> Result<SplitStore> {
        let objects_url = self
            .globals
            .objects_store
            .as_deref()
            .context("missing --objects-store option")?;
        let store_url = self.globals.store.as_deref().context(
            "missing --store option: --objects-store sets the object pool, but the manifest \
             location (--store / $SNAPDIR_STORE) is still required",
        )?;

        let objects_adapter =
            resolve_adapter(objects_url).context("resolving --objects-store protocol")?;
        let objects_config = self.transfer_config_for(Some(objects_adapter.name()))?;
        let objects =
            stream_store_for_adapter(&objects_adapter, objects_url, objects_config, meter.clone())?;

        let manifests_adapter = resolve_adapter(store_url).context("resolving --store protocol")?;
        let manifests_config = self.transfer_config_for(Some(manifests_adapter.name()))?;
        let manifests =
            stream_store_for_adapter(&manifests_adapter, store_url, manifests_config, meter)?;

        Ok(SplitStore::from_boxed(objects, manifests))
    }

    /// `true` when the resolved `--store` URL routes to a third-party
    /// `snapdir-<proto>-store` binary ([`Adapter::External`]).
    ///
    /// The external emit-command contract hands `--staging-dir`/`--cache-dir`
    /// to the emitted scripts as SHARDED store roots (`.objects/<sharded>` +
    /// `.manifests/<sharded>` — the local cache's exact layout), not the
    /// source/dest TREES the in-process stores take; `run_push` and
    /// `fetch_inner` branch on this to swap the tree for the cache root.
    /// Delegates the scheme decision to the same [`resolve_adapter`] router
    /// [`Self::resolve_store`] uses, so the CLI never re-encodes the scheme
    /// map.
    ///
    /// # Errors
    ///
    /// Returns an error when `--store` is missing or its protocol is invalid —
    /// the identical errors `resolve_store` surfaces, so calling this after a
    /// successful `resolve_store` introduces no new failure mode.
    fn store_is_external(&self) -> Result<bool> {
        // A split store (`--objects-store` set) is always in-process: both sides
        // are built via `stream_store_for_adapter`, which rejects external URLs.
        // So a split store never takes the external emit-command push/fetch path.
        if self.globals.objects_store.is_some() {
            return Ok(false);
        }
        let store_url = self.globals.store.as_deref().context(NO_STORE_CONFIGURED)?;
        let adapter = resolve_adapter(store_url).context("resolving --store protocol")?;
        Ok(matches!(adapter, Adapter::External { .. }))
    }

    /// Builds the [`TransferConfig`] from the global `--jobs` / `--limit-rate`
    /// / `--adaptive` / `--max-jobs` flags. `--jobs` unset or `0` falls back to
    /// the stores' auto-detected default concurrency; `--limit-rate` unset means
    /// unlimited bandwidth.
    ///
    /// Without `--adaptive`, the result is byte-for-byte the historical
    /// non-adaptive config: `TransferConfig::new(concurrency, max_bytes_per_sec)`
    /// ([`TransferAdaptivePolicy::Off`]). With `--adaptive=f` the same base
    /// config gains an [`TransferAdaptivePolicy::On`] policy whose `fraction` is
    /// `f` and whose `ceiling` is `--max-jobs` if set, else an explicit
    /// `--jobs N`, else the auto concurrency (the ceiling is clamped to
    /// [`ADAPTIVE_CEILING_CAP`]). `--limit-rate` is carried unchanged as the
    /// rate cap in both paths.
    ///
    /// # Errors
    ///
    /// Returns an error when `--limit-rate` cannot be parsed as a byte rate.
    fn transfer_config(&self) -> Result<TransferConfig> {
        self.transfer_config_for(None)
    }

    /// Builds the [`TransferConfig`], layering in the per-backend rate-limit
    /// defaults and the operator-configured retry policy for `scheme`.
    ///
    /// `scheme` is the canonical adapter name (`"file"`, `"s3"`, `"b2"`,
    /// `"gcs"`); `None` (and any scheme with no published
    /// [`limits::for_scheme`] defaults, e.g. `file`) leaves the byte-rate /
    /// request-rate caps exactly where [`Self::transfer_config`] historically
    /// left them. The retry policy is always installed (it defaults to
    /// [`RetryPolicy::default`], inert until the stores consume it), and the
    /// request-rate cap defaults to `None` unless a backend default or an
    /// operator override applies — so with no new flags/env and a `None` /
    /// capless scheme the result is byte-for-byte the historical config.
    ///
    /// # Errors
    ///
    /// Returns an error when `--limit-rate` cannot be parsed as a byte rate.
    fn transfer_config_for(&self, scheme: Option<&str>) -> Result<TransferConfig> {
        // The byte/request caps layer the operator override over the per-backend
        // default; `--limit-rate` is parsed here so a bad value still errors.
        let limit_rate = match self.globals.limit_rate.as_deref() {
            Some(rate) => Some(parse_rate(rate)?),
            None => None,
        };
        let (max_requests_per_sec, max_bytes_per_sec) = match scheme {
            Some(scheme) => self.resolve_rate_limits(scheme, limit_rate),
            // No scheme: never apply a backend default; preserve historical
            // behavior (request cap None, byte cap = --limit-rate as-is).
            None => (None, limit_rate),
        };
        let auto = TransferConfig::default().concurrency.get();
        let concurrency = match self.globals.jobs {
            Some(n) if n > 0 => n,
            // Unset or 0 => auto (the stores' default).
            _ => auto,
        };
        let base = TransferConfig::new(concurrency, max_bytes_per_sec)
            .with_retry(self.resolve_retry_policy());
        let base = TransferConfig {
            max_requests_per_sec,
            ..base
        };
        match self.globals.adaptive {
            // No `--adaptive`: byte-for-byte the historical non-adaptive config.
            None => Ok(base),
            // `--adaptive[=f]`: opt into adaptive tuning. The ceiling is
            // `--max-jobs` if set, else an explicit `--jobs N`, else `auto`,
            // clamped to a sane upper bound.
            Some(fraction) => {
                let ceiling = self
                    .globals
                    .max_jobs
                    .or(self.globals.jobs.filter(|&n| n > 0))
                    .unwrap_or(auto)
                    .clamp(1, ADAPTIVE_CEILING_CAP);
                Ok(base.with_adaptive(TransferAdaptivePolicy::On { fraction, ceiling }))
            }
        }
    }

    /// Resolves the network-retry schedule with `--max-retries` /
    /// `--retry-base-ms` / `--retry-max-ms` flags taking precedence over the
    /// matching `SNAPDIR_MAX_RETRIES` / `SNAPDIR_RETRY_BASE_MS` /
    /// `SNAPDIR_RETRY_MAX_MS` env vars, falling back to
    /// [`RetryPolicy::default`] (5 attempts / 250ms base / 30000ms cap).
    fn resolve_retry_policy(&self) -> RetryPolicy {
        let default = RetryPolicy::default();
        let max_attempts = self
            .globals
            .max_retries
            .or_else(|| env_u64("SNAPDIR_MAX_RETRIES").and_then(|n| u32::try_from(n).ok()))
            .unwrap_or(default.max_attempts);
        let base_ms = self
            .globals
            .retry_base_ms
            .or_else(|| env_u64("SNAPDIR_RETRY_BASE_MS"))
            .map_or(default.base, std::time::Duration::from_millis);
        let cap_ms = self
            .globals
            .retry_max_ms
            .or_else(|| env_u64("SNAPDIR_RETRY_MAX_MS"))
            .map_or(default.cap, std::time::Duration::from_millis);
        RetryPolicy {
            max_attempts,
            base: base_ms,
            cap: cap_ms,
        }
    }

    /// Resolves the `(req/s, bytes/s)` caps for `scheme`, layering an operator
    /// override over the published [`limits::for_scheme`] default over global
    /// `None`.
    ///
    /// - req/s: `--max-requests` / `SNAPDIR_MAX_REQUESTS` (when `> 0`), else the
    ///   conservative `min(read_rps, write_rps)` backend default, else `None`.
    /// - bytes/s: `limit_rate` (the already-parsed `--limit-rate` /
    ///   `SNAPDIR_LIMIT_RATE`), else the conservative `min(read_bps, write_bps)`
    ///   backend default, else `None`.
    ///
    /// `TransferConfig` carries a single req/s and a single bytes/s, so the
    /// conservative minimum of the read/write caps is chosen per dimension.
    fn resolve_rate_limits(
        &self,
        scheme: &str,
        limit_rate: Option<u64>,
    ) -> (Option<u64>, Option<u64>) {
        let backend = limits::for_scheme(scheme);
        let req_override = self
            .globals
            .max_requests
            .or_else(|| env_u64("SNAPDIR_MAX_REQUESTS"))
            .filter(|&n| n > 0);
        let max_requests_per_sec =
            req_override.or_else(|| min_opt(backend.read_rps, backend.write_rps));
        let max_bytes_per_sec = limit_rate.or_else(|| min_opt(backend.read_bps, backend.write_bps));
        (max_requests_per_sec, max_bytes_per_sec)
    }

    /// Field observability for the transfer commands: under `--verbose`, print
    /// the *effective* transfer concurrency (and `--limit-rate`, if set) ONCE to
    /// stderr so an operator can confirm in the field that concurrent transfers +
    /// bandwidth limiting are actually in effect. Stdout is untouched (the
    /// scriptable id-on-stdout contract stays byte-stable); the deterministic
    /// concurrency proof is the unit test, this is just field observability.
    fn log_transfer_config(&self) {
        // `--quiet` suppresses every stderr banner, including this one (it wins
        // over `--verbose`).
        if self.globals.quiet || !self.globals.verbose {
            return;
        }
        // A bad --limit-rate would already have failed transfer_config() in the
        // command body; here we only render for humans, so on the off chance the
        // config can't resolve we simply skip the diagnostic rather than abort.
        let Ok(config) = self.transfer_config() else {
            return;
        };
        match config.adaptive {
            TransferAdaptivePolicy::On { fraction, ceiling } => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                let pct = (fraction * 100.0).round() as u64;
                match self.globals.limit_rate.as_deref() {
                    Some(rate) => eprintln!(
                        "adaptive: target {pct}% of capacity, ceiling {ceiling}, limit {rate}"
                    ),
                    None => eprintln!("adaptive: target {pct}% of capacity, ceiling {ceiling}"),
                }
            }
            TransferAdaptivePolicy::Off => {
                let concurrency = config.concurrency.get();
                match self.globals.limit_rate.as_deref() {
                    Some(rate) => eprintln!("transfers: {concurrency} concurrent, limit {rate}"),
                    None => eprintln!("transfers: {concurrency} concurrent"),
                }
            }
        }
    }

    /// The effective [`ColorChoice`] from the `--color` flag. The value is
    /// already validated to `auto`/`always`/`never` at parse time (clap
    /// `ValueEnum`), so a bogus `--color bogus` was rejected (exit 2) before we
    /// get here.
    fn color_choice(&self) -> ColorChoice {
        self.globals.color.resolve()
    }

    /// Builds the live progress dashboard for a transfer command.
    ///
    /// Returns a `(meter, reporter)` pair. When the run is interactive (stderr
    /// is a TTY, not `--no-progress`/`--quiet`, `TERM` is not `dumb`) the meter
    /// is `Some` and the reporter spawns the render thread; otherwise the meter
    /// is `None` and the reporter is inert (no thread, no output). The caller
    /// threads the optional meter into the walk and the store, and ALWAYS calls
    /// [`ProgressReporter::finish`] before any stdout write so the id stays
    /// clean.
    /// Resolves the effective walk concurrency the same way the core walk does
    /// (`available_parallelism` capped at 16) when `--walk-jobs` is unset/0. Used
    /// only for the progress dashboard's `jobs <in>/<N>` readout on the
    /// walk-driven commands (`manifest`/`id`); the actual traversal concurrency
    /// is resolved inside the core walk from the same `WalkOptions.walk_jobs`.
    fn walk_jobs(&self) -> usize {
        match self.globals.walk_jobs {
            Some(n) if n > 0 => n,
            _ => std::thread::available_parallelism()
                .map_or(1, std::num::NonZeroUsize::get)
                .clamp(1, 16),
        }
    }

    fn start_progress(&self, jobs: usize) -> (Option<Arc<Meter>>, ProgressReporter) {
        let is_tty = std::io::stderr().is_terminal();
        let active = should_render(
            is_tty,
            self.globals.no_progress || self.globals.quiet,
            std::env::var("TERM").ok().as_deref(),
        );
        if active {
            let meter = Arc::new(Meter::new());
            // Prime the phase to `Discovering` so the reporter's guaranteed
            // first frame shows the enumeration phase even on a tiny/fast tree
            // whose walk completes inside a single render tick. The walk
            // re-asserts `Discovering` then flips to `Hashing` itself; this is
            // purely advisory and never perturbs output.
            meter.set_phase(Phase::Discovering);
            let color = use_color(
                self.color_choice(),
                is_tty,
                std::env::var_os("NO_COLOR").is_some(),
            );
            // Modern (unicode) glyphs unless the terminal is explicitly dumb.
            let ascii = matches!(std::env::var("TERM").as_deref(), Ok("dumb"));
            // Thread the true adaptive politeness fraction (`--adaptive[=F]`)
            // into the renderer's adaptive readout; `None` when not adaptive.
            let reporter = ProgressReporter::start(
                Arc::clone(&meter),
                jobs,
                true,
                color,
                ascii,
                self.globals.adaptive,
            );
            (Some(meter), reporter)
        } else {
            let reporter =
                ProgressReporter::start(Arc::new(Meter::new()), jobs, false, false, false, None);
            (None, reporter)
        }
    }

    /// `snapdir sync --id <id> --from <store> --to <store>`: copy a snapshot
    /// (its manifest + every referenced object) directly between two in-process
    /// stores, streaming through memory with no local staging. Wires
    /// [`snapdir_stores::sync_snapshot`]; objects already present at the
    /// destination are skipped (content-addressed). Honors `--dryrun` (no writes)
    /// and the `--jobs` / `--limit-rate` transfer tuning (applied to the single
    /// store→store pipe). Both endpoints must be in-process stores
    /// (file/s3/b2/gcs); external `snapdir-*-store` URLs are rejected.
    ///
    /// Output convention (consistent with `push`/`stage`): on a real sync the id
    /// is printed to STDOUT; a human summary always goes to STDERR.
    /// Builds the `StreamStore` for ONE side of a `sync`. When `objects_url` is
    /// present, that side is a split store: objects route to `objects_url`,
    /// manifests to `manifest_url` — wrapped in an in-process [`SplitStore`]
    /// exactly as [`Self::resolve_split_store`] does for `--objects-store`. When
    /// absent, the side is the plain colocated store at `manifest_url`,
    /// byte-for-byte as before. BOTH sides of a split are built via
    /// [`stream_store_for_adapter`], so an external `custom://` on either is
    /// rejected — identical to non-split sync.
    ///
    /// `side` names the side (`--from`/`--to`) for error context only.
    fn sync_side_store(
        &self,
        manifest_url: &str,
        objects_url: Option<&str>,
        side: &str,
    ) -> Result<Box<dyn StreamStore + Sync>> {
        let Some(objects_url) = objects_url else {
            let adapter = resolve_adapter(manifest_url)
                .with_context(|| format!("resolving {side} store protocol"))?;
            let config = self.transfer_config_for(Some(adapter.name()))?;
            return stream_store_for_adapter(&adapter, manifest_url, config, None);
        };

        let objects_adapter = resolve_adapter(objects_url)
            .with_context(|| format!("resolving {side}-objects protocol"))?;
        let objects_config = self.transfer_config_for(Some(objects_adapter.name()))?;
        let objects =
            stream_store_for_adapter(&objects_adapter, objects_url, objects_config, None)?;

        let manifests_adapter = resolve_adapter(manifest_url)
            .with_context(|| format!("resolving {side} store protocol"))?;
        let manifests_config = self.transfer_config_for(Some(manifests_adapter.name()))?;
        let manifests =
            stream_store_for_adapter(&manifests_adapter, manifest_url, manifests_config, None)?;

        Ok(Box::new(SplitStore::from_boxed(objects, manifests)))
    }

    fn run_sync(
        &self,
        from_url: &str,
        to_url: &str,
        from_objects: Option<&str>,
        to_objects: Option<&str>,
    ) -> Result<()> {
        let id = self.require_id()?;
        anyhow::ensure!(
            from_url != to_url,
            "sync --from and --to must differ (both are {from_url})"
        );

        let from_adapter = resolve_adapter(from_url).context("resolving --from store protocol")?;
        // Each endpoint gets its own per-backend rate-limit defaults; the shared
        // sync pipe (concurrency, retry, byte budget accounting) is driven by the
        // source-side config below.
        // Per-side stores: when `--from-objects`/`--to-objects` is present, THAT
        // side is a split store (objects = the flag's pool, manifests = the
        // `--from`/`--to` URI), built exactly like `resolve_split_store`. Absent =>
        // a plain colocated store as before. A `SplitStore` IS a `StreamStore`, so
        // the `sync_snapshot` engine is unchanged.
        let from_store = self.sync_side_store(from_url, from_objects, "--from")?;
        let to_store = self.sync_side_store(to_url, to_objects, "--to")?;
        let config = self.transfer_config_for(Some(from_adapter.name()))?;

        self.log_transfer_config();

        // The sync pipe is store→store; the meter is threaded directly into
        // `sync_snapshot` (it accounts the bytes it copies) rather than onto the
        // endpoint stores.
        let jobs = config.concurrency.get();
        let (meter, reporter) = self.start_progress(jobs);
        if let Some(m) = &meter {
            m.set_phase(Phase::Transfer);
        }

        let report = snapdir_stores::sync_snapshot(
            &*from_store,
            &*to_store,
            id,
            &config,
            self.globals.dryrun,
            meter.as_deref(),
        );

        // Clear the live line before ANY stdout/stderr summary write.
        reporter.finish();
        let report =
            report.with_context(|| format!("syncing snapshot {id} from {from_url} to {to_url}"))?;

        if report.dry_run {
            if !self.globals.quiet {
                eprintln!(
                    "dry-run: would copy {} object(s) for {id}",
                    report.objects_copied
                );
            }
        } else {
            // Scriptable id-on-stdout contract (matches push/stage).
            println!("{id}");
            if !self.globals.quiet {
                eprintln!(
                    "synced {id}: {} copied, {} skipped ({} bytes)",
                    report.objects_copied, report.objects_skipped, report.bytes_copied
                );
            }
        }
        Ok(())
    }

    /// `snapdir diff --from <ref>… --to <ref>… [--all] [--json] [--exit-code]
    /// [--on-conflict <error|last-wins>]`: compare two SIDES, each a UNION of
    /// one-or-more manifests, and report file-level differences.
    ///
    /// MANIFESTS ONLY: every ref is resolved via [`Self::resolve_side`], which
    /// calls EXCLUSIVELY [`StreamStore::list_manifest_ids`] (to enumerate a
    /// store) and [`Store::get_manifest`] (BLAKE3-verified) — it NEVER
    /// constructs an object store, calls `get_object`, or fetches a blob. So a
    /// store whose `.objects/` pool is absent/garbage still diffs correctly.
    ///
    /// The comparison itself is the pure map-diff in [`crate::diff`]: each side
    /// unions to a `path -> ManifestEntry` map (collisions handled per
    /// `on_conflict`), then [`crate::diff::classify`] yields the A/D/M(/=) rows.
    fn run_diff(
        &self,
        from: &[String],
        to: &[String],
        all: bool,
        json: bool,
        exit_code: bool,
        on_conflict: crate::diff::OnConflict,
    ) -> Result<()> {
        use crate::diff::{classify, render_json, render_porcelain, union_side};

        // Resolve each side to its set of manifests (manifests-only reads), then
        // union into a path map. A differing-content intra-side collision under
        // OnConflict::Error surfaces as an actionable error naming the path.
        let from_manifests = self.resolve_side(from).context("resolving --from side")?;
        let to_manifests = self.resolve_side(to).context("resolving --to side")?;

        let from_map = union_side(&from_manifests, on_conflict).map_err(|c| {
            anyhow::anyhow!(
                "intra-side conflict on --from: the path {:?} has differing content across \
                 two refs (collision); pass --on-conflict last-wins to let the last ref win",
                c.path
            )
        })?;
        let to_map = union_side(&to_manifests, on_conflict).map_err(|c| {
            anyhow::anyhow!(
                "intra-side conflict on --to: the path {:?} has differing content across \
                 two refs (collision); pass --on-conflict last-wins to let the last ref win",
                c.path
            )
        })?;

        let rows = classify(&from_map, &to_map, all);

        // A "difference" for --exit-code is any A/D/M row (never an unchanged
        // row that only --all surfaces).
        let has_difference = rows
            .iter()
            .any(|r| r.status != crate::diff::Status::Unchanged);

        let out = std::io::stdout();
        let mut out = out.lock();
        if json {
            writeln!(out, "{}", render_json(&rows))?;
        } else {
            // render_porcelain already terminates each line with `\n`.
            write!(out, "{}", render_porcelain(&rows))?;
        }
        out.flush()?;

        // git `diff --exit-code` semantics: exit 1 on any difference, AFTER the
        // porcelain has been written. Without the flag, always exit 0.
        if exit_code && has_difference {
            drop(out);
            std::process::exit(1);
        }
        Ok(())
    }

    /// Resolves one diff SIDE (a list of refs) to its set of manifests, reading
    /// MANIFESTS ONLY.
    ///
    /// For each ref: build the store via the same [`stream_store_for_adapter`]
    /// resolver the plumbing commands use, then enumerate it with
    /// [`StreamStore::list_manifest_ids`] and read each manifest with
    /// [`Store::get_manifest`] (which BLAKE3-verifies the bytes hash back to the
    /// id). When the global `--id` is set AND that id is present in this ref's
    /// store, the side is PINNED to exactly that one manifest (otherwise the
    /// whole store is unioned). An empty/missing manifest store contributes
    /// nothing (its `list_manifest_ids` yields zero ids).
    ///
    /// NO object store is ever constructed and no blob is ever fetched — the
    /// only store methods called are `list_manifest_ids` and `get_manifest`.
    fn resolve_side(&self, refs: &[String]) -> Result<Vec<Manifest>> {
        let pinned_id = self.globals.id.as_deref();
        let mut manifests = Vec::new();
        for store_url in refs {
            let adapter =
                resolve_adapter(store_url).context("resolving a --from/--to ref protocol")?;
            let config = self.transfer_config_for(Some(adapter.name()))?;
            // MANIFESTS-ONLY: a StreamStore exposes both list_manifest_ids and
            // get_manifest; we never touch its object surface.
            let store = stream_store_for_adapter(&adapter, store_url, config, None)?;

            let all_ids = store
                .list_manifest_ids()
                .with_context(|| format!("listing manifests in {store_url}"))?;

            // If --id is set and this store holds it, pin to that single
            // manifest; else union the whole store's manifests.
            let ids: Vec<String> = match pinned_id {
                Some(id) if all_ids.iter().any(|x| x == id) => vec![id.to_owned()],
                _ => all_ids,
            };

            for id in &ids {
                let manifest = store
                    .get_manifest(id)
                    .with_context(|| format!("reading manifest {id} from {store_url}"))?;
                manifests.push(manifest);
            }
        }
        Ok(manifests)
    }

    /// `snapdir objects-needed --store <url>` (hidden plumbing): read candidate
    /// checksums from stdin (one per line) and print the subset the store does
    /// NOT hold, one per line, preserving first-occurrence order.
    ///
    /// Fail-closed: every line is validated against `^[0-9a-f]{64}$` BEFORE the
    /// store is even resolved — a malformed request errors with NOTHING on
    /// stdout (it must never be partially answered). Empty input is valid and
    /// prints nothing. Dedup happens here because the lib's
    /// [`StreamStore::objects_needed`] documents dedup as the caller's job.
    fn run_objects_needed(&self) -> Result<()> {
        // Read to EOF + validate everything FIRST (fail closed, no output yet).
        let ids = read_checksum_lines(std::io::stdin().lock())
            .context("reading object checksums from stdin")?;
        let ids = dedupe_preserving_order(ids);
        let store = self.resolve_stream_store()?;
        let needed = store
            .objects_needed(&ids)
            .context("querying the store for absent objects")?;
        // Only now does anything reach stdout: the exact absent subset, in
        // first-occurrence input order (the contract the accel diff relies on).
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for id in needed {
            writeln!(out, "{id}")?;
        }
        Ok(())
    }

    /// `snapdir send-pack --store <url> --ids <FILE|-> [--manifest-id <id>]`
    /// (hidden plumbing): emit a SNAPPACK stream of the listed objects (and the
    /// optional manifest, last) to RAW stdout — progress/log lines go to stderr
    /// only, the byte stream is the entire stdout contract.
    ///
    /// The id list gets the same fail-closed validation as `objects-needed`
    /// (and is deduped — [`write_pack_with_format`] documents dedup as the
    /// caller's job). Any failure, including a missing object, makes
    /// [`write_pack_with_format`] abort BEFORE the `end` trailer, so the piped
    /// `receive-pack` fails too: no silent partial transfer.
    ///
    /// `format` is the resolved on-wire encoding ([`PackFormat::V1`] by default
    /// — byte-identical to the historical stream — or [`PackFormat::Zstd`] when
    /// the hidden `--pack-format zstd` flag opts in). `receive-pack` needs no
    /// matching flag: it sniffs the magic and accepts either form.
    fn run_send_pack(
        &self,
        ids: &Path,
        manifest_id: Option<&str>,
        format: PackFormat,
    ) -> Result<()> {
        // Read + validate the id list (file path or `-` = stdin) before any
        // store work; a malformed list emits not a single pack byte.
        let ids = if ids == Path::new("-") {
            read_checksum_lines(std::io::stdin().lock())
                .context("reading object checksums from stdin")?
        } else {
            let file = std::fs::File::open(ids)
                .with_context(|| format!("opening --ids file {}", ids.display()))?;
            read_checksum_lines(std::io::BufReader::new(file))
                .with_context(|| format!("reading object checksums from {}", ids.display()))?
        };
        let ids = dedupe_preserving_order(ids);
        if let Some(id) = manifest_id {
            anyhow::ensure!(
                is_hex64(id),
                "invalid --manifest-id {id:?}: expected 64 lowercase hex characters"
            );
        }
        let store = self.resolve_stream_store()?;
        let stdout = std::io::stdout();
        let report = write_pack_with_format(&*store, &ids, manifest_id, format, stdout.lock())
            .context("writing pack stream to stdout")?;
        if !self.globals.quiet {
            eprintln!(
                "sent pack: {} object(s){}",
                report.objects_written,
                if report.manifest_written {
                    " + manifest"
                } else {
                    ""
                }
            );
        }
        Ok(())
    }

    /// `snapdir receive-pack --store <url> [--require-manifest <id>]` (hidden
    /// plumbing): consume a SNAPPACK stream from stdin into the store.
    ///
    /// `file://` stores (the hot ssh path) stream each payload through
    /// [`FileSink`]'s O(1)-memory temp-sibling discipline; every other
    /// [`StreamStore`] goes through the generic buffered [`StreamSink`]. All
    /// verification (incremental BLAKE3, manifest-commits-only-after-`end`)
    /// lives in [`read_pack`] — the CLI adds nothing to the wire logic.
    ///
    /// `--require-manifest <id>` fails the command (after the read) unless the
    /// stream committed a manifest with EXACTLY that id; without the flag an
    /// objects-only stream is success. Summary goes to stderr; stdout stays
    /// silent.
    fn run_receive_pack(&self, require_manifest: Option<&str>) -> Result<()> {
        // Validate the required id up front (fail closed, before any read).
        if let Some(id) = require_manifest {
            anyhow::ensure!(
                is_hex64(id),
                "invalid --require-manifest {id:?}: expected 64 lowercase hex characters"
            );
        }
        let store_url = self.globals.store.as_deref().context(NO_STORE_CONFIGURED)?;
        let adapter = resolve_adapter(store_url).context("resolving --store protocol")?;
        let config = self.transfer_config_for(Some(adapter.name()))?;
        let stdin = std::io::stdin();

        let (report, committed) = if matches!(adapter, Adapter::File) {
            // file:// — the hot ssh path: stream payloads straight to disk.
            // `SNAPDIR_FSYNC` selects crash-durability (default `batch`); the
            // barrier fires through `RecordingSink::flush_barrier` before the
            // manifest commits, so a durable manifest implies durable objects.
            let durability = fsync_durability_from_env()?;
            let store = FileStore::new_with_config(store_url, config);
            let mut sink = FileSink::new(&store).with_durability(durability);
            read_pack_recording(stdin.lock(), &mut sink)?
        } else {
            // Any other StreamStore: one buffered record at a time. External
            // `snapdir-*-store` URLs are rejected by the resolver below.
            let store = stream_store_for_adapter(&adapter, store_url, config, None)?;
            let mut sink = StreamSink::new(&*store);
            read_pack_recording(stdin.lock(), &mut sink)?
        };

        if let Some(required) = require_manifest {
            match committed.as_deref() {
                Some(id) if id == required => {}
                Some(other) => anyhow::bail!(
                    "pack committed manifest {other}, but --require-manifest expected {required}"
                ),
                None => anyhow::bail!(
                    "pack stream carried no manifest record, but --require-manifest {required} \
                     was given"
                ),
            }
        }
        if !self.globals.quiet {
            eprintln!(
                "received pack: {} object(s) written, {} skipped{}",
                report.objects_written,
                report.objects_skipped,
                match &committed {
                    Some(id) => format!(", manifest {id}"),
                    None => String::new(),
                }
            );
        }
        Ok(())
    }

    /// Resolves the global `--store` to a concrete [`StreamStore`] for the
    /// plumbing subcommands, via the same [`stream_store_for_adapter`] router
    /// `sync` uses (external `snapdir-*-store` URLs are rejected there — they
    /// have no in-process streaming surface).
    fn resolve_stream_store(&self) -> Result<Box<dyn StreamStore + Sync>> {
        // When `--objects-store` is set, wrap the pool (objects) + `--store`
        // (manifests) in an in-process `SplitStore`; both sides go through
        // `stream_store_for_adapter`, so external URLs are rejected on either
        // side. When absent, behavior is unchanged.
        if self.globals.objects_store.is_some() {
            return Ok(Box::new(self.resolve_split_store(None)?));
        }
        let store_url = self.globals.store.as_deref().context(NO_STORE_CONFIGURED)?;
        let adapter = resolve_adapter(store_url).context("resolving --store protocol")?;
        let config = self.transfer_config_for(Some(adapter.name()))?;
        stream_store_for_adapter(&adapter, store_url, config, None)
    }

    /// Contextualizes a store-read error (`fetch_files`) so a SPLIT snapshot —
    /// one whose objects were pushed to a separate `--objects-store` pool — fails
    /// with a hint that names the objects-store / split concept, not a bare
    /// `object not found: <hash>` that leaves the user with no clue an objects
    /// pool is required.
    ///
    /// Only adds the hint when the user did NOT already supply `--objects-store`
    /// /`--from-objects` (an objects pool IS configured ⇒ a genuine missing
    /// object is a real corruption error, not a missing-pool mistake). The hint
    /// rides on stderr with the underlying error so the original `object not
    /// found` cause is preserved in the chain.
    fn split_read_hint<T>(&self, result: Result<T>) -> Result<T> {
        if self.globals.objects_store.is_some() {
            return result;
        }
        result.map_err(|e| {
            e.context(
                "if this snapshot was pushed with a split --objects-store, re-run with \
                 --objects-store/--from-objects pointing at that object pool",
            )
        })
    }

    /// The local cache as a `file://`-shaped store, rooted at the resolved cache
    /// directory. The cache uses the identical sharded layout as a `FileStore`.
    /// Cache copies honor `--jobs` / `--limit-rate` via the [`TransferConfig`].
    fn cache_store(&self) -> Result<FileStore> {
        Ok(FileStore::from_root_with_config(
            self.cache_dir(),
            self.transfer_config()?,
        ))
    }

    /// Like [`Self::cache_store`] but attaches an optional progress meter, used
    /// by the cache-write legs of `fetch`/`checkout` so the live dashboard
    /// reflects the cache copy.
    fn cache_store_with_meter(&self, meter: Option<Arc<Meter>>) -> Result<FileStore> {
        Ok(self.cache_store()?.with_meter(meter))
    }

    /// Resolves the cache directory: `--cache-dir`, else
    /// `${XDG_CACHE_HOME:-$HOME/.cache}/snapdir` (the oracle default).
    fn cache_dir(&self) -> PathBuf {
        if let Some(dir) = &self.globals.cache_dir {
            return dir.clone();
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let base = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{home}/.cache"));
        PathBuf::from(format!("{base}/snapdir"))
    }

    /// Returns the manifest's file objects that are ABSENT from the cache at
    /// `cache_dir`, as `(checksum, manifest_path)` pairs in manifest order.
    ///
    /// Only `F` (file) entries map to a `.objects/<sharded>` blob; `D`
    /// (directory) entries are merkle nodes with no stored object, so they are
    /// skipped. This is the shared primitive behind both the `fetch`/`pull`
    /// cache-heal decision (re-fetch when non-empty) and `verify-cache`'s
    /// missing-object detection (fail + name the gap when non-empty). It only
    /// checks for object PRESENCE; byte-level corruption is left to
    /// [`cache::verify_cache`], which re-hashes each present object.
    fn missing_cache_objects(manifest: &Manifest, cache_dir: &Path) -> Vec<(String, String)> {
        let mut missing = Vec::new();
        for entry in manifest.entries() {
            if entry.path_type != PathType::File {
                continue;
            }
            let object = cache_dir.join(snapdir_core::store::object_path(&entry.checksum));
            if !object.is_file() {
                missing.push((entry.checksum.clone(), entry.path.clone()));
            }
        }
        missing
    }

    /// Returns the required `--id`, or a clear error naming the missing option.
    fn require_id(&self) -> Result<&str> {
        self.globals.id.as_deref().context("missing --id option")
    }

    /// Resolve the argument path, expand excludes, select the hasher, and run
    /// the in-process walk — the shared wiring behind `manifest` and `id`.
    fn build_manifest(
        &self,
        path: Option<&Path>,
        absolute: bool,
        no_follow: bool,
        checksum_bin: Option<&str>,
        exclude: &[String],
        meter: Option<&Meter>,
    ) -> Result<Manifest> {
        let (root, options) = self.resolve_walk(
            path,
            absolute,
            no_follow,
            exclude,
            "resolving manifest path",
        )?;

        // Select the checksum function. `b3sum` (or unset) is the default; the
        // CLI reads `SNAPDIR_MANIFEST_CONTEXT` (core stays env-pure) to switch
        // to keyed BLAKE3. `--checksum-bin` selects md5sum / sha256sum.
        match checksum_bin {
            None | Some("b3sum") => {
                let context = std::env::var("SNAPDIR_MANIFEST_CONTEXT").unwrap_or_default();
                if context.is_empty() {
                    walk_with(&root, &options, &Blake3Hasher::new(), meter)
                } else {
                    walk_with(&root, &options, &Blake3KeyedHasher::new(context), meter)
                }
            }
            Some("md5sum") => walk_with(&root, &options, &Md5Hasher::new(), meter),
            Some("sha256sum") => walk_with(&root, &options, &Sha256Hasher::new(), meter),
            Some(other) => {
                anyhow::bail!("snapdir: unsupported --checksum-bin '{other}'")
            }
        }
    }

    /// Sibling of [`Self::build_manifest`] that ALSO returns the walk's
    /// [`CopyGuard`] side channel — used ONLY by `run_stage` so the local-cache
    /// [`FileStore`]'s stat-validated clone-skip can engage. The resolved
    /// `(root, options, hasher)` are identical to [`Self::build_manifest`] (both
    /// route through [`Self::resolve_walk`]), so the returned [`Manifest`] — and
    /// hence the snapshot id — is byte-identical. The map keys are the walk's
    /// absolute file paths (`root.join(rel)`), exactly what `push` looks up.
    fn build_manifest_with_guards(
        &self,
        path: Option<&Path>,
        absolute: bool,
        no_follow: bool,
        checksum_bin: Option<&str>,
        exclude: &[String],
        meter: Option<&Meter>,
    ) -> Result<(Manifest, std::collections::HashMap<PathBuf, CopyGuard>)> {
        let (root, options) =
            self.resolve_walk(path, absolute, no_follow, exclude, "resolving stage path")?;

        match checksum_bin {
            None | Some("b3sum") => {
                let context = std::env::var("SNAPDIR_MANIFEST_CONTEXT").unwrap_or_default();
                if context.is_empty() {
                    walk_with_guards_ctx(&root, &options, &Blake3Hasher::new(), meter)
                } else {
                    walk_with_guards_ctx(&root, &options, &Blake3KeyedHasher::new(context), meter)
                }
            }
            Some("md5sum") => walk_with_guards_ctx(&root, &options, &Md5Hasher::new(), meter),
            Some("sha256sum") => walk_with_guards_ctx(&root, &options, &Sha256Hasher::new(), meter),
            Some(other) => {
                anyhow::bail!("snapdir: unsupported --checksum-bin '{other}'")
            }
        }
    }

    /// Resolves the walk root and [`WalkOptions`] shared by
    /// [`Self::build_manifest`] and [`Self::build_manifest_with_guards`] so both
    /// derive an IDENTICAL traversal (`follow`/`path_mode`/`exclude`/`walk_jobs`).
    fn resolve_walk(
        &self,
        path: Option<&Path>,
        absolute: bool,
        no_follow: bool,
        exclude: &[String],
        context: &'static str,
    ) -> Result<(PathBuf, WalkOptions)> {
        let root = resolve_root(path).context(context)?;

        // Expand the exclude patterns. Each `--exclude` value is expanded
        // independently — `%system%` / `%common%` macros must be expanded
        // per-pattern, never on a raw `|`-joined string (that would split the
        // macro tokens apart) — then OR-combined into a single ERE so a path is
        // excluded if it matches ANY pattern. `%system%` forces no-follow; the
        // runtime `$HOME/.cache/` + cache-dir values come from the CLI (core is
        // env-pure). An empty list means no filtering. A single pattern is
        // byte-identical to the previous single-pattern path: one expansion,
        // wrapped in a `(?:…)` group, with no extra alternation.
        let (home_cache, cache_dir) = exclude_runtime_paths(self.globals.cache_dir.as_deref());
        let combined = combine_excludes(exclude, &home_cache, &cache_dir);
        let matcher = match &combined.pattern {
            Some(pattern) => {
                Some(ExcludeMatcher::new(pattern).context("compiling --exclude pattern")?)
            }
            None => None,
        };

        let follow = if no_follow || combined.forces_no_follow {
            FollowMode::NoFollow
        } else {
            FollowMode::Follow
        };
        let path_mode = if absolute {
            PathMode::Absolute
        } else {
            PathMode::Relative
        };
        let options = WalkOptions {
            follow,
            path_mode,
            exclude: matcher,
            walk_jobs: self.globals.walk_jobs,
        };
        Ok((root, options))
    }
}

/// Constructs the concrete [`Store`] for a resolved [`Adapter`] and store URL.
///
/// The built-in adapters connect in-process via their `snapdir-stores` impls;
/// [`Adapter::External`] dispatches to a `snapdir-<name>-store` binary through
/// the emit-command shim. The `s3`/`b2` endpoint and the `b2` region honor the
/// oracle's environment overrides (`SNAPDIR_S3_STORE_ENDPOINT_URL`,
/// `SNAPDIR_B2_REGION` / `AWS_REGION`).
///
/// Kept as a free function (decoupled from `--store` parsing) so the
/// scheme→store routing is exercised independently of CLI argument plumbing;
/// the pure scheme→adapter decision itself lives in [`resolve_adapter`].
fn store_for_adapter(
    adapter: &Adapter,
    store_url: &str,
    config: TransferConfig,
    meter: Option<Arc<Meter>>,
) -> Result<Box<dyn Store>> {
    match adapter {
        Adapter::File => Ok(Box::new(
            FileStore::new_with_config(store_url, config).with_meter(meter),
        )),
        Adapter::S3 => {
            let endpoint = std::env::var("SNAPDIR_S3_STORE_ENDPOINT_URL").ok();
            let store = S3Store::connect_with(store_url, endpoint.as_deref(), config)
                .with_context(|| format!("connecting to S3 store {store_url}"))?;
            Ok(Box::new(store.with_meter(meter)))
        }
        Adapter::B2 => {
            let endpoint = std::env::var("SNAPDIR_S3_STORE_ENDPOINT_URL").ok();
            let region = std::env::var("SNAPDIR_B2_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .ok();
            let store =
                B2Store::connect_with(store_url, endpoint.as_deref(), region.as_deref(), config)
                    .with_context(|| format!("connecting to B2 store {store_url}"))?;
            Ok(Box::new(store.with_meter(meter)))
        }
        Adapter::Gcs => {
            let store = GcsStore::connect_with(store_url, config)
                .with_context(|| format!("connecting to GCS store {store_url}"))?;
            Ok(Box::new(store.with_meter(meter)))
        }
        Adapter::External { .. } => {
            // The external `snapdir-*-store` shim dispatches to a child process
            // and exposes no in-process meter hook; progress is best-effort and
            // simply absent for these stores.
            let store = ExternalStore::new(store_url)
                .with_context(|| format!("resolving external store for {store_url}"))?;
            Ok(Box::new(store))
        }
    }
}

/// Constructs the concrete [`StreamStore`] for a resolved [`Adapter`] and store
/// URL, for the store→store [`sync`](snapdir_stores::sync_snapshot) pipe.
///
/// Mirrors [`store_for_adapter`]'s endpoint/region environment handling, but is
/// restricted to the in-process stores (file/s3/b2/gcs) — the only ones that
/// implement [`StreamStore`]. [`Adapter::External`] (`snapdir-*-store` URLs) has
/// no in-process streaming surface and is rejected with an actionable error.
///
/// The concrete stores are `Sync`, so each is boxed as `Box<dyn StreamStore +
/// Sync>` (the bound `sync_snapshot` requires).
fn stream_store_for_adapter(
    adapter: &Adapter,
    store_url: &str,
    config: TransferConfig,
    meter: Option<Arc<Meter>>,
) -> Result<Box<dyn StreamStore + Sync>> {
    match adapter {
        Adapter::File => Ok(Box::new(
            FileStore::new_with_config(store_url, config).with_meter(meter),
        )),
        Adapter::S3 => {
            let endpoint = std::env::var("SNAPDIR_S3_STORE_ENDPOINT_URL").ok();
            let store = S3Store::connect_with(store_url, endpoint.as_deref(), config)
                .with_context(|| format!("connecting to S3 store {store_url}"))?;
            Ok(Box::new(store.with_meter(meter)))
        }
        Adapter::B2 => {
            let endpoint = std::env::var("SNAPDIR_S3_STORE_ENDPOINT_URL").ok();
            let region = std::env::var("SNAPDIR_B2_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .ok();
            let store =
                B2Store::connect_with(store_url, endpoint.as_deref(), region.as_deref(), config)
                    .with_context(|| format!("connecting to B2 store {store_url}"))?;
            Ok(Box::new(store.with_meter(meter)))
        }
        Adapter::Gcs => {
            let store = GcsStore::connect_with(store_url, config)
                .with_context(|| format!("connecting to GCS store {store_url}"))?;
            Ok(Box::new(store.with_meter(meter)))
        }
        Adapter::External { .. } => Err(anyhow::anyhow!(
            "sync requires in-process stores (file/s3/b2/gcs); \
             external `snapdir-*-store` URLs are not supported: {store_url}"
        )),
    }
}

/// Reads newline-separated object checksums to EOF, validating EVERY non-empty
/// line against `^[0-9a-f]{64}$` ([`is_hex64`]) — any malformed line is a hard
/// error (fail closed: the plumbing commands must validate the whole request
/// before their first store query / first emitted byte). Empty lines are
/// skipped; order is preserved; duplicates are kept (dedup is a separate,
/// explicit step — [`dedupe_preserving_order`]).
fn read_checksum_lines(reader: impl BufRead) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for line in reader.lines() {
        let line = line.context("reading checksum list")?;
        if line.is_empty() {
            continue;
        }
        anyhow::ensure!(
            is_hex64(&line),
            "invalid object checksum {line:?}: expected 64 lowercase hex characters"
        );
        ids.push(line);
    }
    Ok(ids)
}

/// Drops duplicate checksums, keeping the FIRST occurrence of each and the
/// relative order of survivors. The pack/diff libs document deduplication as
/// the caller's job ([`StreamStore::objects_needed`], [`write_pack_with_format`]),
/// so the CLI is where it happens.
fn dedupe_preserving_order(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// Resolves the receive-pack crash-durability mode from `SNAPDIR_FSYNC`.
///
/// `batch` (the default when unset/empty) enables batched durability — exactly
/// two full syncs per pack so a durable manifest implies durable objects. `off`
/// restores the historical no-fsync filing. Any OTHER value is a hard error
/// (fail closed): we never silently fall back to a weaker durability than the
/// operator asked for.
fn fsync_durability_from_env() -> Result<Durability> {
    match std::env::var("SNAPDIR_FSYNC") {
        Err(std::env::VarError::NotPresent) => Ok(Durability::Batch),
        Ok(raw) => match raw.trim() {
            "" | "batch" => Ok(Durability::Batch),
            "off" => Ok(Durability::Off),
            other => anyhow::bail!(
                "invalid SNAPDIR_FSYNC {other:?}: expected `batch` (default) or `off`"
            ),
        },
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!(
            "invalid SNAPDIR_FSYNC: value is not valid UTF-8; expected `batch` (default) or `off`"
        ),
    }
}

/// A [`PackSink`] decorator that records WHICH manifest id the stream
/// committed: [`read_pack`]'s report only says WHETHER a manifest committed,
/// but `receive-pack --require-manifest <id>` must compare the actual id (a
/// pre-existing manifest in the store could otherwise mask a stream that
/// carried the wrong one). Pure delegation — no wire logic lives here.
struct RecordingSink<'a> {
    inner: &'a mut dyn PackSink,
    manifest_id: Option<String>,
}

impl PackSink for RecordingSink<'_> {
    fn has_object(&mut self, checksum: &str) -> Result<bool, StoreError> {
        self.inner.has_object(checksum)
    }

    fn stage_object(
        &mut self,
        checksum: &str,
        len: u64,
        payload: &mut dyn Read,
    ) -> Result<(), StoreError> {
        self.inner.stage_object(checksum, len, payload)
    }

    fn commit_object(&mut self, checksum: &str) -> Result<(), StoreError> {
        self.inner.commit_object(checksum)
    }

    fn abort_object(&mut self, checksum: &str) {
        self.inner.abort_object(checksum);
    }

    fn put_manifest(&mut self, id: &str, manifest: &Manifest) -> Result<(), StoreError> {
        // Record only after the inner sink actually committed (an error here
        // must not look like a committed manifest to --require-manifest).
        self.inner.put_manifest(id, manifest)?;
        self.manifest_id = Some(id.to_owned());
        Ok(())
    }

    fn flush_barrier(&mut self) -> Result<(), StoreError> {
        // MUST delegate: `read_pack` fires the barrier through the sink it
        // drives (the RecordingSink), right before the manifest. Without this
        // override the no-op trait default would swallow it and the inner
        // FileSink's durability would silently never activate.
        self.inner.flush_barrier()
    }
}

/// Runs [`read_pack`] through a [`RecordingSink`], returning the read report
/// plus the id of the committed manifest (if any).
fn read_pack_recording(
    input: impl Read,
    sink: &mut dyn PackSink,
) -> Result<(PackReadReport, Option<String>)> {
    let mut recording = RecordingSink {
        inner: sink,
        manifest_id: None,
    };
    let report = read_pack(input, &mut recording).context("reading pack stream from stdin")?;
    Ok((report, recording.manifest_id))
}

/// Parses a wget-style byte-rate string into bytes/second.
///
/// Accepts a bare integer (bytes), or a number with a binary-multiple suffix.
/// Suffixes are case-insensitive and the trailing `i`/`B` letters are optional:
/// `K`/`KB`/`KiB` = 1024, `M`/`MB`/`MiB` = 1024², `G`/`GB`/`GiB` = 1024³.
/// Fractional values are supported (`1.5M` = 1572864). Surrounding whitespace
/// is ignored.
///
/// # Errors
///
/// Returns an error for empty input, an unrecognized suffix, a non-numeric
/// mantissa, or a negative value.
fn parse_rate(s: &str) -> Result<u64> {
    let trimmed = s.trim();
    anyhow::ensure!(!trimmed.is_empty(), "empty --limit-rate value");

    // Split the numeric mantissa from the (optional) unit suffix.
    let split = trimmed
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(trimmed.len());
    let (num, suffix) = trimmed.split_at(split);
    anyhow::ensure!(
        !num.is_empty(),
        "invalid --limit-rate '{s}': expected a number, optionally followed by K/M/G"
    );

    let value: f64 = num
        .parse()
        .with_context(|| format!("invalid --limit-rate '{s}': '{num}' is not a number"))?;
    anyhow::ensure!(
        value.is_finite() && value >= 0.0,
        "invalid --limit-rate '{s}': must be a non-negative number"
    );

    // Normalize the suffix: strip an optional 'i' and an optional 'b'
    // (case-insensitive) so K/KB/KiB all collapse to the same multiplier.
    let unit = suffix.trim().to_ascii_lowercase();
    let unit = unit.strip_suffix('b').unwrap_or(&unit);
    let unit = unit.strip_suffix('i').unwrap_or(unit);
    let multiplier: f64 = match unit {
        "" => 1.0,
        "k" => 1024.0,
        "m" => 1024.0 * 1024.0,
        "g" => 1024.0 * 1024.0 * 1024.0,
        other => {
            anyhow::bail!("invalid --limit-rate '{s}': unknown unit '{other}' (use K, M, or G)")
        }
    };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((value * multiplier) as u64)
}

/// clap `value_parser` for `--limit-rate`: validates the wget-style byte-rate
/// string AT PARSE TIME so a malformed value (`--limit-rate bogus`) is rejected
/// with exit 2 and a message naming the accepted forms, instead of slipping
/// through to a later transfer-config error. Returns the ORIGINAL string on
/// success (the field stays `Option<String>`; [`parse_rate`] re-parses it where
/// the byte value is actually needed) and a clap-friendly `String` error
/// naming `10M`/`512K`/`1G` otherwise.
fn parse_rate_arg(s: &str) -> Result<String, String> {
    match parse_rate(s) {
        Ok(_) => Ok(s.to_owned()),
        Err(_) => Err(format!(
            "invalid --limit-rate '{s}': expected a wget-style byte rate, e.g. 10M, 512K, or 1G"
        )),
    }
}

/// Reads an environment variable as a `u64`, returning `None` when the variable
/// is unset, empty, or does not parse as a non-negative integer. Used by the
/// rate-limit / retry resolvers for their `SNAPDIR_*` env fallbacks.
fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

/// The smaller of two optional caps, treating `None` as "no cap on that
/// dimension": `min(Some, Some)` is the lesser, `min(Some, None)` / `min(None,
/// Some)` is the present one, and `min(None, None)` is `None`. Used to collapse
/// a backend's separate read/write caps into the single value `TransferConfig`
/// carries (the conservative choice).
fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// clap `value_parser` for `--adaptive[=FRACTION]`: a finite `f64` in the
/// half-open range `(0.0, 1.0]` (the politeness fraction). Rejects `<= 0` and
/// `> 1` so a nonsensical target can't slip through.
fn parse_adaptive_fraction(s: &str) -> Result<f64, String> {
    let value: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("invalid --adaptive '{s}': expected a number in (0.0, 1.0]"))?;
    if !value.is_finite() || value <= 0.0 || value > 1.0 {
        return Err(format!(
            "invalid --adaptive '{s}': fraction must be in (0.0, 1.0]"
        ));
    }
    Ok(value)
}

/// Walks `root` with the given hasher, mapping the typed [`WalkError`] into an
/// `anyhow` error with context.
///
/// [`WalkError`]: snapdir_core::WalkError
fn walk_with<H: Hasher + HashFile + Sync>(
    root: &Path,
    options: &WalkOptions,
    hasher: &H,
    meter: Option<&Meter>,
) -> Result<Manifest> {
    walk_with_meter(root, options, hasher, meter)
        .with_context(|| format!("walking {}", root.display()))
}

/// Like [`walk_with`], but ALSO returns the walk's [`CopyGuard`] side channel
/// (keyed by each plain regular file's absolute working-tree path). Used only by
/// the stage→local-cache push so [`FileStore`]'s stat-validated clone-skip can
/// engage; the [`Manifest`] is byte-identical to [`walk_with`]'s.
fn walk_with_guards_ctx<H: Hasher + HashFile + Sync>(
    root: &Path,
    options: &WalkOptions,
    hasher: &H,
    meter: Option<&Meter>,
) -> Result<(Manifest, std::collections::HashMap<PathBuf, CopyGuard>)> {
    walk_with_guards(root, options, hasher, meter)
        .with_context(|| format!("walking {}", root.display()))
}

/// Total content bytes a transfer will move for `manifest`: the sum of every
/// File entry's size (directory entries carry merkle checksums, not object
/// bytes). Used to seed the progress meter's `bytes_out` denominator so the
/// determinate bar/ETA reflect the actual object payload.
fn total_object_bytes(manifest: &Manifest) -> u64 {
    manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::File)
        .map(|e| e.size)
        .sum()
}

/// Restores each manifest entry's octal permissions onto the materialized tree
/// at `dest`. The store's `fetch_files` reproduces the bytes and tree shape but
/// not the modes, so the CLI applies them here — without this the checked-out
/// tree would re-manifest to a different snapshot id (perms are part of the
/// manifest text the id hashes). Directories are set after their contents so a
/// read-only directory mode never blocks writing the files inside it.
fn restore_permissions(manifest: &Manifest, dest: &Path) -> Result<()> {
    for entry in manifest.entries() {
        if entry.path_type == PathType::Directory {
            continue;
        }
        apply_mode(dest, entry)?;
    }
    // Directories last, deepest first, so tightening a parent's mode never
    // blocks setting a child's.
    let mut dirs: Vec<&_> = manifest
        .entries()
        .iter()
        .filter(|e| e.path_type == PathType::Directory)
        .collect();
    dirs.sort_by_key(|e| std::cmp::Reverse(e.path.len()));
    for entry in dirs {
        apply_mode(dest, entry)?;
    }
    Ok(())
}

/// Parses a manifest entry's octal permission string and applies it to the
/// entry's path under `dest`.
fn apply_mode(dest: &Path, entry: &ManifestEntry) -> Result<()> {
    let rel = entry.path.strip_prefix("./").unwrap_or(&entry.path);
    let rel = rel.strip_suffix('/').unwrap_or(rel);
    let target = if rel.is_empty() {
        dest.to_path_buf()
    } else {
        dest.join(rel)
    };
    let mode = u32::from_str_radix(&entry.permissions, 8)
        .with_context(|| format!("invalid permissions {:?}", entry.permissions))?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("setting permissions on {}", target.display()))?;
    Ok(())
}

/// A scratch directory under the system temp dir, removed on drop. Used as the
/// throwaway materialization target for `fetch`/`verify`.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(tag: &str) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("snapdir-cli-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path)
            .with_context(|| format!("creating scratch dir {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Resolves the user's path argument to an absolute path, mirroring the
/// oracle's `readlink` (`cd "$(dirname)" && pwd`/`basename`): the parent is
/// made absolute, the basename is appended verbatim. Defaults to the current
/// directory when no path is given.
///
/// The resolved absolute path is then normalized **lexically** so that the four
/// surface forms `foo`, `./foo`, `foo/`, and `./foo/` all collapse to the same
/// clean root. The walk downstream derives each entry's relative path with a
/// string `strip_prefix(root)`; a trailing `/` or an interior `/./` on the root
/// breaks that rewrite (it yields `.bar.txt` instead of `./bar.txt`, or makes
/// `strip_prefix` miss and leak an absolute path). Normalizing here — and only
/// here, in the CLI — keeps the frozen walk contract untouched while making
/// every input form produce the identical manifest + snapshot id.
///
/// This is purely lexical: it does **not** call `.canonicalize()`, so symlinks
/// and `..` keep their existing semantics and the not-found error path is
/// preserved (the path need not exist yet for some commands).
fn resolve_root(path: Option<&Path>) -> Result<PathBuf> {
    let raw = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("getting current directory")?,
    };
    let abs = if raw.is_absolute() {
        raw
    } else {
        let cwd = std::env::current_dir().context("getting current directory")?;
        cwd.join(raw)
    };
    Ok(lexically_normalize_root(&abs))
}

/// Lexically cleans an absolute root path for the walk, WITHOUT touching the
/// filesystem (no symlink resolution):
///
/// - drops `CurDir` (`.`) components — collapses a leading `./` and any interior
///   `/./` segments,
/// - drops a trailing `/` (except the filesystem root `/` itself),
/// - preserves `ParentDir` (`..`) and ordinary names verbatim — `..` keeps the
///   existing semantics and is NOT resolved.
///
/// A canonical absolute path with no `.` segment and no trailing slash is
/// returned byte-for-byte unchanged.
fn lexically_normalize_root(abs: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    let mut pushed_any = false;
    for comp in abs.components() {
        match comp {
            // Skip `.` segments entirely (leading `./`, interior `/./`).
            Component::CurDir => {}
            // The root `/` (and any prefix on non-unix) anchors the path.
            Component::RootDir | Component::Prefix(_) => out.push(comp.as_os_str()),
            // Ordinary names and `..` are kept verbatim, in order.
            Component::Normal(_) | Component::ParentDir => {
                out.push(comp.as_os_str());
                pushed_any = true;
            }
        }
    }
    // `components()` already strips a trailing slash, so dropping it is implicit.
    // Guard the degenerate "everything was `.`" case (e.g. a literal `/.`):
    // fall back to the bare root so we never return an empty path.
    if !pushed_any && out.as_os_str().is_empty() {
        out.push(Component::RootDir.as_os_str());
    }
    out
}

/// OR-combines a list of `--exclude` patterns into a single expanded
/// extended-regex, expanding the `%system%` / `%common%` macros **per pattern**.
///
/// The macros (e.g. `%system%`) expand to parenthesized alternations that
/// embed `|` and `^` anchors, so the raw patterns must NOT be `|`-joined before
/// expansion — that would split a macro token across an alternation boundary.
/// Instead each pattern is run through [`expand_excludes`] on its own, each
/// expanded result is wrapped in a non-capturing group `(?:…)`, and the groups
/// are joined with `|`. `forces_no_follow` is the OR of every pattern's flag
/// (any `%system%` anywhere forces no-follow).
///
/// An empty list yields `pattern: None` (no filtering), exactly as before. A
/// single pattern produces `(?:<expansion>)`, which matches identically to the
/// bare `<expansion>` the previous single-pattern path compiled — the
/// non-capturing group changes grouping only, never the matched set.
fn combine_excludes(patterns: &[String], home_cache: &str, cache_dir: &str) -> ExpandedExclude {
    let mut groups: Vec<String> = Vec::new();
    let mut forces_no_follow = false;
    for pattern in patterns {
        let expanded = expand_excludes(pattern, home_cache, cache_dir);
        forces_no_follow |= expanded.forces_no_follow;
        if let Some(ere) = expanded.pattern {
            groups.push(format!("(?:{ere})"));
        }
    }
    let pattern = if groups.is_empty() {
        None
    } else {
        Some(groups.join("|"))
    };
    ExpandedExclude {
        pattern,
        forces_no_follow,
    }
}

/// Resolves the runtime values the `%system%` macro interpolates: the
/// `$HOME/.cache/` directory and the snapdir cache directory. Mirrors the
/// oracle's `${HOME:-~}/.cache/` and `${XDG_CACHE_HOME:-$HOME/.cache}/snapdir`.
fn exclude_runtime_paths(cache_dir: Option<&Path>) -> (String, String) {
    let home = std::env::var("HOME").unwrap_or_default();
    let home_cache = format!("{home}/.cache/");
    let cache_dir = if let Some(dir) = cache_dir {
        dir.display().to_string()
    } else {
        let base = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| format!("{home}/.cache"));
        format!("{base}/snapdir")
    };
    (home_cache, cache_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The scheme→store routing decision must match the shared stores router for
    // every supported scheme, WITHOUT touching the network or credentials. We
    // assert the decision (`resolve_adapter`) the CLI delegates to, plus that the
    // external scheme constructs the emit-command shim pointed at the right
    // `snapdir-<proto>-store` binary (no spawn, no I/O).
    #[test]
    fn remote_store_routing_resolves_every_scheme_to_its_adapter() {
        // file:// is the built-in local backend.
        assert_eq!(
            resolve_adapter("file:///long/term/x").unwrap(),
            Adapter::File
        );
        // s3:// / b2:// are the native AWS-SDK backends.
        assert_eq!(resolve_adapter("s3://bucket/path").unwrap(), Adapter::S3);
        assert_eq!(resolve_adapter("b2://bucket/path").unwrap(), Adapter::B2);
        // gs:// is the oracle's hardcoded special case → the gcs adapter.
        let gcs = resolve_adapter("gs://bucket/path").unwrap();
        assert_eq!(gcs, Adapter::Gcs);
        assert_eq!(gcs.name(), "gcs");
        assert_eq!(gcs.store_binary(), "snapdir-gcs-store");
        // A third-party scheme routes to the external shim binary.
        let xyz = resolve_adapter("xyz://bucket/path").unwrap();
        assert_eq!(
            xyz,
            Adapter::External {
                name: "xyz".to_owned()
            }
        );
        assert!(!xyz.is_builtin());
        assert_eq!(xyz.store_binary(), "snapdir-xyz-store");
    }

    #[test]
    fn remote_store_routing_file_builds_filestore_without_io() {
        // The file backend can be constructed with no I/O; confirm the routed
        // store is usable behind the trait object (a non-existent id is absent,
        // not an error other than ManifestNotFound semantics surfacing later).
        let adapter = resolve_adapter("file:///tmp/snapdir-routing-test").unwrap();
        let store = store_for_adapter(
            &adapter,
            "file:///tmp/snapdir-routing-test",
            TransferConfig::default(),
            None,
        )
        .unwrap();
        // get_manifest on a missing id must not panic; it returns an Err.
        assert!(store.get_manifest("0".repeat(64).as_str()).is_err());
    }

    #[test]
    fn remote_store_routing_external_builds_shim_for_third_party_scheme() {
        // The external scheme constructs the emit-command shim pointed at the
        // resolved `snapdir-<proto>-store` binary — no subprocess is spawned by
        // construction, so this needs neither the binary nor any I/O.
        let adapter = resolve_adapter("xyz://bucket/base").unwrap();
        let store = ExternalStore::new("xyz://bucket/base").unwrap();
        assert_eq!(store.binary(), Path::new("snapdir-xyz-store"));
        // store_for_adapter routes the same scheme through the shim.
        let routed = store_for_adapter(
            &adapter,
            "xyz://bucket/base",
            TransferConfig::default(),
            None,
        );
        assert!(routed.is_ok());
    }

    #[test]
    fn remote_store_routing_rejects_invalid_protocol() {
        assert!(resolve_adapter("NotAScheme://x").is_err());
    }

    /// Builds a dispatch [`Ctx`] from transfer-family `args`, forcing the
    /// transfer-tuning env vars unset so the parse reflects ONLY the explicit
    /// flags (clap's `env` would otherwise let a leaked `SNAPDIR_JOBS` /
    /// `SNAPDIR_LIMIT_RATE` perturb the result).
    ///
    /// The flags exercised here (`--jobs`/`--limit-rate`/`--adaptive`/
    /// `--max-jobs`/`--max-retries`/…) are the TRANSFER family, so the carrier
    /// subcommand is `fetch` (which flattens [`TransferArgs`]); `Cli::run`'s
    /// merge then folds them into the flat [`Resolved`] the helpers read — the
    /// same path a real `snapdir fetch …` invocation takes.
    fn cli_with(args: &[&str]) -> Ctx {
        // SAFETY: tests in this module that touch these vars run in-process;
        // we remove them before parsing so the flags alone drive the config.
        unsafe {
            std::env::remove_var("SNAPDIR_JOBS");
            std::env::remove_var("SNAPDIR_LIMIT_RATE");
            std::env::remove_var("SNAPDIR_ADAPTIVE");
            std::env::remove_var("SNAPDIR_MAX_JOBS");
        }
        let mut full = vec!["snapdir", "fetch"];
        full.extend_from_slice(args);
        ctx_from(Cli::try_parse_from(full).expect("parse cli"))
    }

    /// Folds a parsed [`Cli`] into the dispatch [`Ctx`] exactly as
    /// [`Cli::run`] does, for tests that need the resolved config / helpers
    /// without running the command.
    fn ctx_from(cli: Cli) -> Ctx {
        let mut globals = Resolved::from_universal(&cli.universal);
        match &cli.command {
            Command::Manifest {
                exclude, walk_jobs, ..
            } => {
                globals.exclude.clone_from(exclude);
                globals.walk_jobs = *walk_jobs;
            }
            Command::Id { walk, .. } => merge_walk(&mut globals, walk),
            Command::Stage { walk, transfer, .. } | Command::Push { walk, transfer, .. } => {
                merge_walk(&mut globals, walk);
                merge_transfer(&mut globals, transfer);
            }
            Command::Fetch { transfer }
            | Command::Pull { transfer, .. }
            | Command::Checkout { transfer, .. }
            | Command::Sync { transfer, .. } => merge_transfer(&mut globals, transfer),
            Command::Verify { cache_mgmt }
            | Command::VerifyCache { cache_mgmt }
            | Command::FlushCache { cache_mgmt } => merge_cache_mgmt(&mut globals, cache_mgmt),
            Command::Locations { catalog }
            | Command::Ancestors { catalog }
            | Command::Revisions { catalog } => merge_catalog(&mut globals, catalog),
            Command::Diff { id_arg, .. } => globals.id.clone_from(&id_arg.id),
            _ => {}
        }
        Ctx {
            globals,
            command: cli.command,
        }
    }

    #[test]
    fn transfer_flags_parse_rate() {
        assert_eq!(parse_rate("10M").unwrap(), 10_485_760);
        assert_eq!(parse_rate("512K").unwrap(), 524_288);
        assert_eq!(parse_rate("1G").unwrap(), 1_073_741_824);
        assert_eq!(parse_rate("1GiB").unwrap(), 1_073_741_824);
        assert_eq!(parse_rate("1GB").unwrap(), 1_073_741_824);
        assert_eq!(parse_rate("1000").unwrap(), 1000);
        assert_eq!(parse_rate("1.5M").unwrap(), 1_572_864);
        assert_eq!(parse_rate("  2k ").unwrap(), 2048);
        assert_eq!(parse_rate("1kib").unwrap(), 1024);

        for bad in ["10X", "abc", "", "   ", "M", "1.2.3", "-5M"] {
            assert!(parse_rate(bad).is_err(), "expected {bad:?} to be rejected");
        }
    }

    #[test]
    fn transfer_flags_jobs_explicit() {
        let cfg = cli_with(&["--jobs", "4"]).transfer_config().unwrap();
        assert_eq!(cfg.concurrency.get(), 4);
        assert_eq!(cfg.max_bytes_per_sec, None);
    }

    #[test]
    fn transfer_flags_jobs_one_is_sequential() {
        let cfg = cli_with(&["--jobs", "1"]).transfer_config().unwrap();
        assert_eq!(cfg.concurrency.get(), 1);
    }

    #[test]
    fn transfer_flags_jobs_unset_is_auto() {
        let cfg = cli_with(&[]).transfer_config().unwrap();
        assert!(cfg.concurrency.get() >= 1 && cfg.concurrency.get() <= 16);
        assert_eq!(
            cfg.concurrency.get(),
            TransferConfig::default().concurrency.get()
        );
    }

    #[test]
    fn transfer_flags_jobs_zero_is_auto() {
        let cfg = cli_with(&["--jobs", "0"]).transfer_config().unwrap();
        assert!(cfg.concurrency.get() >= 1 && cfg.concurrency.get() <= 16);
        assert_eq!(
            cfg.concurrency.get(),
            TransferConfig::default().concurrency.get()
        );
    }

    #[test]
    fn transfer_flags_limit_rate_threads_into_config() {
        let cfg = cli_with(&["--limit-rate", "1M"]).transfer_config().unwrap();
        assert_eq!(cfg.max_bytes_per_sec, Some(1_048_576));

        // The short `-j` alias works and pairs with --limit-rate.
        let cfg = cli_with(&["-j", "2", "--limit-rate", "512K"])
            .transfer_config()
            .unwrap();
        assert_eq!(cfg.concurrency.get(), 2);
        assert_eq!(cfg.max_bytes_per_sec, Some(524_288));
    }

    #[test]
    fn transfer_flags_bad_limit_rate_errors() {
        // A bogus `--limit-rate` is now rejected at PARSE time by the clap
        // `parse_rate_arg` value-parser (exit 2), not deferred to
        // `transfer_config`. So the parse itself fails on a transfer command.
        assert!(
            Cli::try_parse_from(["snapdir", "fetch", "--limit-rate", "nope"]).is_err(),
            "a malformed --limit-rate must be rejected at parse time"
        );
        // A well-formed value still threads through to the resolved byte rate.
        let cfg = cli_with(&["--limit-rate", "1M"]).transfer_config().unwrap();
        assert_eq!(cfg.max_bytes_per_sec, Some(1_048_576));
    }

    #[test]
    fn adaptive_flag_without_value_defaults_to_point_eight() {
        let cfg = cli_with(&["--adaptive"]).transfer_config().unwrap();
        match cfg.adaptive {
            TransferAdaptivePolicy::On { fraction, ceiling } => {
                assert!(
                    (fraction - 0.8).abs() < 1e-9,
                    "expected fraction ~0.8, got {fraction}"
                );
                // Ceiling defaults to the auto concurrency when unset.
                assert_eq!(ceiling, TransferConfig::default().concurrency.get());
            }
            TransferAdaptivePolicy::Off => panic!("expected adaptive On"),
        }
    }

    #[test]
    fn adaptive_flag_with_explicit_fraction() {
        let cfg = cli_with(&["--adaptive=0.5"]).transfer_config().unwrap();
        match cfg.adaptive {
            TransferAdaptivePolicy::On { fraction, .. } => {
                assert!(
                    (fraction - 0.5).abs() < 1e-9,
                    "expected fraction ~0.5, got {fraction}"
                );
            }
            TransferAdaptivePolicy::Off => panic!("expected adaptive On"),
        }
    }

    #[test]
    fn adaptive_flag_out_of_range_is_rejected() {
        // clap value_parser rejects <= 0 and > 1 at parse time.
        for bad in ["0", "0.0", "1.5", "-0.2", "nope"] {
            unsafe {
                std::env::remove_var("SNAPDIR_ADAPTIVE");
                std::env::remove_var("SNAPDIR_MAX_JOBS");
            }
            let arg = format!("--adaptive={bad}");
            // `--adaptive` is a transfer flag: carry it on `fetch` so the ONLY
            // parse failure is the value_parser rejecting the bad fraction.
            assert!(
                Cli::try_parse_from(["snapdir", "fetch", &arg]).is_err(),
                "expected --adaptive={bad} to be rejected"
            );
        }
        // 1.0 is the inclusive upper bound and must be accepted.
        let cfg = cli_with(&["--adaptive=1.0"]).transfer_config().unwrap();
        assert!(matches!(cfg.adaptive, TransferAdaptivePolicy::On { .. }));
    }

    #[test]
    fn adaptive_unset_equals_pre_phase18_value() {
        // The default (no --adaptive) MUST be byte-for-byte the historical
        // non-adaptive config: Off, same concurrency + max_bytes_per_sec.
        let cfg = cli_with(&["--jobs", "4", "--limit-rate", "1M"])
            .transfer_config()
            .unwrap();
        let expected = TransferConfig::new(4, Some(1_048_576));
        assert_eq!(cfg.adaptive, TransferAdaptivePolicy::Off);
        assert_eq!(cfg.adaptive, expected.adaptive);
        assert_eq!(cfg.concurrency, expected.concurrency);
        assert_eq!(cfg.max_bytes_per_sec, expected.max_bytes_per_sec);

        // Same for the all-defaults case.
        let cfg = cli_with(&[]).transfer_config().unwrap();
        let expected = TransferConfig::new(TransferConfig::default().concurrency.get(), None);
        assert_eq!(cfg.adaptive, TransferAdaptivePolicy::Off);
        assert_eq!(cfg.concurrency, expected.concurrency);
        assert_eq!(cfg.max_bytes_per_sec, expected.max_bytes_per_sec);
    }

    #[test]
    fn adaptive_max_jobs_sets_ceiling() {
        let cfg = cli_with(&["--adaptive", "--max-jobs", "8"])
            .transfer_config()
            .unwrap();
        match cfg.adaptive {
            TransferAdaptivePolicy::On { ceiling, .. } => assert_eq!(ceiling, 8),
            TransferAdaptivePolicy::Off => panic!("expected adaptive On"),
        }

        // An explicit --jobs acts as the ceiling under --adaptive when
        // --max-jobs is unset; --limit-rate still threads through.
        let cfg = cli_with(&["--adaptive=0.6", "--jobs", "3", "--limit-rate", "512K"])
            .transfer_config()
            .unwrap();
        match cfg.adaptive {
            TransferAdaptivePolicy::On { fraction, ceiling } => {
                assert!((fraction - 0.6).abs() < 1e-9);
                assert_eq!(ceiling, 3);
            }
            TransferAdaptivePolicy::Off => panic!("expected adaptive On"),
        }
        assert_eq!(cfg.max_bytes_per_sec, Some(524_288));

        // --max-jobs wins over --jobs for the ceiling.
        let cfg = cli_with(&["--adaptive", "--jobs", "2", "--max-jobs", "12"])
            .transfer_config()
            .unwrap();
        match cfg.adaptive {
            TransferAdaptivePolicy::On { ceiling, .. } => assert_eq!(ceiling, 12),
            TransferAdaptivePolicy::Off => panic!("expected adaptive On"),
        }
    }

    #[test]
    fn adaptive_ceiling_is_clamped_to_cap() {
        let cfg = cli_with(&["--adaptive", "--max-jobs", "9999"])
            .transfer_config()
            .unwrap();
        match cfg.adaptive {
            TransferAdaptivePolicy::On { ceiling, .. } => {
                assert_eq!(ceiling, ADAPTIVE_CEILING_CAP);
            }
            TransferAdaptivePolicy::Off => panic!("expected adaptive On"),
        }
    }

    /// Serializes the env-mutating rate-limit/retry tests so their `set_var` /
    /// `remove_var` on the shared `SNAPDIR_*` process env can't race.
    static RATELIMIT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Removes every rate-limit / retry env var so a resolver sees only its
    /// explicit flags (and any var the test sets afterward).
    fn ratelimit_clear_env() {
        unsafe {
            for key in [
                "SNAPDIR_MAX_RETRIES",
                "SNAPDIR_RETRY_BASE_MS",
                "SNAPDIR_RETRY_MAX_MS",
                "SNAPDIR_MAX_REQUESTS",
                "SNAPDIR_LIMIT_RATE",
            ] {
                std::env::remove_var(key);
            }
        }
    }

    #[test]
    fn ratelimit_retry_policy_flag_beats_env_beats_default() {
        let _guard = RATELIMIT_ENV_LOCK.lock().unwrap();

        // Default (no flags, no env) == RetryPolicy::default().
        ratelimit_clear_env();
        let p = cli_with(&[]).resolve_retry_policy();
        assert_eq!(p, RetryPolicy::default());
        assert_eq!(p.max_attempts, 5);
        assert_eq!(p.base, std::time::Duration::from_millis(250));
        assert_eq!(p.cap, std::time::Duration::from_secs(30));

        // Env beats default.
        ratelimit_clear_env();
        unsafe {
            std::env::set_var("SNAPDIR_MAX_RETRIES", "7");
            std::env::set_var("SNAPDIR_RETRY_BASE_MS", "100");
            std::env::set_var("SNAPDIR_RETRY_MAX_MS", "9000");
        }
        let p = cli_with(&[]).resolve_retry_policy();
        assert_eq!(p.max_attempts, 7);
        assert_eq!(p.base, std::time::Duration::from_millis(100));
        assert_eq!(p.cap, std::time::Duration::from_secs(9));

        // Flag beats env.
        let p = cli_with(&[
            "--max-retries",
            "9",
            "--retry-base-ms",
            "500",
            "--retry-max-ms",
            "45000",
        ])
        .resolve_retry_policy();
        assert_eq!(p.max_attempts, 9);
        assert_eq!(p.base, std::time::Duration::from_millis(500));
        assert_eq!(p.cap, std::time::Duration::from_secs(45));

        ratelimit_clear_env();
    }

    #[test]
    fn ratelimit_rate_limits_per_backend_defaults() {
        let _guard = RATELIMIT_ENV_LOCK.lock().unwrap();
        ratelimit_clear_env();
        let cli = cli_with(&[]);

        // b2: min(read_rps 20, write_rps 50) = 20 req/s; min(read_bps 25MiB,
        // write_bps 100MiB) = 25 MiB/s.
        assert_eq!(
            cli.resolve_rate_limits("b2", None),
            (Some(20), Some(25 * 1024 * 1024))
        );
        // s3: min(5500, 3500) = 3500 req/s; no byte cap.
        assert_eq!(cli.resolve_rate_limits("s3", None), (Some(3500), None));
        // gcs / gs: min(5000, 1000) = 1000 req/s; no byte cap.
        assert_eq!(cli.resolve_rate_limits("gcs", None), (Some(1000), None));
        assert_eq!(cli.resolve_rate_limits("gs", None), (Some(1000), None));
        // file / unknown: no caps at all.
        assert_eq!(cli.resolve_rate_limits("file", None), (None, None));
        assert_eq!(cli.resolve_rate_limits("azure", None), (None, None));

        ratelimit_clear_env();
    }

    #[test]
    fn ratelimit_max_requests_flag_beats_env_beats_backend_default() {
        let _guard = RATELIMIT_ENV_LOCK.lock().unwrap();

        // Backend default applies when neither flag nor env is set (b2 => 20).
        ratelimit_clear_env();
        assert_eq!(cli_with(&[]).resolve_rate_limits("b2", None).0, Some(20));
        // ...and is None for a capless scheme (global None).
        assert_eq!(cli_with(&[]).resolve_rate_limits("file", None).0, None);

        // Env beats the backend default.
        ratelimit_clear_env();
        unsafe {
            std::env::set_var("SNAPDIR_MAX_REQUESTS", "2");
        }
        assert_eq!(cli_with(&[]).resolve_rate_limits("b2", None).0, Some(2));
        // Env also supplies a cap for a scheme that has no backend default.
        assert_eq!(cli_with(&[]).resolve_rate_limits("file", None).0, Some(2));

        // Flag beats env (and the backend default).
        let cli = cli_with(&["--max-requests", "3"]);
        assert_eq!(cli.resolve_rate_limits("b2", None).0, Some(3));
        assert_eq!(cli.resolve_rate_limits("file", None).0, Some(3));

        // An explicit 0 means "unset" => fall through to the backend default.
        ratelimit_clear_env();
        let cli = cli_with(&["--max-requests", "0"]);
        assert_eq!(cli.resolve_rate_limits("b2", None).0, Some(20));
        assert_eq!(cli.resolve_rate_limits("file", None).0, None);

        ratelimit_clear_env();
    }

    #[test]
    fn ratelimit_limit_rate_overrides_backend_byte_default() {
        let _guard = RATELIMIT_ENV_LOCK.lock().unwrap();
        ratelimit_clear_env();
        let cli = cli_with(&[]);

        // No --limit-rate: b2 falls back to its 25 MiB/s byte default.
        assert_eq!(
            cli.resolve_rate_limits("b2", None).1,
            Some(25 * 1024 * 1024)
        );
        // An explicit --limit-rate value overrides the backend byte default.
        assert_eq!(
            cli.resolve_rate_limits("b2", Some(1_048_576)).1,
            Some(1_048_576)
        );
        // For a capless scheme the byte cap is exactly the --limit-rate value.
        assert_eq!(
            cli.resolve_rate_limits("file", Some(1_048_576)).1,
            Some(1_048_576)
        );
        assert_eq!(cli.resolve_rate_limits("file", None).1, None);

        ratelimit_clear_env();
    }

    #[test]
    fn ratelimit_transfer_config_unchanged_when_knobs_unset() {
        let _guard = RATELIMIT_ENV_LOCK.lock().unwrap();
        ratelimit_clear_env();

        // No new flags / env + a capless scheme (file) MUST be byte-for-byte the
        // historical config: req-rate None, byte-rate None, retry = default.
        let scheme_cfg = cli_with(&[]).transfer_config_for(Some("file")).unwrap();
        let no_scheme_cfg = cli_with(&[]).transfer_config_for(None).unwrap();
        let historical = TransferConfig::new(TransferConfig::default().concurrency.get(), None);

        for cfg in [&scheme_cfg, &no_scheme_cfg] {
            assert_eq!(cfg.max_requests_per_sec, None);
            assert_eq!(cfg.max_bytes_per_sec, None);
            assert_eq!(cfg.retry, RetryPolicy::default());
            assert_eq!(cfg.adaptive, TransferAdaptivePolicy::Off);
            assert_eq!(cfg.concurrency, historical.concurrency);
        }

        ratelimit_clear_env();
    }
}

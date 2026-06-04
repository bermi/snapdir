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

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use snapdir_catalog::{
    ancestors_json_line, locations_json_line, revisions_json_line, Catalog, SystemClock,
};
use snapdir_core::{
    cache, expand_excludes, snapshot_id, walk, Blake3Hasher, Blake3KeyedHasher, ExcludeMatcher,
    ExpandedExclude, FollowMode, Hasher, Manifest, ManifestEntry, Md5Hasher, PathMode, PathType,
    Sha256Hasher, Store, WalkOptions,
};
use snapdir_stores::{
    resolve_adapter, Adapter, B2Store, ExternalStore, FileStore, GcsStore, S3Store,
};

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
    /// Global options shared across every subcommand.
    #[command(flatten)]
    pub globals: GlobalArgs,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Options accepted by (and meaningful to) most subcommands.
///
/// Mirrors the Bash flag surface. Not every flag applies to every command;
/// validation per command lands with the business logic in later gates.
#[derive(Debug, Args)]
// The bool flags are a faithful 1:1 mirror of the Bash orchestrator's CLI
// surface (`--linked --force --purge --keep --dryrun --verbose --debug`); a
// state machine would obscure that mapping rather than clarify it.
#[allow(clippy::struct_excessive_bools)]
pub struct GlobalArgs {
    /// Directory where the object cache is stored.
    #[arg(long, global = true, value_name = "DIR", env = "SNAPDIR_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Catalog adapter to use.
    #[arg(long, global = true, value_name = "NAME", env = "SNAPDIR_CATALOG")]
    pub catalog: Option<String>,

    /// Store URI: `protocol://location/path`.
    #[arg(long, global = true, value_name = "URI")]
    pub store: Option<String>,

    /// Snapshot ID to operate on.
    #[arg(long, global = true, value_name = "ID")]
    pub id: Option<String>,

    /// Exclude paths matching PATTERN.
    // Accepts both repeated occurrences (`--exclude a --exclude b`) and
    // comma-delimited values (`--exclude a,b`); the collected patterns are
    // OR-combined (a path is excluded if it matches ANY pattern). The doc
    // comment is kept to a single line so `--help` output is byte-stable.
    #[arg(
        long,
        global = true,
        value_name = "PATTERN",
        action = clap::ArgAction::Append,
        value_delimiter = ','
    )]
    pub exclude: Vec<String>,

    /// Only include paths matching PATTERN.
    // Accepts both repeated occurrences and comma-delimited values, matching
    // `--exclude`'s arity. NOTE: this flag is currently UNWIRED — no `--paths`
    // filtering is performed yet (wiring it is out of scope for this gate).
    // Single-line doc comment keeps `--help` byte-stable.
    #[arg(
        long,
        global = true,
        value_name = "PATTERN",
        action = clap::ArgAction::Append,
        value_delimiter = ','
    )]
    pub paths: Vec<String>,

    /// Use symlinks instead of copies.
    #[arg(long, global = true)]
    pub linked: bool,

    /// Force an action to run.
    #[arg(long, global = true)]
    pub force: bool,

    /// Purge objects with invalid checksums.
    #[arg(long, global = true)]
    pub purge: bool,

    /// Keep the staging directory.
    #[arg(long, global = true)]
    pub keep: bool,

    /// Run without making any changes.
    #[arg(long, global = true)]
    pub dryrun: bool,

    /// Enable verbose output.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Enable debug output.
    #[arg(long, global = true)]
    pub debug: bool,

    /// Context (directory or store) for catalog queries.
    #[arg(long, global = true, value_name = "DIR|STORE")]
    pub location: Option<String>,
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

        /// Directory to describe.
        path: Option<PathBuf>,
    },

    /// Print the manifest ID of a directory or a manifest piped via stdin.
    Id {
        /// Directory to describe (omit to read a manifest from stdin).
        path: Option<PathBuf>,
    },

    /// Save a snapshot of a directory into the local cache.
    Stage {
        /// Directory to stage.
        dir: Option<PathBuf>,
    },

    /// Push a snapshot to a store given its path or a staged manifest ID.
    Push {
        /// Directory to push (omit when using `--id`).
        path: Option<PathBuf>,
    },

    /// Fetch a snapshot from a store into the local cache.
    Fetch,

    /// Fetch a snapshot from a store and check it out to the given path.
    Pull {
        /// Destination directory.
        path: Option<PathBuf>,
    },

    /// Check out a snapshot to a directory.
    Checkout {
        /// Destination directory.
        dir: Option<PathBuf>,
    },

    /// Verify the integrity of a staged snapshot.
    Verify,

    /// Verify the integrity of the local cache.
    VerifyCache,

    /// Flush the local cache.
    FlushCache,

    /// List directories and stores where snapshots have been recorded.
    Locations,

    /// List ancestor snapshot IDs and their locations.
    Ancestors,

    /// List snapshot IDs created on a location (store or absolute path).
    Revisions,

    /// Print default settings and arguments.
    Defaults,

    /// Print the version.
    Version,

    /// Generate a shell-completion script to stdout.
    ///
    /// Hidden from the documented 14-subcommand surface: this is a build-time
    /// hook the release pipeline (`release.yml` gen-assets job) calls as
    /// `snapdir completions <shell>` to bundle completions into each archive.
    #[command(hide = true)]
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
}

impl Cli {
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
    pub fn run(&self) -> Result<()> {
        match &self.command {
            Command::Manifest {
                absolute,
                no_follow,
                checksum_bin,
                exclude,
                path,
            } => {
                // Precedence: the subcommand's `--exclude` list overrides the
                // global one when non-empty, else fall back to the global list.
                let exclude: &[String] = if exclude.is_empty() {
                    &self.globals.exclude
                } else {
                    exclude
                };
                let manifest = self.build_manifest(
                    path.as_deref(),
                    *absolute,
                    *no_follow,
                    checksum_bin.as_deref(),
                    exclude,
                )?;
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
                self.log_event("manifest", &id, &abs.to_string_lossy())?;
                Ok(())
            }
            Command::Id { path } => {
                // `snapdir id` reproduces the original `snapdir id`: the snapshot id is the
                // b3sum of the comment-stripped manifest text. The wrapper
                // walks with the default checksum (b3sum) and default
                // path/follow modes; the id is checksum-mode independent here.
                let manifest = self.build_manifest(
                    path.as_deref(),
                    false,
                    false,
                    None,
                    &self.globals.exclude,
                )?;
                let id = snapshot_id(&manifest, &Blake3Hasher::new());
                println!("{id}");
                Ok(())
            }
            Command::Push { path } => self.run_push(path.as_deref()),
            Command::Fetch => self.run_fetch(),
            Command::Checkout { dir } => self.run_checkout(dir.as_deref()),
            Command::Pull { path } => self.run_pull(path.as_deref()),
            Command::Verify => self.run_verify(),
            Command::Stage { dir } => self.run_stage(dir.as_deref()),
            Command::VerifyCache => self.run_verify_cache(),
            Command::FlushCache => self.run_flush_cache(),
            Command::Locations => self.run_locations(),
            Command::Ancestors => self.run_ancestors(),
            Command::Revisions => self.run_revisions(),
            Command::Version => {
                println!("snapdir {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            Command::Defaults => run_defaults(),
            Command::Completions { shell } => {
                // Build-time hook: emit the requested shell's completion script
                // to stdout for the release pipeline to bundle. The bin name is
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
fn run_defaults() -> Result<()> {
    let bin_path = std::env::current_exe()
        .context("resolving the running binary path")?
        .display()
        .to_string();

    let mut lines: Vec<String> = Vec::new();

    // Group 1: the manifest tool's non-option defaults (`grep -v "^-"`). The
    // walk is in-process, so the manifest "binary" is this binary; CONTEXT /
    // EXCLUDE default to the (possibly empty) environment values.
    let manifest_context = std::env::var("SNAPDIR_MANIFEST_CONTEXT").unwrap_or_default();
    let manifest_exclude = std::env::var("SNAPDIR_MANIFEST_EXCLUDE").unwrap_or_default();
    lines.push(format!("SNAPDIR_MANIFEST_BIN_PATH={bin_path}"));
    lines.push(format!("SNAPDIR_MANIFEST_CONTEXT={manifest_context}"));
    lines.push(format!("SNAPDIR_MANIFEST_EXCLUDE={manifest_exclude}"));

    // Group 2: every SNAPDIR* env var (excluding *VERSION*), reformatted by
    // the oracle's `sed`/`tr` rules.
    for (key, value) in std::env::vars() {
        if !key.contains("SNAPDIR") || key.contains("VERSION") {
            continue;
        }
        lines.push(reformat_env_default(&key, &value));
    }

    // Group 3: the running binary path.
    lines.push(format!("SNAPDIR_BIN_PATH={bin_path}"));

    // Final `sort -u`: lexicographic sort, then dedup adjacent equals.
    lines.sort();
    lines.dedup();
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

impl Cli {
    /// `snapdir push [--store file://DIR] <path>`: walk `<path>` into a manifest
    /// and push its objects (objects-before-manifest, skip-if-present) to the
    /// resolved store. Prints the resulting snapshot id, matching the oracle.
    fn run_push(&self, path: Option<&Path>) -> Result<()> {
        let store = self.resolve_store()?;

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
            let cache = self.cache_store();
            let manifest = cache.get_manifest(id).with_context(|| {
                format!("manifest {id} not found in the local cache; stage or fetch it first")
            })?;
            let scratch = ScratchDir::new("push")?;
            cache
                .fetch_files(&manifest, scratch.path())
                .with_context(|| format!("materializing staged snapshot {id}"))?;
            store
                .push(&manifest, scratch.path())
                .with_context(|| format!("pushing snapshot {id} to store"))?;
            println!("{id}");
            let store_url = self
                .globals
                .store
                .as_deref()
                .context("missing --store option")?;
            self.log_event("push", id, store_url)?;
            return Ok(());
        }

        // Push always uses the default checksum surface (b3sum / keyed-b3sum),
        // relative paths, follow symlinks — the same wiring `snapdir id` uses.
        let manifest = self.build_manifest(path, false, false, None, &self.globals.exclude)?;
        let root = resolve_root(path).context("resolving push path")?;
        let id = snapshot_id(&manifest, &Blake3Hasher::new());
        store
            .push(&manifest, &root)
            .with_context(|| format!("pushing snapshot {id} to store"))?;
        println!("{id}");
        // Mirror the oracle's `_snapdir_log_event "push" "$id" "$store"` (L359):
        // record the snapshot in the catalog at the store URI so `locations`/
        // `revisions`/`ancestors` see it. Best-effort and only when the catalog
        // is enabled (`--catalog` / `SNAPDIR_CATALOG`), exactly like the oracle's
        // `_snapdir_log_event` no-op when no catalog adapter is configured.
        let store_url = self
            .globals
            .store
            .as_deref()
            .context("missing --store option")?;
        self.log_event("push", &id, store_url)?;
        Ok(())
    }

    /// `snapdir fetch --store … --id <id>`: read+verify the manifest from the
    /// store, materialize its objects into a scratch tree (the store verifies
    /// each object's BLAKE3), then file the manifest+objects into the local
    /// cache so a later `checkout` can reconstruct the tree offline.
    fn run_fetch(&self) -> Result<()> {
        let store = self.resolve_store()?;
        let id = self.require_id()?;
        let manifest = store
            .get_manifest(id)
            .with_context(|| format!("fetching manifest {id} from store"))?;

        // Materialize the verified objects into a scratch tree, then push that
        // tree into the cache store. This reuses the store's verify/atomic
        // persist on both legs and lands the cache in the same sharded layout.
        let scratch = ScratchDir::new("fetch")?;
        store
            .fetch_files(&manifest, scratch.path())
            .with_context(|| format!("fetching objects for snapshot {id}"))?;

        let cache = self.cache_store();
        cache
            .push(&manifest, scratch.path())
            .with_context(|| format!("saving snapshot {id} to the local cache"))?;
        if self.globals.verbose {
            eprintln!("SAVED: {id}");
        }
        Ok(())
    }

    /// `snapdir checkout --id <id> <dest>`: read the manifest from the local
    /// cache, materialize the tree at `<dest>`, and restore each entry's
    /// permissions so the checked-out tree re-manifests to the same snapshot id.
    fn run_checkout(&self, dir: Option<&Path>) -> Result<()> {
        let id = self.require_id()?;
        let cache = self.cache_store();
        let manifest = cache.get_manifest(id).with_context(|| {
            format!("manifest {id} not found locally; did you forget to fetch it?")
        })?;
        let dest = resolve_root(dir).context("resolving checkout destination")?;
        cache
            .fetch_files(&manifest, &dest)
            .with_context(|| format!("checking out snapshot {id} to {}", dest.display()))?;
        restore_permissions(&manifest, &dest)?;
        Ok(())
    }

    /// `snapdir pull` = fetch + checkout.
    fn run_pull(&self, path: Option<&Path>) -> Result<()> {
        self.run_fetch()?;
        self.run_checkout(path)
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
        let store = self.resolve_store()?;
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
        // Stage uses the same default checksum surface as `push`/`id`: b3sum
        // (or keyed-b3sum), relative paths, follow symlinks.
        let manifest = self.build_manifest(path, false, false, None, &self.globals.exclude)?;
        let root = resolve_root(path).context("resolving stage path")?;
        let id = snapshot_id(&manifest, &Blake3Hasher::new());
        let cache = self.cache_store();
        cache
            .push(&manifest, &root)
            .with_context(|| format!("staging snapshot {id} into the local cache"))?;
        println!("{id}");
        // Mirror the oracle's `_snapdir_log_event "stage" "$id" "$base_dir"`
        // (`snapdir` L826): record the staged snapshot in the catalog at the
        // staged base directory (the absolute path `stage` walked). Best-effort
        // and a no-op unless a catalog is enabled, so it never changes the
        // stdout bytes above.
        self.log_event("stage", &id, &root.to_string_lossy())?;
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
        let report = cache::verify_cache(&cache_dir, self.globals.purge, &Blake3Hasher::new())
            .with_context(|| format!("verifying cache at {}", cache_dir.display()))?;

        for checksum in &report.corrupt {
            eprintln!("Checksum mismatch for {checksum}");
        }
        if self.globals.purge && self.globals.verbose {
            for checksum in &report.purged {
                eprintln!("purged {checksum}");
            }
        }

        if report.is_clean() {
            return Ok(());
        }
        // Oracle: `failed=true` → `return 1`, even after purging.
        anyhow::bail!(
            "snapdir: {} corrupt object(s) in the cache",
            report.corrupt.len()
        )
    }

    /// `snapdir flush-cache`: empty the local cache via [`cache::flush_cache`]
    /// (objects + manifests). Idempotent on an already-empty / missing cache.
    fn run_flush_cache(&self) -> Result<()> {
        let cache_dir = self.cache_dir();
        cache::flush_cache(&cache_dir)
            .with_context(|| format!("flushing cache at {}", cache_dir.display()))?;
        Ok(())
    }

    /// `snapdir locations`: list every location tracked by the catalog (the
    /// latest record per location), one JSON line per record, in the
    /// catalog's order. Reproduces the original `snapdir locations` /
    /// `snapdir-sqlite3-catalog locations` query output.
    fn run_locations(&self) -> Result<()> {
        let catalog = self.open_catalog()?;
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
        let catalog = self.open_catalog()?;
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
        let catalog = self.open_catalog()?;
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

    /// Logs a catalog event (`event`/`id`/`location`) when the catalog is
    /// enabled, mirroring the oracle's `_snapdir_log_event` (`snapdir` L1620): a
    /// no-op unless `--catalog` / `SNAPDIR_CATALOG` selects a catalog. Uses the
    /// shipped [`SystemClock`] (`created_at` = `YYYY-MM-DD HH:MM:SS.SSS`), so the
    /// JSON timestamps are byte-shaped like the oracle's.
    fn log_event(&self, event: &str, id: &str, location: &str) -> Result<()> {
        // The oracle's `_snapdir_log_event` is a no-op when no catalog adapter
        // is configured; only persist when the catalog is enabled.
        let Some(db) = self.catalog_db_path() else {
            return Ok(());
        };
        let catalog =
            Catalog::open(&db).with_context(|| format!("opening catalog at {}", db.display()))?;
        catalog
            .log(event, id, location, &SystemClock)
            .with_context(|| format!("recording catalog event {event} for {id}"))?;
        Ok(())
    }

    /// Opens the catalog for a read query, erroring when no catalog is enabled —
    /// mirroring the oracle's `_snapdir_ensure_catalog` ("Missing
    /// `SNAPDIR_CATALOG` or `--catalog`").
    fn open_catalog(&self) -> Result<Catalog> {
        let db = self
            .catalog_db_path()
            .context("error: Missing SNAPDIR_CATALOG or --catalog")?;
        if let Some(parent) = db.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating catalog directory {}", parent.display()))?;
        }
        Catalog::open(&db).with_context(|| format!("opening catalog at {}", db.display()))
    }

    /// Resolves the catalog database path, or `None` when the catalog is not
    /// enabled (the oracle requires `--catalog` / `SNAPDIR_CATALOG` to be set;
    /// when unset its `_snapdir_log_event` is a no-op and the query commands
    /// error). The single redb backend replaces the oracle's pluggable adapters,
    /// so the value is interpreted as the db path: an absolute/relative path is
    /// used verbatim; a bare adapter name (e.g. the oracle's `"redb"` /
    /// `"sqlite3"`) selects `<cache-dir>/<name>-catalog.redb`.
    fn catalog_db_path(&self) -> Option<PathBuf> {
        let catalog = self.globals.catalog.as_deref()?;
        if catalog.is_empty() {
            return None;
        }
        // A path-like value (contains a path separator) is used as the db file
        // directly; otherwise treat it as a bare adapter name and place the db
        // under the cache dir.
        if catalog.contains(std::path::MAIN_SEPARATOR) {
            Some(PathBuf::from(catalog))
        } else {
            Some(self.cache_dir().join(format!("{catalog}-catalog.redb")))
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
    fn resolve_store(&self) -> Result<Box<dyn Store>> {
        let store_url = self
            .globals
            .store
            .as_deref()
            .context("missing --store option")?;
        let adapter = resolve_adapter(store_url).context("resolving --store protocol")?;
        store_for_adapter(&adapter, store_url)
    }

    /// The local cache as a `file://`-shaped store, rooted at the resolved cache
    /// directory. The cache uses the identical sharded layout as a `FileStore`.
    fn cache_store(&self) -> FileStore {
        FileStore::from_root(self.cache_dir())
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
    ) -> Result<Manifest> {
        let root = resolve_root(path).context("resolving manifest path")?;

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
        };

        // Select the checksum function. `b3sum` (or unset) is the default; the
        // CLI reads `SNAPDIR_MANIFEST_CONTEXT` (core stays env-pure) to switch
        // to keyed BLAKE3. `--checksum-bin` selects md5sum / sha256sum.
        match checksum_bin {
            None | Some("b3sum") => {
                let context = std::env::var("SNAPDIR_MANIFEST_CONTEXT").unwrap_or_default();
                if context.is_empty() {
                    walk_with(&root, &options, &Blake3Hasher::new())
                } else {
                    walk_with(&root, &options, &Blake3KeyedHasher::new(context))
                }
            }
            Some("md5sum") => walk_with(&root, &options, &Md5Hasher::new()),
            Some("sha256sum") => walk_with(&root, &options, &Sha256Hasher::new()),
            Some(other) => {
                anyhow::bail!("snapdir: unsupported --checksum-bin '{other}'")
            }
        }
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
fn store_for_adapter(adapter: &Adapter, store_url: &str) -> Result<Box<dyn Store>> {
    match adapter {
        Adapter::File => Ok(Box::new(FileStore::new(store_url))),
        Adapter::S3 => {
            let endpoint = std::env::var("SNAPDIR_S3_STORE_ENDPOINT_URL").ok();
            let store = S3Store::connect(store_url, endpoint.as_deref())
                .with_context(|| format!("connecting to S3 store {store_url}"))?;
            Ok(Box::new(store))
        }
        Adapter::B2 => {
            let endpoint = std::env::var("SNAPDIR_S3_STORE_ENDPOINT_URL").ok();
            let region = std::env::var("SNAPDIR_B2_REGION")
                .or_else(|_| std::env::var("AWS_REGION"))
                .ok();
            let store = B2Store::connect(store_url, endpoint.as_deref(), region.as_deref())
                .with_context(|| format!("connecting to B2 store {store_url}"))?;
            Ok(Box::new(store))
        }
        Adapter::Gcs => {
            let store = GcsStore::connect(store_url)
                .with_context(|| format!("connecting to GCS store {store_url}"))?;
            Ok(Box::new(store))
        }
        Adapter::External { .. } => {
            let store = ExternalStore::new(store_url)
                .with_context(|| format!("resolving external store for {store_url}"))?;
            Ok(Box::new(store))
        }
    }
}

/// Reformats a `SNAPDIR*` environment variable into the oracle's `defaults`
/// option line, faithfully reproducing the `sed -E 's|^_?SNAPDIR_|--|; s|_|-|g;'
/// | tr '[:upper:]' '[:lower:]'` pipeline applied to the `KEY=VALUE` text:
///
/// 1. strip a single leading `_SNAPDIR_` or `SNAPDIR_` prefix, replacing it with
///    `--` (only the first match, anchored at the start — `sed` with `^`);
/// 2. replace every remaining `_` with `-` (`s|_|-|g`, across the whole line —
///    so underscores in the value are rewritten too);
/// 3. lowercase the whole line (`tr '[:upper:]' '[:lower:]'`, value included).
///
/// So `SNAPDIR_CACHE_DIR=/x` → `--cache-dir=/x`, and
/// `_SNAPDIR_BIN_DIR=/X` → `--bin-dir=/x`.
fn reformat_env_default(key: &str, value: &str) -> String {
    let line = format!("{key}={value}");
    // `s|^_?SNAPDIR_|--|`: optional leading `_`, then `SNAPDIR_`, anchored.
    let stripped = line
        .strip_prefix("_SNAPDIR_")
        .or_else(|| line.strip_prefix("SNAPDIR_"));
    let body = match stripped {
        Some(rest) => format!("--{rest}"),
        None => line,
    };
    // `s|_|-|g` then `tr '[:upper:]' '[:lower:]'` over the whole line.
    body.replace('_', "-").to_lowercase()
}

/// Walks `root` with the given hasher, mapping the typed [`WalkError`] into an
/// `anyhow` error with context.
///
/// [`WalkError`]: snapdir_core::WalkError
fn walk_with<H: Hasher>(root: &Path, options: &WalkOptions, hasher: &H) -> Result<Manifest> {
    walk(root, options, hasher).with_context(|| format!("walking {}", root.display()))
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
fn resolve_root(path: Option<&Path>) -> Result<PathBuf> {
    let raw = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("getting current directory")?,
    };
    if raw.is_absolute() {
        return Ok(raw);
    }
    let cwd = std::env::current_dir().context("getting current directory")?;
    Ok(cwd.join(raw))
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
        let store = store_for_adapter(&adapter, "file:///tmp/snapdir-routing-test").unwrap();
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
        let routed = store_for_adapter(&adapter, "xyz://bucket/base");
        assert!(routed.is_ok());
    }

    #[test]
    fn remote_store_routing_rejects_invalid_protocol() {
        assert!(resolve_adapter("NotAScheme://x").is_err());
    }
}

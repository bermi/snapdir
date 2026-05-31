//! clap-derive command surface for the `snapdir` binary.
//!
//! The surface mirrors the Bash oracle (`./snapdir`) exactly: 14 subcommands
//! plus `version`, and the global options shared across commands. Pinned to the
//! scripts, not the docs (e.g. `--linked`, never `--link`).
//!
//! `manifest` and `id` are wired to `snapdir-core`'s in-process walk and emit
//! oracle-identical stdout (matching `./snapdir-manifest` / `./snapdir id`).
//! The remaining subcommands are still stubs; their behavior arrives in later
//! phases.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use snapdir_core::{
    expand_excludes, snapshot_id, walk, Blake3Hasher, Blake3KeyedHasher, ExcludeMatcher,
    FollowMode, Hasher, Manifest, ManifestEntry, Md5Hasher, PathMode, PathType, Sha256Hasher,
    Store, WalkOptions,
};
use snapdir_stores::{resolve_adapter, Adapter, FileStore};

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
    #[arg(long, global = true, value_name = "PATTERN")]
    pub exclude: Option<String>,

    /// Only include paths matching PATTERN.
    #[arg(long, global = true, value_name = "PATTERN")]
    pub paths: Option<String>,

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
        #[arg(long, value_name = "PATTERN")]
        exclude: Option<String>,

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
}

impl Cli {
    /// Dispatch the parsed command.
    ///
    /// `manifest` and `id` are wired to `snapdir-core`. The remaining
    /// subcommands are stubs that report "not implemented" via [`Err`]; real
    /// behavior arrives in later gates.
    ///
    /// # Errors
    ///
    /// Returns any error raised while resolving the path, building the exclude
    /// matcher, or walking the tree, plus a "not implemented" error for the
    /// stubbed subcommands.
    pub fn run(&self) -> Result<()> {
        match &self.command {
            Command::Manifest {
                absolute,
                no_follow,
                checksum_bin,
                exclude,
                path,
            } => {
                let exclude = exclude.as_deref().or(self.globals.exclude.as_deref());
                let manifest = self.build_manifest(
                    path.as_deref(),
                    *absolute,
                    *no_follow,
                    checksum_bin.as_deref(),
                    exclude,
                )?;
                println!("{manifest}");
                Ok(())
            }
            Command::Id { path } => {
                // `snapdir id` mirrors `./snapdir id`: the snapshot id is the
                // b3sum of the comment-stripped manifest text. The wrapper
                // walks with the default checksum (b3sum) and default
                // path/follow modes; the id is checksum-mode independent here.
                let exclude = self.globals.exclude.as_deref();
                let manifest = self.build_manifest(path.as_deref(), false, false, None, exclude)?;
                let id = snapshot_id(&manifest, &Blake3Hasher::new());
                println!("{id}");
                Ok(())
            }
            Command::Push { path } => self.run_push(path.as_deref()),
            Command::Fetch => self.run_fetch(),
            Command::Checkout { dir } => self.run_checkout(dir.as_deref()),
            Command::Pull { path } => self.run_pull(path.as_deref()),
            Command::Verify => self.run_verify(),
            other => {
                let name = match other {
                    Command::Stage { .. } => "stage",
                    Command::VerifyCache => "verify-cache",
                    Command::FlushCache => "flush-cache",
                    Command::Locations => "locations",
                    Command::Ancestors => "ancestors",
                    Command::Revisions => "revisions",
                    Command::Defaults => "defaults",
                    Command::Version => {
                        println!("snapdir {}", env!("CARGO_PKG_VERSION"));
                        return Ok(());
                    }
                    Command::Manifest { .. }
                    | Command::Id { .. }
                    | Command::Push { .. }
                    | Command::Fetch
                    | Command::Checkout { .. }
                    | Command::Pull { .. }
                    | Command::Verify => unreachable!(),
                };
                anyhow::bail!("snapdir: `{name}` is not implemented yet")
            }
        }
    }

    /// `snapdir push [--store file://DIR] <path>`: walk `<path>` into a manifest
    /// and push its objects (objects-before-manifest, skip-if-present) to the
    /// resolved store. Prints the resulting snapshot id, matching the oracle.
    fn run_push(&self, path: Option<&Path>) -> Result<()> {
        let store = self.resolve_store()?;
        // Push always uses the default checksum surface (b3sum / keyed-b3sum),
        // relative paths, follow symlinks — the same wiring `snapdir id` uses.
        let manifest =
            self.build_manifest(path, false, false, None, self.globals.exclude.as_deref())?;
        let root = resolve_root(path).context("resolving push path")?;
        let id = snapshot_id(&manifest, &Blake3Hasher::new());
        store
            .push(&manifest, &root)
            .with_context(|| format!("pushing snapshot {id} to store"))?;
        println!("{id}");
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
    fn run_verify(&self) -> Result<()> {
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

    /// Resolves `--store` to a concrete backend. For this gate only the built-in
    /// `file://` backend is wired; other adapters bail with a clear message.
    fn resolve_store(&self) -> Result<FileStore> {
        let store_url = self
            .globals
            .store
            .as_deref()
            .context("missing --store option")?;
        match resolve_adapter(store_url).context("resolving --store protocol")? {
            Adapter::File => Ok(FileStore::new(store_url)),
            other => anyhow::bail!(
                "snapdir: store adapter `{}` is not implemented yet",
                other.name()
            ),
        }
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
        exclude: Option<&str>,
    ) -> Result<Manifest> {
        let root = resolve_root(path).context("resolving manifest path")?;

        // Expand the exclude pattern. `%system%` forces no-follow; the runtime
        // `$HOME/.cache/` + cache-dir values come from the CLI (core is
        // env-pure). When no `--exclude` is given there is no filtering.
        let (home_cache, cache_dir) = exclude_runtime_paths(self.globals.cache_dir.as_deref());
        let expanded = expand_excludes(exclude.unwrap_or(""), &home_cache, &cache_dir);
        let matcher = match &expanded.pattern {
            Some(pattern) => {
                Some(ExcludeMatcher::new(pattern).context("compiling --exclude pattern")?)
            }
            None => None,
        };

        let follow = if no_follow || expanded.forces_no_follow {
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

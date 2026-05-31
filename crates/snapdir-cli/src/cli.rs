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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use snapdir_core::{
    expand_excludes, snapshot_id, walk, Blake3Hasher, Blake3KeyedHasher, ExcludeMatcher,
    FollowMode, Hasher, Manifest, Md5Hasher, PathMode, Sha256Hasher, WalkOptions,
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
            other => {
                let name = match other {
                    Command::Stage { .. } => "stage",
                    Command::Push { .. } => "push",
                    Command::Fetch => "fetch",
                    Command::Pull { .. } => "pull",
                    Command::Checkout { .. } => "checkout",
                    Command::Verify => "verify",
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
                    Command::Manifest { .. } | Command::Id { .. } => unreachable!(),
                };
                anyhow::bail!("snapdir: `{name}` is not implemented yet")
            }
        }
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

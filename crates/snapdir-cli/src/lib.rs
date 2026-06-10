//! snapdir CLI implementation library.
//!
//! Thin clap-derive front end for the `snapdir` orchestrator. `manifest` and
//! `id` are wired to `snapdir-core`'s in-process walk; the remaining
//! subcommands are wired to `snapdir-stores`/`snapdir-catalog`. Business
//! logic lives in the libraries — this crate only parses, dispatches, and
//! maps errors to exit codes.
//!
//! The shipped `snapdir` binary lives in the `snapdir` crate
//! (`crates/snapdir`), a shim whose `main` calls [`run`]. Two workspace
//! packages cannot both emit a `snapdir` bin (cargo warns "output filename
//! collision"), so the bin target moved there and this crate became the
//! implementation library. `snapdir-cli` versions <= 1.5 keep installing the
//! old binary.
//!
//! **Stability:** [`run`] is a *binary entrypoint*, not a stable library
//! API. It reads `std::env::args`, prints to stdout/stderr, and returns the
//! process exit code; no other items are exported and no semver guarantees
//! are made beyond "the `snapdir` binary keeps behaving as documented".

mod cli;
// The progress renderer engine, wired into every transfer command and gated by
// the --no-progress/--quiet/--color flags.
mod progress;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

/// Parses `std::env::args`, dispatches the subcommand, and maps the result
/// to the process exit code (success → 0, error → 1 after printing the
/// error chain to stderr).
///
/// This is the whole public surface: the `snapdir` binary's `main` is
/// `fn main() -> ExitCode { snapdir_cli::run() }`.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match cli.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

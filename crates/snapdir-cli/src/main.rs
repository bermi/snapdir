//! snapdir CLI binary.
//!
//! Thin clap-derive front end for the `snapdir` orchestrator. `manifest` and
//! `id` are wired to `snapdir-core`'s in-process walk; the remaining
//! subcommands are stubs pending later gates. Business logic lives in the
//! libraries — this binary only parses, dispatches, and maps errors to exit
//! codes.

mod cli;
// The progress renderer engine, wired into every transfer command and gated by
// the --no-progress/--quiet/--color flags.
mod progress;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

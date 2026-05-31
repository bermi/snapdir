//! snapdir CLI binary.
//!
//! Thin clap-derive front end for the `snapdir` orchestrator. This gate
//! (`cli-skeleton`) wires up the full command surface — all 14 subcommands and
//! the global options — but the subcommands are stubbed. Business logic lands
//! in later gates via `snapdir-core`, `snapdir-catalog`, and `snapdir-stores`.

mod cli;

use clap::Parser;

use crate::cli::Cli;

fn main() {
    let cli = Cli::parse();
    cli.run();
}

//! The `snapdir` binary: content-addressed directory snapshots.
//!
//! A shim over the `snapdir-cli` implementation library — parsing, dispatch,
//! and exit-code mapping all live in [`snapdir_cli::run`]. This crate exists
//! so `cargo install snapdir` installs the flagship binary.

fn main() -> std::process::ExitCode {
    snapdir_cli::run()
}

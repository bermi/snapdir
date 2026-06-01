//! Static CLI-surface snapshots for the `snapdir` binary, driven by `trycmd`.
//!
//! Each `tests/cmd/*.trycmd` case pins the *current* output of a deterministic,
//! path-stable invocation — top-level `--help`/`--version`, every subcommand's
//! `--help`, the unknown-command / missing-required-arg parse errors, and the
//! stub subcommands' real "not implemented yet" message. These snapshots are
//! generated from the actual binary (regenerate with `TRYCMD=overwrite`), so a
//! diff here means the CLI surface changed — not that a test drifted.
//!
//! Stateful end-to-end behavior (push/fetch/checkout round-trips over a temp
//! store) lives in `tests/e2e.rs` (`assert_cmd` + `assert_fs`); these cases are
//! intentionally restricted to help/error/stub text with no temp dirs or
//! absolute paths so the snapshots stay reproducible across machines.

#[test]
fn cli_surface_snapshots() {
    trycmd::TestCases::new().case("tests/cmd/*.trycmd");
}

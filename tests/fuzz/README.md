# snapdir manifest-parser fuzzing

A [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) target that feeds
arbitrary bytes to the `snapdir-core` manifest parser
(`ManifestEntry::parse_line` and `Manifest::parse`).

## Invariants

- **No panic / no UB** on any input (the core invariant — libFuzzer treats a
  panic, abort, OOM, or sanitizer trip as a crash).
- **Round-trip stability**: if a document parses, then
  `parse -> Display -> parse` succeeds and yields an *equal* `Manifest`.

## Requirements

Fuzzing requires a **nightly** toolchain and `cargo-fuzz` (neither is needed for
the normal workspace build, and this crate is intentionally isolated from the
root workspace via its own nested `[workspace]` table so the rest of the repo
builds without them):

```sh
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Running

From this directory (`tests/fuzz/`):

```sh
# Compile the fuzz target (CI smoke-test; runs on a cron).
cargo +nightly fuzz build

# Actually fuzz the manifest parser.
cargo +nightly fuzz run manifest_parser

# List available targets.
cargo +nightly fuzz list
```

CI runs `cargo +nightly fuzz build` on a schedule to keep the target compiling,
and may run `cargo fuzz run manifest_parser -- -max_total_time=<n>` for a
time-boxed campaign. Any reproducer is written to `artifacts/manifest_parser/`
and can be replayed with `cargo +nightly fuzz run manifest_parser <path>`.

## Workspace isolation

`Cargo.toml` declares its own empty `[workspace]` table. That makes cargo treat
this directory as a separate workspace, so the repository-root workspace
(`cargo build --workspace --locked`) never tries to compile it (it would fail
without a nightly toolchain + libFuzzer). Do **not** add `tests/fuzz` to the
root `Cargo.toml` members.

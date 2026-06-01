//! snapdir microbenchmark harness crate.
//!
//! This crate exists solely to host criterion benchmarks for snapdir's
//! hot paths (hashing, walk, manifest). It ships no library code; the
//! benchmark targets live under `benches/`. See `benches/hot_paths.rs`.

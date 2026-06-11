//! snapdir synthetic-scenario generator + catalog.
//!
//! This crate hosts the deterministic synthetic directory trees that the rest of
//! Phase 16 reuses: the determinism gate ([`tests/scenarios.rs`]), the criterion
//! benches ([`benches/hot_paths.rs`]), and the iai-callgrind perf gate. A single
//! source of truth means every gate measures and verifies the SAME corpora.
//!
//! ## Determinism guarantees
//!
//! Each [`Scenario`] [`materialize`](Scenario::materialize)s a tree of
//! **regular files and directories ONLY** — never a symlink (their lstat perms
//! differ across platforms and would break cross-platform golden ids). Every
//! entry gets an **explicit, umask-independent** mode via
//! [`PermissionsExt::from_mode`]: files `0o644`, dirs `0o755`. File contents come
//! from [`deterministic_bytes`] — a fixed byte ramp, NO RNG / clock / time /
//! randomness anywhere. Materializing the same scenario into two different
//! directories therefore yields byte-identical trees (and identical snapshot
//! ids).
//!
//! ## Purity
//!
//! The generator is **std-only** (std + [`PermissionsExt`]). It just writes
//! files; it does NOT depend on `snapdir-core`. The determinism/perf gates that
//! *consume* these trees pull `snapdir-core` / `snapdir-stores` in as
//! dev-dependencies.
//!
//! [`PermissionsExt`]: std::os::unix::fs::PermissionsExt

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Explicit, umask-independent file mode for every materialized regular file.
const FILE_MODE: u32 = 0o644;
/// Explicit, umask-independent directory mode for every materialized directory.
const DIR_MODE: u32 = 0o755;

/// Fills a buffer of `len` bytes with a deterministic, non-trivial pattern (no
/// RNG, so corpora are reproducible across runs and machines).
///
/// A simple byte ramp masked to the low 8 bits: cheap, fully deterministic, and
/// non-uniform so a hasher can't shortcut on a constant page. Moved here from
/// `benches/hot_paths.rs` so the benches, the determinism gate, and the perf
/// gate all share one definition.
#[must_use]
pub fn deterministic_bytes(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i.wrapping_mul(31).wrapping_add(7) & 0xff).expect("masked to u8"))
        .collect()
}

/// Avalanches `seed` into a well-mixed nonzero `xorshift64*` start state.
///
/// This is the splitmix64 finalizer. It matters because callers seed objects by
/// `base + i` for consecutive `i`: feeding those *adjacent* seeds straight into
/// `xorshift64*` would leave the per-object streams correlated (zstd's window
/// then finds cross-object redundancy and an "incompressible" corpus compresses
/// anyway). The finalizer's full avalanche makes `seed` and `seed + 1` produce
/// statistically independent streams. The `| 1` guarantees the nonzero state
/// `xorshift64*` requires.
const fn mix_seed(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) | 1
}

/// A small, fixed word table used by [`text_like_bytes`] to synthesize
/// compressible, text-shaped corpora. Common English-ish tokens so the byte
/// stream has the redundancy a real text snapshot would (and zstd can shrink
/// it well below v1), with NO RNG, clock, or external data.
const WORD_TABLE: &[&str] = &[
    "the",
    "snapshot",
    "store",
    "object",
    "manifest",
    "content",
    "address",
    "hash",
    "blake3",
    "stream",
    "pack",
    "verify",
    "commit",
    "durable",
    "file",
    "directory",
    "checksum",
    "snapdir",
    "transfer",
    "compress",
    "and",
    "of",
    "into",
    "a",
    "with",
    "every",
    "record",
    "bytes",
    "wire",
    "format",
];

/// Builds `len` bytes of **deterministic, compressible, text-like** content by
/// concatenating space-separated words from [`WORD_TABLE`], chosen by a seeded
/// xorshift index, then truncated/zero-padded to exactly `len`.
///
/// The high token redundancy means zstd compresses this materially better than
/// the plain v1 wire — the realistic "small-text snapshot" workload the pack
/// path targets. Fully deterministic (no RNG/clock); two calls with the same
/// `(len, seed)` are byte-identical. Std-only, no new deps.
///
/// Note: [`deterministic_bytes`]' 256-byte ramp is *trivially* compressible and
/// only suitable as a best-case third data point, not a realistic corpus — use
/// this for the text scenario.
#[must_use]
pub fn text_like_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = mix_seed(seed); // avalanched: adjacent seeds → independent streams
    while out.len() < len {
        // xorshift64* step → pick a word.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let idx = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as usize % WORD_TABLE.len();
        out.extend_from_slice(WORD_TABLE[idx].as_bytes());
        out.push(b' ');
    }
    out.truncate(len);
    out
}

/// Builds `len` bytes of **deterministic, effectively incompressible** content
/// using a seeded `xorshift64*` PRNG.
///
/// `xorshift64*` (seeded through the [`mix_seed`] splitmix64 finalizer so
/// adjacent object seeds yield INDEPENDENT, non-cross-correlated streams) has
/// output close to uniform, so zstd cannot shrink it — the worst case for the
/// wire, proving the compressed path never materially *expands* a payload it
/// can't help. Fully deterministic: same `(len, seed)` ⇒ same bytes. No RNG
/// crate, no clock, no new deps.
#[must_use]
pub fn incompressible_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = mix_seed(seed); // avalanched: adjacent seeds → independent streams
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let word = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let take = (len - out.len()).min(8);
        out.extend_from_slice(&word.to_le_bytes()[..take]);
    }
    out
}

/// Which tier a [`Scenario`] belongs to.
///
/// `Gate` scenarios are tiny/fast — they run inside `cargo test` (the
/// determinism gate) and must finish in well under a second. `Bench` scenarios
/// are larger (but still modest) corpora for criterion / the perf gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Tiny, fast: materialized + walked inside the determinism gate.
    Gate,
    /// Larger (but modest): materialized for criterion / iai perf benches.
    Bench,
}

/// A named, deterministic synthetic directory scenario.
///
/// [`materialize`](Scenario::materialize) writes the tree (regular files +
/// directories only) into a caller-provided directory. The optional
/// [`exclude`](Scenario::exclude) pattern is carried verbatim for later gates
/// that exercise snapdir's `--exclude` (an ERE matched anywhere in the absolute
/// scan path); it is NOT applied during materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scenario {
    /// Stable scenario name (also used as a sub-id by criterion / the gate).
    pub name: &'static str,
    /// The tier this scenario belongs to.
    pub tier: Tier,
    /// An optional `--exclude` pattern carried for later gates. A distinctive
    /// token (e.g. `skip/`) that will not match the tempdir ancestor.
    pub exclude: Option<&'static str>,
    /// The shape of the tree this scenario materializes.
    shape: Shape,
}

/// The internal generator recipe for a [`Scenario`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `files` tiny files of `bytes` each, round-robin across `dirs` subdirs.
    ManySmall {
        files: usize,
        bytes: usize,
        dirs: usize,
    },
    /// `files` larger files of `bytes` each in the root.
    FewLarge { files: usize, bytes: usize },
    /// A single chain `d000/d001/.../d{depth-1}/leaf.bin`, `depth` deep.
    DeepNest { depth: usize, bytes: usize },
    /// One dir holding `children` sibling files of `bytes` each.
    WideFanout { children: usize, bytes: usize },
    /// A mix: a few root files, a couple of nested subtrees.
    Mixed,
    /// `copies` files with IDENTICAL content (BLAKE3 checksums collide).
    Dedup { copies: usize, bytes: usize },
    /// A small tree plus an excludable `skip/` subdir (pattern on `exclude`).
    WithExcludes,
    /// Empty files AND empty directories.
    Edge,
}

impl Scenario {
    /// Writes this scenario's tree into `dir` (which must already exist).
    ///
    /// Only regular files (`0o644`) and directories (`0o755`) are created — no
    /// symlinks. Contents are [`deterministic_bytes`]; there is no randomness,
    /// so two separate materializations are byte-identical.
    ///
    /// # Errors
    ///
    /// Returns the first [`std::io::Error`] from any filesystem operation.
    pub fn materialize(&self, dir: &Path) -> std::io::Result<()> {
        match self.shape {
            Shape::ManySmall { files, bytes, dirs } => {
                for d in 0..dirs {
                    mkdir(&dir.join(format!("d{d:03}")))?;
                }
                let content = deterministic_bytes(bytes);
                for i in 0..files {
                    let sub = dir.join(format!("d{:03}", i % dirs));
                    write_file(&sub.join(format!("f{i:05}.bin")), &content)?;
                }
            }
            Shape::FewLarge { files, bytes } => {
                let content = deterministic_bytes(bytes);
                for i in 0..files {
                    write_file(&dir.join(format!("big{i:02}.bin")), &content)?;
                }
            }
            Shape::DeepNest { depth, bytes } => {
                let mut cur = dir.to_path_buf();
                for d in 0..depth {
                    cur = cur.join(format!("d{d:03}"));
                    mkdir(&cur)?;
                }
                write_file(&cur.join("leaf.bin"), &deterministic_bytes(bytes))?;
            }
            Shape::WideFanout { children, bytes } => {
                let fan = dir.join("fan");
                mkdir(&fan)?;
                let content = deterministic_bytes(bytes);
                for i in 0..children {
                    write_file(&fan.join(format!("c{i:04}.bin")), &content)?;
                }
            }
            Shape::Mixed => {
                // A few root files of varying sizes.
                for (name, n) in [
                    ("root_a.bin", 16usize),
                    ("root_b.bin", 256),
                    ("root_c.bin", 0),
                ] {
                    write_file(&dir.join(name), &deterministic_bytes(n))?;
                }
                // A nested subtree.
                let nested = dir.join("dir_a").join("nested");
                mkdir(&nested)?;
                write_file(&dir.join("dir_a").join("a1.bin"), &deterministic_bytes(48))?;
                write_file(&nested.join("deep.bin"), &deterministic_bytes(72))?;
                // A second, shallower subtree.
                let dir_b = dir.join("dir_b");
                mkdir(&dir_b)?;
                write_file(&dir_b.join("b1.bin"), &deterministic_bytes(128))?;
                write_file(&dir_b.join("b2.bin"), &deterministic_bytes(8))?;
            }
            Shape::Dedup { copies, bytes } => {
                // Every file has IDENTICAL content, so their BLAKE3 checksums
                // collide -> unique-object count < file count.
                let content = deterministic_bytes(bytes);
                for i in 0..copies {
                    write_file(&dir.join(format!("dup{i:03}.bin")), &content)?;
                }
            }
            Shape::WithExcludes => {
                // The kept tree.
                write_file(&dir.join("keep_a.bin"), &deterministic_bytes(32))?;
                let kept = dir.join("kept");
                mkdir(&kept)?;
                write_file(&kept.join("k1.bin"), &deterministic_bytes(40))?;
                // The excludable subtree (matched by the `skip/` pattern carried
                // on `self.exclude`). A distinctive token that will not match a
                // tempdir ancestor.
                let skip = dir.join("skip");
                mkdir(&skip)?;
                write_file(&skip.join("ignored.bin"), &deterministic_bytes(64))?;
            }
            Shape::Edge => {
                // Empty files...
                write_file(&dir.join("empty_a.bin"), &[])?;
                write_file(&dir.join("empty_b.bin"), &[])?;
                // ...AND empty directories.
                mkdir(&dir.join("empty_dir_a"))?;
                mkdir(&dir.join("empty_dir_b"))?;
                // A non-empty dir holding an empty file, for good measure.
                let sub = dir.join("sub");
                mkdir(&sub)?;
                write_file(&sub.join("empty_c.bin"), &[])?;
            }
        }
        Ok(())
    }
}

/// Creates `dir` (and missing parents) and pins its mode to `0o755`.
fn mkdir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, PermissionsExt::from_mode(DIR_MODE))
}

/// Writes `content` to `path` and pins its mode to `0o644`.
fn write_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    fs::set_permissions(path, PermissionsExt::from_mode(FILE_MODE))
}

/// The full scenario catalog (both tiers), in a stable order: GATE first, then
/// BENCH.
#[must_use]
pub fn all() -> Vec<Scenario> {
    let mut scenarios = gate_scenarios();
    scenarios.extend(bench_scenarios());
    scenarios
}

/// The GATE-tier scenarios (tiny/fast) used by the determinism gate.
#[must_use]
pub fn gate_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "many_small",
            tier: Tier::Gate,
            exclude: None,
            shape: Shape::ManySmall {
                files: 24,
                bytes: 16,
                dirs: 4,
            },
        },
        Scenario {
            name: "few_large",
            tier: Tier::Gate,
            exclude: None,
            shape: Shape::FewLarge {
                files: 3,
                bytes: 4 * 1024,
            },
        },
        Scenario {
            name: "deep_nest",
            tier: Tier::Gate,
            exclude: None,
            shape: Shape::DeepNest {
                depth: 12,
                bytes: 16,
            },
        },
        Scenario {
            name: "wide_fanout",
            tier: Tier::Gate,
            exclude: None,
            shape: Shape::WideFanout {
                children: 32,
                bytes: 16,
            },
        },
        Scenario {
            name: "mixed",
            tier: Tier::Gate,
            exclude: None,
            shape: Shape::Mixed,
        },
        Scenario {
            name: "dedup",
            tier: Tier::Gate,
            exclude: None,
            shape: Shape::Dedup {
                copies: 8,
                bytes: 32,
            },
        },
        Scenario {
            name: "with_excludes",
            tier: Tier::Gate,
            // ERE matched anywhere in the absolute scan path; `skip/` is
            // distinctive enough not to collide with a tempdir ancestor.
            exclude: Some("skip/"),
            shape: Shape::WithExcludes,
        },
        Scenario {
            name: "edge",
            tier: Tier::Gate,
            exclude: None,
            shape: Shape::Edge,
        },
    ]
}

/// The BENCH-tier scenarios (larger, but modest) used by criterion / the perf
/// gate.
#[must_use]
pub fn bench_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "many_small_bench",
            tier: Tier::Bench,
            exclude: None,
            shape: Shape::ManySmall {
                files: 4096,
                bytes: 64,
                dirs: 32,
            },
        },
        Scenario {
            name: "few_large_bench",
            tier: Tier::Bench,
            exclude: None,
            shape: Shape::FewLarge {
                files: 8,
                bytes: 4 * 1024 * 1024,
            },
        },
        Scenario {
            name: "deep_nest_bench",
            tier: Tier::Bench,
            exclude: None,
            shape: Shape::DeepNest {
                depth: 64,
                bytes: 64,
            },
        },
        Scenario {
            name: "wide_fanout_bench",
            tier: Tier::Bench,
            exclude: None,
            shape: Shape::WideFanout {
                children: 2048,
                bytes: 64,
            },
        },
        Scenario {
            name: "dedup_bench",
            tier: Tier::Bench,
            exclude: None,
            shape: Shape::Dedup {
                copies: 512,
                bytes: 256,
            },
        },
    ]
}

#!/bin/sh
# build-sandbox.sh — deterministic, idempotent QA sandbox builder for the
# Phase-30 adversarial CLI DX/UX review.
#
# Pure POSIX shell. NO compile, NO RNG (no $RANDOM, no /dev/urandom), NO clock.
# Re-running rebuilds tree/ byte-for-byte identically, so `snapdir id` over the
# tree is stable across runs and machines. The shapes mirror
# benches/src/lib.rs `gate_scenarios()`, materialized directly in shell.
#
# Layout produced under .gatesmith/evidence/dx-sandbox/ (gitignored):
#   tree/            — a realistic source tree the personas snapshot
#   store/           — empty local file store to push to
#   objects-store/   — empty split object pool
#
# Usage:  sh utils/dx/build-sandbox.sh
set -eu

# --- Resolve the sandbox root relative to the repo, not the cwd. -------------
# This script lives at <repo>/utils/dx/build-sandbox.sh.
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
sandbox="$repo_root/.gatesmith/evidence/dx-sandbox"
tree="$sandbox/tree"

# --- Deterministic byte generators (no RNG, no clock). -----------------------
# A fixed-length run of a single repeating pattern. Same args => same bytes.
# `yes` emits a constant line forever; `head -c` truncates to an exact size.
# This is fully reproducible (constant pattern), unlike /dev/urandom.
emit_bytes() { # emit_bytes <size_bytes> <pattern>
    yes "$2" | head -c "$1"
}

# Write exactly <size> bytes built from <pattern> to <path>, creating parents.
write_file() { # write_file <path> <size_bytes> <pattern>
    mkdir -p -- "$(dirname -- "$1")"
    if [ "$2" -eq 0 ]; then
        : >"$1"
    else
        emit_bytes "$2" "$3" >"$1"
    fi
}

# --- Wipe + rebuild tree/ deterministically (idempotent). --------------------
rm -rf -- "$tree"
mkdir -p -- "$tree"

# (1) MANY SMALL FILES: ~2000 files of a few KB, spread across 16 subdirs.
#     Round-robin like Shape::ManySmall. ~3 KB each => ~6 MB total.
small_dirs=16
small_files=2000
small_bytes=3072
i=0
d=0
while [ "$d" -lt "$small_dirs" ]; do
    mkdir -p -- "$tree/small/d$(printf '%02d' "$d")"
    d=$((d + 1))
done
while [ "$i" -lt "$small_files" ]; do
    sub=$((i % small_dirs))
    write_file "$tree/small/d$(printf '%02d' "$sub")/f$(printf '%05d' "$i").bin" \
        "$small_bytes" "small-file-payload-row-$sub"
    i=$((i + 1))
done

# (2) A FEW LARGE FILES: 3 files of ~8 MB each (Shape::FewLarge), so a snapshot
#     takes a noticeable moment. Deterministic constant pattern => reproducible.
large_bytes=8388608 # 8 * 1024 * 1024
i=0
while [ "$i" -lt 3 ]; do
    write_file "$tree/large/big$(printf '%02d' "$i").bin" \
        "$large_bytes" "large-object-stream-block-pattern-fixed"
    i=$((i + 1))
done

# (3) DEEP NESTED CHAIN: 12 levels d000/d001/.../d011/leaf.bin (Shape::DeepNest).
deep="$tree/deep"
d=0
cur="$deep"
while [ "$d" -lt 12 ]; do
    cur="$cur/d$(printf '%03d' "$d")"
    mkdir -p -- "$cur"
    d=$((d + 1))
done
write_file "$cur/leaf.bin" 64 "deep-leaf"

# (4) WIDE FAN-OUT: 64 sibling files in one dir (Shape::WideFanout).
mkdir -p -- "$tree/fan"
i=0
while [ "$i" -lt 64 ]; do
    write_file "$tree/fan/c$(printf '%04d' "$i").bin" 256 "fan-sibling-$i"
    i=$((i + 1))
done

# (5) DUPLICATE-CONTENT FILES: several files with IDENTICAL bytes => identical
#     checksums (Shape::Dedup). Same pattern + same size => byte-identical.
i=0
while [ "$i" -lt 12 ]; do
    write_file "$tree/dup/dup$(printf '%03d' "$i").bin" 4096 "identical-dedup-payload"
    i=$((i + 1))
done

# (6) EMPTY FILES AND EMPTY DIRECTORIES (Shape::Edge).
write_file "$tree/edge/empty_a.bin" 0 ""
write_file "$tree/edge/empty_b.bin" 0 ""
mkdir -p -- "$tree/edge/empty_dir_a"
mkdir -p -- "$tree/edge/empty_dir_b"
write_file "$tree/edge/sub/empty_c.bin" 0 ""

# (7) EXCLUDABLE SUBDIR: node_modules/ and skip/ so --exclude/--paths bite.
write_file "$tree/keep/keep_a.bin" 512 "kept-content"
write_file "$tree/node_modules/left-pad/index.js" 2048 "module-exports-noise"
write_file "$tree/node_modules/.cache/blob.bin" 4096 "cache-blob-noise"
write_file "$tree/skip/ignored.bin" 1024 "ignored-content"

# A couple of realistic top-level files so the tree reads like a project root.
write_file "$tree/README.md" 256 "readme-line-fixed"
write_file "$tree/config.toml" 128 "config-line-fixed"

# --- Stores the personas push to (empty). ------------------------------------
mkdir -p -- "$sandbox/store"
mkdir -p -- "$sandbox/objects-store"

# --- Summary. ----------------------------------------------------------------
file_count=$(find "$tree" -type f | wc -l | tr -d ' ')
dir_count=$(find "$tree" -type d | wc -l | tr -d ' ')
# Portable total byte count: sum `wc -c` over all regular files.
total_bytes=$(find "$tree" -type f -exec wc -c {} + 2>/dev/null | tail -n 1 | awk '{print $1}')
[ -n "${total_bytes:-}" ] || total_bytes=0

printf 'dx-sandbox built at %s\n' "$sandbox"
printf 'tree: %s files, %s dirs, %s bytes (%s MB)\n' \
    "$file_count" "$dir_count" "$total_bytes" "$((total_bytes / 1024 / 1024))"
printf 'top-level layout:\n'
ls -1 "$tree" | sed 's/^/  tree\//'
printf '  store/ (empty)  objects-store/ (empty)\n'

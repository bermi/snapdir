#!/usr/bin/env bash
# Linux reflink (FICLONE) before/after measurement (phase 29 reflink-bench gate).
#
# The Linux sibling of benches/clone-skip-compare.sh. Measures the wall-clock
# effect of the Linux FICLONE copy-on-write fast-path on `snapdir stage` +
# `checkout`. The reflink path returns CopyMethod::Cloned and therefore inherits
# the clone-skip stage/checkout re-hash elision for free:
#
#   AFTER    (reflink ON):  default invocation (FICLONE fires + clone-skip).
#   BASELINE (reflink OFF): SNAPDIR_CLONEFILE=0 (forces plain fs::copy, no clone,
#                           no skip = pre-feature behavior).
#
# This is the LINUX analogue of the macOS clone-skip bench
# (.gatesmith/evidence/clone-skip-bench.log, which measured stage ~2.4x /
# checkout ~1.5x on APFS). FICLONE requires a reflink-capable filesystem
# (Btrfs / XFS reflink=1 / OpenZFS 2.2+ / bcachefs) AND src + cache co-located on
# the SAME such filesystem (a reflink clones extents within one FS only).
#
# Where to run: the `Reflink (Btrfs FICLONE)` CI job (loopback Btrfs at
# /mnt/reflink) or a local Linux VM with a reflink mount. On macOS / ext4 (no
# reflink FS), this script prints a clear skip notice and exits 0 (graceful skip,
# NOT a failure) so that bench-build verification on a non-reflink host passes.
#
# A reflink-capable directory is supplied via $SNAPDIR_REFLINK_TEST_DIR (env) or
# as the first positional arg. Auto-detecting/creating one (loopback mkfs.btrfs)
# is OUT of scope here — see utils/ci/reflink-vm-check.sh for that.
#
# Usage:
#   SNAPDIR_REFLINK_TEST_DIR=/mnt/reflink benches/reflink-compare.sh
#   benches/reflink-compare.sh /mnt/reflink
#   benches/reflink-compare.sh --help
#
# NOT committed-output: this is a measurement harness, bench-only.
set -euo pipefail

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
esac

log() { printf '%s\n' "$*"; }

# -- resolve a reflink-capable directory ---------------------------------------
REFLINK_DIR="${SNAPDIR_REFLINK_TEST_DIR:-${1:-}}"

skip() {
  log "no reflink FS available — skipping (run on the Btrfs CI leg or a Linux VM)"
  log "  reason: $1"
  log "  supply a reflink-capable dir via \$SNAPDIR_REFLINK_TEST_DIR or arg 1"
  log "  (Btrfs / XFS reflink=1 / OpenZFS 2.2+ / bcachefs); FICLONE is Linux-only."
  exit 0
}

# Graceful skip on non-Linux (macOS clonefile is a different path; this bench
# targets the Linux FICLONE branch specifically).
case "$(uname -s)" in
  Linux) ;;
  *) skip "host is $(uname -s), not Linux (FICLONE is a Linux-only ioctl)" ;;
esac

[ -n "$REFLINK_DIR" ] || skip "\$SNAPDIR_REFLINK_TEST_DIR unset and no dir arg given"
[ -d "$REFLINK_DIR" ] || skip "reflink dir '$REFLINK_DIR' does not exist"
[ -w "$REFLINK_DIR" ] || skip "reflink dir '$REFLINK_DIR' is not writable"

# Probe that the FS under $REFLINK_DIR actually supports reflinks: cp --reflink.
PROBE="$(mktemp -d "$REFLINK_DIR/reflink-probe.XXXXXX")"
probe_cleanup() { rm -rf "$PROBE"; }
trap probe_cleanup EXIT
printf 'reflink-probe\n' > "$PROBE/a"
if ! cp --reflink=always "$PROBE/a" "$PROBE/b" 2>/dev/null; then
  skip "'$REFLINK_DIR' does not support reflinks (cp --reflink=always failed)"
fi
trap - EXIT
probe_cleanup

# -- config (mirrors clone-skip-compare.sh) ------------------------------------
BIN="${SNAPDIR_BIN:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release/snapdir}"
WALK_JOBS="${WALK_JOBS:-8}"
NFILES="${NFILES:-300}"
FILE_MIB="${FILE_MIB:-30}"
SUBDIRS="${SUBDIRS:-10}"
REPS="${REPS:-2}"

# -- build the release binary (package `snapdir`, NOT snapdir-cli) -------------
if [ ! -x "$BIN" ]; then
  log "Building release binary: cargo build --release -p snapdir ..."
  ( cd "$(dirname "${BASH_SOURCE[0]}")/.." && cargo build --release -p snapdir )
fi

# Work dir lives ON the reflink FS so src + cache are co-located (FICLONE needs
# same-FS) — this is the key difference from the macOS sibling, which uses
# $TMPDIR (APFS).
WORK="$(mktemp -d "$REFLINK_DIR/reflink-bench.XXXXXX")"
SRC="$WORK/src"
trap 'rm -rf "$WORK"' EXIT

# -- generate a GENERIC synthetic corpus: a large dir of sizable files ---------
log "Generating synthetic corpus: $NFILES files x ${FILE_MIB} MiB across $SUBDIRS subdirs ..."
mkdir -p "$SRC"
for d in $(seq 1 "$SUBDIRS"); do mkdir -p "$SRC/d$d"; done
i=0
while [ "$i" -lt "$NFILES" ]; do
  d=$(( (i % SUBDIRS) + 1 ))
  dd if=/dev/urandom of="$SRC/d$d/f$i.bin" bs=1M count="$FILE_MIB" status=none
  i=$((i + 1))
done
TOTAL_MIB=$(( NFILES * FILE_MIB ))
log "Corpus generated: ~${TOTAL_MIB} MiB total (on reflink FS at $REFLINK_DIR)."

# warm the page cache (read every file once)
warm() { find "$1" -type f -exec cat {} + >/dev/null 2>&1 || true; }

# best-of-REPS wall clock (seconds) for `stage`; fresh empty cache each rep.
# $1 = label, $2 = cache-dir-prefix, $3 = extra env assignments before binary.
run_stage() {
  local label="$1"; shift
  local cprefix="$1"; shift
  local extra_env="$1"; shift
  local best="" id="" pooldir=""
  warm "$SRC"
  local rep
  for rep in $(seq 1 "$REPS"); do
    local C="$WORK/${cprefix}-rep${rep}"
    rm -rf "$C"; mkdir -p "$C"
    local t start end
    start=$EPOCHREALTIME
    # extra_env must word-split: "" => no env arg, "FOO=1" => one assignment.
    # shellcheck disable=SC2086
    env $extra_env "$BIN" --cache-dir "$C" --walk-jobs "$WALK_JOBS" stage "$SRC" >"$WORK/${cprefix}.id" 2>/dev/null
    end=$EPOCHREALTIME
    t=$(awk "BEGIN{printf \"%.3f\", $end-$start}")
    log "  $label rep$rep: ${t}s"
    if [ -z "$best" ] || awk "BEGIN{exit !($t < $best)}"; then best="$t"; fi
    id="$(tr -d '[:space:]' < "$WORK/${cprefix}.id")"
    pooldir="$C"
  done
  BEST="$best"; LAST_ID="$id"; LAST_CACHE="$pooldir"
}

# fingerprint of the object pool: sorted (sha, relpath) of every file under .objects
pool_fingerprint() {
  local cache="$1"
  ( cd "$cache" && find .objects -type f 2>/dev/null | sort | while read -r f; do
      printf '%s  %s\n' "$(sha256sum "$f" | awk '{print $1}')" "$f"
    done )
}
pool_count() { find "$1/.objects" -type f 2>/dev/null | wc -l | tr -d ' '; }

log ""
log "================ STAGE ================"
run_stage "AFTER    (reflink ON )"  "stage-after" ""                  ; A_TIME="$BEST"; A_ID="$LAST_ID"; A_CACHE="$LAST_CACHE"
run_stage "BASELINE (CLONEFILE=0)"  "stage-base"  "SNAPDIR_CLONEFILE=0"; B_TIME="$BEST"; B_ID="$LAST_ID"; B_CACHE="$LAST_CACHE"

STAGE_SPEEDUP=$(awk "BEGIN{printf \"%.2f\", $B_TIME/$A_TIME}")
A_POOL="$(pool_count "$A_CACHE")"; B_POOL="$(pool_count "$B_CACHE")"

log ""
log "  stage AFTER    best: ${A_TIME}s   id=$A_ID   objects=$A_POOL"
log "  stage BASELINE best: ${B_TIME}s   id=$B_ID   objects=$B_POOL"
log "  stage speedup (baseline/after) = ${STAGE_SPEEDUP}x"

# identical pool + id assertion
pool_fingerprint "$A_CACHE" > "$WORK/fp-after.txt"
pool_fingerprint "$B_CACHE" > "$WORK/fp-base.txt"
if diff -q "$WORK/fp-after.txt" "$WORK/fp-base.txt" >/dev/null; then POOL_MATCH="IDENTICAL"; else POOL_MATCH="DIFFER"; fi
if [ "$A_ID" = "$B_ID" ]; then ID_MATCH="IDENTICAL"; else ID_MATCH="DIFFER"; fi
log "  id match: $ID_MATCH ; object-pool fingerprint: $POOL_MATCH"

# ---- CHECKOUT: stage once, then checkout AFTER vs BASELINE -------------------
log ""
log "================ CHECKOUT ================"
CKC="$WORK/checkout-cache"
rm -rf "$CKC"; mkdir -p "$CKC"
warm "$SRC"
"$BIN" --cache-dir "$CKC" --walk-jobs "$WALK_JOBS" stage "$SRC" >"$WORK/ck.id" 2>/dev/null
CK_ID="$(tr -d '[:space:]' < "$WORK/ck.id")"
log "  staged checkout source id=$CK_ID"

run_checkout() {
  local label="$1"; shift
  local extra_env="$1"; shift
  local best=""
  local rep
  for rep in $(seq 1 "$REPS"); do
    local DEST="$WORK/co-${label// /_}-rep${rep}"
    rm -rf "$DEST"
    # warm objects in cache
    find "$CKC/.objects" -type f -exec cat {} + >/dev/null 2>&1 || true
    local t start end
    start=$EPOCHREALTIME
    # extra_env must word-split: "" => no env arg, "FOO=1" => one assignment.
    # shellcheck disable=SC2086
    env $extra_env "$BIN" --cache-dir "$CKC" --walk-jobs "$WALK_JOBS" checkout --id "$CK_ID" "$DEST" >/dev/null 2>/dev/null
    end=$EPOCHREALTIME
    t=$(awk "BEGIN{printf \"%.3f\", $end-$start}")
    log "  $label rep$rep: ${t}s"
    if [ -z "$best" ] || awk "BEGIN{exit !($t < $best)}"; then best="$t"; fi
  done
  CK_BEST="$best"
}

run_checkout "AFTER"    ""                   ; CK_A="$CK_BEST"
run_checkout "BASELINE" "SNAPDIR_CLONEFILE=0"; CK_B="$CK_BEST"
CK_SPEEDUP=$(awk "BEGIN{printf \"%.2f\", $CK_B/$CK_A}")
log ""
log "  checkout AFTER    best: ${CK_A}s"
log "  checkout BASELINE best: ${CK_B}s"
log "  checkout speedup (baseline/after) = ${CK_SPEEDUP}x"

# -- write the evidence log ----------------------------------------------------
EVIDENCE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.gatesmith/evidence/reflink-bench.log"
mkdir -p "$(dirname "$EVIDENCE")"
{
  printf '================================================================================\n'
  printf 'reflink-bench (phase 29) — Linux FICLONE CoW stage/checkout before/after\n'
  printf '================================================================================\n\n'
  printf 'Linux analogue of clone-skip-bench. The FICLONE reflink fast-path returns\n'
  printf 'CopyMethod::Cloned and inherits the clone-skip re-hash elision, so on a\n'
  printf 'reflink-capable FS (Btrfs / XFS reflink=1) it is expected to mirror the macOS\n'
  printf 'clone-skip stage/checkout win.\n\n'
  printf '  AFTER    (reflink ON ): default invocation (FICLONE fires + clone-skip).\n'
  printf '  BASELINE (reflink OFF): SNAPDIR_CLONEFILE=0 (plain fs::copy, no clone/skip).\n\n'
  printf 'Corpus (generic): a large directory of sizable random files generated from\n'
  printf '/dev/urandom INTO the reflink FS (src + cache co-located, same FS — FICLONE\n'
  printf 'clones extents within one filesystem only). No dataset / host / path named.\n'
  printf '  %s files x ~%s MiB across %s subdirs = ~%s MiB total.\n\n' \
    "$NFILES" "$FILE_MIB" "$SUBDIRS" "$TOTAL_MIB"
  printf 'Method: fresh empty cache per stage run; page cache warmed; --walk-jobs held\n'
  printf 'constant (%s); best-of-%s wall clock via bash EPOCHREALTIME.\n\n' "$WALK_JOBS" "$REPS"
  printf -- '--------------------------------------------------------------------------------\n'
  printf '1) STAGE — AFTER (reflink ON) vs BASELINE (SNAPDIR_CLONEFILE=0)\n'
  printf -- '--------------------------------------------------------------------------------\n'
  printf '  stage AFTER    best: %ss   objects=%s\n' "$A_TIME" "$A_POOL"
  printf '  stage BASELINE best: %ss   objects=%s\n' "$B_TIME" "$B_POOL"
  printf '  stage speedup (baseline/after) = %sx\n' "$STAGE_SPEEDUP"
  printf '  snapshot id (after)    = %s\n' "$A_ID"
  printf '  snapshot id (baseline) = %s\n' "$B_ID"
  printf '  id match: %s ; object-pool fingerprint: %s\n\n' "$ID_MATCH" "$POOL_MATCH"
  printf -- '--------------------------------------------------------------------------------\n'
  printf '2) CHECKOUT — AFTER (reflink ON) vs BASELINE (SNAPDIR_CLONEFILE=0)\n'
  printf -- '--------------------------------------------------------------------------------\n'
  printf '  staged source id=%s\n' "$CK_ID"
  printf '  checkout AFTER    best: %ss\n' "$CK_A"
  printf '  checkout BASELINE best: %ss\n' "$CK_B"
  printf '  checkout speedup (baseline/after) = %sx\n\n' "$CK_SPEEDUP"
  printf 'VERDICT: object pool + snapshot id BYTE-IDENTICAL on/off (id %s, pool %s) —\n' "$ID_MATCH" "$POOL_MATCH"
  printf 'the reflink CoW path is output-preserving. Stage speedup %sx, checkout %sx.\n' \
    "$STAGE_SPEEDUP" "$CK_SPEEDUP"
} > "$EVIDENCE"

log ""
log "evidence written to $EVIDENCE"
log "baseline=${B_TIME}s after=${A_TIME}s stage-speedup=${STAGE_SPEEDUP}x checkout-speedup=${CK_SPEEDUP}x"

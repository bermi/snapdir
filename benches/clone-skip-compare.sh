#!/usr/bin/env bash
# clone-skip end-to-end before/after measurement (phase 29 clone-skip-bench gate).
#
# Measures the wall-clock effect of the clone-skip optimization: on a CoW clone,
# `persist` skips its redundant post-copy re-hash.
#
#   AFTER    (clone-skip ON):  default invocation (clone fires + skip).
#   BASELINE (clone-skip OFF): SNAPDIR_VERIFY_COPIES=1 (clone still fires but
#                              `persist` re-hashes the temp = pre-feature behavior).
#
# Clone is ON in BOTH arms, so the delta isolates the skipped re-hash, NOT the
# clonefile-vs-fs::copy difference (already measured in apfs-clone-bench).
#
# Generates a synthetic corpus from /dev/urandom. Source dir and cache dir both
# live under $TMPDIR (same APFS volume) so the clone path is eligible.
#
# NOT committed-output: this is a measurement harness, bench-only.
set -euo pipefail

BIN="${SNAPDIR_BIN:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release/snapdir}"
WALK_JOBS="${WALK_JOBS:-8}"
NFILES="${NFILES:-300}"
FILE_MIB="${FILE_MIB:-30}"
SUBDIRS="${SUBDIRS:-10}"
REPS="${REPS:-2}"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/clone-skip-bench.XXXXXX")"
SRC="$WORK/src"
trap 'rm -rf "$WORK"' EXIT

log() { printf '%s\n' "$*"; }

# -- generate synthetic corpus: a large directory of sizable files -------------
log "Generating synthetic corpus: $NFILES files x ${FILE_MIB} MiB across $SUBDIRS subdirs ..."
mkdir -p "$SRC"
for d in $(seq 1 "$SUBDIRS"); do mkdir -p "$SRC/d$d"; done
i=0
while [ "$i" -lt "$NFILES" ]; do
  d=$(( (i % SUBDIRS) + 1 ))
  dd if=/dev/urandom of="$SRC/d$d/f$i.bin" bs=1m count="$FILE_MIB" status=none
  i=$((i + 1))
done
TOTAL_MIB=$(( NFILES * FILE_MIB ))
log "Corpus generated: ~${TOTAL_MIB} MiB total."

# warm the page cache (read every file once)
warm() { find "$1" -type f -exec cat {} + >/dev/null 2>&1 || true; }

# best-of-REPS wall clock (seconds) for a command; fresh empty cache each rep.
# $1 = label, $2 = cache-dir-prefix, $3.. = extra env assignments before binary
# Returns best time via global BEST; captures snapshot id via global LAST_ID.
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
      printf '%s  %s\n' "$(shasum -a 256 "$f" | awk '{print $1}')" "$f"
    done )
}
pool_count() { find "$1/.objects" -type f 2>/dev/null | wc -l | tr -d ' '; }

log ""
log "================ STAGE ================"
run_stage "AFTER    (clone-skip ON )" "stage-after"    ""                        ; A_TIME="$BEST"; A_ID="$LAST_ID"; A_CACHE="$LAST_CACHE"
run_stage "BASELINE (VERIFY_COPIES=1)" "stage-base"    "SNAPDIR_VERIFY_COPIES=1" ; B_TIME="$BEST"; B_ID="$LAST_ID"; B_CACHE="$LAST_CACHE"

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
    env $extra_env "$BIN" --cache-dir "$CKC" --walk-jobs "$WALK_JOBS" checkout --id "$CK_ID" "$DEST" >/dev/null 2>/dev/null
    end=$EPOCHREALTIME
    t=$(awk "BEGIN{printf \"%.3f\", $end-$start}")
    log "  $label rep$rep: ${t}s"
    if [ -z "$best" ] || awk "BEGIN{exit !($t < $best)}"; then best="$t"; fi
  done
  CK_BEST="$best"
}

run_checkout "AFTER"    ""                        ; CK_A="$CK_BEST"
run_checkout "BASELINE" "SNAPDIR_VERIFY_COPIES=1" ; CK_B="$CK_BEST"
CK_SPEEDUP=$(awk "BEGIN{printf \"%.2f\", $CK_B/$CK_A}")
log ""
log "  checkout AFTER    best: ${CK_A}s"
log "  checkout BASELINE best: ${CK_B}s"
log "  checkout speedup (baseline/after) = ${CK_SPEEDUP}x"

# export results for the log writer
cat > "$WORK/results.env" <<EOF
A_TIME=$A_TIME
B_TIME=$B_TIME
STAGE_SPEEDUP=$STAGE_SPEEDUP
A_ID=$A_ID
B_ID=$B_ID
A_POOL=$A_POOL
B_POOL=$B_POOL
ID_MATCH=$ID_MATCH
POOL_MATCH=$POOL_MATCH
CK_ID=$CK_ID
CK_A=$CK_A
CK_B=$CK_B
CK_SPEEDUP=$CK_SPEEDUP
NFILES=$NFILES
FILE_MIB=$FILE_MIB
TOTAL_MIB=$TOTAL_MIB
SUBDIRS=$SUBDIRS
WALK_JOBS=$WALK_JOBS
REPS=$REPS
EOF
cp "$WORK/results.env" "${RESULTS_OUT:-/tmp/clone-skip-results.env}"
log ""
log "results written to ${RESULTS_OUT:-/tmp/clone-skip-results.env}"

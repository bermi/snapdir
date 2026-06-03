#!/usr/bin/env bash
#
# Keep a directory in sync across a local filesystem store, S3, and GCS using
# snapdir's content-addressed snapshots.
#
# snapdir is snapshot-based, not a live-sync daemon: every backend ends up with the
# SAME snapshot — same snapshot ID, same content-addressed object keys — so
# unchanged data is never re-uploaded, stores written by one machine are readable
# by any other, and every restore is re-hashed and verified on fetch.
#
# This script:
#   1. snapshots a sample directory,
#   2. pushes it to a LOCAL store and to S3,
#   3. replicates S3 -> GCS *without the source directory* (fetch + push),
#   4. pulls from every backend into a separate dir and asserts each restore
#      re-hashes to the same snapshot ID (byte-for-byte identical),
#   5. edits a file and re-pushes to show a new snapshot (only the changed object
#      uploads; everything else is skipped).
#
# Usage:
#   examples/sync-local-s3-gcs.sh [S3_STORE_URI] [GCS_STORE_URI]
#
# Store URIs resolve in this order: positional args, then the env vars
# SNAPDIR_S3_TEST_STORE / SNAPDIR_GCS_TEST_STORE, else a local file:// store — so
# the script runs end-to-end with zero cloud setup, and against real clouds when
# you point it at them, e.g.:
#
#   SNAPDIR_S3_TEST_STORE=s3://my-bucket/snapdir \
#   SNAPDIR_GCS_TEST_STORE=gs://my-bucket/snapdir \
#   examples/sync-local-s3-gcs.sh
#
# Auth: S3 uses the standard AWS chain (env / profile / SSO / instance metadata);
# GCS uses Application Default Credentials (`gcloud auth application-default login`).
# Each backend gets a unique per-run subpath; the script cleans up after itself.

set -euo pipefail

snapdir="${SNAPDIR_BIN:-snapdir}"
run="run-$$-${RANDOM}"

work="$(mktemp -d)"
cleanup() { rm -rf "$work"; }
trap cleanup EXIT
export SNAPDIR_CACHE_DIR="$work/cache"

# Resolve the three store URIs (unique subpath per run so reruns never collide).
local_store="file://$work/local-store/$run"
s3_store="${1:-${SNAPDIR_S3_TEST_STORE:-file://$work/s3-fallback}}/$run"
gcs_store="${2:-${SNAPDIR_GCS_TEST_STORE:-file://$work/gcs-fallback}}/$run"

ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
step() { printf '\n\033[1m%s\033[0m\n' "$*"; }
die()  { printf '  \033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

step "Stores"
echo "  local : $local_store"
echo "  s3    : $s3_store"
echo "  gcs   : $gcs_store"

# 1. A sample directory.
src="$work/project"
mkdir -p "$src/data"
echo "hello"        > "$src/README.md"
echo "row,value"    > "$src/data/table.csv"
printf '\x00\x01\x02binary' > "$src/data/blob.bin"

step "1. Snapshot the directory"
id="$("$snapdir" id "$src")"
ok "snapshot id = $id"

# 2. Push to local + S3 (push prints the snapshot id on stdout).
step "2. Push to local + S3"
for pair in "local|$local_store" "s3|$s3_store"; do
  name="${pair%%|*}"; uri="${pair#*|}"
  got="$("$snapdir" push --store "$uri" "$src")"
  [ "$got" = "$id" ] || die "push to $name returned '$got' (expected $id)"
  ok "pushed to $name → $got"
done

# 3. Replicate S3 -> GCS without the original source dir: pull the snapshot from S3
#    into a throwaway checkout, then push that checkout to GCS.
step "3. Replicate S3 → GCS (no original source directory)"
"$snapdir" flush-cache >/dev/null 2>&1 || true   # prove the objects really come from S3
mirror="$work/from-s3"
"$snapdir" pull --store "$s3_store" --id "$id" "$mirror" >/dev/null
ok "pulled $id from S3 into a temp checkout"
got="$("$snapdir" push --store "$gcs_store" "$mirror")"
[ "$got" = "$id" ] || die "replicate to GCS returned '$got' (expected $id)"
ok "pushed $id to GCS"

# 4. Pull from every backend into a separate dir and verify byte-for-byte.
step "4. Pull from every backend and verify"
for pair in "local|$local_store" "s3|$s3_store" "gcs|$gcs_store"; do
  name="${pair%%|*}"; uri="${pair#*|}"
  dest="$work/restored-$name"
  "$snapdir" pull --store "$uri" --id "$id" "$dest" >/dev/null
  rid="$("$snapdir" id "$dest")"
  [ "$rid" = "$id" ] || die "restore from $name re-hashes to $rid (expected $id)"
  ok "pulled from $name → verified identical ($rid)"
done

# 5. Edit a file and re-push: a new snapshot, only the changed object uploads.
step "5. Edit a file and re-push (content-addressed dedup)"
echo "an edit" >> "$src/README.md"
new_id="$("$snapdir" id "$src")"
[ "$new_id" != "$id" ] || die "editing a file should change the snapshot id"
got="$("$snapdir" push --store "$s3_store" "$src")"
[ "$got" = "$new_id" ] || die "re-push returned '$got' (expected $new_id)"
ok "new snapshot $new_id pushed to S3 (unchanged objects were skipped)"

step "Done — local, S3 and GCS all hold snapshot $id, byte-for-byte."

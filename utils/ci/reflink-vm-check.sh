#!/usr/bin/env bash
#
# reflink-vm-check.sh — operator-run LOCAL feasibility check for the Linux
# FICLONE (reflink / copy-on-write) fast-path.
#
# Run this INSIDE a Linux VM (Lima / colima / multipass) on a macOS dev box (or
# on any Linux host) to eyeball that the FICLONE ioctl genuinely fires before —
# or independent of — the CI `reflink` job in .github/workflows/ci.yaml. The
# stock ubuntu/dev root FS is usually ext4, where FICLONE never fires (only the
# fs::copy fallback), so we need a reflink-capable filesystem (Btrfs / XFS
# reflink=1 / OpenZFS 2.2+ / bcachefs) to actually exercise the clone path.
#
# This is NOT a CI gate and NOT wired into pre-push.sh — it is a manual,
# operator-driven cross-check. The CI gate lives in the `reflink` job, which
# does the same Btrfs-loopback dance on the hosted ubuntu-latest runner.
#
# What it does:
#   1. Find a reflink-capable dir: prefer an existing one named by
#      $SNAPDIR_REFLINK_TEST_DIR; otherwise create a loopback Btrfs image at a
#      temp file, mount it (needs sudo), and clean it up on exit.
#   2. Build the release `snapdir` binary (cargo build --release -p snapdir).
#   3. Run `cargo test -p snapdir-stores --test reflink --locked` with
#      SNAPDIR_REFLINK_TEST_DIR / TMPDIR set + SNAPDIR_REFLINK_TEST_REQUIRE=1
#      (the suite panics rather than skips if the reflink dir is missing, and
#      asserts clonefile_hits() advanced = FICLONE fired).
#   4. Report whether the reflink suite passed (i.e. FICLONE fired).
#
# Usage:
#   utils/ci/reflink-vm-check.sh            # auto: use $SNAPDIR_REFLINK_TEST_DIR
#                                           # if set, else create a loopback Btrfs
#   SNAPDIR_REFLINK_TEST_DIR=/mnt/reflink \
#     utils/ci/reflink-vm-check.sh          # reuse an existing reflink mount
#   utils/ci/reflink-vm-check.sh --help
#
# Exit status: 0 if the reflink suite passed (FICLONE fired); non-zero otherwise.

set -euo pipefail

PROG="$(basename "$0")"
# Repo root = two levels up from utils/ci/.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Loopback image size for the self-created Btrfs FS.
IMG_SIZE="2G"

# State for cleanup (only used when we create our own loopback mount).
CREATED_MOUNT=""
CREATED_IMG=""

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

log()  { printf '>> %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; }

# Invoked indirectly via `trap ... EXIT`; shellcheck cannot see that.
# shellcheck disable=SC2329
cleanup() {
  # Only unmount/remove the loopback FS if THIS script created it.
  if [ -n "$CREATED_MOUNT" ]; then
    log "cleaning up loopback mount $CREATED_MOUNT"
    sudo umount "$CREATED_MOUNT" 2>/dev/null || true
    sudo rmdir "$CREATED_MOUNT" 2>/dev/null || true
  fi
  if [ -n "$CREATED_IMG" ] && [ -f "$CREATED_IMG" ]; then
    rm -f "$CREATED_IMG" 2>/dev/null || true
  fi
}
trap cleanup EXIT

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  "") : ;;
  *) echo "$PROG: unknown argument: $1" >&2; echo "Try '$PROG --help'." >&2; exit 2 ;;
esac

if [ "$(uname -s)" != "Linux" ]; then
  fail "this is a Linux-only check (FICLONE/reflink). Run it inside a Linux VM (Lima/colima/multipass)."
  exit 2
fi

cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# 1. Obtain a reflink-capable directory.
# ---------------------------------------------------------------------------
REFLINK_DIR=""

if [ -n "${SNAPDIR_REFLINK_TEST_DIR:-}" ]; then
  REFLINK_DIR="$SNAPDIR_REFLINK_TEST_DIR"
  log "using existing reflink dir from \$SNAPDIR_REFLINK_TEST_DIR: $REFLINK_DIR"
  if [ ! -d "$REFLINK_DIR" ]; then
    fail "\$SNAPDIR_REFLINK_TEST_DIR ($REFLINK_DIR) does not exist or is not a directory"
    exit 2
  fi
else
  log "no \$SNAPDIR_REFLINK_TEST_DIR set — creating a loopback Btrfs image (needs sudo)"
  if ! command -v mkfs.btrfs >/dev/null 2>&1; then
    fail "mkfs.btrfs not found. Install btrfs-progs (e.g. sudo apt-get install -y btrfs-progs)."
    exit 2
  fi
  CREATED_IMG="$(mktemp /tmp/reflink-vm-check.XXXXXX.img)"
  CREATED_MOUNT="$(mktemp -d /tmp/reflink-vm-check.XXXXXX.mnt)"
  log "creating ${IMG_SIZE} Btrfs image at $CREATED_IMG, mounting at $CREATED_MOUNT"
  truncate -s "$IMG_SIZE" "$CREATED_IMG"
  mkfs.btrfs -q "$CREATED_IMG"
  sudo mount -o loop "$CREATED_IMG" "$CREATED_MOUNT"
  # The test user must be able to create src+store dirs under the mount.
  sudo chmod 1777 "$CREATED_MOUNT"
  REFLINK_DIR="$CREATED_MOUNT"
fi

# ---------------------------------------------------------------------------
# 2. Build the release binary.
# ---------------------------------------------------------------------------
log "building release snapdir binary"
cargo build --release -p snapdir

# ---------------------------------------------------------------------------
# 3. Run the reflink suite against the reflink FS (REQUIRE => panic, not skip).
# ---------------------------------------------------------------------------
log "running the reflink suite on $REFLINK_DIR (FICLONE must fire)"
set +e
SNAPDIR_REFLINK_TEST_DIR="$REFLINK_DIR" \
  TMPDIR="$REFLINK_DIR" \
  SNAPDIR_REFLINK_TEST_REQUIRE=1 \
  cargo test -p snapdir-stores --test reflink --locked
status=$?
set -e

# ---------------------------------------------------------------------------
# 4. Report.
# ---------------------------------------------------------------------------
if [ "$status" -eq 0 ]; then
  log "PASS — the reflink suite passed: FICLONE fired and clonefile_hits() advanced on $REFLINK_DIR"
else
  fail "the reflink suite FAILED (exit $status): FICLONE did not fire as expected on $REFLINK_DIR"
  fail "check that $REFLINK_DIR is a reflink-capable FS (Btrfs / XFS reflink=1 / OpenZFS 2.2+ / bcachefs)"
fi

exit "$status"

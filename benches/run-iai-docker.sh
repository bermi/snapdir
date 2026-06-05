#!/usr/bin/env bash
#
# Run the deterministic INSTRUCTION-COUNT perf gate (the `iai_hot` bench) inside a
# PINNED Linux image with valgrind installed, so macOS users (no native valgrind)
# and CI get IDENTICAL instruction counts.
#
# iai-callgrind measures CPU instruction counts via valgrind/callgrind. valgrind
# is Linux-only, so on macOS the bench can't run natively — it can only COMPILE
# (`cargo bench -p snapdir-benches --bench iai_hot --no-run`). This script runs
# the real measurement in a Docker container that matches CI.
#
# Requirements: Docker (the daemon must be running). Run from the repo ROOT:
#
#   bash benches/run-iai-docker.sh
#
# Everything is pinned for reproducibility but overridable via env vars:
#   RUST_IMAGE  — the base Rust image (must satisfy the workspace MSRV 1.91.1).
#   IAI_VERSION — the iai-callgrind-runner version. MUST equal the
#                 `iai-callgrind` dev-dependency pin in benches/Cargo.toml
#                 (=0.16.1). A mismatch makes the runner refuse to run.
#
set -euo pipefail

# Pinned, MSRV-compatible base image. 1.91-slim-bookworm ships a toolchain at or
# above the workspace MSRV (1.91.1) and a Debian base with a valgrind package.
RUST_IMAGE="${RUST_IMAGE:-rust:1.91-slim-bookworm}"

# MUST match the `iai-callgrind = "=0.16.1"` dev-dependency in benches/Cargo.toml.
IAI_VERSION="${IAI_VERSION:-0.16.1}"

# Pin the container architecture to linux/amd64 — the same arch CI runs on, so the
# instruction counts (and the baseline) match. It is also REQUIRED on Apple Silicon
# (arm64) hosts: iai-callgrind's runner disables ASLR via `setarch`, which an
# emulated arm64 container can't permit ("setarch: failed to set personality"),
# whereas an emulated amd64 container runs callgrind cleanly (just slower under
# QEMU). Override with `DOCKER_PLATFORM=` (e.g. empty) on a native amd64 Linux host
# if you prefer the host arch.
DOCKER_PLATFORM="${DOCKER_PLATFORM:-linux/amd64}"

# Resolve the repo root (this script lives in <repo>/benches/) so the script works
# regardless of the caller's CWD.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker not found on PATH. This script needs Docker to run valgrind." >&2
  echo "       On Linux with native valgrind you can instead run directly:" >&2
  echo "         cargo install iai-callgrind-runner --version ${IAI_VERSION} --locked" >&2
  echo "         cargo bench -p snapdir-benches --bench iai_hot" >&2
  exit 1
fi

echo "Running iai_hot under ${RUST_IMAGE} (iai-callgrind-runner ${IAI_VERSION})..."

# Only pass --platform when DOCKER_PLATFORM is non-empty (empty = host arch).
platform_args=()
if [[ -n "${DOCKER_PLATFORM}" ]]; then
  platform_args=(--platform "${DOCKER_PLATFORM}")
fi

# Mount the repo at /work and run the whole pipeline inside the container:
#   1. install valgrind (callgrind backend),
#   2. install the version-matched iai-callgrind-runner,
#   3. run the iai_hot bench, which fails on a >5% Ir / EstimatedCycles regression.
exec docker run --rm \
  "${platform_args[@]}" \
  -v "${REPO_ROOT}":/work \
  -w /work \
  -e IAI_VERSION="${IAI_VERSION}" \
  "${RUST_IMAGE}" \
  bash -euo pipefail -c '
    apt-get update
    apt-get install -y --no-install-recommends valgrind
    rm -rf /var/lib/apt/lists/*
    cargo install iai-callgrind-runner --version "${IAI_VERSION}" --locked
    cargo bench -p snapdir-benches --bench iai_hot
  '

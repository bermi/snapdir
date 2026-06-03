#!/usr/bin/env bash
#
# pre-push.sh — the local CI-equivalent gate.
#
# Mirrors EVERY job in .github/workflows/ci.yaml and BLOCKS a push when any
# check fails, so breakage never reaches the paid GitHub Actions runners.
# This is what the git `pre-push` hook runs (see utils/git-hooks/pre-push and
# `make install-hooks`).
#
# Operator rationale: ci.yaml triggers on every push and burns Actions minutes.
# We catch lint / build / test / musl / coverage failures locally first.
#
# The six CI job groups reproduced here:
#   1. Lint        — fmt, clippy (-D warnings, --all-features), typos,
#                    actionlint, cargo-shear, cargo-semver-checks (non-blocking)
#   2. Supply chain — cargo-deny, cargo-audit
#   3. Test        — build + test (host stable, plus MSRV 1.91.1 if installed)
#   4. Static musl — x86_64-unknown-linux-musl build, debug AND release
#   5. Doctests    — cargo test --doc
#   6. Coverage    — cargo llvm-cov --fail-under-lines 75
#
# Usage:
#   utils/ci/pre-push.sh [--fast] [--no-install] [--help]
#
#   --fast        Skip the musl leg (group 4) and coverage (group 6) for quick
#                 local iteration. The installed git pre-push hook uses --fast
#                 (musl + coverage are covered by CI on native Linux runners).
#                 Run the FULL suite manually via `make ci-local` (or
#                 `bash utils/ci/pre-push.sh` with no flag) before a release or
#                 when touching the TLS/musl path.
#   --no-install  Never auto-install a missing tool; fail with the exact
#                 install command instead.
#   --help        Show this help and exit.
#
# Exit status: 0 only if every (non-fast-skipped) check passes. Any failure
# yields a non-zero exit so the push is blocked. All groups run; the failing
# checks and their exact reproduce commands are summarized at the end.

set -euo pipefail

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

PROG="$(basename "$0")"
# Repo root = two levels up from utils/ci/.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

FAST=0
NO_INSTALL=0

MUSL_TARGET="x86_64-unknown-linux-musl"
MSRV="1.91.1"
COVERAGE_FLOOR=75
BUILDER_IMAGE="rust:1.96-slim-bookworm"

# Accumulated failures: each entry is "CHECK NAME|||reproduce command".
FAILURES=()

# ---------------------------------------------------------------------------
# Pretty output
# ---------------------------------------------------------------------------

if [ -t 1 ]; then
  C_RESET=$'\033[0m'; C_BOLD=$'\033[1m'; C_RED=$'\033[31m'
  C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_BLUE=$'\033[34m'
else
  C_RESET=""; C_BOLD=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""
fi

banner() { printf '\n%s========== %s ==========%s\n' "$C_BOLD$C_BLUE" "$*" "$C_RESET"; }
info()   { printf '%s>>%s %s\n' "$C_BLUE" "$C_RESET" "$*"; }
ok()     { printf '%s  ok%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
warn()   { printf '%swarn%s %s\n' "$C_YELLOW" "$C_RESET" "$*"; }
fail()   { printf '%sFAIL%s %s\n' "$C_RED" "$C_RESET" "$*"; }

usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

# Run a check. Args: <name> <reproduce-cmd> -- <command...>
# Records a failure (does not abort) so the summary reports ALL failures.
run_check() {
  local name="$1"; shift
  local repro="$1"; shift
  [ "$1" = "--" ] && shift
  info "$name"
  if "$@"; then
    ok "$name"
    return 0
  fi
  fail "$name"
  FAILURES+=("${name}|||${repro}")
  return 1
}

# ---------------------------------------------------------------------------
# Tool bootstrap
# ---------------------------------------------------------------------------

# ensure_cargo_tool <binary-on-path> <cargo-install-args...>
# Returns 0 if the tool is available (already, or after auto-install).
ensure_cargo_tool() {
  local bin="$1"; shift
  if command -v "$bin" >/dev/null 2>&1; then
    return 0
  fi
  local install_cmd="cargo install $*"
  if [ "$NO_INSTALL" -eq 1 ]; then
    warn "$bin not found and --no-install set. Install with: $install_cmd"
    return 1
  fi
  info "installing missing tool: $bin  ($install_cmd)"
  if cargo install "$@"; then
    ok "installed $bin"
    return 0
  fi
  warn "failed to install $bin. Install manually with: $install_cmd"
  return 1
}

# ensure_target <triple>. Returns 0 if the rustup target is installed.
ensure_target() {
  local triple="$1"
  if rustup target list --installed 2>/dev/null | grep -qx "$triple"; then
    return 0
  fi
  if [ "$NO_INSTALL" -eq 1 ]; then
    warn "rustup target $triple not installed and --no-install set. Add with: rustup target add $triple"
    return 1
  fi
  info "adding rustup target: $triple"
  rustup target add "$triple"
}

msrv_installed() {
  rustup toolchain list 2>/dev/null | grep -q "^${MSRV}"
}

# ---------------------------------------------------------------------------
# Static musl leg (group 4)
# ---------------------------------------------------------------------------
# Picks the most robust available cross path and runs the x86_64-musl build in
# BOTH debug and release. The leg must actually run — it is the operator's
# explicit ask — or fail with actionable install instructions. Never silently
# skipped.
#
# Strategy preference:
#   1. Native musl target present + a working musl linker (cross-linker, or a
#      native x86_64 Linux host) — cargo build directly.
#   2. `cross` (docker-based) — robust on any host.
#   3. `x86_64-linux-musl-gcc` cross-linker (brew musl-cross) wired via
#      CARGO_TARGET_..._LINKER.
#   4. Docker amd64 `rust:slim` container that builds x86_64-musl NATIVELY
#      (no cross C linker needed) — the macOS Apple-Silicon fallback.
#   5. Otherwise FAIL with exact install commands.

# Record a musl failure for the summary.
musl_fail() {
  fail "$1"
  FAILURES+=("$1|||$2")
}

# Build directly via cargo for one profile (used when a native/cross linker is
# available on the host). $1 = "debug"|"release".
# Invoked indirectly through run_check's "$@"; shellcheck cannot see that.
# shellcheck disable=SC2329
musl_cargo_build() {
  local profile="$1"
  if [ "$profile" = "release" ]; then
    cargo build --workspace --all-features --locked --target "$MUSL_TARGET" --release
  else
    cargo build --workspace --all-features --locked --target "$MUSL_TARGET"
  fi
}

run_musl_leg() {
  local os; os="$(uname -s)"
  local arch; arch="$(uname -m)"

  # --- Path 1: native (Linux x86_64) or an already-working host linker. ---
  if [ "$os" = "Linux" ]; then
    if ensure_target "$MUSL_TARGET"; then
      if ! command -v musl-gcc >/dev/null 2>&1 && [ "$arch" = "x86_64" ]; then
        warn "musl-tools (musl-gcc) not detected; install: sudo apt-get install -y musl-tools"
      fi
      local p
      for p in debug release; do
        run_check "musl build ($p)" \
          "cargo build --workspace --all-features --locked --target $MUSL_TARGET$([ "$p" = release ] && echo ' --release')" \
          -- musl_cargo_build "$p" || true
      done
      return 0
    fi
  fi

  # --- Path 2: `cross` (docker-based), works on any host incl. macOS. ---
  if command -v cross >/dev/null 2>&1; then
    info "using 'cross' (docker) for the musl leg"
    local p flag
    for p in debug release; do
      flag=""; [ "$p" = release ] && flag="--release"
      run_check "musl build ($p, via cross)" \
        "cross build --workspace --all-features --locked --target $MUSL_TARGET $flag" \
        -- cross build --workspace --all-features --locked --target "$MUSL_TARGET" $flag || true
    done
    return 0
  fi

  # --- Path 3: a brew musl-cross linker (x86_64-linux-musl-gcc) on the host. ---
  if command -v x86_64-linux-musl-gcc >/dev/null 2>&1 && ensure_target "$MUSL_TARGET"; then
    info "using x86_64-linux-musl-gcc cross-linker for the musl leg"
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="x86_64-linux-musl-gcc"
    export CC_x86_64_unknown_linux_musl="x86_64-linux-musl-gcc"
    local p
    for p in debug release; do
      run_check "musl build ($p, musl-cross linker)" \
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc cargo build --workspace --all-features --locked --target $MUSL_TARGET$([ "$p" = release ] && echo ' --release')" \
        -- musl_cargo_build "$p" || true
    done
    return 0
  fi

  # --- Path 4: docker amd64 rust:slim — native x86_64-musl build in-container. ---
  # On Apple Silicon, docker runs amd64 via emulation; inside an amd64 container
  # the musl target is NATIVE (no cross C linker needed), which is the most
  # robust fallback. We mount the repo read-only-ish and keep a named cache
  # volume so repeat runs are fast.
  if command -v docker >/dev/null 2>&1; then
    info "no host musl toolchain; building the musl leg inside an amd64 $BUILDER_IMAGE container (docker)"
    local p flag
    for p in debug release; do
      flag=""; [ "$p" = release ] && flag="--release"
      run_check "musl build ($p, docker amd64 $BUILDER_IMAGE)" \
        "docker run --rm --platform=linux/amd64 -v \$PWD:/src -w /src $BUILDER_IMAGE <bootstrap musl-tools + target, then cargo build --target $MUSL_TARGET $flag>" \
        -- docker_musl_build "$p" || true
    done
    return 0
  fi

  # --- Path 5: nothing available — fail loudly with install instructions. ---
  musl_fail "static musl leg (no cross path available)" \
    "Install ONE of: 'cargo install cross' (docker-based, recommended on macOS) | 'brew install filosottile/musl-cross/musl-cross' | a working docker daemon"
}

# Run the musl build for one profile inside an amd64 rust:slim container.
# $1 = "debug"|"release". The container natively targets x86_64-musl.
# Invoked indirectly through run_check's "$@"; shellcheck cannot see that.
# shellcheck disable=SC2329
docker_musl_build() {
  local profile="$1"
  local relflag=""
  [ "$profile" = "release" ] && relflag="--release"
  # A named volume caches cargo registry + target between runs.
  docker run --rm --platform=linux/amd64 \
    -v "$REPO_ROOT":/src \
    -v snapdir-musl-cargo-registry:/usr/local/cargo/registry \
    -v snapdir-musl-target:/src/target-musl \
    -e CARGO_TERM_COLOR=always \
    -e CARGO_TARGET_DIR=/src/target-musl \
    -w /src \
    "$BUILDER_IMAGE" \
    bash -euo pipefail -c "
      rustup target add $MUSL_TARGET >/dev/null 2>&1 || true
      apt-get update -qq && apt-get install -y --no-install-recommends musl-tools >/dev/null 2>&1
      cargo build --workspace --all-features --locked --target $MUSL_TARGET $relflag
    "
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

while [ $# -gt 0 ]; do
  case "$1" in
    --fast)       FAST=1 ;;
    --no-install) NO_INSTALL=1 ;;
    -h|--help)    usage; exit 0 ;;
    *) echo "$PROG: unknown argument: $1" >&2; echo "Try '$PROG --help'." >&2; exit 2 ;;
  esac
  shift
done

banner "snapdir local pre-push gate (mirrors .github/workflows/ci.yaml)"
if [ "$FAST" -eq 1 ]; then
  warn "--fast: skipping the static musl leg and coverage (covered by CI; run 'make ci-local' for the full suite)."
fi

# ===========================================================================
# Group 1 — Lint
# ===========================================================================
banner "1/6 Lint"

run_check "rustfmt --check" \
  "cargo fmt --all --check" \
  -- cargo fmt --all --check || true

run_check "clippy (-D warnings, --all-features)" \
  "cargo clippy --workspace --all-targets --all-features --locked -- -D warnings" \
  -- cargo clippy --workspace --all-targets --all-features --locked -- -D warnings || true

if ensure_cargo_tool typos typos-cli; then
  run_check "typos" "typos" -- typos || true
else
  FAILURES+=("typos (tool missing)|||cargo install typos-cli && typos")
fi

if command -v actionlint >/dev/null 2>&1; then
  run_check "actionlint" "actionlint -color" -- actionlint -color || true
else
  warn "actionlint not found. Install: brew install actionlint  (or: go install github.com/rhysd/actionlint/cmd/actionlint@latest)"
  FAILURES+=("actionlint (tool missing)|||brew install actionlint && actionlint -color")
fi

if ensure_cargo_tool cargo-shear cargo-shear; then
  run_check "cargo-shear (unused deps)" "cargo shear" -- cargo shear || true
else
  FAILURES+=("cargo-shear (tool missing)|||cargo install cargo-shear && cargo shear")
fi

# semver-checks mirrors ci.yaml's `|| true` — non-blocking, informational.
if ensure_cargo_tool cargo-semver-checks cargo-semver-checks; then
  info "cargo-semver-checks (snapdir-core, non-blocking)"
  if cargo semver-checks check-release --package snapdir-core; then
    ok "cargo-semver-checks"
  else
    warn "cargo-semver-checks reported changes (non-blocking, mirrors ci.yaml '|| true')"
  fi
else
  warn "cargo-semver-checks unavailable (non-blocking); install: cargo install cargo-semver-checks"
fi

# ===========================================================================
# Group 2 — Supply chain
# ===========================================================================
banner "2/6 Supply chain"

if ensure_cargo_tool cargo-deny cargo-deny; then
  run_check "cargo-deny" "cargo deny --workspace --all-features check" \
    -- cargo deny --workspace --all-features check || true
else
  FAILURES+=("cargo-deny (tool missing)|||cargo install cargo-deny && cargo deny --workspace --all-features check")
fi

if ensure_cargo_tool cargo-audit cargo-audit; then
  run_check "cargo-audit" "cargo audit" -- cargo audit || true
else
  FAILURES+=("cargo-audit (tool missing)|||cargo install cargo-audit && cargo audit")
fi

# ===========================================================================
# Group 3 — Build + Test (host stable; MSRV 1.91.1 if installed)
# ===========================================================================
banner "3/6 Build + Test"

run_check "build (workspace, --all-features, locked)" \
  "cargo build --workspace --all-features --locked" \
  -- cargo build --workspace --all-features --locked || true

run_check "test (workspace, --all-features, locked)" \
  "cargo test --workspace --all-features --locked" \
  -- cargo test --workspace --all-features --locked || true

if msrv_installed; then
  run_check "MSRV ${MSRV} build" \
    "cargo +${MSRV} build --workspace --all-features --locked" \
    -- cargo "+${MSRV}" build --workspace --all-features --locked || true
  run_check "MSRV ${MSRV} test" \
    "cargo +${MSRV} test --workspace --all-features --locked" \
    -- cargo "+${MSRV}" test --workspace --all-features --locked || true
else
  warn "MSRV ${MSRV} toolchain not installed, skipping (CI covers it). Add: rustup toolchain install ${MSRV}"
fi

# ===========================================================================
# Group 4 — Static musl (x86_64-unknown-linux-musl, debug + release)
# ===========================================================================
# The load-bearing leg: proves the ring rustls provider links cleanly into a
# fully-static binary. On Linux this is a plain target add + musl-tools. On this
# Apple-Silicon macOS host we need a musl cross path; we pick the most robust
# available one (see musl_strategy below).
# ===========================================================================
if [ "$FAST" -eq 1 ]; then
  banner "4/6 Static musl — SKIPPED (--fast)"
else
  banner "4/6 Static musl ($MUSL_TARGET, debug + release)"
  run_musl_leg
fi

# ===========================================================================
# Group 5 — Doctests
# ===========================================================================
banner "5/6 Doctests"
run_check "doctests" \
  "cargo test --workspace --all-features --locked --doc" \
  -- cargo test --workspace --all-features --locked --doc || true

# ===========================================================================
# Group 6 — Coverage (fail-under 75)
# ===========================================================================
if [ "$FAST" -eq 1 ]; then
  banner "6/6 Coverage — SKIPPED (--fast)"
else
  banner "6/6 Coverage (cargo llvm-cov, fail-under-lines ${COVERAGE_FLOOR})"
  if ensure_cargo_tool cargo-llvm-cov cargo-llvm-cov; then
    if ! rustup component list --installed 2>/dev/null | grep -q llvm-tools; then
      if [ "$NO_INSTALL" -eq 1 ]; then
        warn "llvm-tools-preview component missing and --no-install set. Add: rustup component add llvm-tools-preview"
      else
        info "adding rustup component: llvm-tools-preview"
        rustup component add llvm-tools-preview || true
      fi
    fi
    run_check "coverage (>= ${COVERAGE_FLOOR}% lines)" \
      "cargo llvm-cov --workspace --all-features --locked --fail-under-lines ${COVERAGE_FLOOR} --lcov --output-path lcov.info" \
      -- cargo llvm-cov --workspace --all-features --locked \
         --fail-under-lines "$COVERAGE_FLOOR" --lcov --output-path lcov.info || true
  else
    FAILURES+=("cargo-llvm-cov (tool missing)|||cargo install cargo-llvm-cov && cargo llvm-cov --workspace --all-features --locked --fail-under-lines ${COVERAGE_FLOOR}")
  fi
fi

# ===========================================================================
# Summary
# ===========================================================================
banner "Summary"
if [ "${#FAILURES[@]}" -eq 0 ]; then
  printf '%sAll checks passed. Safe to push.%s\n' "$C_GREEN$C_BOLD" "$C_RESET"
  exit 0
fi

printf '%s%d check(s) FAILED — push BLOCKED:%s\n\n' "$C_RED$C_BOLD" "${#FAILURES[@]}" "$C_RESET"
for entry in "${FAILURES[@]}"; do
  name="${entry%%|||*}"
  repro="${entry#*|||}"
  printf '  %s✗ %s%s\n      reproduce: %s\n' "$C_RED" "$name" "$C_RESET" "$repro"
done
printf '\nFix the above (or run with %s--fast%s while iterating) before pushing.\n' "$C_BOLD" "$C_RESET"
exit 1

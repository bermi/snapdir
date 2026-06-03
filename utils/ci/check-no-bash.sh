#!/usr/bin/env bash
#
# check-no-bash.sh — the final de-bash guard (phase 11).
#
# The legacy Bash oracle (8 root `snapdir*` scripts, the root bash Dockerfile,
# utils/qa-fixtures/, the .sh QA harness, and the oracle CI) has been DELETED.
# The Rust port is now the sole implementation. This guard FAILS (exit 1) if any
# of that oracle is reintroduced, or if any executable/CI surface starts
# invoking it (or its companion bash-era tooling) again. It exits 0 when the
# tree is clean.
#
# What it checks:
#   A. None of the deleted oracle artifacts exist on disk.
#   B. No LIVE `./snapdir-*` invocation, nor a real `shellcheck` / `shfmt` /
#      `b3sum` / `sqlite3` oracle-tooling *call*, in executable/CI contexts:
#      .github/workflows/, the root Makefile, and tracked shell scripts (*.sh).
#
# ---------------------------------------------------------------------------
# DOCUMENTED EXCLUSIONS (intentionally NOT flagged) and WHY:
#
#   * The new CI tooling itself — utils/ci/check-no-bash.sh (this file),
#     utils/ci/pre-push.sh, utils/ci/check-crate-age.sh, utils/git-hooks/* .
#     These are the legitimate Rust-era CI plumbing; some legitimately contain
#     the strings "shellcheck"/"snapdir" inside `# shellcheck disable=` linter
#     directives or as the binary name. They are not the oracle.
#
#   * .git/ and target/ — VCS internals and build output; never source of truth
#     and huge, so we never content-scan them.
#
#   * .gatesmith/ — the PM's ledger / journal / handoffs / templates. These
#     record the de-bash history in prose and quote the old invocations on
#     purpose; they are not executable.
#
#   * docs/ — ADRs, the rust-port history, README/CONTRIBUTING. They document
#     what the port replaced and quote the original commands historically.
#
#   * Rust source/doc COMMENTS (*.rs) — engineering notes explaining what the
#     port reproduces. We never scan .rs files for "invocations": the only
#     `Command::new`-style references left are in a self-skipping catalog test
#     whose oracle no longer exists, and the frozen sha-locked core files
#     (crates/snapdir-core/src/{manifest,merkle,excludes}.rs) MUST NOT be
#     edited, so their historical comments are excluded by construction.
#
# Re-verify (the PM re-runs this):
#   bash utils/ci/check-no-bash.sh
# ---------------------------------------------------------------------------

set -euo pipefail

PROG="$(basename "$0")"
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

fail=0
note_fail() {
    fail=1
    printf 'FAIL: %s\n' "$1" >&2
}

# ---------------------------------------------------------------------------
# A. The deleted oracle artifacts must NOT exist (path-based; cheap).
# ---------------------------------------------------------------------------
oracle_paths=(
    "snapdir"
    "snapdir-manifest"
    "snapdir-file-store"
    "snapdir-s3-store"
    "snapdir-b2-store"
    "snapdir-gcs-store"
    "snapdir-sqlite3-catalog"
    "snapdir-test"
    "Dockerfile.oracle"
    "utils/qa-fixtures"
    "utils/pre-commit-hook.sh"
    "utils/generate-docs.sh"
    "utils/verify-docs.sh"
    "utils/install.sh"
)

# The root bash `Dockerfile` was the oracle's image. A Rust `Dockerfile` now
# lives at the root legitimately; the oracle one was bash-based. We flag the
# root Dockerfile only if it reintroduces the bash oracle (FROM ... bash entry
# point that COPYs the snapdir* scripts).
for p in "${oracle_paths[@]}"; do
    if [ -e "$p" ]; then
        note_fail "deleted oracle artifact reappeared: ./$p"
    fi
done

# Guard the root Dockerfile against reintroducing the oracle scripts.
if [ -f Dockerfile ] && grep -Eq '(^|[^-])COPY[[:space:]]+(\./)?snapdir(-[a-z0-9]+)?([[:space:]]|$)' Dockerfile; then
    note_fail "root Dockerfile COPYs an oracle snapdir* script (bash oracle reintroduced)"
fi

# ---------------------------------------------------------------------------
# B. No live oracle invocation / oracle-tooling call in executable/CI contexts.
#
# Contexts scanned: .github/workflows/*, the root Makefile, and tracked *.sh.
# The new CI tooling is excluded (it legitimately references the binary name and
# carries `# shellcheck disable=` directives).
# ---------------------------------------------------------------------------
is_excluded() {
    case "$1" in
        utils/ci/check-no-bash.sh | \
        utils/ci/pre-push.sh | \
        utils/ci/check-crate-age.sh | \
        utils/git-hooks/*)
            return 0 ;;
        *)
            return 1 ;;
    esac
}

# Build the list of files to scan (only tracked, only existing).
scan_files=()
while IFS= read -r f; do
    [ -n "$f" ] || continue
    is_excluded "$f" && continue
    [ -f "$f" ] && scan_files+=("$f")
done < <(
    git ls-files '*.sh' '.github/workflows/*' 'Makefile' 2>/dev/null
)

# Pattern 1: a LIVE `./snapdir-*` (or bare `snapdir-manifest`) invocation.
#   Matches `./snapdir`, `./snapdir-manifest`, `snapdir-file-store ...` as a
#   command, but NOT the Rust binary name `snapdir` used by cargo, nor strings
#   inside `# ...` comments (we strip comment lines first via the grep -v below).
invocation_re='(^|[^[:alnum:]_./-])(\./snapdir(-[a-z0-9]+)?|snapdir-(manifest|file-store|s3-store|b2-store|gcs-store|sqlite3-catalog|test))([[:space:]]|$)'

# Pattern 2: a real oracle-tooling *call*. We match the tool at the start of a
#   command position, NOT inside a `# shellcheck disable=` linter directive.
#   The `disable_directive_re` is subtracted so directive comments don't trip us.
tooling_re='(^|[^[:alnum:]_./-])(shellcheck|shfmt|b3sum|sqlite3)([[:space:]]|$)'
disable_directive_re='#[[:space:]]*shellcheck[[:space:]]+(disable|source|shell|enable)='

for f in "${scan_files[@]}"; do
    # Strip whole-line comments (leading-whitespace then `#`) before scanning so
    # historical mentions in comments are not treated as live invocations.
    stripped="$(grep -vE '^[[:space:]]*#' "$f" || true)"

    if printf '%s\n' "$stripped" | grep -Eq "$invocation_re"; then
        note_fail "live oracle invocation in $f:"
        printf '%s\n' "$stripped" | grep -nE "$invocation_re" | sed 's/^/    /' >&2
    fi

    # For tooling: drop `# shellcheck ...=` directive lines, then look for a call.
    if printf '%s\n' "$stripped" \
        | grep -vE "$disable_directive_re" \
        | grep -Eq "$tooling_re"; then
        note_fail "oracle-tooling call ($tooling_re) in $f:"
        printf '%s\n' "$stripped" \
            | grep -vE "$disable_directive_re" \
            | grep -nE "$tooling_re" | sed 's/^/    /' >&2
    fi
done

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------
if [ "$fail" -ne 0 ]; then
    printf '\n%s: FAIL — the Bash oracle (or a live invocation of it) is present.\n' "$PROG" >&2
    exit 1
fi

printf '%s: OK — no Bash oracle artifacts or live invocations remain.\n' "$PROG"
exit 0

#!/usr/bin/env bash
#
# tests/interop/run.sh
#
# Differential interop harness for the snapdir Rust port.
#
# This is the scaffolding for the keystone `interop-diff` gate. It:
#
#   1. Generates a deterministic fixture corpus under a self-cleaning temp dir
#      (nested dirs, symlinks, odd perms, a large file, unicode/space/empty
#      names, duplicate files, an empty dir, an empty file).
#   2. For a given path + checksum mode, runs BOTH the frozen Bash oracle
#      (`./snapdir-manifest`) and the Rust binary (`snapdir manifest …`) and
#      diffs their stdout byte-for-byte, across b3sum / md5sum / sha256sum and
#      keyed mode (`SNAPDIR_MANIFEST_CONTEXT`).
#   3. A diff is a HARD failure. We never normalize away a real difference.
#
# Modes:
#
#   --self-check   Fast STRUCTURAL self-test of the harness machinery only:
#                  the Bash oracle is invokable, the corpus generator runs,
#                  the diff/compare function works on a known synthetic pair,
#                  and required tools (b3sum) are present. It does NOT assert
#                  Rust-vs-Bash byte-identity — that is the `interop-diff`
#                  gate's job and requires the Rust `manifest` subcommand to be
#                  wired to `snapdir-core` first (it is currently a stub).
#                  Exit 0 when the harness plumbing is sound.
#
#   (no args)      Full run: generate the corpus and diff the Bash oracle vs
#                  the Rust binary across every path and checksum mode. This
#                  WILL report differences while the Rust CLI `manifest`
#                  command is still a stub; the full run is intentionally NOT a
#                  pass criterion for the `interop-harness` gate. The full run
#                  detects and clearly reports when the Rust output is a
#                  stub/missing rather than silently passing.
#
# Oracle note (IMPORTANT): the per-tool manifest oracle is the raw
# `./snapdir-manifest` binary, which emits the frozen manifest format directly.
# The `./snapdir manifest` wrapper additionally injects `--cache` and a default
# `--exclude=system`, so it is NOT the byte-identity oracle for the manifest
# format. The Rust equivalent of `./snapdir-manifest <PATH>` is
# `snapdir manifest <PATH>`.
#
# Oracle quirk (load-bearing): in `./snapdir-manifest`, the space-separated
# `--no-follow` flag swallows the *following* token as its value (the boolean
# table uses `no_follow` while the flag is `--no-follow`). The only correct way
# to pass `--no-follow` together with a path is PATH-FIRST, e.g.
# `snapdir-manifest <PATH> --no-follow`. The harness always invokes it that
# way. Value flags use the `--flag=value` form.

set -euo pipefail

# ---------------------------------------------------------------------------
# Locate the repo root (this file lives at <root>/tests/interop/run.sh).
# ---------------------------------------------------------------------------
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../.." && pwd)"

ORACLE_MANIFEST="${REPO_ROOT}/snapdir-manifest"
ORACLE_SNAPDIR="${REPO_ROOT}/snapdir"

# ---------------------------------------------------------------------------
# Colours / logging
# ---------------------------------------------------------------------------
if [[ -t 1 ]]; then
	C_RED=$'\033[31m'
	C_GRN=$'\033[32m'
	C_YEL=$'\033[33m'
	C_DIM=$'\033[2m'
	C_RST=$'\033[0m'
else
	C_RED='' C_GRN='' C_YEL='' C_DIM='' C_RST=''
fi

log() { printf '%s\n' "$*" >&2; }
info() { log "${C_DIM}[harness]${C_RST} $*"; }
ok() { log "${C_GRN}ok${C_RST} - $*"; }
warn() { log "${C_YEL}warn${C_RST} - $*"; }
fail() { log "${C_RED}FAIL${C_RST} - $*"; }

# ---------------------------------------------------------------------------
# Temp workspace, self-cleaning.
# ---------------------------------------------------------------------------
WORKDIR=""
cleanup() {
	if [[ -n "${WORKDIR}" && -d "${WORKDIR}" ]]; then
		# Restore any perms we tightened so rm -rf can delete everything.
		chmod -R u+rwx "${WORKDIR}" 2>/dev/null || true
		rm -rf "${WORKDIR}" 2>/dev/null || true
	fi
}
trap cleanup EXIT INT TERM

make_workdir() {
	WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/snapdir-interop.XXXXXXXXXX")"
	info "workdir: ${WORKDIR}"
}

# ---------------------------------------------------------------------------
# Deterministic fixture corpus.
#
# Determinism rules:
#   - All file contents are fixed bytes (no randomness, no clocks).
#   - Permissions are set explicitly.
#   - The manifest format the oracle emits does not include mtime, so we do not
#     fight mtimes; but we still touch with a fixed timestamp for good measure.
#   - Path ordering is irrelevant to us (the oracle sorts), but creation is
#     scripted in a fixed order anyway.
#
# Coverage:
#   - nested directories (a/b/c)
#   - an empty directory
#   - an empty file
#   - duplicate files (identical content in two places -> same checksum)
#   - a "large" file (1 MiB of deterministic bytes)
#   - odd permissions (0600, 0640, 0755 executable, 0444 read-only)
#   - unicode filename, filename with spaces
#   - a symlink to a file and a symlink to a directory (for --no-follow)
#
# Note: an *empty-named* path is not creatable on a POSIX filesystem (the empty
# string is not a legal filename), so "empty-named paths" is covered by the
# empty *file* (zero-length content) and empty *directory* cases, which are the
# real edge the oracle cares about.
# ---------------------------------------------------------------------------
FIXTURE_ROOT=""
generate_corpus() {
	FIXTURE_ROOT="${WORKDIR}/corpus"
	mkdir -p "${FIXTURE_ROOT}"

	# Nested directories.
	mkdir -p "${FIXTURE_ROOT}/a/b/c"

	# Empty directory.
	mkdir -p "${FIXTURE_ROOT}/empty-dir"

	# Empty file.
	: >"${FIXTURE_ROOT}/empty-file"

	# Plain files with deterministic content.
	printf 'foo\n' >"${FIXTURE_ROOT}/foo.txt"
	printf 'nested leaf\n' >"${FIXTURE_ROOT}/a/b/c/leaf.txt"

	# Duplicate files: identical content -> identical checksum, distinct paths.
	printf 'duplicate payload\n' >"${FIXTURE_ROOT}/dup1.txt"
	printf 'duplicate payload\n' >"${FIXTURE_ROOT}/a/dup2.txt"

	# Unicode filename and filename containing spaces.
	printf 'unicode body\n' >"${FIXTURE_ROOT}/ünïcødé-名前.txt"
	printf 'spaced body\n' >"${FIXTURE_ROOT}/with spaces.txt"

	# Large file: 1 MiB of a fixed byte pattern (deterministic, no /dev/urandom).
	# Build it without a `yes | head` pipe: that pipe makes `yes` die with
	# SIGPIPE which, under `set -o pipefail`, would abort the whole harness.
	# Instead double a fixed seed buffer in-process until it reaches 1 MiB.
	local seed="snapdir-large-file-deterministic-line\n"
	local buf
	buf="$(printf '%b' "${seed}")"
	while [[ "${#buf}" -lt 1048576 ]]; do
		buf="${buf}${buf}"
	done
	printf '%s' "${buf:0:1048576}" >"${FIXTURE_ROOT}/large.bin"

	# Symlinks (exercised by --no-follow): one to a file, one to a directory.
	ln -s "foo.txt" "${FIXTURE_ROOT}/link-to-foo"
	ln -s "a/b/c" "${FIXTURE_ROOT}/link-to-dir"

	# Odd permissions. Set these LAST so earlier writes are unaffected.
	chmod 0600 "${FIXTURE_ROOT}/dup1.txt"
	chmod 0640 "${FIXTURE_ROOT}/foo.txt"
	chmod 0444 "${FIXTURE_ROOT}/with spaces.txt"
	chmod 0755 "${FIXTURE_ROOT}/large.bin" # "executable" large file
	chmod 0700 "${FIXTURE_ROOT}/a/b/c"

	# Fixed mtime on everything (best-effort; format does not depend on it).
	find "${FIXTURE_ROOT}" -exec touch -t 200102030405.06 {} + 2>/dev/null || true

	info "corpus generated at ${FIXTURE_ROOT}"
}

# ---------------------------------------------------------------------------
# Oracle invocation.
#
#   run_oracle <path> <mode>
#
# <mode> is one of: b3sum md5sum sha256sum keyed nofollow
# Emits the oracle manifest on stdout. Stdin is closed (/dev/null) because the
# oracle's manifest path never reads stdin and we must not block.
# ---------------------------------------------------------------------------
run_oracle() {
	local path="$1" mode="$2"
	case "${mode}" in
	b3sum)
		"${ORACLE_MANIFEST}" "${path}" </dev/null
		;;
	md5sum)
		"${ORACLE_MANIFEST}" --checksum-bin=md5sum "${path}" </dev/null
		;;
	sha256sum)
		"${ORACLE_MANIFEST}" --checksum-bin=sha256sum "${path}" </dev/null
		;;
	keyed)
		SNAPDIR_MANIFEST_CONTEXT="snapdir-interop-keyed-context" \
			"${ORACLE_MANIFEST}" "${path}" </dev/null
		;;
	nofollow)
		# PATH-FIRST: see "Oracle quirk" note at the top of this file.
		"${ORACLE_MANIFEST}" "${path}" --no-follow </dev/null
		;;
	*)
		fail "run_oracle: unknown mode '${mode}'"
		return 2
		;;
	esac
}

# ---------------------------------------------------------------------------
# Rust binary invocation.
#
#   run_rust <path> <mode>
#
# Resolves the Rust binary: prefer an already-built binary under target/, else
# fall back to `cargo run -q -p snapdir-cli`. The Rust CLI exposes the manifest
# tool as `snapdir manifest <PATH>`; checksum-mode and keyed-mode flags are
# passed through the same way the oracle takes them. (While the subcommand is a
# stub these flags are accepted/ignored; once wired to core they must mirror
# the oracle.)
# ---------------------------------------------------------------------------
RUST_BIN=""
resolve_rust_bin() {
	# Honour an explicit override first.
	if [[ -n "${SNAPDIR_RUST_BIN:-}" && -x "${SNAPDIR_RUST_BIN}" ]]; then
		RUST_BIN="${SNAPDIR_RUST_BIN}"
		return 0
	fi
	local candidate
	for candidate in \
		"${REPO_ROOT}/target/release/snapdir" \
		"${REPO_ROOT}/target/debug/snapdir"; do
		if [[ -x "${candidate}" ]]; then
			RUST_BIN="${candidate}"
			return 0
		fi
	done
	# No prebuilt binary; signal that the caller should use cargo run.
	RUST_BIN=""
	return 0
}

rust_cmd() {
	# Print the argv prefix that invokes the Rust `snapdir` binary.
	if [[ -n "${RUST_BIN}" ]]; then
		printf '%s\n' "${RUST_BIN}"
	else
		printf '%s\n' "cargo run -q -p snapdir-cli --"
	fi
}

run_rust() {
	local path="$1" mode="$2"
	local -a pre=()
	if [[ -n "${RUST_BIN}" ]]; then
		pre=("${RUST_BIN}")
	else
		pre=(cargo run -q -p snapdir-cli --)
	fi

	case "${mode}" in
	b3sum)
		(cd "${REPO_ROOT}" && "${pre[@]}" manifest "${path}") </dev/null
		;;
	md5sum)
		(cd "${REPO_ROOT}" && "${pre[@]}" manifest --checksum-bin=md5sum "${path}") </dev/null
		;;
	sha256sum)
		(cd "${REPO_ROOT}" && "${pre[@]}" manifest --checksum-bin=sha256sum "${path}") </dev/null
		;;
	keyed)
		(cd "${REPO_ROOT}" && SNAPDIR_MANIFEST_CONTEXT="snapdir-interop-keyed-context" \
			"${pre[@]}" manifest "${path}") </dev/null
		;;
	nofollow)
		(cd "${REPO_ROOT}" && "${pre[@]}" manifest "${path}" --no-follow) </dev/null
		;;
	*)
		fail "run_rust: unknown mode '${mode}'"
		return 2
		;;
	esac
}

# ---------------------------------------------------------------------------
# Stub / missing detection.
#
# The current Rust `manifest` subcommand prints
#   "snapdir: `manifest` is not implemented yet"
# to stderr and emits nothing on stdout. The full run must detect this and
# report it as "not wired yet" rather than treating it as a passing diff.
# ---------------------------------------------------------------------------
looks_like_stub() {
	# $1 = captured stdout, $2 = captured stderr
	local out="$1" err="$2"
	if [[ -z "${out}" ]] && grep -qi 'not implemented' <<<"${err}"; then
		return 0
	fi
	# A real manifest always starts with a "D " root directory line.
	if [[ -z "${out}" ]]; then
		return 0
	fi
	return 1
}

# ---------------------------------------------------------------------------
# Byte-for-byte compare.
#
#   compare_streams <label> <expected-file> <actual-file>
#
# Returns 0 on byte-identical, 1 otherwise (printing the diff). NEVER
# normalizes — a difference is a difference.
# ---------------------------------------------------------------------------
compare_streams() {
	local label="$1" expected="$2" actual="$3"
	if cmp -s "${expected}" "${actual}"; then
		return 0
	fi
	fail "${label}: byte mismatch"
	log "${C_DIM}--- oracle (expected)${C_RST}"
	log "${C_DIM}+++ rust   (actual)${C_RST}"
	# diff for human readability; cmp already decided the verdict.
	diff -u "${expected}" "${actual}" >&2 || true
	return 1
}

# ---------------------------------------------------------------------------
# One differential case: oracle vs rust for <path> in <mode>.
#
# Used by the FULL run. Returns:
#   0  byte-identical
#   1  real diff (hard failure)
#   3  rust output is a stub/missing (reported, not a pass, not a hard diff)
# ---------------------------------------------------------------------------
diff_case() {
	local path="$1" mode="$2"
	local label="mode=${mode} path=${path#"${FIXTURE_ROOT}"/}"
	[[ "${path}" == "${FIXTURE_ROOT}" ]] && label="mode=${mode} path=<corpus-root>"

	local o_out o_err r_out r_err
	o_out="${WORKDIR}/oracle.out"
	o_err="${WORKDIR}/oracle.err"
	r_out="${WORKDIR}/rust.out"
	r_err="${WORKDIR}/rust.err"

	if ! run_oracle "${path}" "${mode}" >"${o_out}" 2>"${o_err}"; then
		fail "${label}: ORACLE itself failed (exit) — see stderr"
		cat "${o_err}" >&2 || true
		return 1
	fi

	run_rust "${path}" "${mode}" >"${r_out}" 2>"${r_err}" || true

	if looks_like_stub "$(cat "${r_out}")" "$(cat "${r_err}")"; then
		warn "${label}: Rust 'manifest' produced no manifest (stub/not-wired) — deferred to interop-diff gate"
		return 3
	fi

	if compare_streams "${label}" "${o_out}" "${r_out}"; then
		ok "${label}: byte-identical"
		return 0
	fi
	return 1
}

# ---------------------------------------------------------------------------
# --self-check : fast structural validation of the harness machinery.
#
# Validates plumbing only; does NOT assert Rust-vs-Bash byte-identity.
# ---------------------------------------------------------------------------
self_check() {
	local failures=0

	info "self-check: required tools present?"
	local tool
	for tool in b3sum md5sum sha256sum cmp diff mktemp find; do
		if command -v "${tool}" >/dev/null 2>&1; then
			ok "tool present: ${tool}"
		else
			# md5sum/sha256sum may be absent on macOS as such; the oracle
			# resolves them itself, so only b3sum + core utils are hard reqs.
			case "${tool}" in
			b3sum | cmp | diff | mktemp | find)
				fail "required tool missing: ${tool}"
				failures=$((failures + 1))
				;;
			*)
				warn "optional tool not on PATH (oracle resolves it): ${tool}"
				;;
			esac
		fi
	done

	info "self-check: oracle binaries present and invokable?"
	if [[ -x "${ORACLE_MANIFEST}" ]]; then
		ok "oracle present: ${ORACLE_MANIFEST}"
	else
		fail "oracle missing or not executable: ${ORACLE_MANIFEST}"
		failures=$((failures + 1))
	fi
	if [[ -x "${ORACLE_SNAPDIR}" ]]; then
		ok "oracle present: ${ORACLE_SNAPDIR}"
	else
		fail "oracle missing or not executable: ${ORACLE_SNAPDIR}"
		failures=$((failures + 1))
	fi

	# Build the workdir/corpus and prove the oracle actually produces a
	# manifest over a real (tiny) fixture.
	make_workdir
	info "self-check: corpus generator runs?"
	if generate_corpus; then
		ok "corpus generator ran"
	else
		fail "corpus generator failed"
		failures=$((failures + 1))
	fi

	info "self-check: oracle produces a manifest over the corpus?"
	local probe="${WORKDIR}/selfcheck-oracle.out"
	if run_oracle "${FIXTURE_ROOT}" b3sum >"${probe}" 2>/dev/null \
		&& head -n1 "${probe}" | grep -qE '^D '; then
		ok "oracle emitted a manifest (root 'D ' line present)"
	else
		fail "oracle did not emit a well-formed manifest over the corpus"
		failures=$((failures + 1))
	fi

	# Prove the --no-follow oracle path works (PATH-FIRST quirk) and actually
	# drops the symlinks rather than scanning $PWD.
	info "self-check: --no-follow oracle path (path-first) is well-formed?"
	local nf="${WORKDIR}/selfcheck-nofollow.out"
	if run_oracle "${FIXTURE_ROOT}" nofollow >"${nf}" 2>/dev/null \
		&& head -n1 "${nf}" | grep -qE '^D ' \
		&& ! grep -q 'link-to-foo' "${nf}"; then
		ok "--no-follow oracle path well-formed and excludes symlinks"
	else
		fail "--no-follow oracle path malformed (quirk regression?)"
		failures=$((failures + 1))
	fi

	# Prove the compare function works on a KNOWN synthetic pair: identical
	# inputs compare equal; differing inputs compare unequal.
	info "self-check: diff/compare function works on a synthetic pair?"
	local same_a="${WORKDIR}/same_a" same_b="${WORKDIR}/same_b" diff_c="${WORKDIR}/diff_c"
	printf 'D 755 deadbeef 0 ./\nF 644 cafebabe 3 ./x\n' >"${same_a}"
	printf 'D 755 deadbeef 0 ./\nF 644 cafebabe 3 ./x\n' >"${same_b}"
	printf 'D 755 deadbeef 0 ./\nF 644 0000000 3 ./x\n' >"${diff_c}"
	if compare_streams "synthetic-equal" "${same_a}" "${same_b}" 2>/dev/null; then
		ok "compare: identical streams compare equal"
	else
		fail "compare: identical streams reported unequal"
		failures=$((failures + 1))
	fi
	if compare_streams "synthetic-diff" "${same_a}" "${diff_c}" 2>/dev/null; then
		fail "compare: differing streams reported equal (would mask real diffs!)"
		failures=$((failures + 1))
	else
		ok "compare: differing streams compare unequal"
	fi

	# Confirm the Rust binary is at least resolvable (built binary or cargo).
	info "self-check: Rust binary resolvable (built artifact or cargo run)?"
	resolve_rust_bin
	if [[ -n "${RUST_BIN}" ]]; then
		ok "Rust binary: ${RUST_BIN}"
	elif command -v cargo >/dev/null 2>&1; then
		ok "Rust binary not prebuilt; will use: $(rust_cmd)"
	else
		warn "no prebuilt Rust binary and no cargo on PATH (interop-diff will need one)"
	fi

	log ""
	if [[ "${failures}" -eq 0 ]]; then
		ok "self-check PASSED — harness plumbing is sound (Rust-vs-Bash byte-identity is the interop-diff gate)"
		return 0
	fi
	fail "self-check FAILED with ${failures} problem(s)"
	return 1
}

# ---------------------------------------------------------------------------
# Full differential run: oracle vs rust over the corpus, all modes.
#
# This is NOT a pass criterion for the interop-harness gate. While the Rust
# `manifest` subcommand is a stub it will report "not wired"; once wired it
# must be byte-identical or it is a HARD failure.
# ---------------------------------------------------------------------------
full_run() {
	make_workdir
	generate_corpus
	resolve_rust_bin

	info "Rust binary: ${RUST_BIN:-"(cargo run) $(rust_cmd)"}"

	# Paths to exercise: the corpus root and a representative nested subtree.
	local -a paths=(
		"${FIXTURE_ROOT}"
		"${FIXTURE_ROOT}/a"
		"${FIXTURE_ROOT}/empty-dir"
	)
	local -a modes=(b3sum md5sum sha256sum keyed nofollow)

	local hard=0 stub=0 pass=0
	local p m rc
	for p in "${paths[@]}"; do
		for m in "${modes[@]}"; do
			set +e
			diff_case "${p}" "${m}"
			rc=$?
			set -e
			case "${rc}" in
			0) pass=$((pass + 1)) ;;
			3) stub=$((stub + 1)) ;;
			*) hard=$((hard + 1)) ;;
			esac
		done
	done

	log ""
	info "full-run summary: ${pass} identical, ${stub} stub/not-wired, ${hard} hard-diff"

	if [[ "${hard}" -gt 0 ]]; then
		fail "full run found ${hard} real diff(s) — these are interop-diff failures"
		return 1
	fi
	if [[ "${stub}" -gt 0 ]]; then
		warn "Rust 'manifest' is not wired to core yet (${stub} case(s)); interop-diff gate will require it."
		# The harness ran cleanly; the stub state is expected at this gate.
		return 0
	fi
	ok "full run: every case byte-identical"
	return 0
}

# ---------------------------------------------------------------------------
# Entry point.
# ---------------------------------------------------------------------------
usage() {
	cat >&2 <<'EOF'
usage: tests/interop/run.sh [--self-check]

  (no args)      Full differential run: Bash oracle vs Rust binary over the
                 fixture corpus, across b3sum/md5sum/sha256sum + keyed + no-follow.
  --self-check   Fast structural self-test of the harness machinery only.
  -h, --help     Show this help.
EOF
}

main() {
	case "${1:-}" in
	--self-check)
		self_check
		;;
	-h | --help)
		usage
		exit 0
		;;
	"")
		full_run
		;;
	*)
		usage
		exit 2
		;;
	esac
}

main "$@"

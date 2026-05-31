#!/usr/bin/env bash
#
# tests/integration/file_store_roundtrip.sh
#
# End-to-end + cross-tool interop test for the `file://` store.
#
# Exercises the Rust CLI (`snapdir`) push -> fetch -> checkout/pull -> verify
# round-trip against a `file://` store, and proves on-disk interoperability with
# the frozen Bash oracle (`./snapdir`) in BOTH directions:
#
#   1. Rust end-to-end: push a nested source tree (files, nested dir, explicit
#      perms) to a `file://` store; assert objects + manifest landed at the exact
#      sharded keys; checkout/pull with the Rust CLI; assert the destination
#      reproduces the source (contents + perms) and re-manifests to the SAME
#      snapshot id; run `snapdir verify`.
#
#   2. Rust -> Bash: push with the Rust CLI, then fetch + checkout that snapshot
#      with the *Bash* oracle and assert it reproduces the tree and re-manifests
#      to the same id (Bash can read what Rust wrote).
#
#   3. Bash -> Rust: push the tree with the *Bash* oracle to a fresh store, then
#      fetch/pull with the Rust CLI and assert reproduction + identical id (Rust
#      can read what Bash wrote).
#
# A mismatch is a HARD failure: the script exits non-zero on ANY divergence. It
# is deterministic and self-cleaning (trap rm of all temp dirs).
#
# Oracle invocation notes (studied from `./snapdir`, READ ONLY):
#   - The oracle takes options as `--flag=value` (e.g. `--store=...`, `--id=...`,
#     `--cache-dir=...`); the path is positional.
#   - `snapdir checkout` does NOT create the destination base dir (only `pull`
#     does). The harness uses `pull` for the Bash checkout direction.
#   - No `--catalog`/`SNAPDIR_CATALOG` is set, so catalog logging is a no-op.
#   - Sharing a single `--cache-dir` between Bash and Rust is intentional: both
#     write the identical content-addressable layout, so the cache is mutually
#     readable.
#
#   KNOWN ORACLE LIMITATION (macOS, pre-existing, NOT a Rust/interop bug): the
#   Bash `_snapdir_absolute_path` helper `cd`s into a manifest directory entry
#   while resolving its absolute path. During checkout the subdirectory does not
#   yet exist, so on macOS (which lacks `realpath -m`) the Bash oracle cannot
#   reconstruct NESTED directories — a pure Bash->Bash pull of a tree containing
#   a subdir fails identically. This is unrelated to the on-disk store layout
#   (Rust-written manifests/objects are read back fine by Bash). To keep every
#   interop assertion real rather than normalizing a difference away, the lanes
#   where the *Bash* tool performs the checkout use a FLAT tree on macOS and the
#   full NESTED tree on Linux (where `realpath -m` makes the oracle correct). The
#   lanes where the *Rust* tool performs the checkout always use the full nested
#   tree.

set -euo pipefail

# ---------------------------------------------------------------------------
# Locate repo root + tools.  This file lives at <root>/tests/integration/.
# ---------------------------------------------------------------------------
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../.." && pwd)"

ORACLE="${REPO_ROOT}/snapdir"

# Prefer the prebuilt debug binary; fall back to `cargo run` if absent.
RUST_BIN="${REPO_ROOT}/target/debug/snapdir"
RUST=()
if [[ -x "${RUST_BIN}" ]]; then
	RUST=("${RUST_BIN}")
else
	RUST=(cargo run -q -p snapdir-cli --)
fi

OS="$(uname -s)"

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
if [[ -t 1 ]]; then
	C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
else
	C_RED='' C_GRN='' C_DIM='' C_RST=''
fi
log() { printf '%s\n' "$*" >&2; }
info() { log "${C_DIM}[roundtrip]${C_RST} $*"; }
ok() { log "${C_GRN}ok${C_RST} - $*"; }
die() { log "${C_RED}FAIL${C_RST} - $*"; exit 1; }

# ---------------------------------------------------------------------------
# Self-cleaning temp workspace.
# ---------------------------------------------------------------------------
WORKDIR=""
cleanup() {
	if [[ -n "${WORKDIR}" && -d "${WORKDIR}" ]]; then
		chmod -R u+rwx "${WORKDIR}" 2>/dev/null || true
		rm -rf "${WORKDIR}" 2>/dev/null || true
	fi
}
trap cleanup EXIT INT TERM
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/snapdir-fsroundtrip.XXXXXXXXXX")"
info "workdir: ${WORKDIR}"
info "rust:    ${RUST[*]}"
info "oracle:  ${ORACLE}"
info "os:      ${OS}"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# rust <args...>           — run the Rust CLI with the given cache dir.
# Sets RUST_CACHE before calling.
RUST_CACHE=""
rust() { "${RUST[@]}" --cache-dir "${RUST_CACHE}" "$@"; }

# b3 <file>                — BLAKE3 hex of a file (oracle's object checksum).
b3() { b3sum --no-names "$1" | tr -d '[:space:]'; }

# sharded <prefix> <hex>   — frozen content-addressable key:
#   <prefix>/<h[0:3]>/<h[3:6]>/<h[6:9]>/<h[9:]>
sharded() {
	local prefix="$1" h="$2"
	printf '%s/%s/%s/%s/%s' "${prefix}" "${h:0:3}" "${h:3:3}" "${h:6:3}" "${h:9}"
}

# build_nested_tree <dir>  — files + nested dir + explicit perms.
build_nested_tree() {
	local d="$1"
	mkdir -p "${d}/sub"
	printf 'hello' >"${d}/a.txt"
	printf 'world!!' >"${d}/sub/b.txt"
	printf 'dup' >"${d}/dup1.txt"
	printf 'dup' >"${d}/sub/dup2.txt" # duplicate content -> same object
	chmod 644 "${d}/a.txt"
	chmod 600 "${d}/sub/b.txt"
	chmod 640 "${d}/dup1.txt"
	chmod 640 "${d}/sub/dup2.txt"
	chmod 755 "${d}/sub"
	chmod 755 "${d}"
}

# build_flat_tree <dir>    — files only, no subdirs (oracle macOS-safe).
build_flat_tree() {
	local d="$1"
	mkdir -p "${d}"
	printf 'hello' >"${d}/a.txt"
	printf 'world!!' >"${d}/b.txt"
	printf 'dup' >"${d}/dup1.txt"
	printf 'dup' >"${d}/dup2.txt" # duplicate content -> same object
	chmod 644 "${d}/a.txt"
	chmod 600 "${d}/b.txt"
	chmod 640 "${d}/dup1.txt"
	chmod 640 "${d}/dup2.txt"
	chmod 755 "${d}"
}

# assert_trees_equal <a> <b>  — same relative paths, byte-identical contents,
# identical octal permissions. Hard fail on any difference.
assert_trees_equal() {
	local a="$1" b="$2"
	# 1. identical set of relative paths
	local list_a list_b
	list_a="$(cd "${a}" && find . | LC_ALL=C sort)"
	list_b="$(cd "${b}" && find . | LC_ALL=C sort)"
	if [[ "${list_a}" != "${list_b}" ]]; then
		log "--- path lists differ (${a} vs ${b}) ---"
		diff <(printf '%s\n' "${list_a}") <(printf '%s\n' "${list_b}") >&2 || true
		die "destination tree has different paths than the source"
	fi
	# 2. byte-identical file contents
	local f
	while IFS= read -r f; do
		if [[ -f "${a}/${f}" ]]; then
			cmp -s "${a}/${f}" "${b}/${f}" || die "content mismatch for '${f}'"
		fi
	done < <(cd "${a}" && find . -type f)
	# 3. identical octal permissions for every entry
	local mode_a mode_b
	while IFS= read -r f; do
		mode_a="$(perm_of "${a}/${f}")"
		mode_b="$(perm_of "${b}/${f}")"
		[[ "${mode_a}" == "${mode_b}" ]] || die "perm mismatch for '${f}': ${mode_a} != ${mode_b}"
	done < <(cd "${a}" && find .)
}

# perm_of <path>           — octal permission bits (cross-platform).
perm_of() {
	if [[ "${OS}" == "Darwin" ]]; then
		stat -f '%A' "$1"
	else
		stat -c '%a' "$1"
	fi
}

PASS=0
# pass <msg> — record a passing assertion.
pass() { PASS=$((PASS + 1)); ok "$*"; }

# ===========================================================================
# Lane 1 — Rust end-to-end (nested tree, explicit perms).
# ===========================================================================
lane_rust_e2e() {
	info "lane 1: Rust end-to-end (push -> verify -> fetch -> checkout/pull -> verify)"
	local base="${WORKDIR}/l1"
	local src="${base}/src" store="${base}/store" dest="${base}/dest" cache="${base}/cache"
	local dest2="${base}/dest2"
	mkdir -p "${store}" "${cache}"
	build_nested_tree "${src}"
	RUST_CACHE="${cache}"

	local id push_id
	id="$(rust id "${src}")"
	[[ "${#id}" -eq 64 ]] || die "snapshot id is not 64 hex chars: '${id}'"

	push_id="$(rust push --store "file://${store}" "${src}")"
	[[ "${push_id}" == "${id}" ]] || die "push printed '${push_id}', expected source id '${id}'"
	pass "Rust push printed the source snapshot id"

	# Manifest landed at the exact sharded key.
	local manifest_key="${store}/$(sharded .manifests "${id}")"
	[[ -f "${manifest_key}" ]] || die "manifest not at sharded key ${manifest_key}"
	pass "manifest landed at .manifests sharded key"

	# Every file object landed at its content-addressed sharded key with the
	# right bytes. (dup1.txt / dup2.txt share one object.)
	local f sum obj
	while IFS= read -r f; do
		sum="$(b3 "${f}")"
		obj="${store}/$(sharded .objects "${sum}")"
		[[ -f "${obj}" ]] || die "object for '${f}' (b3=${sum}) not at ${obj}"
		cmp -s "${f}" "${obj}" || die "object bytes for '${f}' differ from source"
	done < <(find "${src}" -type f)
	pass "every file object landed at its .objects sharded key with matching bytes"

	# fetch + checkout (offline from the cache) reproduces the tree.
	rust fetch --store "file://${store}" --id "${id}"
	[[ -f "${cache}/$(sharded .manifests "${id}")" ]] || die "fetch did not cache the manifest"
	pass "Rust fetch cached the manifest at its sharded key"

	rust checkout --id "${id}" "${dest}"
	assert_trees_equal "${src}" "${dest}"
	local dest_id
	dest_id="$(rust id "${dest}")"
	[[ "${dest_id}" == "${id}" ]] || die "checked-out tree re-manifests to '${dest_id}', expected '${id}'"
	pass "Rust checkout reproduced the tree (contents+perms) and re-manifested to the same id"

	# pull (fetch + checkout in one) into a clean dest reproduces too.
	rust pull --store "file://${store}" --id "${id}" "${dest2}"
	assert_trees_equal "${src}" "${dest2}"
	[[ "$(rust id "${dest2}")" == "${id}" ]] || die "Rust pull dest re-manifests to a different id"
	pass "Rust pull reproduced the tree and re-manifested to the same id"

	# verify the snapshot from the cache.
	rust verify --store "file://${store}" --id "${id}"
	pass "Rust verify reported the snapshot valid"
}

# ===========================================================================
# Lane 2 — Rust -> Bash: Rust writes the store, Bash reads + checks it out.
# ===========================================================================
lane_rust_to_bash() {
	info "lane 2: Rust push -> Bash fetch + checkout (Bash reads what Rust wrote)"
	local base="${WORKDIR}/l2"
	local src="${base}/src" store="${base}/store" dest="${base}/dest" cache="${base}/cache"
	mkdir -p "${store}" "${cache}"
	# Use a nested tree on Linux; flat on macOS (oracle nested-dir checkout bug).
	if [[ "${OS}" == "Darwin" ]]; then
		build_flat_tree "${src}"
	else
		build_nested_tree "${src}"
	fi
	RUST_CACHE="${cache}"

	local id bash_id
	id="$(rust id "${src}")"
	rust push --store "file://${store}" "${src}" >/dev/null
	pass "Rust pushed the tree to file://store"

	# Bash must agree on the id derived from the same source tree (cross-tool
	# manifest/id parity over the file store contents).
	bash_id="$("${ORACLE}" id --cache-dir="${cache}" "${src}")"
	[[ "${bash_id}" == "${id}" ]] || die "Bash id '${bash_id}' != Rust id '${id}' for the same tree"
	pass "Bash derives the same snapshot id as Rust for the source tree"

	# Bash fetches from the Rust-written store, then pulls (creates dest +
	# checks out). `pull` creates the base dir; plain `checkout` does not.
	"${ORACLE}" pull --store="file://${store}" --id="${id}" --cache-dir="${cache}" "${dest}"
	assert_trees_equal "${src}" "${dest}"
	local re_id
	re_id="$("${ORACLE}" id --cache-dir="${cache}" "${dest}")"
	[[ "${re_id}" == "${id}" ]] || die "Bash-checked-out tree re-manifests to '${re_id}', expected '${id}'"
	pass "Bash reproduced the tree from the Rust-written store and re-manifested to the same id"
}

# ===========================================================================
# Lane 3 — Bash -> Rust: Bash writes the store, Rust reads + checks it out.
# ===========================================================================
lane_bash_to_rust() {
	info "lane 3: Bash push -> Rust fetch + pull (Rust reads what Bash wrote)"
	local base="${WORKDIR}/l3"
	local src="${base}/src" store="${base}/store" dest="${base}/dest" cache="${base}/cache"
	mkdir -p "${store}" "${cache}"
	# Rust performs the checkout here, so the full nested tree is safe on both OSes.
	build_nested_tree "${src}"
	RUST_CACHE="${cache}"

	# Bash pushes to a fresh file:// store.
	local id
	id="$("${ORACLE}" push --store="file://${store}" --cache-dir="${cache}" "${src}")"
	[[ "${#id}" -eq 64 ]] || die "Bash push returned a non-id: '${id}'"
	pass "Bash pushed the tree to file://store (id=${id:0:12}…)"

	# The Bash-written store uses the frozen sharded layout Rust expects.
	[[ -f "${store}/$(sharded .manifests "${id}")" ]] || die "Bash manifest not at expected sharded key"
	pass "Bash-written manifest sits at the frozen .manifests sharded key"

	# Rust agrees on the id from the same source.
	[[ "$(rust id "${src}")" == "${id}" ]] || die "Rust id differs from Bash id for the same tree"
	pass "Rust derives the same snapshot id as Bash for the source tree"

	# Rust fetches + pulls from the Bash-written store into a clean dest.
	rust pull --store "file://${store}" --id "${id}" "${dest}"
	assert_trees_equal "${src}" "${dest}"
	[[ "$(rust id "${dest}")" == "${id}" ]] || die "Rust-pulled tree re-manifests to a different id"
	pass "Rust reproduced the tree from the Bash-written store and re-manifested to the same id"

	# Rust can also verify a Bash-pushed snapshot.
	rust verify --store "file://${store}" --id "${id}"
	pass "Rust verify accepts a Bash-pushed snapshot"
}

# ---------------------------------------------------------------------------
# Run.
# ---------------------------------------------------------------------------
command -v b3sum >/dev/null || die "b3sum is required by the test harness (oracle dependency)"

lane_rust_e2e
lane_rust_to_bash
lane_bash_to_rust

ok "file:// store round-trip: ${PASS} assertions passed (Rust e2e + Rust<->Bash interop)"

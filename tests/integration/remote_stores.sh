#!/usr/bin/env bash
#
# tests/integration/remote_stores.sh
#
# Differential integration + cross-tool interop harness for the REMOTE snapdir
# stores (S3 / B2 / GCS). It is meant to be RUN BY THE OPERATOR against live
# emulators (MinIO / B2 sandbox / fake-gcs-server) at the `remote-interop`
# human-checkpoint gate. With `--self-check` it validates its own plumbing
# WITHOUT any live emulator and exits 0.
#
# For each backend whose env is configured, the harness:
#   1. Builds a scratch source tree (deterministic corpus).
#   2. Rust round-trip: push -> fetch/checkout, asserting the destination
#      reproduces the source AND re-manifests to the SAME snapshot id, and that
#      the manifest + every object landed at the exact frozen sharded keys.
#   3. Cross-tool, BOTH directions:
#        - Rust push  -> Bash fetch  (Bash reads what Rust wrote), same id.
#        - Bash push  -> Rust fetch  (Rust reads what Bash wrote), same id.
#      Identical sharded keys + identical snapshot ids in every direction.
#
# A divergence is a HARD failure. Nothing is ever normalized away. Backends
# whose env vars are UNSET are SKIPPED (not failed); the run prints which
# backends ran vs. which were skipped so there are no silent passes.
#
# The harness is deterministic and self-cleaning (trap rm of all temp dirs).
#
# ---------------------------------------------------------------------------
# ENV-VAR CONTRACT (read from the environment; nothing is hardcoded)
# ---------------------------------------------------------------------------
# Studied READ-ONLY from `./snapdir-s3-store`, `./snapdir-b2-store`,
# `./snapdir-gcs-store` and the Rust store crate's live-test gates.
#
#   S3  (MinIO / SeaweedFS / any S3-compatible emulator)
#     SNAPDIR_S3_TEST_STORE       s3://bucket/prefix         (REQUIRED to run)
#     SNAPDIR_S3_TEST_ENDPOINT    http://127.0.0.1:9000      (REQUIRED to run)
#     plus AWS creds for BOTH tools:
#       SNAPDIR_S3_STORE_AWS_ACCESS_KEY_ID  (or AWS_ACCESS_KEY_ID)
#       SNAPDIR_S3_STORE_AWS_SECRET_ACCESS_KEY (or AWS_SECRET_ACCESS_KEY)
#       AWS_DEFAULT_REGION         (the aws cli requires a region; e.g. us-east-1)
#     The Bash store reads the endpoint from SNAPDIR_S3_STORE_ENDPOINT_URL; the
#     harness mirrors SNAPDIR_S3_TEST_ENDPOINT into it so a single var configures
#     both tools. The Rust store reads SNAPDIR_S3_TEST_ENDPOINT directly.
#
#   B2  (Backblaze B2 sandbox / S3-compatible endpoint)
#     SNAPDIR_B2_TEST_STORE       b2://bucket/prefix         (REQUIRED to run)
#     For the Rust side (S3-compatible API):
#       SNAPDIR_B2_TEST_ENDPOINT  https://s3.us-west-004.backblazeb2.com
#     For the Bash side (native b2 CLI):
#       SNAPDIR_B2_STORE_APPLICATION_KEY      (or B2_APPLICATION_KEY)
#       SNAPDIR_B2_STORE_APPLICATION_KEY_ID   (or B2_APPLICATION_KEY_ID)
#       and the `b2` CLI on PATH.
#     (B2 has no drop-in local emulator that serves BOTH the native b2 API and
#     the S3 API, so the live B2 lane targets the real B2 sandbox bucket. It is
#     SKIPPED unless SNAPDIR_B2_TEST_STORE is set.)
#
#   GCS (fake-gcs-server / real GCS)
#     SNAPDIR_GCS_TEST_STORE      gs://bucket/prefix         (REQUIRED to run)
#     For fake-gcs-server, point BOTH tools at the emulator:
#       STORAGE_EMULATOR_HOST     http://127.0.0.1:4443
#     The Bash store uses `gcloud storage`; for an emulator the operator should
#     also export CLOUDSDK_API_ENDPOINT_OVERRIDES_STORAGE accordingly. Against
#     real GCS, credentials default to the active gcloud / ADC account.
#
# ---------------------------------------------------------------------------
# KNOWN LIMITATION surfaced while writing this harness (NOT this lane's bug):
#   The Rust CLI's `resolve_store()` (crates/snapdir-cli/src/cli.rs) currently
#   wires only `file://`; `s3://`/`b2://`/`gs://` bail with
#   "store adapter `…` is not implemented yet". The remote `Store` impls exist
#   in `snapdir-stores` but are not yet reachable from the `push/fetch/...`
#   subcommands. Until that CLI wiring lands, the live Rust round-trip lanes
#   here will surface that as a REAL, un-normalized failure when the operator
#   runs them against an emulator — which is the correct behavior for a
#   differential harness (it must not hide a real gap). `--self-check` does not
#   exercise the network and is unaffected. Owner of the fix: the `cli`/`stores`
#   lane (extend resolve_store to construct S3Store/B2Store/GcsStore as a
#   boxed dyn Store, mirroring the FileStore arm).
# ---------------------------------------------------------------------------

set -euo pipefail

# ---------------------------------------------------------------------------
# Locate repo root + tools. This file lives at <root>/tests/integration/.
# ---------------------------------------------------------------------------
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../.." && pwd)"

ORACLE="${REPO_ROOT}/snapdir"
# Bash store scripts — driven indirectly through the oracle, but the self-check
# asserts they are present + executable (the operator's live run needs them).
S3_STORE="${REPO_ROOT}/snapdir-s3-store"
B2_STORE="${REPO_ROOT}/snapdir-b2-store"
GCS_STORE="${REPO_ROOT}/snapdir-gcs-store"

# Prefer the prebuilt debug binary; fall back to `cargo run` if absent.
RUST_BIN="${REPO_ROOT}/target/debug/snapdir"
RUST=()
if [[ -x "${RUST_BIN}" ]]; then
	RUST=("${RUST_BIN}")
else
	RUST=(cargo run -q -p snapdir-cli --)
fi

OS="$(uname -s)"
SELF_CHECK=false
[[ "${1:-}" == "--self-check" ]] && SELF_CHECK=true

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
if [[ -t 1 ]]; then
	C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
else
	C_RED='' C_GRN='' C_YEL='' C_DIM='' C_RST=''
fi
log() { printf '%s\n' "$*" >&2; }
info() { log "${C_DIM}[remote]${C_RST} $*"; }
ok() { log "${C_GRN}ok${C_RST} - $*"; }
skip() { log "${C_YEL}skip${C_RST} - $*"; }
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
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/snapdir-remote.XXXXXXXXXX")"

# ---------------------------------------------------------------------------
# Counters
# ---------------------------------------------------------------------------
PASS=0
RAN_BACKENDS=()
SKIPPED_BACKENDS=()
pass() { PASS=$((PASS + 1)); ok "$*"; }

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# rust <args...>  — run the Rust CLI with the active RUST_CACHE.
RUST_CACHE=""
rust() { "${RUST[@]}" --cache-dir "${RUST_CACHE}" "$@"; }

# b3 <file>       — BLAKE3 hex of a file (the oracle's object checksum).
b3() { b3sum --no-names "$1" | tr -d '[:space:]'; }

# sharded <prefix> <hex> — frozen content-addressable key:
#   <prefix>/<h[0:3]>/<h[3:6]>/<h[6:9]>/<h[9:]>
sharded() {
	local prefix="$1" h="$2"
	printf '%s/%s/%s/%s/%s' "${prefix}" "${h:0:3}" "${h:3:3}" "${h:6:3}" "${h:9}"
}

# build_corpus <dir> — deterministic source tree: nested dirs, duplicate
# content (one shared object), explicit perms, a large-ish file, unicode +
# space-bearing names, an empty file. Kept identical run-to-run so ids are
# reproducible.
build_corpus() {
	local d="$1"
	mkdir -p "${d}/sub/deep"
	printf 'hello' >"${d}/a.txt"
	printf 'world!!' >"${d}/sub/b.txt"
	printf 'dup' >"${d}/dup1.txt"
	printf 'dup' >"${d}/sub/deep/dup2.txt"   # duplicate content -> same object
	: >"${d}/empty"                           # zero-byte file
	printf 'unicode-\xc3\xa9\xc3\xb1' >"${d}/uni_éñ.txt" 2>/dev/null || printf 'unicode' >"${d}/uni.txt"
	printf 'with space' >"${d}/has space.txt"
	# a larger file with stable bytes (deterministic, ~64KiB)
	yes 'snapdir-large-line-0123456789' 2>/dev/null | head -c 65536 >"${d}/large.bin" || head -c 65536 /dev/zero >"${d}/large.bin"
	chmod 644 "${d}/a.txt"
	chmod 600 "${d}/sub/b.txt"
	chmod 640 "${d}/dup1.txt"
	chmod 640 "${d}/sub/deep/dup2.txt"
	chmod 755 "${d}/sub" "${d}/sub/deep" "${d}"
}

# build_flat_corpus <dir> — files only, no subdirs. Used where the BASH tool
# performs the checkout on macOS (frozen oracle cannot rebuild nested dirs on
# macOS — documented in file_store_roundtrip.sh; NOT a Rust/interop bug).
build_flat_corpus() {
	local d="$1"
	mkdir -p "${d}"
	printf 'hello' >"${d}/a.txt"
	printf 'world!!' >"${d}/b.txt"
	printf 'dup' >"${d}/dup1.txt"
	printf 'dup' >"${d}/dup2.txt"
	: >"${d}/empty"
	chmod 644 "${d}/a.txt"
	chmod 600 "${d}/b.txt"
	chmod 640 "${d}/dup1.txt" "${d}/dup2.txt"
	chmod 755 "${d}"
}

# perm_of <path> — octal permission bits (cross-platform).
perm_of() {
	if [[ "${OS}" == "Darwin" ]]; then
		stat -f '%A' "$1"
	else
		stat -c '%a' "$1"
	fi
}

# compare_trees <a> <b> — same relative paths, byte-identical contents,
# identical octal perms. HARD fail on ANY difference (never normalized).
# Returns 0 on identical, 1 on diff (so it is testable in --self-check without
# aborting the whole run).
compare_trees() {
	local a="$1" b="$2" f mode_a mode_b
	local list_a list_b
	list_a="$(cd "${a}" && find . | LC_ALL=C sort)"
	list_b="$(cd "${b}" && find . | LC_ALL=C sort)"
	if [[ "${list_a}" != "${list_b}" ]]; then
		log "--- path lists differ (${a} vs ${b}) ---"
		diff <(printf '%s\n' "${list_a}") <(printf '%s\n' "${list_b}") >&2 || true
		return 1
	fi
	while IFS= read -r f; do
		[[ -f "${a}/${f}" ]] || continue
		cmp -s "${a}/${f}" "${b}/${f}" || { log "content mismatch for '${f}'"; return 1; }
	done < <(cd "${a}" && find . -type f)
	while IFS= read -r f; do
		mode_a="$(perm_of "${a}/${f}")"
		mode_b="$(perm_of "${b}/${f}")"
		[[ "${mode_a}" == "${mode_b}" ]] || { log "perm mismatch for '${f}': ${mode_a} != ${mode_b}"; return 1; }
	done < <(cd "${a}" && find .)
	return 0
}

# assert_trees_equal <a> <b> — compare_trees, hard-failing the run on a diff.
assert_trees_equal() {
	compare_trees "$1" "$2" || die "destination tree diverged from the source ($1 vs $2)"
}

# assert_sharded_layout <store_root_url> <id> <src> — assert the manifest + every
# object sit at the frozen sharded keys under a LOCAL file-shaped store root.
# Only meaningful for stores we can inspect on the local filesystem; for true
# remote stores the per-tool re-manifest id equality is the layout proof.
assert_local_sharded_layout() {
	local root="$1" id="$2" src="$3" f sum obj
	[[ -f "${root}/$(sharded .manifests "${id}")" ]] || die "manifest not at sharded key under ${root}"
	pass "manifest at the frozen .manifests sharded key"
	while IFS= read -r f; do
		sum="$(b3 "${f}")"
		obj="${root}/$(sharded .objects "${sum}")"
		[[ -f "${obj}" ]] || die "object for '${f}' (b3=${sum}) not at ${obj}"
	done < <(find "${src}" -type f)
	pass "every object at its frozen .objects sharded key"
}

# ===========================================================================
# Per-backend differential lane.
#
#   run_backend <name> <scheme> <store_url>
#
# The Bash side is driven through `./snapdir` (the oracle), which discovers the
# matching `snapdir-<scheme>-store` script by scheme on its own bin dir — so the
# store-script path is not threaded through here.
#
# Performs, for the given backend:
#   Lane A  Rust round-trip:  push -> fetch -> checkout, reproduce + same id.
#   Lane B  Rust push -> Bash fetch/pull (Bash reads Rust), same id.
#   Lane C  Bash push -> Rust fetch/pull (Rust reads Bash), same id.
#
# Each round-trip uses a UNIQUE store sub-prefix (timestamp+pid) so reruns and
# the three lanes never collide. Stores are content-addressable; the snapshot
# id is derived purely from the source tree, so cross-tool id equality proves
# byte-identical manifests + identical sharded keys end to end.
# ===========================================================================
run_backend() {
	local name="$1" scheme="$2" base_url="$3"
	local tag; tag="run-$(date +%s)-$$"
	local base="${WORKDIR}/${name}"
	local cache="${base}/cache"
	mkdir -p "${cache}"

	info "=== backend ${name} (${scheme}) against ${base_url} ==="

	# --- Lane A: Rust end-to-end round-trip -------------------------------
	(
		local src="${base}/a/src" dest="${base}/a/dest"
		local store="${base_url%/}/${tag}/a"
		mkdir -p "${src%/*}"
		build_corpus "${src}"
		RUST_CACHE="${cache}/a"; mkdir -p "${RUST_CACHE}"

		local id push_id
		id="$(rust id "${src}")"
		[[ "${#id}" -eq 64 ]] || die "[${name}/A] snapshot id not 64 hex: '${id}'"

		push_id="$(rust push --store "${store}" "${src}")"
		[[ "${push_id}" == "${id}" ]] || die "[${name}/A] push printed '${push_id}', expected '${id}'"
		pass "[${name}/A] Rust push printed the source snapshot id"

		rust fetch --store "${store}" --id "${id}"
		[[ -f "${RUST_CACHE}/$(sharded .manifests "${id}")" ]] || die "[${name}/A] fetch did not cache the manifest"
		pass "[${name}/A] Rust fetch cached the manifest at its sharded key"

		rust pull --store "${store}" --id "${id}" "${dest}"
		assert_trees_equal "${src}" "${dest}"
		[[ "$(rust id "${dest}")" == "${id}" ]] || die "[${name}/A] reproduced tree re-manifests to a different id"
		pass "[${name}/A] Rust round-trip reproduced the tree + same id"

		rust verify --store "${store}" --id "${id}"
		pass "[${name}/A] Rust verify accepted the snapshot"
	)

	# --- Lane B: Rust push -> Bash fetch ----------------------------------
	(
		local src="${base}/b/src" dest="${base}/b/dest"
		local store="${base_url%/}/${tag}/b"
		mkdir -p "${src%/*}"
		if [[ "${OS}" == "Darwin" ]]; then build_flat_corpus "${src}"; else build_corpus "${src}"; fi
		RUST_CACHE="${cache}/b"; mkdir -p "${RUST_CACHE}"

		local id bash_id
		id="$(rust id "${src}")"
		rust push --store "${store}" "${src}" >/dev/null
		pass "[${name}/B] Rust pushed the tree to ${scheme}"

		bash_id="$("${ORACLE}" id --cache-dir="${cache}/b-oracle" "${src}")"
		[[ "${bash_id}" == "${id}" ]] || die "[${name}/B] Bash id '${bash_id}' != Rust id '${id}' for the same tree"
		pass "[${name}/B] Bash derives the same snapshot id as Rust"

		"${ORACLE}" pull --store="${store}" --id="${id}" --cache-dir="${cache}/b-oracle" "${dest}"
		assert_trees_equal "${src}" "${dest}"
		local re_id
		re_id="$("${ORACLE}" id --cache-dir="${cache}/b-oracle" "${dest}")"
		[[ "${re_id}" == "${id}" ]] || die "[${name}/B] Bash-checked-out tree re-manifests to '${re_id}', expected '${id}'"
		pass "[${name}/B] Bash reproduced the Rust-written snapshot + same id"
	)

	# --- Lane C: Bash push -> Rust fetch ----------------------------------
	(
		local src="${base}/c/src" dest="${base}/c/dest"
		local store="${base_url%/}/${tag}/c"
		mkdir -p "${src%/*}"
		build_corpus "${src}"   # Rust does the checkout -> nested-safe on both OSes
		RUST_CACHE="${cache}/c"; mkdir -p "${RUST_CACHE}"

		local id
		id="$("${ORACLE}" push --store="${store}" --cache-dir="${cache}/c-oracle" "${src}")"
		[[ "${#id}" -eq 64 ]] || die "[${name}/C] Bash push returned a non-id: '${id}'"
		pass "[${name}/C] Bash pushed the tree to ${scheme} (id=${id:0:12}…)"

		[[ "$(rust id "${src}")" == "${id}" ]] || die "[${name}/C] Rust id differs from Bash id for the same tree"
		pass "[${name}/C] Rust derives the same snapshot id as Bash"

		rust pull --store "${store}" --id "${id}" "${dest}"
		assert_trees_equal "${src}" "${dest}"
		[[ "$(rust id "${dest}")" == "${id}" ]] || die "[${name}/C] Rust-pulled tree re-manifests to a different id"
		pass "[${name}/C] Rust reproduced the Bash-written snapshot + same id"

		rust verify --store "${store}" --id "${id}"
		pass "[${name}/C] Rust verify accepted the Bash-pushed snapshot"
	)

	RAN_BACKENDS+=("${name}")
	ok "backend ${name}: all differential lanes passed"
}

# ===========================================================================
# Backend detection + dispatch (skip-not-fail when env is unset).
# ===========================================================================
maybe_run_s3() {
	if [[ -z "${SNAPDIR_S3_TEST_STORE:-}" || -z "${SNAPDIR_S3_TEST_ENDPOINT:-}" ]]; then
		skip "s3: skipped (no s3 endpoint configured — set SNAPDIR_S3_TEST_STORE + SNAPDIR_S3_TEST_ENDPOINT)"
		SKIPPED_BACKENDS+=("s3"); return 0
	fi
	command -v aws >/dev/null || { skip "s3: skipped (aws CLI not on PATH)"; SKIPPED_BACKENDS+=("s3"); return 0; }
	# Mirror the single test endpoint into the var the Bash store reads.
	export SNAPDIR_S3_STORE_ENDPOINT_URL="${SNAPDIR_S3_STORE_ENDPOINT_URL:-${SNAPDIR_S3_TEST_ENDPOINT}}"
	run_backend s3 "s3://" "${SNAPDIR_S3_TEST_STORE}"
}

maybe_run_b2() {
	if [[ -z "${SNAPDIR_B2_TEST_STORE:-}" ]]; then
		skip "b2: skipped (no b2 endpoint configured — set SNAPDIR_B2_TEST_STORE [+ SNAPDIR_B2_TEST_ENDPOINT, B2 creds])"
		SKIPPED_BACKENDS+=("b2"); return 0
	fi
	command -v b2 >/dev/null || { skip "b2: skipped (b2 CLI not on PATH; required by the Bash oracle side)"; SKIPPED_BACKENDS+=("b2"); return 0; }
	run_backend b2 "b2://" "${SNAPDIR_B2_TEST_STORE}"
}

maybe_run_gcs() {
	if [[ -z "${SNAPDIR_GCS_TEST_STORE:-}" ]]; then
		skip "gcs: skipped (no gcs endpoint configured — set SNAPDIR_GCS_TEST_STORE [+ STORAGE_EMULATOR_HOST for fake-gcs-server])"
		SKIPPED_BACKENDS+=("gcs"); return 0
	fi
	command -v gcloud >/dev/null || { skip "gcs: skipped (gcloud CLI not on PATH; required by the Bash oracle side)"; SKIPPED_BACKENDS+=("gcs"); return 0; }
	run_backend gcs "gs://" "${SNAPDIR_GCS_TEST_STORE}"
}

# ===========================================================================
# --self-check: emulator-FREE validation of harness plumbing. Exit 0.
# ===========================================================================
self_check() {
	info "self-check: validating harness plumbing (no live emulators)"

	# 1. Required tools / scripts resolvable.
	command -v b3sum >/dev/null || die "self-check: b3sum not found (harness dependency)"
	pass "self-check: b3sum resolvable"

	if [[ -x "${RUST_BIN}" ]]; then
		"${RUST_BIN}" --version >/dev/null 2>&1 || "${RUST_BIN}" -V >/dev/null 2>&1 \
			|| die "self-check: Rust binary at ${RUST_BIN} did not run"
		pass "self-check: Rust binary resolvable + runs (${RUST_BIN})"
	else
		# `cargo run` fallback: just confirm cargo is present; do not compile here.
		command -v cargo >/dev/null || die "self-check: no prebuilt Rust binary and cargo not on PATH"
		skip "self-check: prebuilt Rust binary absent; would fall back to 'cargo run' at runtime"
	fi

	[[ -x "${ORACLE}" ]] || die "self-check: Bash oracle not executable at ${ORACLE}"
	"${ORACLE}" --version >/dev/null 2>&1 || "${ORACLE}" version >/dev/null 2>&1 \
		|| die "self-check: Bash oracle did not report a version"
	pass "self-check: Bash oracle resolvable + runs (${ORACLE})"

	local s
	for s in "${S3_STORE}" "${B2_STORE}" "${GCS_STORE}"; do
		[[ -x "${s}" ]] || die "self-check: store script not executable: ${s}"
	done
	pass "self-check: s3/b2/gcs Bash store scripts all resolvable + executable"

	# 2. Corpus generator runs and produces the expected shape.
	local cdir="${WORKDIR}/selfcheck-corpus"
	build_corpus "${cdir}"
	[[ -f "${cdir}/a.txt" && -f "${cdir}/sub/b.txt" && -f "${cdir}/sub/deep/dup2.txt" ]] \
		|| die "self-check: corpus generator did not produce the expected tree"
	[[ -f "${cdir}/empty" && ! -s "${cdir}/empty" ]] || die "self-check: corpus empty file wrong"
	[[ "$(b3 "${cdir}/dup1.txt")" == "$(b3 "${cdir}/sub/deep/dup2.txt")" ]] \
		|| die "self-check: duplicate-content files must share one object checksum"
	pass "self-check: corpus generator produces nested dirs + dup objects + empty file"

	# Corpus must re-manifest to a STABLE snapshot id (determinism) if the Rust
	# binary is available — proves id derivation is reproducible run-to-run.
	if [[ -x "${RUST_BIN}" ]]; then
		RUST_CACHE="${WORKDIR}/selfcheck-cache"; mkdir -p "${RUST_CACHE}"
		local id1 id2
		id1="$(rust id "${cdir}")"
		local cdir2="${WORKDIR}/selfcheck-corpus2"
		build_corpus "${cdir2}"
		id2="$(rust id "${cdir2}")"
		[[ "${id1}" == "${id2}" && "${#id1}" -eq 64 ]] \
			|| die "self-check: corpus is not deterministic ('${id1}' != '${id2}')"
		pass "self-check: corpus re-manifests to a stable 64-hex snapshot id"
	fi

	# 3. The sharded-key helper matches the frozen layout.
	[[ "$(sharded .objects abcdefghijk)" == ".objects/abc/def/ghi/jk" ]] \
		|| die "self-check: sharded() does not match the frozen content-addressable layout"
	pass "self-check: sharded() matches the frozen 3/3/3/rest layout"

	# 4. compare_trees works on synthetic pairs: equal -> 0, diverged -> 1.
	local pa="${WORKDIR}/cmp/a" pb="${WORKDIR}/cmp/b"
	build_corpus "${pa}"; build_corpus "${pb}"
	compare_trees "${pa}" "${pb}" || die "self-check: compare_trees reported identical trees as different"
	pass "self-check: compare_trees accepts identical trees"

	printf 'tampered' >"${pb}/a.txt"   # content divergence
	if compare_trees "${pa}" "${pb}" 2>/dev/null; then
		die "self-check: compare_trees FAILED to detect a content divergence"
	fi
	pass "self-check: compare_trees detects a content divergence (no false pass)"

	build_corpus "${pb}"; printf 'x' >"${pb}/extra.txt"   # path-set divergence
	if compare_trees "${pa}" "${pb}" 2>/dev/null; then
		die "self-check: compare_trees FAILED to detect a path-set divergence"
	fi
	pass "self-check: compare_trees detects a path-set divergence (no false pass)"

	build_corpus "${pb}"; chmod 700 "${pb}/a.txt"   # perm divergence
	if compare_trees "${pa}" "${pb}" 2>/dev/null; then
		die "self-check: compare_trees FAILED to detect a permission divergence"
	fi
	pass "self-check: compare_trees detects a permission divergence (no false pass)"

	# 5. Skip-not-fail: with NO backend env set, detection must SKIP cleanly.
	(
		unset SNAPDIR_S3_TEST_STORE SNAPDIR_S3_TEST_ENDPOINT
		unset SNAPDIR_B2_TEST_STORE SNAPDIR_B2_TEST_ENDPOINT
		unset SNAPDIR_GCS_TEST_STORE STORAGE_EMULATOR_HOST
		SKIPPED_BACKENDS=()
		maybe_run_s3; maybe_run_b2; maybe_run_gcs
		[[ "${#SKIPPED_BACKENDS[@]}" -eq 3 ]] \
			|| die "self-check: expected all 3 backends skipped when env unset, got ${#SKIPPED_BACKENDS[@]}"
	) || exit 1
	pass "self-check: all 3 backends report 'skipped (no … endpoint configured)' when env unset"

	ok "self-check passed: ${PASS} plumbing assertions OK (no emulators required)"
	exit 0
}

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
command -v b3sum >/dev/null || die "b3sum is required by the test harness (oracle dependency)"

if [[ "${SELF_CHECK}" == "true" ]]; then
	self_check
fi

info "workdir: ${WORKDIR}"
info "rust:    ${RUST[*]}"
info "oracle:  ${ORACLE}"
info "os:      ${OS}"

maybe_run_s3
maybe_run_b2
maybe_run_gcs

# No silent passes: always report which backends ran vs. skipped.
info "backends ran:     ${RAN_BACKENDS[*]:-(none)}"
info "backends skipped: ${SKIPPED_BACKENDS[*]:-(none)}"

if [[ "${#RAN_BACKENDS[@]}" -eq 0 ]]; then
	die "no remote backends were configured — set at least one of SNAPDIR_{S3,B2,GCS}_TEST_STORE (+ endpoint/creds) to run live differential round-trips"
fi

ok "remote-store differential harness: ${PASS} assertions passed across [${RAN_BACKENDS[*]}]"

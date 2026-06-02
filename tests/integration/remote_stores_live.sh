#!/usr/bin/env bash
#
# tests/integration/remote_stores_live.sh
#
# Self-contained, deterministic, self-cleaning LIVE driver for the
# `remote-interop` keystone gate. The Gatesmith PM runs this EVERY tick to
# actually PROVE Bash<->Rust remote-store interoperability instead of relying on
# a human rubber-stamp.
#
# It brings up local emulators with docker, exports the env contract that the
# existing differential harness (`tests/integration/remote_stores.sh`) expects,
# and DELEGATES every cross-tool assertion to that harness — it does NOT
# re-implement the per-backend lanes. On top of that it adds the single most
# important new assertion of this gate:
#
#   ZERO-EXTERNAL-DEPENDENCY: the Rust binary's built-in s3:// store completes a
#   full push -> fetch -> pull -> verify round-trip with `aws`, `b2`, and
#   `gcloud` REMOVED FROM PATH. If the Rust binary ever shelled out to a cloud
#   CLI for a built-in store it would fail here. This is a HARD assertion.
#
# ---------------------------------------------------------------------------
# EMULATOR / BACKEND CHOICES (and why)
# ---------------------------------------------------------------------------
#   S3  -> MinIO (docker). Needs only docker, so this lane ALWAYS runs and is the
#          guaranteed-green backend. Bucket is created with the `aws` CLI against
#          the MinIO endpoint (no `mc` needed). Both tools are pointed at the
#          MinIO endpoint via SNAPDIR_S3_TEST_ENDPOINT (Rust) +
#          SNAPDIR_S3_STORE_ENDPOINT_URL (Bash oracle AND the Rust CLI's
#          resolve_store, which reads SNAPDIR_S3_STORE_ENDPOINT_URL — verified in
#          crates/snapdir-cli/src/cli.rs).
#
#   GCS  -> REAL GCS, env-gated. fake-gcs-server canNOT drive BOTH sides: the
#          Rust `google-cloud-storage` v1.x SDK (gcs_store.rs `Storage::builder()
#          .build()`) has no STORAGE_EMULATOR_HOST / anonymous-auth path wired and
#          insists on real ADC, so it never talks to the emulator (empirically
#          confirmed: it errors on credentials and would hit real GCS endpoints
#          regardless). gcs_store.rs is under crates/** and FROZEN to this lane,
#          so we cannot teach it the emulator. Therefore the GCS lane honors the
#          operator's pre-sourced real-GCS env (SNAPDIR_GCS_TEST_STORE + active
#          gcloud / ADC). Because the Rust SDK uses Application Default
#          Credentials (NOT the active `gcloud` account), a wrong/missing ADC
#          surfaces as PERMISSION_DENIED; we PREFLIGHT a tiny Rust round-trip and,
#          if ADC cannot reach the bucket, report GCS as EXPLICITLY skipped with
#          the exact remediation (an operator-env issue, never a silent pass and
#          never a normalized diff).
#
#   B2   -> REAL B2 sandbox, env-gated. No local emulator serves BOTH B2's native
#          API (Bash side) and the S3 API (Rust side), so B2 runs only if
#          SNAPDIR_B2_TEST_STORE is set AND the `b2` CLI is on PATH (the Bash
#          oracle's dependency). Otherwise it is reported EXPLICITLY skipped.
#
# The `aws` / `b2` / `gcloud` CLIs on this machine exist ONLY to drive the BASH
# ORACLE side of the differential test. They are the oracle's dependency, never
# Rust's — which the zero-dependency lane proves by running Rust without them.
#
# ---------------------------------------------------------------------------
# MODES
# ---------------------------------------------------------------------------
#   (no args)      Full live run: MinIO S3 (+ real GCS/B2 if env'd) + zero-dep.
#   --self-check   Emulator-FREE validation of THIS wrapper's own plumbing
#                  (docker present? existing harness present + its --self-check
#                  passes? PATH-sanitizer correct?) and exit 0 WITHOUT starting
#                  any container — so CI / a quick PM check validates plumbing
#                  without docker pulls.
#
# Deterministic, shellcheck-clean, set -euo pipefail, self-cleaning (trap tears
# down every emulator container + temp dir on EXIT/INT/TERM, even on failure).
# A real interop diff is a HARD failure — NEVER normalized away.
# ---------------------------------------------------------------------------

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../.." && pwd)"
HARNESS="${HERE}/remote_stores.sh"
RUST_BIN="${REPO_ROOT}/target/debug/snapdir"

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
log()  { printf '%s\n' "$*" >&2; }
info() { log "${C_DIM}[live]${C_RST} $*"; }
ok()   { log "${C_GRN}ok${C_RST} - $*"; }
skip() { log "${C_YEL}skip${C_RST} - $*"; }
die()  { log "${C_RED}FAIL${C_RST} - $*"; exit 1; }

# ---------------------------------------------------------------------------
# Self-cleaning workspace + container teardown.
# ---------------------------------------------------------------------------
WORKDIR=""
CONTAINERS=()
cleanup() {
	local c
	for c in "${CONTAINERS[@]:-}"; do
		[[ -n "${c}" ]] || continue
		docker rm -f "${c}" >/dev/null 2>&1 || true
	done
	if [[ -n "${WORKDIR}" && -d "${WORKDIR}" ]]; then
		chmod -R u+rwx "${WORKDIR}" 2>/dev/null || true
		rm -rf "${WORKDIR}" 2>/dev/null || true
	fi
}
trap cleanup EXIT INT TERM
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/snapdir-live.XXXXXXXXXX")"

# Reporting (mirrors the delegate harness's no-silent-pass contract).
RAN_BACKENDS=()
SKIPPED_BACKENDS=()

# ---------------------------------------------------------------------------
# Free-port picker: ask the OS for an unused TCP port (deterministic enough; the
# whole run is ephemeral + self-cleaning).
# ---------------------------------------------------------------------------
free_port() {
	# Bind :0 and read back the assigned port. Pure-Python is always present on
	# macOS/Linux CI images; fall back to a fixed-but-checked range otherwise.
	if command -v python3 >/dev/null 2>&1; then
		python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
		return 0
	fi
	# Fallback: scan a high range for a free port using bash /dev/tcp.
	local p
	for p in $(seq 20000 20200); do
		if ! (exec 3<>"/dev/tcp/127.0.0.1/${p}") 2>/dev/null; then
			printf '%s\n' "${p}"; return 0
		fi
		exec 3>&- 2>/dev/null || true
	done
	die "could not find a free TCP port"
}

# wait_http <url> <label> — poll an HTTP health URL until it answers (<=60s).
wait_http() {
	local url="$1" label="$2" i
	for i in $(seq 1 60); do
		if curl -fsS "${url}" >/dev/null 2>&1; then
			info "${label} ready (after ${i}s)"
			return 0
		fi
		sleep 1
	done
	die "${label} did not become ready at ${url} within 60s"
}

# ===========================================================================
# Rust runner with an isolated cache. Mirrors the delegate harness so the
# zero-dependency lane exercises the SAME built-in store code path.
# ===========================================================================
RUST=()
if [[ -x "${RUST_BIN}" ]]; then
	RUST=("${RUST_BIN}")
else
	RUST=(cargo run -q -p snapdir-cli --)
fi

# ===========================================================================
# build_corpus <dir> — small deterministic tree (kept independent of the
# delegate harness so the zero-dep + preflight lanes don't depend on its
# internals). Includes a nested dir, a duplicate-content pair (one shared
# object), an empty file and explicit perms. NO space-bearing names (the frozen
# oracle can't push those — see remote_stores.sh build_nospace_corpus), so this
# corpus is safe for BOTH tools in every direction.
# ===========================================================================
build_corpus() {
	local d="$1"
	mkdir -p "${d}/sub"
	printf 'hello' >"${d}/a.txt"
	printf 'world!!' >"${d}/sub/b.txt"
	printf 'dup' >"${d}/dup1.txt"
	printf 'dup' >"${d}/sub/dup2.txt"
	: >"${d}/empty"
	chmod 644 "${d}/a.txt"
	chmod 600 "${d}/sub/b.txt"
	chmod 640 "${d}/dup1.txt" "${d}/sub/dup2.txt"
	chmod 755 "${d}/sub" "${d}"
}

# compare_trees <a> <b> — byte-identical files at identical relative paths.
# HARD fail on any difference (never normalized). Used only by the zero-dep
# lane; the cross-tool lanes use the delegate harness's stricter comparator.
compare_trees() {
	local a="$1" b="$2" f la lb
	la="$(cd "${a}" && find . | LC_ALL=C sort)"
	lb="$(cd "${b}" && find . | LC_ALL=C sort)"
	[[ "${la}" == "${lb}" ]] || { log "path lists differ"; diff <(printf '%s\n' "${la}") <(printf '%s\n' "${lb}") >&2 || true; return 1; }
	while IFS= read -r f; do
		[[ -f "${a}/${f}" ]] || continue
		cmp -s "${a}/${f}" "${b}/${f}" || { log "content mismatch for '${f}'"; return 1; }
	done < <(cd "${a}" && find . -type f)
	return 0
}

# ===========================================================================
# build_sanitized_bin — create (once) a symlink-farm bin dir under WORKDIR that
# contains a symlink to every executable currently on PATH EXCEPT the cloud CLIs
# (aws, b2, b2v3, b2v4, gcloud, gsutil, bq). Prints its absolute path on stdout.
#
# Name-level exclusion (not directory-level) is required because a cloud CLI can
# co-reside with a tool the Rust binary legitimately needs (here b2 and b3sum
# both live in /opt/homebrew/bin). The result is the ONLY PATH entry the Rust
# binary gets in the zero-dependency lane, so if the binary ever shelled out to a
# cloud CLI for a built-in store it would fail to find it.
# ===========================================================================
SANITIZED_BIN=""
build_sanitized_bin() {
	if [[ -n "${SANITIZED_BIN}" ]]; then
		printf '%s\n' "${SANITIZED_BIN}"; return 0
	fi
	local dir="${WORKDIR}/zerodep-bin"
	mkdir -p "${dir}"
	local banned=" aws b2 b2v3 b2v4 gcloud gsutil bq "
	local pdir name target
	# Walk the real PATH; symlink each executable whose basename is NOT banned.
	while IFS= read -r pdir; do
		[[ -n "${pdir}" && -d "${pdir}" ]] || continue
		for target in "${pdir}"/*; do
			[[ -x "${target}" && ! -d "${target}" ]] || continue
			name="$(basename "${target}")"
			[[ "${banned}" == *" ${name} "* ]] && continue
			# First-wins (mirror PATH precedence): don't clobber an earlier link.
			[[ -e "${dir}/${name}" ]] && continue
			ln -s "${target}" "${dir}/${name}" 2>/dev/null || true
		done
	done < <(printf '%s\n' "${PATH}" | tr ':' '\n')
	SANITIZED_BIN="${dir}"
	printf '%s\n' "${dir}"
}

# ===========================================================================
# b2 CLI v3 provisioning for the BASH ORACLE side.
#
# The frozen oracle `snapdir-b2-store` is written against b2 CLI v3 subcommands
# (`upload-file`, `download-file-by-name`, two-arg `b2 ls <bucket> <dir>`). b2
# CLI v4 REMOVED those (they became `b2 file upload` / `b2 file download`), so a
# system b2 v4+ makes the oracle's push/fetch fail. This is purely an oracle-side
# CLI-version mismatch, NOT a Rust defect.
#
# Strategy (prints, on stdout, a directory to PREPEND to PATH for the B2 lane,
# or nothing if the system b2 is already usable / no shim is needed):
#   * system b2 is v3.x         -> use it as-is, print nothing (empty).
#   * system b2 is v4+ + uvx    -> emit a tiny `b2` shim that execs a pinned
#                                  b2<4 via `uvx` (with docutils==0.18.1, which
#                                  b2 v3 needs — b2 v3 imports
#                                  docutils.utils.error_reporting, removed in
#                                  docutils 0.19). Print the shim dir.
#   * no v3 b2 and no uvx        -> return 1 (caller LOUD-SKIPs with remediation).
#
# The shim dir lives under WORKDIR, so the EXIT trap removes it. It is named
# `b2`, which the zero-dependency PATH-sanitizer already drops by name, so it can
# never leak into the pure-Rust zero-dep lane (which runs BEFORE this anyway).
# ===========================================================================
B2_SHIM_DIR=""
b2_is_v3() {
	# True iff the b2 currently first on PATH is major version 3.
	#
	# `b2 version` prints e.g. "b2 command line tool, version 4.7.0 (b2sdk
	# version 2.12.0)". We extract the FIRST major.minor.patch token that follows
	# the word "version" (the CLI version, not the b2sdk version) and test its
	# major. Note: a probe like `b2 upload-file --help` is NOT reliable — b2 v4.x
	# still accepts `upload-file` as a deprecated alias (exit 0), so it cannot
	# distinguish v3 from v4. The version string is the source of truth.
	local verline major
	command -v b2 >/dev/null 2>&1 || return 1
	verline="$(b2 version 2>/dev/null | head -n1)"
	# Take the FIRST "<digits>.<digits>.<digits>" token on the line (the CLI
	# version; the b2sdk version that follows is ignored). grep -o + head avoids
	# sed's greedy ".*version" matching the trailing "b2sdk version".
	major="$(printf '%s\n' "${verline}" \
		| grep -oE '[0-9]+\.[0-9]+\.[0-9]+' \
		| head -n1 \
		| cut -d. -f1)"
	[[ "${major}" == "3" ]]
}
provision_b2_v3() {
	# Echo a PATH dir that resolves `b2` to a v3 client, or empty if the system
	# b2 already works. Return 1 if a v3 b2 cannot be provided at all.
	if [[ -n "${B2_SHIM_DIR}" ]]; then
		printf '%s\n' "${B2_SHIM_DIR}"; return 0
	fi
	if b2_is_v3; then
		# System b2 already speaks v3 — no shim needed.
		printf '%s\n' ""
		return 0
	fi
	# System b2 is v4+ (or unusable). Need uvx to run a pinned b2<4.
	command -v uvx >/dev/null 2>&1 || return 1
	local dir="${WORKDIR}/b2v3-shim"
	mkdir -p "${dir}"
	cat >"${dir}/b2" <<'SHIM'
#!/usr/bin/env bash
# Pinned b2 CLI v3 for the snapdir Bash oracle (system b2 is v4+, which dropped
# the oracle's subcommands). docutils==0.18.1 is REQUIRED: b2 v3 imports
# docutils.utils.error_reporting, removed in docutils 0.19.
exec uvx --quiet --python 3.11 --from 'b2<4' --with 'docutils==0.18.1' b2 "$@"
SHIM
	chmod 755 "${dir}/b2"
	B2_SHIM_DIR="${dir}"
	printf '%s\n' "${dir}"
	return 0
}

# ===========================================================================
# MinIO S3 emulator bring-up. Exports the env both tools read, then returns 0.
# ===========================================================================
S3_PORT=""
S3_ENDPOINT=""
S3_BUCKET="snapdir-interop"
start_minio() {
	command -v docker >/dev/null || die "docker is required for the MinIO S3 lane"
	command -v aws >/dev/null    || die "aws CLI is required to drive the Bash oracle S3 side + create the bucket"

	S3_PORT="$(free_port)"
	S3_ENDPOINT="http://127.0.0.1:${S3_PORT}"
	local cname="snapdir-minio-$$-${S3_PORT}"
	CONTAINERS+=("${cname}")

	info "starting MinIO (${cname}) on ${S3_ENDPOINT}"
	docker run -d --name "${cname}" -p "${S3_PORT}:9000" \
		-e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
		minio/minio:latest server /data >/dev/null \
		|| die "failed to start MinIO container"

	wait_http "${S3_ENDPOINT}/minio/health/live" "MinIO"

	# Create the bucket with the aws CLI (idempotent). The bucket lives only for
	# this run; objects land under a per-run prefix so reruns never collide.
	AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin AWS_DEFAULT_REGION=us-east-1 \
		aws --endpoint-url "${S3_ENDPOINT}" s3 mb "s3://${S3_BUCKET}" >/dev/null 2>&1 \
		|| info "bucket s3://${S3_BUCKET} already exists (ok)"

	# Export the ENV-VAR CONTRACT the delegate harness + both tools expect.
	#   * SNAPDIR_S3_TEST_STORE      — the store URL (Rust + harness).
	#   * SNAPDIR_S3_TEST_ENDPOINT   — Rust store endpoint (read by S3Store live
	#                                  tests + mirrored by the harness).
	#   * SNAPDIR_S3_STORE_ENDPOINT_URL — read by the Bash oracle AND by the Rust
	#                                  CLI's resolve_store (cli.rs). Mirrored here
	#                                  so a single endpoint configures everything.
	#   * AWS creds + region         — required by both the aws CLI and the SDK.
	local prefix
	prefix="run-$(date +%s)-$$"
	export SNAPDIR_S3_TEST_STORE="s3://${S3_BUCKET}/${prefix}"
	export SNAPDIR_S3_TEST_ENDPOINT="${S3_ENDPOINT}"
	export SNAPDIR_S3_STORE_ENDPOINT_URL="${S3_ENDPOINT}"
	export AWS_ACCESS_KEY_ID=minioadmin
	export AWS_SECRET_ACCESS_KEY=minioadmin
	export AWS_DEFAULT_REGION=us-east-1
	export SNAPDIR_S3_STORE_AWS_ACCESS_KEY_ID=minioadmin
	export SNAPDIR_S3_STORE_AWS_SECRET_ACCESS_KEY=minioadmin
	ok "MinIO S3 ready: ${SNAPDIR_S3_TEST_STORE} via ${S3_ENDPOINT}"
}

# ===========================================================================
# store_preflight <name> <store-url> — REACHABILITY probe for a real cloud
# backend (GCS / B2) before delegating its differential lanes. Does a tiny Rust
# push + verify against the configured store. Returns 0 if reachable, 1 if not
# (wrong/missing creds, wrong endpoint/region, missing bucket, etc.), writing the
# Rust stderr to "${WORKDIR}/<name>-preflight.err" for a precise skip message.
#
# This distinguishes an OPERATOR-ENV problem (backend not reachable — skip loudly
# with remediation) from a genuine INTEROP DIFF (a tree/id mismatch in the
# delegate harness — a HARD failure we never normalize). A backend we cannot even
# reach cannot have produced an interop diff, so skipping is correct; the diff
# assertions only run once the backend is provably reachable.
# ===========================================================================
store_preflight() {
	local name="$1" base_url="$2"
	local src="${WORKDIR}/${name}-preflight/src"
	local cache="${WORKDIR}/${name}-preflight/cache"
	local errf="${WORKDIR}/${name}-preflight.err"
	mkdir -p "${src}" "${cache}"
	printf '%s-preflight' "${name}" >"${src}/probe.txt"
	chmod 755 "${src}"; chmod 644 "${src}/probe.txt"

	local store id
	store="${base_url%/}/preflight-$(date +%s)-$$"
	if ! id="$("${RUST[@]}" --cache-dir "${cache}" push --store "${store}" "${src}" 2>"${errf}")"; then
		return 1
	fi
	[[ "${#id}" -eq 64 ]] || return 1
	"${RUST[@]}" --cache-dir "${cache}" verify --store "${store}" --id "${id}" 2>>"${errf}" || return 1
	return 0
}

# ===========================================================================
# Backend dispatch via the DELEGATE harness. We export the env each backend
# needs, then run the existing harness restricted to that single backend by
# unsetting the others — reusing its per-backend differential lanes verbatim.
# ===========================================================================
run_delegate_for() {
	# run_delegate_for <backend-name>  — runs remote_stores.sh with ONLY <name>'s
	# env active so exactly one backend's lanes execute. The harness exits 0 iff
	# that backend's lanes (Rust round-trip + Bash<->Rust both directions) pass
	# byte-identically; a real diff is a HARD non-zero exit we propagate.
	local name="$1"
	(
		case "${name}" in
			s3)  unset SNAPDIR_B2_TEST_STORE SNAPDIR_GCS_TEST_STORE STORAGE_EMULATOR_HOST 2>/dev/null || true ;;
			gcs) unset SNAPDIR_S3_TEST_STORE SNAPDIR_S3_TEST_ENDPOINT SNAPDIR_B2_TEST_STORE 2>/dev/null || true ;;
			b2)  unset SNAPDIR_S3_TEST_STORE SNAPDIR_S3_TEST_ENDPOINT SNAPDIR_GCS_TEST_STORE STORAGE_EMULATOR_HOST SNAPDIR_S3_STORE_ENDPOINT_URL 2>/dev/null || true ;;
		esac
		bash "${HARNESS}"
	)
}

# ===========================================================================
# ZERO-EXTERNAL-DEPENDENCY lane (the keystone new assertion).
#
# Run a full Rust-only push -> fetch -> pull -> verify round-trip against the
# MinIO S3 store with `aws`, `b2`, and `gcloud` REMOVED FROM PATH. We build a
# sanitized PATH that contains ONLY the directories the Rust binary itself needs
# (the dirs of `b3sum` etc.) and EXPLICITLY excludes every cloud CLI's dir. If
# the Rust binary ever tried to shell out to a cloud CLI for its built-in s3://
# store, it would fail to find it and this HARD assertion would fail.
# ===========================================================================
zero_dependency_lane() {
	[[ -n "${SNAPDIR_S3_TEST_STORE:-}" ]] || die "zero-dep lane requires the MinIO S3 store to be up"

	info "=== zero-external-dependency lane (aws/b2/gcloud removed from PATH) ==="

	# Build a sanitized bin dir that is the ONLY PATH entry the Rust binary sees.
	# We symlink in every executable currently reachable on PATH EXCEPT aws, b2
	# and gcloud (and their common aliases). Filtering by NAME — not by directory
	# — is essential because cloud CLIs can share a dir with tools the binary
	# legitimately needs (e.g. on this machine `b2` and `b3sum` both live in
	# /opt/homebrew/bin, so dropping the whole dir would also drop b3sum). With a
	# name-level allowlist the cloud CLIs literally do not exist in the Rust
	# process's PATH, while b3sum + coreutils remain reachable.
	local sanitized_bin
	sanitized_bin="$(build_sanitized_bin)"
	info "sanitized PATH=${sanitized_bin} (symlink farm; aws/b2/gcloud excluded by name)"

	# Sanity: the cloud CLIs MUST be invisible under the sanitized PATH; tools the
	# binary needs (b3sum) MUST still resolve.
	local cli
	for cli in aws b2 gcloud; do
		if command -v "${cli}" >/dev/null 2>&1; then
			PATH="${sanitized_bin}" command -v "${cli}" >/dev/null 2>&1 \
				&& die "zero-dep lane: '${cli}' is still visible under the sanitized PATH — sanitizer failed"
		fi
	done
	PATH="${sanitized_bin}" command -v b3sum >/dev/null 2>&1 \
		|| die "zero-dep lane: b3sum unreachable under the sanitized PATH — sanitizer too aggressive"
	ok "zero-dependency: aws/b2/gcloud are invisible under the sanitized PATH"

	# Full round-trip under the sanitized PATH against the SAME MinIO store.
	local src="${WORKDIR}/zerodep/src" dest="${WORKDIR}/zerodep/dest" cache="${WORKDIR}/zerodep/cache"
	mkdir -p "${src}" "${dest}" "${cache}"
	build_corpus "${src}"
	local store id push_id re_id
	store="${SNAPDIR_S3_TEST_STORE%/}/zerodep-$(date +%s)-$$"

	id="$("${RUST[@]}" --cache-dir "${cache}" id "${src}")"
	[[ "${#id}" -eq 64 ]] || die "zero-dep: snapshot id not 64 hex: '${id}'"

	# From here on, run Rust with the cloud CLIs absent from PATH. The S3 creds +
	# endpoint env stay set; only PATH is sanitized.
	push_id="$(PATH="${sanitized_bin}" "${RUST[@]}" --cache-dir "${cache}" push --store "${store}" "${src}")"
	[[ "${push_id}" == "${id}" ]] || die "zero-dep: push printed '${push_id}', expected '${id}'"

	PATH="${sanitized_bin}" "${RUST[@]}" --cache-dir "${cache}" fetch --store "${store}" --id "${id}"
	PATH="${sanitized_bin}" "${RUST[@]}" --cache-dir "${cache}" pull --store "${store}" --id "${id}" "${dest}"
	compare_trees "${src}" "${dest}" || die "zero-dep: pulled tree diverged from the source"

	re_id="$(PATH="${sanitized_bin}" "${RUST[@]}" --cache-dir "${cache}" id "${dest}")"
	[[ "${re_id}" == "${id}" ]] || die "zero-dep: reproduced tree re-manifests to '${re_id}', expected '${id}'"

	PATH="${sanitized_bin}" "${RUST[@]}" --cache-dir "${cache}" verify --store "${store}" --id "${id}"

	ok "zero-external-dependency: Rust round-trip succeeded with aws/b2/gcloud absent from PATH"
}

# ===========================================================================
# --self-check: emulator-FREE plumbing validation. Exit 0 without containers.
# ===========================================================================
self_check() {
	info "self-check: validating live-wrapper plumbing (no containers)"

	command -v docker >/dev/null || die "self-check: docker not on PATH (required for the live run)"
	docker version >/dev/null 2>&1 || die "self-check: docker daemon not reachable"
	ok "self-check: docker present + daemon reachable"

	[[ -f "${HARNESS}" ]] || die "self-check: delegate harness missing at ${HARNESS}"
	ok "self-check: delegate harness present (${HARNESS})"

	# The delegate harness's own --self-check must pass (its plumbing is ours too).
	bash "${HARNESS}" --self-check >/dev/null 2>&1 \
		|| die "self-check: delegate harness --self-check FAILED (run 'bash ${HARNESS} --self-check' to see why)"
	ok "self-check: delegate harness --self-check passed"

	# free_port returns a plausible TCP port.
	local p; p="$(free_port)"
	[[ "${p}" =~ ^[0-9]+$ && "${p}" -ge 1 && "${p}" -le 65535 ]] \
		|| die "self-check: free_port did not return a valid port ('${p}')"
	ok "self-check: free_port returns a valid TCP port (${p})"

	# PATH-sanitizer logic: the constructed symlink-farm PATH must hide whatever
	# cloud CLIs exist on this machine while keeping b3sum reachable. Exercise the
	# SAME build_sanitized_bin used by the live zero-dependency lane.
	command -v b3sum >/dev/null || die "self-check: b3sum not on PATH (harness dependency)"
	local sanitized_bin cli
	sanitized_bin="$(build_sanitized_bin)"
	[[ -d "${sanitized_bin}" ]] || die "self-check: sanitized bin dir was not created"
	PATH="${sanitized_bin}" command -v b3sum >/dev/null 2>&1 \
		|| die "self-check: b3sum unreachable under the sanitized PATH (sanitizer too aggressive)"
	for cli in aws b2 gcloud; do
		if command -v "${cli}" >/dev/null 2>&1; then
			if PATH="${sanitized_bin}" command -v "${cli}" >/dev/null 2>&1; then
				die "self-check: '${cli}' still visible under the sanitized PATH — sanitizer is broken"
			fi
		fi
	done
	ok "self-check: PATH-sanitizer hides aws/b2/gcloud while keeping b3sum reachable"

	# Rust binary (or cargo fallback) resolvable.
	if [[ -x "${RUST_BIN}" ]]; then
		"${RUST_BIN}" --version >/dev/null 2>&1 || die "self-check: Rust binary did not run"
		ok "self-check: Rust binary resolvable (${RUST_BIN})"
	else
		command -v cargo >/dev/null || die "self-check: no prebuilt Rust binary and cargo not on PATH"
		skip "self-check: prebuilt Rust binary absent; would fall back to 'cargo run'"
	fi

	ok "self-check passed: live-wrapper plumbing OK (no emulators required)"
	exit 0
}

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
command -v b3sum >/dev/null || die "b3sum is required (oracle + harness dependency)"

if [[ "${SELF_CHECK}" == "true" ]]; then
	self_check
fi

# Ensure the Rust binary exists; build it if missing (cargo run fallback is slow
# and re-builds per invocation, so prefer a one-time build).
if [[ ! -x "${RUST_BIN}" ]]; then
	info "Rust binary missing at ${RUST_BIN}; building (cargo build -p snapdir-cli)"
	( cd "${REPO_ROOT}" && cargo build -p snapdir-cli ) || die "failed to build the Rust binary"
	RUST=("${RUST_BIN}")
fi

info "workdir: ${WORKDIR}"
info "rust:    ${RUST[*]}"
info "harness: ${HARNESS}"

# --- S3 via MinIO (always runs; needs only docker) -------------------------
start_minio
info "--- delegating S3 differential lanes to ${HARNESS} ---"
if run_delegate_for s3; then
	RAN_BACKENDS+=("s3")
	ok "s3 (MinIO): all Bash<->Rust differential lanes passed byte-identically"
else
	die "s3 (MinIO): differential lanes FAILED — a real interop diff (NOT normalized). Inspect the harness output above; owner: core/stores."
fi

# --- ZERO-EXTERNAL-DEPENDENCY lane (keystone) ------------------------------
zero_dependency_lane

# --- GCS (real, env-gated; emulator can't drive the Rust SDK) --------------
if [[ -z "${SNAPDIR_GCS_TEST_STORE:-}" ]]; then
	skip "gcs: skipped (no SNAPDIR_GCS_TEST_STORE — source ~/.config/snapdir/test-creds.sh for the real-GCS lane; fake-gcs-server cannot drive the Rust SDK, see header)"
	SKIPPED_BACKENDS+=("gcs")
elif ! command -v gcloud >/dev/null; then
	skip "gcs: skipped (gcloud CLI not on PATH; required by the Bash oracle side)"
	SKIPPED_BACKENDS+=("gcs")
else
	info "gcs: preflighting real-GCS ADC against ${SNAPDIR_GCS_TEST_STORE} (Rust SDK uses Application Default Credentials, not the active gcloud account)"
	if store_preflight gcs "${SNAPDIR_GCS_TEST_STORE}"; then
		ok "gcs: ADC preflight OK; delegating GCS differential lanes"
		if run_delegate_for gcs; then
			RAN_BACKENDS+=("gcs")
			ok "gcs (real): all Bash<->Rust differential lanes passed byte-identically"
		else
			die "gcs (real): differential lanes FAILED — a real interop diff (NOT normalized). Owner: core/stores."
		fi
	else
		skip "gcs: skipped — Rust ADC could not reach ${SNAPDIR_GCS_TEST_STORE}. Remediation (operator-env, NOT a code bug): refresh Application Default Credentials for the right account, e.g. 'gcloud auth application-default login' as the bucket owner, then re-source creds. Preflight error: $(tr '\n' ' ' <"${WORKDIR}/gcs-preflight.err" 2>/dev/null | tail -c 300)"
		SKIPPED_BACKENDS+=("gcs")
	fi
fi

# --- B2 (real sandbox, env-gated) ------------------------------------------
if [[ -z "${SNAPDIR_B2_TEST_STORE:-}" ]]; then
	skip "b2: skipped (no SNAPDIR_B2_TEST_STORE — no local emulator serves both B2 native + S3 APIs; set it + B2 creds for the real-sandbox lane)"
	SKIPPED_BACKENDS+=("b2")
elif ! command -v b2 >/dev/null && ! command -v uvx >/dev/null; then
	skip "b2: skipped (neither a b2 CLI nor uvx is on PATH; the Bash oracle side needs a b2 v3 client — install b2 v3.x or uvx so a pinned 'b2<4' can be provisioned)"
	SKIPPED_BACKENDS+=("b2")
else
	# FIX A (endpoint leak): the S3 lane exported SNAPDIR_S3_STORE_ENDPOINT_URL=<MinIO>
	# (see start_minio) and never unset it. The Rust CLI's store_for_adapter reads
	# SNAPDIR_S3_STORE_ENDPOINT_URL as an endpoint override that takes precedence
	# over SNAPDIR_B2_TEST_ENDPOINT (crates/snapdir-cli/src/cli.rs, Adapter::B2), so
	# the B2 preflight PUT would be misrouted to MinIO -> NoSuchBucket -> silent
	# B2 skip. Clear it here, BEFORE the preflight, so the Rust B2 client targets
	# real B2. The S3 + zero-dependency lanes (which legitimately need it pointed at
	# MinIO) already ran above, so unsetting it now is safe.
	unset SNAPDIR_S3_STORE_ENDPOINT_URL 2>/dev/null || true

	# FIX B (b2 CLI version): the frozen oracle uses b2 v3 subcommands; a system
	# b2 v4+ dropped them. Provision a v3 client (system b2 if already v3, else a
	# uvx-pinned 'b2<4' shim) and PREPEND it to PATH for the B2 lane only.
	B2_LANE_BIN=""
	if ! B2_LANE_BIN="$(provision_b2_v3)"; then
		# No system v3 b2 and no uvx to run a pinned 'b2<4' — LOUD-SKIP, never a
		# false pass.
		skip "b2: skipped — no usable b2 v3 client (system b2 is v4+ and uvx is unavailable to run a pinned 'b2<4'). Remediation (operator-env, NOT a code bug): install b2 CLI v3.x, or install uvx (e.g. 'brew install uv') so a pinned 'b2<4' can be provisioned."
		SKIPPED_BACKENDS+=("b2")
	else
		if [[ -n "${B2_LANE_BIN}" ]]; then
			info "b2: using pinned b2 v3 client via shim ${B2_LANE_BIN} (system b2 is v4+; oracle needs v3 subcommands)"
			PATH="${B2_LANE_BIN}:${PATH}"
			export PATH
		else
			info "b2: system b2 already speaks v3 subcommands; using it as-is"
		fi
		# FIX C (AWS credential chain): the Rust B2 store uses aws-sdk-s3 against
		# Backblaze's S3 endpoint and reads the STANDARD AWS credential chain
		# (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY). test-creds set AWS_* to the
		# real-AWS key (AKIA…) for the S3/AWS lane; remap them to the B2 application
		# key for this lane so the Rust client authenticates against B2 (else the
		# preflight HEAD 403s). This is in scope for BOTH store_preflight b2 and the
		# subsequent run_delegate_for b2 subshell (which inherits the parent env).
		# The S3/MinIO + zero-dependency lanes ran earlier and are unaffected.
		export AWS_ACCESS_KEY_ID="${SNAPDIR_B2_STORE_APPLICATION_KEY_ID:?b2 lane needs SNAPDIR_B2_STORE_APPLICATION_KEY_ID}"
		export AWS_SECRET_ACCESS_KEY="${SNAPDIR_B2_STORE_APPLICATION_KEY:?b2 lane needs SNAPDIR_B2_STORE_APPLICATION_KEY}"
		export AWS_REGION="${SNAPDIR_B2_REGION:-us-west-001}"

		info "b2: preflighting real B2 sandbox reachability against ${SNAPDIR_B2_TEST_STORE} (Rust uses the S3-compatible endpoint SNAPDIR_B2_TEST_ENDPOINT=${SNAPDIR_B2_TEST_ENDPOINT:-<unset>})"
		if store_preflight b2 "${SNAPDIR_B2_TEST_STORE}"; then
			ok "b2: reachability preflight OK; delegating B2 differential lanes"
			if run_delegate_for b2; then
				RAN_BACKENDS+=("b2")
				ok "b2 (real sandbox): all Bash<->Rust differential lanes passed byte-identically"
			else
				die "b2 (real sandbox): differential lanes FAILED — a real interop diff (NOT normalized). Owner: core/stores."
			fi
		else
			skip "b2: skipped — Rust could not reach ${SNAPDIR_B2_TEST_STORE} via SNAPDIR_B2_TEST_ENDPOINT=${SNAPDIR_B2_TEST_ENDPOINT:-<unset>}. Remediation (operator-env, NOT a code bug): the B2 application key resolves its own S3 endpoint/region (e.g. 'b2 account authorize' then read .s3endpoint); set SNAPDIR_B2_TEST_ENDPOINT to that region's endpoint so the Rust S3-compatible client and the bucket are in the same region. Preflight error: $(tr '\n' ' ' <"${WORKDIR}/b2-preflight.err" 2>/dev/null | tail -c 300)"
			SKIPPED_BACKENDS+=("b2")
		fi
	fi
fi

# --- No silent passes ------------------------------------------------------
info "backends ran:     ${RAN_BACKENDS[*]:-(none)}"
info "backends skipped: ${SKIPPED_BACKENDS[*]:-(none)}"

if [[ "${#RAN_BACKENDS[@]}" -eq 0 ]]; then
	die "no real backend round-tripped — expected at least s3 (MinIO needs only docker)"
fi

ok "remote-interop LIVE: backends ran [${RAN_BACKENDS[*]}] + zero-external-dependency lane passed"

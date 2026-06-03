#!/usr/bin/env bash
#
# benches/compare.sh
#
# End-to-end Rust-vs-Bash performance comparison for the snapdir
# `manifest` hot path.
#
# It does TWO things, in this order:
#
#   1. CORRECTNESS GATE (hard fail). For every corpus, it runs the frozen Bash
#      oracle (`./snapdir manifest <dir>`) and the Rust binary
#      (`target/release/snapdir manifest <dir>`) and asserts their stdout is
#      BYTE-IDENTICAL. A perf "win" that changes output bytes is a
#      frozen-contract violation -> it prints the diff and `exit 1`. Timings are
#      NEVER reported for divergent output.
#
#   2. TIMING. Only after byte-identity holds, it measures both tools on each
#      corpus and reports the Rust-vs-Bash speedup. It uses `hyperfine` when
#      present; otherwise a portable median-of-N bash wall-clock fallback
#      (N>=5). The absolute "beats baseline + meets target" judgement is the
#      perf-gate human's call -- this harness only produces the real numbers.
#
# A jq-readable JSON report is emitted to stdout and to
# benches/last-compare.json, e.g.:
#
#   {
#     "tool": "hyperfine" | "fallback",
#     "all_output_identical": true,
#     "corpora": {
#       "many_small": { "bash_median_s": .., "rust_median_s": .., "speedup": .., "output_identical": true },
#       "few_large":  { "bash_median_s": .., "rust_median_s": .., "speedup": .., "output_identical": true }
#     }
#   }
#
# Exit 0 only when every corpus produced byte-identical output.
#
# ----------------------------------------------------------------------------
# Oracle notes (load-bearing, mirrored from tests/interop/run.sh):
#
#   - The byte-identity oracle for this gate is the `./snapdir manifest <dir>`
#     WRAPPER (which injects `--cache` + a default `--exclude=system`), compared
#     against the Rust wrapper `target/release/snapdir manifest <dir>`. Both are
#     the user-facing `manifest` command, so this is an apples-to-apples
#     comparison of the same subcommand.
#   - `./snapdir manifest` caches file hashes under a cache dir. We pin that to
#     an isolated temp dir (`--cache-dir`) so we (a) never touch the user's real
#     `$HOME/.cache/snapdir` and (b) keep every measured run in the same warm
#     state. The Rust `manifest` walks in-process and uses no hash cache, so the
#     comparison reflects the realistic `manifest` invocation for each tool.
#
# Modes:
#
#   (no args)       Full run: build release, generate the many-small + few-large
#                   corpora, run the byte-identity gate, then time both tools and
#                   emit the JSON report + human summary.
#
#   --self-check    Fast plumbing self-test (NO hyperfine required, TINY corpus):
#                   asserts the release binary builds (or cargo is present), a
#                   tiny corpus generates, Bash + Rust `manifest` both run on it
#                   and their output is byte-identical, the JSON report shape is
#                   emitted, and the portable timing fallback works on a trivial
#                   command. This is what the `perf-harness` gate runs. Exit 0
#                   when the plumbing is sound.
#
set -euo pipefail

# ---------------------------------------------------------------------------
# Repo root (this file lives at <root>/benches/compare.sh).
# ---------------------------------------------------------------------------
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/.." && pwd)"

ORACLE_SNAPDIR="${REPO_ROOT}/snapdir"
RUST_BIN="${REPO_ROOT}/target/release/snapdir"
REPORT_JSON="${HERE}/last-compare.json"

# Number of samples for the portable timing fallback (median-of-N).
FALLBACK_RUNS="${SNAPDIR_BENCH_RUNS:-7}"

# ---------------------------------------------------------------------------
# Colours / logging (all to stderr; stdout is reserved for the JSON report).
# ---------------------------------------------------------------------------
if [[ -t 2 ]]; then
	C_RED=$'\033[31m'
	C_GRN=$'\033[32m'
	C_YEL=$'\033[33m'
	C_DIM=$'\033[2m'
	C_RST=$'\033[0m'
else
	C_RED='' C_GRN='' C_YEL='' C_DIM='' C_RST=''
fi

log() { printf '%s\n' "$*" >&2; }
info() { log "${C_DIM}[bench]${C_RST} $*"; }
ok() { log "${C_GRN}ok${C_RST} - $*"; }
warn() { log "${C_YEL}warn${C_RST} - $*"; }
fail() { log "${C_RED}FAIL${C_RST} - $*"; }

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

make_workdir() {
	WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/snapdir-compare.XXXXXXXXXX")"
	info "workdir: ${WORKDIR}"
}

# ---------------------------------------------------------------------------
# Deterministic corpora (NO RNG, NO clocks).
#
# many_small: thousands of tiny files across nested dirs -> walk/syscall-bound.
# few_large : a handful of multi-MB files            -> hash-throughput-bound.
#
# Sizes are parameterised so --self-check can use a tiny corpus.
# ---------------------------------------------------------------------------

# gen_many_small <dir> <fanout> <files_per_dir>
# Creates <fanout> nested subdirs, each holding <files_per_dir> tiny files of
# deterministic, index-derived content. Total files ~= fanout * files_per_dir.
gen_many_small() {
	local root="$1" fanout="$2" per_dir="$3"
	local d f content sub
	# Deterministic tiny payload; content varies by index so checksums differ
	# across files (exercising distinct hashes).
	for ((d = 0; d < fanout; d++)); do
		sub="${root}/d$(printf '%03d' "$d")/nested"
		mkdir -p "${sub}"
		for ((f = 0; f < per_dir; f++)); do
			# Vary length slightly by index for distinct content/checksums,
			# fully deterministic (no RNG).
			content="$(printf 'x%.0s' $(seq 1 $(((d + f) % 16 + 1))))"
			printf '%s-%d-%d' "${content}" "$d" "$f" >"${sub}/f$(printf '%04d' "$f").txt"
		done
	done
}

# gen_few_large <dir> <count> <mib>
# Creates <count> files of <mib> MiB of deterministic bytes each.
gen_few_large() {
	local root="$1" count="$2" mib="$3"
	mkdir -p "${root}"
	local i
	for ((i = 0; i < count; i++)); do
		# Deterministic multi-MB payload via /dev/zero (no RNG). A small
		# per-file marker byte keeps file checksums distinct.
		dd if=/dev/zero bs=1048576 count="${mib}" 2>/dev/null \
			>"${root}/large_$(printf '%02d' "$i").bin"
		printf '%d' "$i" >>"${root}/large_$(printf '%02d' "$i").bin"
	done
}

# ---------------------------------------------------------------------------
# Build the release Rust binary (skip if fresh).
# ---------------------------------------------------------------------------
build_release() {
	if [[ "${SNAPDIR_BENCH_SKIP_BUILD:-0}" == "1" && -x "${RUST_BIN}" ]]; then
		info "skipping build (SNAPDIR_BENCH_SKIP_BUILD=1, binary present)"
		return 0
	fi
	info "building release binary (cargo build --release -p snapdir-cli --locked)"
	(cd "${REPO_ROOT}" && cargo build --release -p snapdir-cli --locked) >&2
	if [[ ! -x "${RUST_BIN}" ]]; then
		fail "release binary not found at ${RUST_BIN} after build"
		return 1
	fi
	ok "release binary: ${RUST_BIN}"
}

# ---------------------------------------------------------------------------
# Invocations. Both go through the user-facing `manifest` subcommand. The Bash
# wrapper's hash cache is isolated to ${WORKDIR}/cache so we never touch the
# user's real cache and keep measured runs in a consistent warm state.
# ---------------------------------------------------------------------------
bash_manifest() {
	local dir="$1"
	(cd "${REPO_ROOT}" && "${ORACLE_SNAPDIR}" manifest --cache-dir="${WORKDIR}/cache" "${dir}") </dev/null
}

rust_manifest() {
	local dir="$1"
	(cd "${REPO_ROOT}" && "${RUST_BIN}" manifest "${dir}") </dev/null
}

# ---------------------------------------------------------------------------
# Byte-identity gate. Returns 0 if identical, 1 otherwise (prints the diff).
# ---------------------------------------------------------------------------
assert_identical() {
	local name="$1" dir="$2"
	local bash_out rust_out
	bash_out="${WORKDIR}/${name}.bash.manifest"
	rust_out="${WORKDIR}/${name}.rust.manifest"

	bash_manifest "${dir}" >"${bash_out}"
	rust_manifest "${dir}" >"${rust_out}"

	if cmp -s "${bash_out}" "${rust_out}"; then
		ok "output identical: ${name} ($(wc -l <"${bash_out}" | tr -d ' ') manifest lines)"
		return 0
	fi

	fail "OUTPUT DIVERGED for corpus '${name}' -- frozen-contract violation"
	log "--- diff (bash vs rust), first 40 lines: ---"
	diff "${bash_out}" "${rust_out}" 2>&1 | head -40 >&2 || true
	return 1
}

# ---------------------------------------------------------------------------
# Portable median-of-N timing fallback (no hyperfine needed).
#
# time_median_s <runs> -- <command...>
# Runs <command> <runs> times, captures wall-clock per run via EPOCHREALTIME
# (bash >= 5) or `date +%s.%N`, and prints two space-separated floats to stdout:
#   <median_seconds> <min_seconds>
# Command stdout/stderr are discarded.
# ---------------------------------------------------------------------------
_now_s() {
	# Prefer bash's EPOCHREALTIME (microsecond, no fork); fall back to date.
	if [[ -n "${EPOCHREALTIME:-}" ]]; then
		# EPOCHREALTIME uses the locale decimal point; normalise comma -> dot.
		printf '%s' "${EPOCHREALTIME/,/.}"
	else
		date +%s.%N
	fi
}

time_median_s() {
	local runs="$1"
	shift
	[[ "$1" == "--" ]] && shift
	local -a samples=()
	local i start end dur
	for ((i = 0; i < runs; i++)); do
		start="$(_now_s)"
		"$@" >/dev/null 2>&1 || true
		end="$(_now_s)"
		dur="$(awk -v a="${start}" -v b="${end}" 'BEGIN { printf "%.6f", (b - a) }')"
		samples+=("${dur}")
	done
	# Sort numerically and pick the median + min.
	printf '%s\n' "${samples[@]}" | sort -g | awk '
		{ v[NR] = $1 }
		END {
			n = NR
			min = v[1]
			if (n % 2 == 1) { med = v[(n + 1) / 2] }
			else            { med = (v[n / 2] + v[n / 2 + 1]) / 2 }
			printf "%.6f %.6f\n", med, min
		}'
}

# ---------------------------------------------------------------------------
# Per-corpus timing. Sets globals BASH_MED / RUST_MED (median seconds) and
# SPEEDUP (bash_median / rust_median). Uses hyperfine if present.
# ---------------------------------------------------------------------------
BASH_MED="" RUST_MED="" SPEEDUP="" TIMING_TOOL=""

time_corpus() {
	local dir="$1"

	if command -v hyperfine >/dev/null 2>&1; then
		TIMING_TOOL="hyperfine"
		local hf_json="${WORKDIR}/hf.json"
		# --shell=none would block our subshell `cd`; keep default shell but
		# point the commands at absolute paths. Warmup runs prime the FS/cache.
		hyperfine \
			--warmup 1 \
			--min-runs 5 \
			--export-json "${hf_json}" \
			--command-name bash "${ORACLE_SNAPDIR} manifest --cache-dir=${WORKDIR}/cache ${dir}" \
			--command-name rust "${RUST_BIN} manifest ${dir}" \
			>&2
		BASH_MED="$(jq -r '.results[] | select(.command_name=="bash") | .median' "${hf_json}")"
		RUST_MED="$(jq -r '.results[] | select(.command_name=="rust") | .median' "${hf_json}")"
	else
		TIMING_TOOL="fallback"
		# Warmup once each (prime FS cache / let the wrapper warm its hash cache).
		bash_manifest "${dir}" >/dev/null 2>&1 || true
		rust_manifest "${dir}" >/dev/null 2>&1 || true
		read -r BASH_MED _ < <(time_median_s "${FALLBACK_RUNS}" -- bash_manifest "${dir}")
		read -r RUST_MED _ < <(time_median_s "${FALLBACK_RUNS}" -- rust_manifest "${dir}")
	fi

	SPEEDUP="$(awk -v b="${BASH_MED}" -v r="${RUST_MED}" 'BEGIN {
		if (r <= 0) { printf "0"; } else { printf "%.3f", b / r }
	}')"
}

# ---------------------------------------------------------------------------
# JSON report builder. Accepts the all-identical flag, tool, and per-corpus
# rows passed as: <name> <bash_med> <rust_med> <speedup> <identical(true/false)>
# ---------------------------------------------------------------------------
emit_report() {
	local all_identical="$1" tool="$2"
	shift 2
	local -a corpora_args=("$@")

	# shellcheck disable=SC2016  # single quotes are intentional: this is a jq
	# program, $tool/$all are jq variables bound via --arg, not shell vars.
	local jq_filter='{ tool: $tool, all_output_identical: ($all=="true"), corpora: {} }'
	local -a jq_vars=(--arg tool "${tool}" --arg all "${all_identical}")

	local i=0
	while ((i < ${#corpora_args[@]})); do
		local name="${corpora_args[i]}"
		local bmed="${corpora_args[i + 1]}"
		local rmed="${corpora_args[i + 2]}"
		local spd="${corpora_args[i + 3]}"
		local ident="${corpora_args[i + 4]}"
		jq_vars+=(
			--arg "n_${name}" "${name}"
			--argjson "b_${name}" "${bmed:-null}"
			--argjson "r_${name}" "${rmed:-null}"
			--argjson "s_${name}" "${spd:-null}"
			--argjson "i_${name}" "${ident}"
		)
		jq_filter+=" | .corpora[\$n_${name}] = { bash_median_s: \$b_${name}, rust_median_s: \$r_${name}, speedup: \$s_${name}, output_identical: \$i_${name} }"
		i=$((i + 5))
	done

	jq -n "${jq_vars[@]}" "${jq_filter}" | tee "${REPORT_JSON}"
}

# ===========================================================================
# --self-check : fast plumbing validation. No hyperfine. TINY corpus.
# ===========================================================================
self_check() {
	info "self-check: validating compare.sh plumbing"
	make_workdir

	# 1. cargo present (and build the binary so the rest of the check is real).
	if ! command -v cargo >/dev/null 2>&1; then
		fail "self-check: cargo not found on PATH"
		return 1
	fi
	ok "cargo present: $(command -v cargo)"
	build_release

	# 2. Tiny corpus generates.
	local tiny="${WORKDIR}/tiny"
	gen_many_small "${tiny}/small" 3 4
	gen_few_large "${tiny}/large" 1 1
	if [[ ! -d "${tiny}/small" || ! -d "${tiny}/large" ]]; then
		fail "self-check: tiny corpus did not materialise"
		return 1
	fi
	ok "tiny corpus generated ($(find "${tiny}" -type f | wc -l | tr -d ' ') files)"

	# 3. Bash + Rust manifest both run and their output is byte-identical.
	local ident_small ident_large
	if assert_identical "selfcheck_small" "${tiny}/small"; then ident_small=true; else ident_small=false; fi
	if assert_identical "selfcheck_large" "${tiny}/large"; then ident_large=true; else ident_large=false; fi
	if [[ "${ident_small}" != "true" || "${ident_large}" != "true" ]]; then
		fail "self-check: Bash vs Rust manifest output diverged on the tiny corpus"
		return 1
	fi

	# 4. Timing fallback works on a trivial command.
	local med min
	read -r med min < <(time_median_s 5 -- true)
	if [[ -z "${med}" ]]; then
		fail "self-check: timing fallback produced no measurement"
		return 1
	fi
	ok "timing fallback works (median=${med}s min=${min}s over 5 runs of 'true')"

	# 5. JSON report shape is emitted and is valid + jq-readable.
	local report
	report="$(emit_report "true" "fallback" \
		selfcheck_small "${med}" "${med}" "1.0" "true" \
		selfcheck_large "${med}" "${med}" "1.0" "true")"
	# Validate the shape the perf-gate will json_path against.
	if ! jq -e '.all_output_identical == true
		and .tool == "fallback"
		and (.corpora.selfcheck_small.output_identical == true)
		and (.corpora.selfcheck_small | has("bash_median_s") and has("rust_median_s") and has("speedup"))' \
		<<<"${report}" >/dev/null; then
		fail "self-check: JSON report shape invalid"
		printf '%s\n' "${report}" >&2
		return 1
	fi
	ok "JSON report shape valid and jq-readable"

	info "self-check PASSED"
	return 0
}

# ===========================================================================
# Full run.
# ===========================================================================
full_run() {
	make_workdir
	build_release

	info "generating corpora (deterministic, no RNG)"
	# many_small: ~ 40 dirs * 60 files = 2400 tiny files.
	local many="${WORKDIR}/many_small"
	gen_many_small "${many}" "${SNAPDIR_BENCH_MANY_FANOUT:-40}" "${SNAPDIR_BENCH_MANY_PER_DIR:-60}"
	ok "many_small: $(find "${many}" -type f | wc -l | tr -d ' ') files"

	# few_large: 4 files * 16 MiB.
	local few="${WORKDIR}/few_large"
	gen_few_large "${few}" "${SNAPDIR_BENCH_FEW_COUNT:-4}" "${SNAPDIR_BENCH_FEW_MIB:-16}"
	ok "few_large: $(find "${few}" -type f | wc -l | tr -d ' ') files ($(du -sh "${few}" 2>/dev/null | cut -f1) total)"

	# ---- Correctness gate FIRST (hard fail on drift). -------------------
	info "correctness gate: asserting byte-identical Bash vs Rust output"
	local ident_many ident_few all_identical="true"
	if assert_identical "many_small" "${many}"; then ident_many=true; else ident_many=false; all_identical=false; fi
	if assert_identical "few_large" "${few}"; then ident_few=true; else ident_few=false; all_identical=false; fi

	if [[ "${all_identical}" != "true" ]]; then
		fail "byte-identity gate FAILED -- refusing to report timings for divergent output"
		# Emit a report capturing the failure for the record, then exit 1.
		emit_report "false" "n/a" \
			many_small null null null "${ident_many}" \
			few_large null null null "${ident_few}" >/dev/null || true
		return 1
	fi

	# ---- Timing (only reached when output is identical). ----------------
	info "timing many_small ..."
	time_corpus "${many}"
	local many_bash="${BASH_MED}" many_rust="${RUST_MED}" many_speedup="${SPEEDUP}"
	ok "many_small: bash=${many_bash}s rust=${many_rust}s speedup=${many_speedup}x (${TIMING_TOOL})"

	info "timing few_large ..."
	time_corpus "${few}"
	local few_bash="${BASH_MED}" few_rust="${RUST_MED}" few_speedup="${SPEEDUP}"
	ok "few_large: bash=${few_bash}s rust=${few_rust}s speedup=${few_speedup}x (${TIMING_TOOL})"

	# ---- Report. --------------------------------------------------------
	info "emitting JSON report -> ${REPORT_JSON}"
	emit_report "true" "${TIMING_TOOL}" \
		many_small "${many_bash}" "${many_rust}" "${many_speedup}" "true" \
		few_large "${few_bash}" "${few_rust}" "${few_speedup}" "true"

	# ---- Human summary. -------------------------------------------------
	log ""
	log "================ snapdir manifest: Rust vs Bash ================"
	log "  timing tool      : ${TIMING_TOOL}"
	log "  output identical : ${C_GRN}PASS${C_RST} (byte-identical on all corpora)"
	log "  many_small       : bash ${many_bash}s | rust ${many_rust}s | ${C_GRN}${many_speedup}x${C_RST}"
	log "  few_large        : bash ${few_bash}s | rust ${few_rust}s | ${C_GRN}${few_speedup}x${C_RST}"
	log "==============================================================="
	log "  (perf-gate human judges absolute target + beats-baseline.)"

	return 0
}

# ===========================================================================
# Entrypoint.
# ===========================================================================
main() {
	case "${1:-}" in
	--self-check)
		self_check
		;;
	"")
		full_run
		;;
	-h | --help)
		sed -n '2,70p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
		;;
	*)
		fail "unknown argument: $1 (use --self-check or no args)"
		return 2
		;;
	esac
}

main "$@"

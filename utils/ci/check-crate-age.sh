#!/usr/bin/env bash
#
# check-crate-age.sh — enforce a minimum public age for every crates.io
# dependency pinned in Cargo.lock.
#
# Supply-chain hardening: never adopt a crate version that has been public for
# fewer than MIN_AGE_DAYS days. A freshly-published malicious or compromised
# release is usually caught and yanked within a few days, so refusing anything
# younger than the threshold sharply narrows the window of exposure.
#
# For every [[package]] in Cargo.lock whose source is the crates.io registry,
# this queries the crates.io API for that exact name+version's `created_at`
# timestamp and asserts (now - created_at) >= MIN_AGE_DAYS. Path/git/workspace
# members (no registry source) are skipped.
#
# Exit 0 if every registry dependency is old enough; exit 1 listing every
# offender. Any transient/network/API error is a hard non-zero exit (we never
# silently pass).
#
# Usage:
#   utils/ci/check-crate-age.sh [--lock <path>] [--min-age-days N]
#
# Env:
#   MIN_AGE_DAYS   minimum age in days (default 3; --min-age-days overrides)

set -euo pipefail

PROG="$(basename "$0")"
LOCK_PATH="Cargo.lock"
MIN_AGE_DAYS="${MIN_AGE_DAYS:-3}"
USER_AGENT="snapdir-ci-crate-age-check (https://github.com/bermi/snapdir)"
REGISTRY_SOURCE="registry+https://github.com/rust-lang/crates.io-index"
API_BASE="https://crates.io/api/v1/crates"
SLEEP_BETWEEN="0.3"
MAX_RETRIES=5

usage() {
    cat <<EOF
$PROG — enforce a minimum public age for crates.io dependencies in Cargo.lock.

Every registry dependency in the lockfile must have been published on crates.io
at least MIN_AGE_DAYS days ago, or the script exits 1 and lists the offenders.

Usage:
  $PROG [--lock <path>] [--min-age-days N]
  $PROG --help

Options:
  --lock <path>        Path to Cargo.lock (default: Cargo.lock)
  --min-age-days N     Minimum age in days (default: \$MIN_AGE_DAYS or 3)
  -h, --help           Show this help and exit

Environment:
  MIN_AGE_DAYS         Default minimum age in days (overridden by --min-age-days)

Requires: jq, curl. Exit 0 = all dependencies old enough; exit 1 = offenders
found; other non-zero = a usage or transient API/network error.
EOF
}

die() {
    printf '%s: error: %s\n' "$PROG" "$*" >&2
    exit 2
}

# ---- argument parsing ------------------------------------------------------
while [ "$#" -gt 0 ]; do
    case "$1" in
        -h | --help)
            usage
            exit 0
            ;;
        --lock)
            [ "$#" -ge 2 ] || die "--lock requires a path argument"
            LOCK_PATH="$2"
            shift 2
            ;;
        --lock=*)
            LOCK_PATH="${1#*=}"
            shift
            ;;
        --min-age-days)
            [ "$#" -ge 2 ] || die "--min-age-days requires a value"
            MIN_AGE_DAYS="$2"
            shift 2
            ;;
        --min-age-days=*)
            MIN_AGE_DAYS="${1#*=}"
            shift
            ;;
        *)
            die "unknown argument: $1 (try --help)"
            ;;
    esac
done

case "$MIN_AGE_DAYS" in
    '' | *[!0-9]*) die "--min-age-days must be a non-negative integer (got: $MIN_AGE_DAYS)" ;;
esac

# ---- dependency checks -----------------------------------------------------
command -v curl >/dev/null 2>&1 || die "curl is required but not found in PATH"
command -v jq >/dev/null 2>&1 || die "jq is required but not found in PATH"
[ -f "$LOCK_PATH" ] || die "lockfile not found: $LOCK_PATH"

# ---- parse Cargo.lock ------------------------------------------------------
# Emit "name<TAB>version" only for packages whose source is the crates.io
# registry. We walk the TOML by hand: each [[package]] block has name/version/
# source lines; we only keep a (name,version) pair if a matching registry
# source line appears in the same block.
parse_registry_packages() {
    awk -v reg="$REGISTRY_SOURCE" '
        function flush() {
            if (name != "" && version != "" && is_registry) {
                print name "\t" version
            }
            name = ""; version = ""; is_registry = 0
        }
        /^\[\[package\]\]/ { flush(); next }
        /^\[/ && !/^\[\[package\]\]/ { flush(); next }
        {
            line = $0
            if (line ~ /^name = "/) {
                v = line; sub(/^name = "/, "", v); sub(/".*$/, "", v); name = v
            } else if (line ~ /^version = "/) {
                v = line; sub(/^version = "/, "", v); sub(/".*$/, "", v); version = v
            } else if (line ~ /^source = "/) {
                v = line; sub(/^source = "/, "", v); sub(/".*$/, "", v)
                if (v == reg) is_registry = 1
            }
        }
        END { flush() }
    ' "$LOCK_PATH" | sort -u
}

# ---- crates.io query -------------------------------------------------------
# Fetch the JSON body for a specific crate version, with polite retry/backoff
# on HTTP 429 and transient 5xx. Prints the body on stdout; returns non-zero on
# unrecoverable error.
fetch_version_json() {
    name="$1"
    version="$2"
    url="$API_BASE/$name/$version"
    attempt=0
    while [ "$attempt" -lt "$MAX_RETRIES" ]; do
        attempt=$((attempt + 1))
        body_file="$(mktemp)"
        http_code="$(
            curl --silent --show-error --location \
                --max-time 30 \
                --user-agent "$USER_AGENT" \
                --write-out '%{http_code}' \
                --output "$body_file" \
                "$url" 2>/dev/null || true
        )"
        case "$http_code" in
            200)
                cat "$body_file"
                rm -f "$body_file"
                return 0
                ;;
            429 | 500 | 502 | 503 | 504)
                rm -f "$body_file"
                backoff=$((attempt * 2))
                printf '%s: warning: HTTP %s for %s@%s, retry %d/%d in %ds\n' \
                    "$PROG" "$http_code" "$name" "$version" \
                    "$attempt" "$MAX_RETRIES" "$backoff" >&2
                sleep "$backoff"
                ;;
            *)
                rm -f "$body_file"
                printf '%s: error: HTTP %s querying %s\n' \
                    "$PROG" "${http_code:-000}" "$url" >&2
                return 1
                ;;
        esac
    done
    printf '%s: error: gave up after %d retries for %s@%s\n' \
        "$PROG" "$MAX_RETRIES" "$name" "$version" >&2
    return 1
}

# Portable "epoch seconds for an ISO-8601 UTC timestamp" via jq (avoids GNU vs
# BSD date incompatibilities). crates.io returns e.g. 2024-01-02T03:04:05.678Z.
iso_to_epoch() {
    # Reads an ISO timestamp on stdin, prints epoch seconds.
    jq -rR 'sub("\\.[0-9]+Z$"; "Z") | sub("\\.[0-9]+\\+"; "+") | fromdateiso8601'
}

now_epoch="$(date -u +%s)"
min_age_seconds=$((MIN_AGE_DAYS * 86400))

echo "Checking crate ages against MIN_AGE_DAYS=$MIN_AGE_DAYS (lock: $LOCK_PATH)"
echo

packages="$(parse_registry_packages)"
if [ -z "$packages" ]; then
    die "no crates.io registry packages found in $LOCK_PATH (unexpected)"
fi

total=0
offenders=0
checked=0

# Iterate over (name, version) pairs.
while IFS="$(printf '\t')" read -r name version; do
    [ -n "$name" ] || continue
    total=$((total + 1))

    json="$(fetch_version_json "$name" "$version")" || {
        die "failed to query crates.io for $name@$version (network/API error)"
    }

    created_at="$(printf '%s' "$json" | jq -r '.version.created_at // empty')"
    if [ -z "$created_at" ]; then
        die "could not extract created_at for $name@$version from crates.io response"
    fi

    created_epoch="$(printf '%s' "$created_at" | iso_to_epoch 2>/dev/null || true)"
    case "$created_epoch" in
        '' | *[!0-9-]*) die "could not parse created_at '$created_at' for $name@$version" ;;
    esac

    age_seconds=$((now_epoch - created_epoch))
    age_days=$((age_seconds / 86400))
    checked=$((checked + 1))

    if [ "$age_seconds" -lt "$min_age_seconds" ]; then
        offenders=$((offenders + 1))
        printf 'FAIL  %-30s %-12s age=%dd  (published %s, < %dd)\n' \
            "$name" "$version" "$age_days" "$created_at" "$MIN_AGE_DAYS"
    else
        printf 'PASS  %-30s %-12s age=%dd\n' "$name" "$version" "$age_days"
    fi

    sleep "$SLEEP_BETWEEN"
done <<EOF
$packages
EOF

echo
if [ "$offenders" -gt 0 ]; then
    printf '%s: %d of %d registry crate(s) younger than %d day(s).\n' \
        "$PROG" "$offenders" "$checked" "$MIN_AGE_DAYS" >&2
    exit 1
fi

printf '%s: OK — all %d registry crate(s) are at least %d day(s) old.\n' \
    "$PROG" "$checked" "$MIN_AGE_DAYS"
exit 0

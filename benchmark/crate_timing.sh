#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Per-crate CI cost attribution: how long each crate's tests and benchmarks take to
# EXECUTE. Used by .github/workflows/crate-timing.yml. Measures only, never gates:
# failures are recorded in the `failed` column and the run continues.
#
# Everything is built upfront, untimed, so timed runs never compile. Two exceptions:
# doc tests compile inside `cargo test --doc`, and the bin_tests row includes its
# nextest `prebuild-bin-tests` setup script -- both match what test.yml pays too.
# The timed invocations mirror test.yml's test shapes and run_benchmarks_ci.sh's bench
# settings, so the numbers attribute the real CI jobs.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" &>/dev/null && pwd 2>/dev/null)"
readonly SCRIPT_DIR
PROJECT_DIR="${SCRIPT_DIR%/benchmark}"
readonly PROJECT_DIR

# shellcheck disable=1091
source "${SCRIPT_DIR}/bench_features.sh"

RESULTS_DIR="${RESULTS_DIR:-${PROJECT_DIR}/target/crate-timing}"
readonly RESULTS_DIR
RESULTS_TSV="${RESULTS_DIR}/results.tsv"
LOGS_DIR="${RESULTS_DIR}/logs"

# Same crates and flags as .github/workflows/test.yml.
TRACING_CRATES="libdd-data-pipeline libdd-trace-utils libdd-tracer-flare"
CRASHTRACKER_FEATURE="libdd-crashtracker/generate-unit-test-files"
# builder has no tests of interest in test.yml either; test_spawn_from_lib only runs
# there under RUSTFLAGS="-C prefer-dynamic", a shape this script never builds.
SKIPPED_CRATES="builder test_spawn_from_lib"

# Must stay identical to benchmark/run_benchmarks_ci.sh's criterion args, so the
# bench column is the real benchmarks job's per-crate cost.
CRITERION_ARGS=(--warm-up-time 1 --measurement-time 5 --sample-size=200)

cd "${PROJECT_DIR}"

metadata="$(cargo metadata --no-deps --format-version 1)"

is_skipped() {
  case " ${SKIPPED_CRATES} " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}
is_tracing_crate() {
  case " ${TRACING_CRATES} " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}

# All workspace members except the skipped crates.
CRATES="$(
  jq -r '.packages[].name' <<< "${metadata}" \
    | { while read -r c; do is_skipped "$c" || echo "$c"; done; } \
    | sort | tr '\n' ' '
)"
# Same bench-target discovery as benchmark/run_benchmarks_ci.sh.
BENCH_CRATES="$(
  jq -r '.packages[] | select(any(.targets[]?; .kind[]? == "bench")) | .name' \
    <<< "${metadata}" | sort | tr '\n' ' '
)"

# Optional scope (space-separated crate names), mainly for local runs; empty = all.
SELECTED="${CRATE_TIMING_PACKAGES:-}"
if [[ -n "${SELECTED}" ]]; then
  CRATES="$(for c in ${CRATES}; do
    case " ${SELECTED} " in *" $c "*) echo "$c" ;; esac
  done | sort | tr '\n' ' ')"
  BENCH_CRATES="$(for c in ${BENCH_CRATES}; do
    case " ${SELECTED} " in *" $c "*) echo "$c" ;; esac
  done | sort | tr '\n' ' ')"
  echo "Scoped to:${CRATES}${BENCH_CRATES:+ (benches:${BENCH_CRATES})}"
fi

mkdir -p "${LOGS_DIR}"
: > "${RESULTS_TSV}"

record() { # crate phase seconds status
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" >> "${RESULTS_TSV}"
}

timed() { # timed <phase> <crate> <cmd...>
  local phase=$1 crate=$2
  shift 2
  local log="${LOGS_DIR}/${crate}-${phase}.log" start status seconds
  start=$(date +%s)
  if "$@" > "${log}" 2>&1; then
    status=ok
  else
    status=fail
  fi
  seconds=$(( $(date +%s) - start ))
  record "${crate}" "${phase}" "${seconds}" "${status}"
  printf '%-9s %-38s %6ds %s\n' "${phase}" "${crate}" "${seconds}" "${status}"
}

# --- Phase 1: untimed builds. sccache stays active so an unexpected fingerprint
# change is a cache hit, not a rebuild inside a timed window.

echo "== Building test binaries (untimed)"
for crate in ${CRATES}; do
  args=(nextest run -p "${crate}" --profile ci --no-run)
  if [[ "${crate}" == "libdd-crashtracker" ]]; then
    # cargo rejects --features for a crate outside the selection
    args+=(--features "${CRASHTRACKER_FEATURE}")
  fi
  cargo "${args[@]}" > "${LOGS_DIR}/${crate}-build.log" 2>&1
done

echo "== Building bench binaries (untimed)"
for crate in ${BENCH_CRATES}; do
  args=(bench -p "${crate}" --no-run)
  features="$(bench_features_for_crate "${crate}")"
  if [[ -n "${features}" ]]; then
    args+=(--features "${features// /,}")
  fi
  cargo "${args[@]}" > "${LOGS_DIR}/${crate}-bench-build.log" 2>&1
done

# --- Phase 2: timed runs.

echo "== Timing tests"
for crate in ${CRATES}; do
  args=(nextest run -p "${crate}" --profile ci --no-tests=pass
    -E '!test(tracing_integration_tests::)')
  if [[ "${crate}" == "libdd-crashtracker" ]]; then
    args+=(--features "${CRASHTRACKER_FEATURE}")
  fi
  timed tests "${crate}" cargo "${args[@]}"

  if is_tracing_crate "${crate}"; then
    timed tracing "${crate}" \
      cargo nextest run -p "${crate}" --profile ci --test-threads=1 --no-tests=pass \
      -E 'test(tracing_integration_tests::)'
  fi

  timed doc "${crate}" cargo test -p "${crate}" --doc --quiet
done

echo "== Timing benchmarks"
for crate in ${BENCH_CRATES}; do
  args=(bench -p "${crate}")
  features="$(bench_features_for_crate "${crate}")"
  if [[ -n "${features}" ]]; then
    args+=(--features "${features// /,}")
  fi
  args+=(-- "${CRITERION_ARGS[@]}")
  timed bench "${crate}" cargo "${args[@]}"
done

# --- Phase 3: report.

echo "== Report"

jq -Rrs '
  split("\n") | map(select(length > 0) | split("\t"))
  | map({crate: .[0], phase: .[1], seconds: (.[2] | tonumber), status: .[3]})
  | group_by(.crate)
  | map(reduce .[] as $r (
      {crate: (.[0].crate), tests: 0, tracing: 0, doc: 0, bench: 0, failed: []};
      .[$r.phase] = $r.seconds
      | (if $r.status == "fail" then .failed += [$r.phase] else . end)
    ))
  | map(. + {total: (.tests + .tracing + .doc + .bench)})
  | sort_by(-.total)
' "${RESULTS_TSV}" > "${RESULTS_DIR}/results.json"

{
  echo "# Per-crate test & benchmark execution time"
  echo
  echo "_Measured $(date -u +"%Y-%m-%d %H:%M UTC") by benchmark/crate_timing.sh (weekly crate-timing"
  echo "workflow, or a local run). Covers"
  echo "test.yml's main nextest pass (+ doc tests, + the serial tracing-integration pass) and"
  echo "the GitLab benchmarks job's criterion settings. Other shapes (all-features,"
  echo "no-default-features, fips, centos7, other platforms) are not attributed here._"
  echo
  echo "| crate | tests | tracing | doc | bench | total | failed |"
  echo "|---|---:|---:|---:|---:|---:|---|"
  jq -r '.[] | "| \(.crate) | \(.tests)s | \(.tracing)s | \(.doc)s | \(.bench)s | \(.total)s | \(if (.failed | length) > 0 then (.failed | join(",")) else "" end) |"' \
    "${RESULTS_DIR}/results.json"
  jq -r '"| **total** | **\(map(.tests) | add)s** | **\(map(.tracing) | add)s** | **\(map(.doc) | add)s** | **\(map(.bench) | add)s** | **\(map(.total) | add)s** | |"' \
    "${RESULTS_DIR}/results.json"
} > "${RESULTS_DIR}/results.md"

cat "${RESULTS_DIR}/results.md"

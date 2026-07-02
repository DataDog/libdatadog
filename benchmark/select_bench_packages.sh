#!/usr/bin/env bash

# Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Decides which benchmark crates THIS shard should run, and prints exactly one of:
#   SKIP        - this shard has nothing to run (the job should exit 0)
#   FULL        - run the whole workspace (run_benchmarks_ci.sh uses --workspace)
#   <crate ...> - space-separated crates this shard should benchmark
#
# Inputs (from the compute_impacted_crates job, consumed via the benchmarks job env):
#   $CI_COMMIT_BRANCH, $IMPACTED_STATUS, $AFFECTED_CRATES (JSON array), and
#   ./changed_files.txt in the current directory. Crates with benchmarks are discovered
#   from `cargo metadata`.
#
# Usage: select_bench_packages.sh <node_index_1based> <node_total>
#
# Only diagnostics go to stderr; stdout is solely the decision token above.
set -eu

node_index="${1:-1}"
node_total="${2:-1}"

# Known-heavy crates. They are distributed across shards first so the expensive
# benchmarks are spread evenly, then the remaining crates fill in round-robin.
# Tune this list from measured per-crate benchmark durations as the suite changes.
SLOW_CRATES="libdd-trace-obfuscation libdd-sampling libdd-trace-utils libdd-data-pipeline"

log() { echo "$@" >&2; }

# All workspace crates that declare a benchmark target, as a sorted JSON array.
bench_json="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | jq -c '[.packages[] | select(any(.targets[]?; .kind[]? == "bench")) | .name] | sort' 2>/dev/null \
  || echo "")"
all_bench="$(printf '%s' "${bench_json:-[]}" | jq -r '.[]?' 2>/dev/null | tr '\n' ' ')"

# Determine the full set of crates to benchmark (before sharding).
packages=""
if [ "${CI_COMMIT_BRANCH:-}" = "main" ]; then
  log "main -> full benchmark suite."
  packages="$all_bench"
elif [ "${IMPACTED_STATUS:-}" != "success" ] || [ -z "${AFFECTED_CRATES:-}" ] || [ -z "$all_bench" ]; then
  log "Impacted crates undetermined -> full benchmark suite."
  packages="$all_bench"
# TEMP (DO NOT MERGE): infra-change guard disabled so this branch scopes to the impacted crates
# (for testing) even though it modifies the benchmark infra itself. Restore before merging.
# elif grep -qE '^(benchmark/|\.gitlab/benchmarks\.yml|\.gitlab/impacted-crates\.yml)' changed_files.txt 2>/dev/null; then
#   log "Benchmark infrastructure changed -> full benchmark suite."
#   packages="$all_bench"
else
  packages="$(jq -nr --argjson a "$AFFECTED_CRATES" --argjson b "$bench_json" \
    '($a - ($a - $b)) | .[]' 2>/dev/null | LC_ALL=C sort | tr '\n' ' ')"
  if [ -z "$(printf '%s' "$packages" | tr -d '[:space:]')" ]; then
    echo "SKIP"
    exit 0
  fi
  log "Impacted benchmarked crates: $packages"
fi

# If the crate list could not be determined, fall back to a single full-workspace run
# on shard 1 only (so we don't run the whole workspace on every shard).
if [ -z "$(printf '%s' "$packages" | tr -d '[:space:]')" ]; then
  if [ "$node_index" = "1" ]; then echo "FULL"; else echo "SKIP"; fi
  exit 0
fi

# Shard assignment: spread the slow crates first, then the rest, round-robin across shards.
is_slow() {
  local candidate=$1 slow
  for slow in $SLOW_CRATES; do
    [ "$candidate" = "$slow" ] && return 0
  done
  return 1
}

slow_present=""
rest=""
for crate in $packages; do
  if is_slow "$crate"; then
    slow_present="$slow_present $crate"
  else
    rest="$rest $crate"
  fi
done
ordered="$slow_present $rest"

out=""
position=0
for crate in $ordered; do
  if [ "$(( position % node_total ))" -eq "$(( node_index - 1 ))" ]; then
    out="$out $crate"
  fi
  position=$(( position + 1 ))
done
out="$(printf '%s' "$out" | sed 's/^ *//;s/ *$//')"

if [ -z "$out" ]; then
  echo "SKIP"
else
  echo "$out"
fi
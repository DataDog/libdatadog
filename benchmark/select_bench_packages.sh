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

# Approximate per-crate benchmark cost (~minutes of candidate wall-time) used to balance the shards.
# Measured 2026-07-02 from a full run (all 11 benchmarked crates); only relative magnitudes matter.
# The seven crates that fall through to the default are all <1 min (normalization ~0.7, profiling
# ~0.6, ffe ~0.4, ipc ~0.2, trace-stats ~0.1, crashtracker ~0.1, trace-obfuscation ~1.4), so they
# act as interchangeable filler. Retune as benchmarks are added/removed; unknown crates default to 1.
crate_weight() {
  case "$1" in
    libdd-trace-utils) echo 9 ;;
    libdd-sampling) echo 6 ;;
    libdd-ddsketch) echo 2 ;;
    libdd-data-pipeline) echo 2 ;;
    *) echo 1 ;;
  esac
}

log() { echo "$@" >&2; }

# All workspace crates that declare a benchmark target, as a sorted JSON array.
bench_json="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | jq -c '[.packages[] | select(any(.targets[]?; .kind[]? == "bench")) | .name] | sort' 2>/dev/null \
  || echo "")"
all_bench="$(printf '%s' "${bench_json:-[]}" | jq -r '.[]?' 2>/dev/null | tr '\n' ' ')"

# Determine the full set of crates to benchmark (before sharding).
packages=""
case "${CI_COMMIT_BRANCH:-}" in
  main)
    # main always runs the full suite. (release/hotfix and merge-queue never reach this script --
    # benchmarks.yml skips them entirely.)
    log "main -> full benchmark suite."
    packages="$all_bench"
    ;;
  *)
    if [ "${IMPACTED_STATUS:-}" != "success" ] || [ -z "${AFFECTED_CRATES:-}" ] || [ -z "$all_bench" ]; then
      log "Impacted crates undetermined -> full benchmark suite."
      packages="$all_bench"
    elif grep -qE '^(benchmark/|\.gitlab/benchmarks\.yml|\.gitlab/impacted-crates\.yml)' changed_files.txt 2>/dev/null; then
      log "Benchmark infrastructure changed -> full benchmark suite."
      packages="$all_bench"
    elif grep -qE '^(Cargo\.lock|Cargo\.toml|rust-toolchain\.toml|nightly-toolchain\.toml)$' changed_files.txt 2>/dev/null; then
      # Workspace-level dependency or toolchain changes can affect the performance of every crate,
      # even though crates-reporter maps them to no member crate.
      log "Workspace-level dependency/toolchain change -> full benchmark suite."
      packages="$all_bench"
    else
      packages="$(jq -nr --argjson a "$AFFECTED_CRATES" --argjson b "$bench_json" \
        '($a - ($a - $b)) | .[]' 2>/dev/null | tr '\n' ' ')"
      if [ -z "$(printf '%s' "$packages" | tr -d '[:space:]')" ]; then
        echo "SKIP"
        exit 0
      fi
      log "Impacted benchmarked crates: $packages"
    fi
    ;;
esac

# If the crate list could not be determined, fall back to a single full-workspace run
# on shard 1 only (so we don't run the whole workspace on every shard).
if [ -z "$(printf '%s' "$packages" | tr -d '[:space:]')" ]; then
  if [ "$node_index" = "1" ]; then echo "FULL"; else echo "SKIP"; fi
  exit 0
fi

# Shard assignment via longest-processing-time: process crates heaviest-first and give each to
# the currently-lightest shard. Deterministic (stable sort by weight desc, then name asc), so every
# shard computes the same assignment and just reads its own bucket.
weighted="$(for crate in $packages; do echo "$(crate_weight "$crate") $crate"; done | LC_ALL=C sort -k1,1nr -k2,2)"

idx=0
while [ "$idx" -lt "$node_total" ]; do
  loads[$idx]=0
  assigned[$idx]=""
  idx=$(( idx + 1 ))
done

while read -r weight crate; do
  [ -z "$crate" ] && continue
  min_idx=0
  min_load="${loads[0]}"
  j=1
  while [ "$j" -lt "$node_total" ]; do
    if [ "${loads[$j]}" -lt "$min_load" ]; then
      min_load="${loads[$j]}"
      min_idx="$j"
    fi
    j=$(( j + 1 ))
  done
  loads[$min_idx]=$(( min_load + weight ))
  assigned[$min_idx]="${assigned[$min_idx]} $crate"
done <<EOF
$weighted
EOF

out="$(printf '%s' "${assigned[$(( node_index - 1 ))]}" | sed 's/^ *//;s/ *$//')"

if [ -z "$out" ]; then
  echo "SKIP"
else
  echo "$out"
fi

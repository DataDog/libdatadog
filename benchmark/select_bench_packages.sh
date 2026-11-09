#!/usr/bin/env bash

# Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Usage: select_bench_packages.sh <node_index_1based> <node_total>
# Print this shard's decision: SKIP, FULL, a space-separated crate list, or a
# JSON array of {crate, target, features, ids} specs for BENCH_SELECTION.
# Diagnostics go to stderr. Missing/untrusted selection falls back to crates,
# then the full suite. A valid empty selection skips the run.
# Inputs: CI_COMMIT_BRANCH, IMPACTED_STATUS, AFFECTED_CRATES (JSON array),
# changed_files.txt and bench-selection.json from the selection jobs.
set -eu

node_index="${1:-1}"
node_total="${2:-1}"

BENCH_SELECTION_FILE="bench-selection.json"

# Approximate per-crate benchmark cost (~30 s of criterion time per pass) used
# to balance the shards in the crate-level fallback.
#
# Retune as benchmarks are added/removed; unknown crates default to 1.
#
# Latest re-evaluation of weights: 2026-10-13
crate_weight() {
  case "$1" in
    libdd-trace-utils) echo 15 ;;
    libdd-profiling-heap-allocator) echo 7 ;;
    libdd-sampling) echo 6 ;;
    libdd-ddsketch) echo 5 ;;
    libdd-data-pipeline) echo 5 ;;
    libdd-ffe-test-suite) echo 4 ;;
    libdd-crashtracker) echo 4 ;;
    libdd-profiling) echo 2 ;;
    libdd-trace-obfuscation) echo 3 ;;
    *) echo 1 ;;
  esac
}

log() { echo "$@" >&2; }

# Changed selection tooling cannot be trusted to narrow its own validation.
bench_tooling_changed() {
  # Explicit escape hatch for PRs validating the selector's narrowing behavior.
  if [ "${BENCH_SELECTOR_SELF_TEST:-}" = "1" ]; then
    return 1
  fi
  grep -qE '^(benchmark/|\.gitlab/benchmarks\.yml|\.gitlab/impacted-crates\.yml|\.github/actions/bench-selector/|\.github/actions/crates-reporter/|\.github/actions/ci-shared/|\.github/actions/(Cargo\.toml|Cargo\.lock))' changed_files.txt 2>/dev/null
}

# Toolchain changes affect every crate. Root manifests/lockfiles instead use
# dependency scoping when a selection exists, with a full-suite fallback below.
workspace_level_changed() {
  grep -qE '^(rust-toolchain\.toml|ci-toolchain\.toml)$' changed_files.txt 2>/dev/null
}

root_manifest_changed() {
  grep -qE '^Cargo\.toml$' changed_files.txt 2>/dev/null
}

lockfile_changed() {
  grep -qE '^Cargo\.lock$' changed_files.txt 2>/dev/null
}

# The runner intersects these IDs with each checkout's --list (baseline may differ).
bench_selection_specs() {
  [ -s "$BENCH_SELECTION_FILE" ] || return 1
  jq -c '[.binaries[] | {crate: .crate_name, target: .target, features: .features, ids: .bench_ids}]' \
    "$BENCH_SELECTION_FILE" 2>/dev/null
}

# Deterministic longest-processing-time assignment: read sorted "weight key"
# lines, placing each item on the lightest shard. Shared by both selection modes.
lpt_assign() {
  local idx=0 j min_idx min_load weight key
  while [ "$idx" -lt "$node_total" ]; do
    loads[$idx]=0
    assigned[$idx]=""
    idx=$(( idx + 1 ))
  done
  while read -r weight key; do
    [ -z "$key" ] && continue
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
    assigned[$min_idx]="${assigned[$min_idx]} ${key}"
  done
}

# Emit this shard's bench-level specs as a JSON array (or SKIP).
print_bench_level_decision() {
  local specs="$1"
  # Each ID costs roughly one fixed-duration criterion pass.
  local weighted
  weighted="$(jq -r '.[] | "\(.ids | length) \(.crate):\(.target)"' <<< "$specs" | LC_ALL=C sort -k1,1nr -k2,2)"

  lpt_assign <<< "$weighted"

  local shard_keys="${assigned[$(( node_index - 1 ))]}"
  if [ -z "${shard_keys// /}" ]; then
    echo "SKIP"
    return 0
  fi

  local out="[]" key
  for key in $shard_keys; do
    out="$(jq -cn --arg c "${key%%:*}" --arg t "${key#*:}" \
      --argjson acc "$out" --argjson all "$specs" \
      '$acc + [$all[] | select(.crate == $c and .target == $t)]')"
  done
  echo "$out"
}

# All workspace crates that declare a benchmark target, as a sorted JSON array.
bench_json="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | jq -c '[.packages[] | select(any(.targets[]?; .kind[]? == "bench")) | .name] | sort' 2>/dev/null \
  || echo "")"
all_bench="$(printf '%s' "${bench_json:-[]}" | jq -r '.[]?' 2>/dev/null | tr '\n' ' ')"

# The full set to benchmark (before sharding) in the crate-level fallback.
packages=""

case "${CI_COMMIT_BRANCH:-}" in
  main)
    # main always runs the full suite. (release/hotfix and merge-queue never reach this script --
    # benchmarks.yml skips them entirely.) Like every "full suite" case below, the suite is
    # sharded across the nodes, not duplicated on each one.
    log "main -> full benchmark suite."
    packages="$all_bench"
    ;;
esac

# Empty string: unavailable; []: valid selection with nothing to run.
specs=""
full_suite="false"
if [ -z "$packages" ] && [ "${IMPACTED_STATUS:-}" = "success" ] && [ -n "${AFFECTED_CRATES:-}" ] && [ -n "$all_bench" ]; then
  specs="$(bench_selection_specs || true)"
  if [ -n "$specs" ]; then
    if [ "$specs" != "[]" ]; then
      log "Bench-level selection available."
    fi
    full_suite="$(jq -r '.full_suite // false' "$BENCH_SELECTION_FILE" 2>/dev/null || echo false)"
  fi
fi

if [ -z "$packages" ]; then
  # Full-suite guards take precedence over either selection mode.
  force_full=""
  if [ -z "$specs" ] && { [ "${IMPACTED_STATUS:-}" != "success" ] || [ -z "${AFFECTED_CRATES:-}" ] || [ -z "$all_bench" ]; }; then
    force_full="Impacted crates undetermined"
  elif bench_tooling_changed; then
    force_full="Benchmark tooling changed"
  elif workspace_level_changed; then
    force_full="Workspace-level dependency/toolchain change"
  elif [ -n "$specs" ] && [ "$full_suite" = "true" ]; then
    force_full="Selection requires the full suite (unscopable root manifest change)"
  elif [ -z "$specs" ] && lockfile_changed; then
    force_full="Cargo.lock changed without bench-level selection"
  elif [ -z "$specs" ] && root_manifest_changed; then
    force_full="Root Cargo.toml changed without bench-level selection"
  fi

  if [ -n "$force_full" ]; then
    log "$force_full -> full benchmark suite."
    packages="$all_bench"
  elif [ -n "$specs" ]; then
    if [ "$specs" = "[]" ]; then
      log "Bench-level selection selected no benchmarks for this PR."
      echo "SKIP"
    else
      print_bench_level_decision "$specs"
    fi
    exit 0
  else
    # Crate-level fallback: affected crates ∩ crates with benchmarks.
    packages="$(jq -nr --argjson a "$AFFECTED_CRATES" --argjson b "$bench_json" \
      '($a - ($a - $b)) | .[]' 2>/dev/null | tr '\n' ' ')"
    if [ -z "$(printf '%s' "$packages" | tr -d '[:space:]')" ]; then
      echo "SKIP"
      exit 0
    fi
    log "Impacted benchmarked crates: $packages"
  fi
fi

weighted="$(for crate in $packages; do echo "$(crate_weight "$crate") $crate"; done | LC_ALL=C sort -k1,1nr -k2,2)"

lpt_assign <<< "$weighted"

out="$(printf '%s' "${assigned[$(( node_index - 1 ))]}" | sed 's/^ *//;s/ *$//')"

if [ -z "$out" ]; then
  echo "SKIP"
else
  echo "$out"
fi

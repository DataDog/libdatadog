#!/usr/bin/env bash

# Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Decides which benchmarks THIS shard should run, and prints exactly one of:
#   SKIP        - this shard has nothing to run (the job should exit 0)
#   FULL        - run the whole workspace (run_benchmarks_ci.sh uses --workspace)
#   <crate ...> - space-separated crates this shard should benchmark (the
#                 crate-level fallback, used when bench-level selection is
#                 unavailable or undetermined)
#   <json>      - bench-level selection: this shard's share of the benchmark
#                 binaries selected by bench-selector, as a JSON array of
#                 specs {"crate", "target", "features": [...], "ids": [...]}
#                 (consumed via $BENCH_SELECTION by run_benchmarks_ci.sh).
#
# Bench-level selection applies when the compute_bench_selection job produced
# a valid bench-selection.json (see .gitlab/impacted-crates.yml and
# .github/actions/bench-selector). Every unexpected condition — missing file,
# malformed JSON, undetermined impacted crates, benchmark-tooling or
# workspace-level changes — falls back to the crate-level logic below, which
# itself falls back to the full suite. The decision can only widen what runs.
#
# Inputs (from the compute_impacted_crates / compute_bench_selection jobs,
# consumed via the benchmarks job env):
#   $CI_COMMIT_BRANCH, $IMPACTED_STATUS, $AFFECTED_CRATES (JSON array), and
#   ./changed_files.txt and ./bench-selection.json in the current directory.
#   Crates with benchmarks are discovered from `cargo metadata`.
#
# Usage: select_bench_packages.sh <node_index_1based> <node_total>
#
# Only diagnostics go to stderr; stdout is solely the decision token above.
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

# True when benchmark selection tooling itself changed: its behavior cannot be
# trusted for this PR, so run everything.
bench_tooling_changed() {
  # BENCH_SELECTOR_SELF_TEST=1 skips this guard: the validation branches'
  # diffs necessarily contain the tooling changes they are validating (the
  # same escape hatch exists in bench-selector's select). Only a deliberate
  # CI configuration change can set it, and editing that configuration is
  # itself a tooling change that re-enables this guard on real PRs.
  if [ "${BENCH_SELECTOR_SELF_TEST:-}" = "1" ]; then
    return 1
  fi
  grep -qE '^(benchmark/|\.gitlab/benchmarks\.yml|\.gitlab/impacted-crates\.yml|\.github/actions/bench-selector/|\.github/actions/ci-shared/)' changed_files.txt 2>/dev/null
}

# Workspace-level dependency or toolchain changes can affect the performance of
# every crate, even though crates-reporter maps them to no member crate.
# Cargo.lock is NOT here: a lockfile bump changes exactly the packages named in
# its diff, which bench-selector scopes against the packages each benchmark
# cluster executes (Reason: dependency-changed). When no bench-level selection
# is available, lockfile_changed() below restores the old full-suite fallback.
workspace_level_changed() {
  grep -qE '^(Cargo\.toml|rust-toolchain\.toml|ci-toolchain\.toml)$' changed_files.txt 2>/dev/null
}

# True when the lockfile changed. Only consulted when no bench-level
# selection is available (selection job failed or did not run): with a
# selection, bench-selector scopes the bump to the changed packages.
lockfile_changed() {
  grep -qE '^Cargo\.lock$' changed_files.txt 2>/dev/null
}

# Print the selection's binaries as shardable specs, or nothing when the file
# is absent or malformed. Every spec carries the exact benchmark IDs
# (run_benchmarks_ci.sh intersects them with what the checkout it runs in
# actually has, so the baseline can predate new benches).
bench_selection_specs() {
  [ -s "$BENCH_SELECTION_FILE" ] || return 1
  jq -c '[.binaries[] | {crate: .crate_name, target: .target, features: .features, ids: .bench_ids}]' \
    "$BENCH_SELECTION_FILE" 2>/dev/null
}

# Longest-processing-time shard assignment: heaviest items first, each to the
# currently-lightest shard. Deterministic, so every shard computes the same
# assignment and just reads its own bucket. The load arrays are shared by the
# bench-level (weight = selected benchmark IDs) and crate-level (weight =
# crate_weight) callers, which pass "weight key" lines on stdin.
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
  # Weight = number of selected benchmark IDs: each costs one criterion pass,
  # which the fixed criterion_args make roughly uniform. A better proxy than
  # the hand-tuned crate weights below, and it needs no maintenance.
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

# Bench-level selection is only trustworthy when impacted crates were
# determined; a valid selection file also implies the producing job succeeded.
# Tri-state: "absent" (fall back to crate-level), "empty" (valid selection
# that selected nothing — the whole point of per-benchmark selection, so the
# run must skip, not widen), or "specs" (bench-level).
# Only decided here when a full suite was not already forced (main).
selection_state="absent"
specs=""
if [ -z "$packages" ] && [ "${IMPACTED_STATUS:-}" = "success" ] && [ -n "${AFFECTED_CRATES:-}" ] && [ -n "$all_bench" ]; then
  specs="$(bench_selection_specs || true)"
  if [ -n "$specs" ]; then
    if [ "$specs" = "[]" ]; then
      selection_state="empty"
    else
      selection_state="specs"
      log "Bench-level selection available."
    fi
  fi
fi

if [ -z "$packages" ]; then
  # Full-suite guards, in the original precedence order. They apply in both
  # modes: when the selection tooling itself or the workspace-level build
  # inputs changed, per-benchmark selection cannot be trusted for this PR.
  force_full=""
  if [ -z "$specs" ] && { [ "${IMPACTED_STATUS:-}" != "success" ] || [ -z "${AFFECTED_CRATES:-}" ] || [ -z "$all_bench" ]; }; then
    force_full="Impacted crates undetermined"
  elif bench_tooling_changed; then
    force_full="Benchmark tooling changed"
  elif workspace_level_changed; then
    force_full="Workspace-level dependency/toolchain change"
  elif [ "$selection_state" = "absent" ] && lockfile_changed; then
    # No bench-level selection to scope the bump with: old behavior. (An
    # "empty" selection means bench-selector scoped the bump and no cluster
    # exercises the changed packages — trust it and run nothing.)
    force_full="Cargo.lock changed without bench-level selection"
  fi

  if [ -n "$force_full" ]; then
    log "$force_full -> full benchmark suite."
    packages="$all_bench"
  elif [ "$selection_state" = "specs" ]; then
    # Bench-level: the selection already accounts for the affected crates.
    print_bench_level_decision "$specs"
    exit 0
  elif [ "$selection_state" = "empty" ]; then
    log "Bench-level selection selected no benchmarks for this PR."
    echo "SKIP"
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

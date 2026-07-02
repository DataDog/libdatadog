#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -eu

function message() {
  local message=$1 verbose=${2:-"true"}
  if [[ "${verbose}" == "true" ]]; then
    echo "$(date +"%T%:z"): $message"
  fi
}

CURRENT_PATH=$(pwd)
readonly CURRENT_PATH="${CURRENT_PATH%/}"
readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" &>/dev/null && pwd 2>/dev/null)"
readonly PROJECT_DIR="${SCRIPT_DIR}/.."
OUTPUT_DIR="${1:-}"

pushd "${PROJECT_DIR}" > /dev/null

# Some bench targets need crate-specific features enabled; when scoping the run to a subset of
# crates we must pass only the features for the selected crates (cargo errors on --features for a
# crate that isn't part of the selection).
bench_features_for_crate() {
  case "$1" in
    libdd-crashtracker) echo "libdd-crashtracker/benchmarking" ;;
    libdd-sampling) echo "libdd-sampling/v04_span libdd-sampling/bench-internals" ;;
    libdd-trace-utils) echo "libdd-trace-utils/bench-internals" ;;
    *) echo "" ;;
  esac
}

# Run benchmarks
message "Running benchmarks"
# BENCH_PACKAGES (optional, space-separated crate names) scopes the run to specific crates -- set by
# the GitLab benchmarks job so a PR only benchmarks the crates it impacts. When empty (e.g. on main)
# the full workspace is benchmarked.
if [[ -n "${BENCH_PACKAGES:-}" ]]; then
  package_args=()
  features=()
  for crate in ${BENCH_PACKAGES}; do
    package_args+=(-p "${crate}")
    for feature in $(bench_features_for_crate "${crate}"); do
      features+=("${feature}")
    done
  done
  feature_args=()
  if (( ${#features[@]} > 0 )); then
    feature_args=(--features "$(IFS=,; echo "${features[*]}")")
  fi
  message "Benchmarking selected crates: ${BENCH_PACKAGES}"
  cargo bench "${package_args[@]}" "${feature_args[@]}" -- --warm-up-time 1 --measurement-time 5 --sample-size=200
else
  cargo bench --workspace --features libdd-crashtracker/benchmarking,libdd-sampling/v04_span,libdd-sampling/bench-internals,libdd-trace-utils/bench-internals -- --warm-up-time 1 --measurement-time 5 --sample-size=200
fi
message "Finished running benchmarks"

# Copy the benchmark results to the output directory
if [[ -n "${OUTPUT_DIR}" && -d "target" ]]; then
  # Is this a relative path?
  if [[ "$OUTPUT_DIR" != /* ]]; then
    OUTPUT_DIR="${CURRENT_PATH}/${OUTPUT_DIR}"
  fi
  message "Copying benchmark results to ${OUTPUT_DIR}"
  pushd target > /dev/null
  find criterion -type d -regex '.*/new$' | while read -r dir; do
    mkdir -p "${OUTPUT_DIR}/${dir}"
    cp -r "${dir}"/* "${OUTPUT_DIR}/${dir}"
  done
  popd > /dev/null
fi

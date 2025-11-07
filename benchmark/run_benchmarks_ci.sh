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

# Run benchmarks
message "Running benchmarks"
cargo bench --workspace --features libdd-crashtracker/benchmarking -- --warm-up-time 1 --measurement-time 5 --sample-size=200
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

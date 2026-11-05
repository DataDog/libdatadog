#!/usr/bin/env bash

# Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# The single source of truth for which crate-specific features each crate's benchmarks need.
# Sourced by run_benchmarks_ci.sh, and called as `bench_features.sh <crate>` by
# stable-benchmarks.py. When scoping a run we must pass only the features for the selected crates
# (cargo errors on --features for a crate that isn't part of the selection).
bench_features_for_crate() {
  case "$1" in
    libdd-crashtracker) echo "libdd-crashtracker/benchmarking" ;;
    libdd-sampling) echo "libdd-sampling/v04_span libdd-sampling/bench-internals" ;;
    libdd-trace-utils) echo "libdd-trace-utils/bench-internals" ;;
    *) echo "" ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  bench_features_for_crate "${1:?usage: bench_features.sh <crate>}"
fi

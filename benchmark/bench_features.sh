#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Crate-specific features each crate's benchmarks need, shared by run_benchmarks_ci.sh
# and crate_timing.sh. Only pass features for crates in the current selection: cargo
# errors on --features for a crate outside it.
bench_features_for_crate() {
  case "$1" in
    libdd-crashtracker) echo "libdd-crashtracker/benchmarking" ;;
    libdd-sampling) echo "libdd-sampling/v04_span libdd-sampling/bench-internals" ;;
    libdd-trace-utils) echo "libdd-trace-utils/bench-internals" ;;
    *) echo "" ;;
  esac
}

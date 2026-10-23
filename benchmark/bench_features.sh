#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Crate-specific features each crate's benchmarks need, used by the GitLab benchmarks
# job. Only pass features for crates in the current selection: cargo errors on
# --features for a crate outside it. tools/src/bin/crate_timing.rs carries the same
# mapping for the weekly timing job; keep the two in sync.
bench_features_for_crate() {
  case "$1" in
    libdd-crashtracker) echo "libdd-crashtracker/benchmarking" ;;
    libdd-sampling) echo "libdd-sampling/v04_span libdd-sampling/bench-internals" ;;
    libdd-trace-utils) echo "libdd-trace-utils/bench-internals" ;;
    *) echo "" ;;
  esac
}

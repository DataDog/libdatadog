#!/bin/bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Asserts that every FFI sub-crate aggregated into libdd-profiling-ffi keeps
# its `catch_panic` feature enabled in the combined artifact.
#
# libdd-profiling-ffi pulls its sub-crates with `default-features = false`, and
# `catch_panic` is a *default* feature of the crates that have one. Dropping the
# defaults silently degrades `catch_panic!` to a bare call, so a Rust panic
# crosses the non-unwinding `extern "C"` boundary and aborts the host process.
# TODO: APMSP-3874 - evaluate if catch_panic should even be an optional feature.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$ROOT_DIR"

AGGREGATOR="libdd-profiling-ffi"

METADATA=$(cargo metadata --no-deps --format-version 1)

# has_catch_panic <crate>
# 0 if the workspace crate declares a `catch_panic` feature, 1 otherwise.
has_catch_panic() {
    printf '%s' "$METADATA" | jq -e --arg name "$1" \
        '.packages[] | select(.name == $name) | .features | has("catch_panic")' > /dev/null 2>&1
}

# activating_feature <crate>
# The aggregator feature listing `dep:<crate>`, empty if there is none.
activating_feature() {
    printf '%s' "$METADATA" | jq -r --arg agg "$AGGREGATOR" --arg dep "$1" \
        'first(.packages[] | select(.name == $agg) | .features | to_entries[]
               | select(any(.value[]; . == "dep:" + $dep)) | .key)'
}

mapfile -t OPTIONAL_DEPS < <(printf '%s' "$METADATA" | jq -r --arg agg "$AGGREGATOR" \
    '.packages[] | select(.name == $agg) | .dependencies[] | select(.optional) | .name' | sort -u)

if [ "${#OPTIONAL_DEPS[@]}" -eq 0 ]; then
    echo "no optional dependencies found for $AGGREGATOR" >&2
    exit 2
fi

errors=0
checked=0
skipped=0

for dep in "${OPTIONAL_DEPS[@]}"; do
    if ! has_catch_panic "$dep"; then
        skipped=$((skipped + 1))
        continue
    fi

    feature=$(activating_feature "$dep")
    if [ -z "$feature" ]; then
        echo "ERROR: no $AGGREGATOR feature activates optional dependency $dep" >&2
        exit 2
    fi

    checked=$((checked + 1))

    output=$(cargo tree -p "$AGGREGATOR" --features "$feature" --edges features -i "$dep" 2>&1) || {
        echo "ERROR: cargo tree failed for $AGGREGATOR --features $feature -i $dep:" >&2
        echo "$output" | sed 's/^/  /' >&2
        exit 2
    }

    if grep -qF "$dep feature \"catch_panic\"" <<<"$output"; then
        echo "ok: $AGGREGATOR --features $feature keeps $dep/catch_panic"
    else
        echo "FAIL: $AGGREGATOR --features $feature does not enable $dep/catch_panic"
        echo "      A panic in $dep would abort the host process instead of returning an error."
        echo "      Fix: add \"$dep/catch_panic\" to the \"$feature\" feature in $AGGREGATOR/Cargo.toml"
        echo "$output" | sed 's/^/    /'
        errors=$((errors + 1))
    fi
done

if [ "$checked" -eq 0 ]; then
    echo "no aggregated crate declares a catch_panic feature -- discovery is broken" >&2
    exit 2
fi

if [ "$errors" -gt 0 ]; then
    echo
    echo "FFI panic containment check failed: $errors violation(s) across $checked edge(s)"
    exit 1
fi

echo "FFI panic containment check passed for $checked edge(s) ($skipped crate(s) without the feature skipped)"

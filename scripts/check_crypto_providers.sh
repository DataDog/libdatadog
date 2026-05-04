#!/bin/bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Asserts that no workspace crate ends up with both `ring` and `aws-lc-rs` in
# its runtime dependency graph at the same time.
#
# Mixing both backends bloats release binaries (e.g. datadog-lambda-extension
# pulls a few hundred KiB of unused crypto) and breaks downstream FIPS
# compliance checks. See #1816 and #1872 for the original gating work.
#
# Iterates every workspace member (libdd-* and the datadog-* / sidecar /
# remote-config / live-debugger / etc. crates that depend transitively on
# libdd-common) and runs:
#   * default feature set (whatever `cargo` picks)
#   * `--no-default-features --features fips` if the crate exposes `fips`
#   * `--no-default-features --features https` if the crate exposes `https`
#
# Each crate is resolved against its own Cargo.toml so workspace-level feature
# unification from other members does not skew the result, and dev-deps are
# excluded so test-only graphs do not produce false positives.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$ROOT_DIR"

# crate_has_feature <Cargo.toml> <feature_name>
# 0 if the [features] table declares the named feature, 1 otherwise.
crate_has_feature() {
    awk -v feat="$2" '
        /^\[features\]/ { in_features = 1; next }
        /^\[/           { in_features = 0 }
        in_features && $1 == feat && $2 == "=" { found = 1; exit }
        END { exit !found }
    ' "$1"
}

# pulls <manifest> <package> [extra-cargo-flags...]
# 0 if `package` is in the runtime dep graph of `manifest`. Resolved against
# the crate's manifest in isolation, with dev-deps excluded.
#
# `cargo tree -i` has three observed outcomes:
#   - exit 0, output starts with "<pkg> v..."         → pulled
#   - exit 0, output contains "nothing to print"      → not pulled (in workspace
#                                                       but not in this graph)
#   - exit non-zero, "did not match any packages"     → not pulled (not in
#                                                       workspace at all)
# Anything else (transient registry / git failures, manifest errors, etc.)
# is surfaced as a hard error so the guard cannot silently pass when
# dependency resolution itself broke.
pulls() {
    local manifest="$1" pkg="$2"
    shift 2
    local output rc
    output=$(cargo tree --manifest-path "$manifest" --edges no-dev "$@" -i "$pkg" 2>&1) && rc=0 || rc=$?

    # The tree heading line is "<pkg> v<version>" — match it line-anchored so
    # `Downloading crates...` progress noise printed first does not throw us off.
    if grep -qE "^${pkg} v[0-9]" <<<"$output"; then
        return 0
    fi
    if [[ "$output" == *"nothing to print"* ]] || \
       [[ "$output" == *"did not match any packages"* ]]; then
        return 1
    fi

    echo "ERROR: cargo tree failed for $(basename "$(dirname "$manifest")") -i $pkg (exit $rc):" >&2
    echo "$output" | sed 's/^/  /' >&2
    exit 2
}

tree_for() {
    local manifest="$1" pkg="$2"
    shift 2
    cargo tree --manifest-path "$manifest" --edges no-dev "$@" -i "$pkg" 2>&1 | sed 's/^/    /'
}

# check <manifest> <label> [cargo flags...]
# Fails if the dep graph contains both ring and aws-lc-rs.
check() {
    local manifest="$1" label="$2"
    shift 2
    local crate
    crate="$(basename "$(dirname "$manifest")")"

    if pulls "$manifest" "ring" "$@" && pulls "$manifest" "aws-lc-rs" "$@"; then
        echo "FAIL: $crate ($label) pulls both ring and aws-lc-rs"
        tree_for "$manifest" "ring" "$@"
        tree_for "$manifest" "aws-lc-rs" "$@"
        return 1
    fi
    return 0
}

errors=0
checked=0

# Enumerate all workspace members (libdd-* and the datadog-* / sidecar /
# live-debugger / remote-config / ffi crates) via `cargo metadata` so we
# don't have to hardcode prefixes or maintain an allow-list.
manifests=$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys; m=json.load(sys.stdin); print("\n".join(p["manifest_path"] for p in m["packages"]))')

for manifest in $manifests; do
    crate="$(basename "$(dirname "$manifest")")"
    checked=$((checked + 1))

    check "$manifest" "default" || errors=$((errors + 1))

    if crate_has_feature "$manifest" "https"; then
        check "$manifest" "--features https" --no-default-features --features https \
            || errors=$((errors + 1))
    fi

    if crate_has_feature "$manifest" "fips"; then
        check "$manifest" "--features fips" --no-default-features --features fips \
            || errors=$((errors + 1))
    fi
done

if [ "$checked" -eq 0 ]; then
    echo "no libdd-* crates found" >&2
    exit 2
fi

if [ "$errors" -gt 0 ]; then
    echo
    echo "crypto provider check failed: $errors violation(s) across $checked crate(s)"
    exit 1
fi

echo "crypto provider check passed for $checked crate(s)"

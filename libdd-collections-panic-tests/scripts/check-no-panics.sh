#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

verbose=0

usage() {
    printf 'usage: %s [-v]\n' "${0##*/}"
}

while getopts ':vh' opt; do
    case "$opt" in
        v)
            verbose=1
            ;;
        h)
            usage
            exit 0
            ;;
        :)
            usage >&2
            exit 2
            ;;
        \?)
            usage >&2
            exit 2
            ;;
    esac
done
shift "$((OPTIND - 1))"

if [[ "$#" -ne 0 ]]; then
    usage >&2
    exit 2
fi

crate_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$crate_root/Cargo.toml"
target_dir="${CARGO_TARGET_DIR:-$crate_root/target}"
release_dir="$target_dir/release"
lib_name="dd_collections_symbol_tests"
pattern='panic|unwind|rust_eh|personality'

cargo_bin="${CARGO:-cargo}"
build_log=''

cleanup() {
    if [[ -n "$build_log" ]]; then
        rm -f "$build_log"
    fi
}
trap cleanup EXIT

say() {
    if [[ "$verbose" -eq 1 ]]; then
        printf '%s\n' "$*" >&2
    fi
}

fail() {
    printf '%s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

first_existing_artifact() {
    local candidate

    for candidate in "$@"; do
        if [[ -f "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

run_build() {
    local cmd=("$cargo_bin" "build" "--manifest-path" "$manifest" "--release")

    if [[ "$verbose" -eq 1 ]]; then
        say "+ ${cmd[*]}"
        "${cmd[@]}"
    else
        build_log="$(mktemp "${TMPDIR:-/tmp}/libdd-collections-panic-tests.XXXXXX")"
        if ! "${cmd[@]}" --quiet >"$build_log" 2>&1; then
            cat "$build_log" >&2
            exit 1
        fi
    fi
}

scan_output() {
    local label="$1"
    local output="$2"
    local matches

    matches="$(printf '%s\n' "$output" | grep -Ei "$pattern" || true)"
    if [[ -n "$matches" ]]; then
        printf '%s\n' "$label contains panic/unwind machinery:" >&2
        printf '%s\n' "$matches" >&2
        exit 1
    fi
}

require_export() {
    local exports="$1"
    local symbol="$2"

    if ! grep -q "$symbol" <<<"$exports"; then
        fail "missing expected panic-test export: $symbol"
    fi
}

require_command cargo
require_command grep
require_command nm
require_command awk

say "building libdd-collections panic-test release artifacts"
run_build

dylib="$(first_existing_artifact \
    "$release_dir/lib${lib_name}.dylib" \
    "$release_dir/lib${lib_name}.so" \
    "$release_dir/${lib_name}.dll")" \
    || fail "missing cdylib for $lib_name"
staticlib="$(first_existing_artifact \
    "$release_dir/lib${lib_name}.a" \
    "$release_dir/${lib_name}.lib")" \
    || fail "missing staticlib for $lib_name"

say "checking expected panic-test exports"
exports="$(nm -g "$dylib")"
expected_exports=(
    ddog_collections_vec_i64_new
    ddog_collections_vec_i32_new
    ddog_collections_vec_i64_free
    ddog_collections_vec_i32_free
    ddog_collections_vec_i64_reserve
    ddog_collections_vec_i32_reserve
    ddog_collections_vec_i64_reserve_exact
    ddog_collections_vec_i64_push
    ddog_collections_vec_i64_try_push
    ddog_collections_vec_i64_try_from_slice
    ddog_collections_vec_i64_extend_from_slice_within_capacity
    ddog_collections_vec_i64_extend_within_capacity
    ddog_collections_vec_i64_shrink_to_fit
    ddog_collections_vec_i64_shrink_to
    ddog_collections_vec_i64_try_resize
    ddog_collections_vec_i64_truncate
    ddog_collections_vec_i64_clear
    ddog_collections_vec_i64_pop
    ddog_collections_vec_i64_retain_odd
    ddog_collections_vec_i64_retain_mut_increment_even
    ddog_collections_vec_i64_dedup
    ddog_collections_vec_i64_dedup_by_mod_10
    ddog_collections_vec_i64_dedup_by_key_parity
    ddog_collections_vec_i64_recycle_same
    ddog_collections_vec_i64_read_api_smoke
    ddog_collections_vec_i64_mut_api_smoke
    ddog_collections_vec_i64_into_iter_next_then_drop
    ddog_collections_vec_zst_smoke
    ddog_collections_vec_i64_len
    ddog_collections_vec_i64_get
    ddog_collections_vec_i64_get_mut_add
)
for symbol in "${expected_exports[@]}"; do
    require_export "$exports" "$symbol"
done

say "scanning linked cdylib symbols"
dylib_symbols="$(nm -a "$dylib")"
scan_output "$dylib" "$dylib_symbols"

say "scanning panic-test object inside staticlib"
own_static_symbols="$(
    nm -a "$staticlib" 2>/dev/null \
        | awk -v name="$lib_name.$lib_name" '
            $0 ~ name { capture = 1; next }
            capture && $0 == "" { capture = 0 }
            capture { print }
        '
)"
require_export "$own_static_symbols" ddog_collections_vec_i64_new
scan_output "$staticlib panic-test object" "$own_static_symbols"

say "no panic or unwind machinery found in panic-test outputs"

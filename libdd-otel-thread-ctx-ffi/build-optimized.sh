#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Build libdd-otel-thread-ctx-ffi with cross-language LTO so the C TLS shim is
# inlined into the Rust FFI functions, eliminating a function-call indirection
# on every TLS access.
#
# Requirements: clang, lld (rust-lld from the toolchain is used automatically).
#
# Usage:
#   ./build-optimized.sh              # auto-detect host triple
#   ./build-optimized.sh --target aarch64-unknown-linux-gnu  # explicit target
#
# Any extra arguments are forwarded to `cargo build`.
set -euo pipefail

# Parse --target from args, or auto-detect the host triple.
TARGET=""
EXTRA_ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            TARGET="$2"; shift 2 ;;
        --target=*)
            TARGET="${1#--target=}"; shift ;;
        *)
            EXTRA_ARGS+=("$1"); shift ;;
    esac
done

if [[ -z "$TARGET" ]]; then
    TARGET=$(rustc -vV | sed -n 's/host: //p')
fi

# CARGO_TARGET_<TRIPLE>_RUSTFLAGS scopes the flags to the target only, keeping
# build scripts and proc-macros unaffected.
TARGET_ENV=$(echo "$TARGET" | tr 'a-z-' 'A-Z_')
export "CARGO_TARGET_${TARGET_ENV}_RUSTFLAGS=-Clinker-plugin-lto -Clinker=clang"
export LIBDD_OTEL_THREAD_CTX_INLINE=1

exec cargo build --release \
    --target "$TARGET" \
    -p libdd-otel-thread-ctx-ffi \
    "${EXTRA_ARGS[@]}"

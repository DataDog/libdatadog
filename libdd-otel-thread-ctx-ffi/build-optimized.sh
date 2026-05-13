#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Build libdd-otel-thread-ctx-ffi with cross-language LTO so the C TLS shim is
# inlined into the Rust FFI functions, eliminating a function-call indirection
# on every TLS access.
#
# Requirements: clang, lld (rust-lld from the toolchain is used automatically).
# The requirements are checked by the build.rs script.
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

cargo build --release \
    --target "$TARGET" \
    -p libdd-otel-thread-ctx-ffi \
    "${EXTRA_ARGS[@]}"

# Sanity-check that the C shim was actually inlined, if `nm` is available.
if ! command -v nm &>/dev/null; then
		echo >&2 "WARNING: skipping sanity check that the C TLS shim was inlined (\`nm\` not found)"
else
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
    SO="$REPO_ROOT/target/$TARGET/release/liblibdd_otel_thread_ctx_ffi.so"

    if [[ -f "$SO" ]] && nm "$SO" 2>/dev/null | grep -q 'libdd_get_otel_thread_ctx'; then
        echo >&2 "WARNING: build succeeded but the C TLS shim (libdd_get_otel_thread_ctx_v1) was NOT inlined."
        echo >&2 "Cross-language LTO may not be working. Check that clang and lld versions are compatible with the Rust toolchain's LLVM."
        exit 1
    fi
fi

#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Builds libdd-profiling-ffi as a cdylib whose panic handler calls an
# undefined symbol, and fails if any panic path survived optimization (the
# link fails, or the symbol is still undefined), or if the library imports
# anything outside the allowlist below (environment variables, threads, TLS
# destructors, atexit, ...).
#
#   scripts/check-profiling-ffi-no-panic.sh            # the check
#   scripts/check-profiling-ffi-no-panic.sh --canary   # must fail: proves the check works

set -euo pipefail
cd "$(dirname "$0")/.."

cfgs=(--cfg ddprof_ffi_cdylib)
canary=0
if [[ "${1:-}" == "--canary" ]]; then
    cfgs+=(--cfg ddprof_ffi_canary)
    canary=1
fi

case "$(uname -s)" in
    Linux)
        # Undefined symbols are an error, not a runtime lookup.
        link=(-C link-arg=-Wl,-z,defs)
        lib=libdatadog_profiling_ffi.so
        nm_undefined=(nm -D --undefined-only)
        ;;
    Darwin)
        # ld64 rejects undefined symbols in a dylib by default.
        link=()
        lib=libdatadog_profiling_ffi.dylib
        nm_undefined=(nm -u)
        ;;
    *)
        echo "unsupported OS: $(uname -s)" >&2
        exit 2
        ;;
esac

# zstd's optional tracing hooks are weak undefined symbols on Linux; the
# artifact has no tracer, so compile them out.
export CFLAGS="${CFLAGS:-} -DZSTD_TRACE=0"

# Allowed imports: libc memory and string routines, the memory mapping and
# lock-file calls of the intern tables, and platform runtime glue (macOS
# memset_pattern16 is clang's pattern fill for zstd's table loops). On aarch64
# Linux, compiler_builtins calls getauxval once at load to pick LSE or LL/SC
# atomics: read-only CPU feature detection.
allowed='^_?_?(malloc|calloc|realloc|free|posix_memalign|memcpy|memmove|memset|memset_pattern16|memcmp|bcmp|bzero|strlen|mmap|munmap|madvise|sysconf|open|unlink|fcntl|close|getpid|getauxval|__errno_location|__error|__stack_chk_fail|__stack_chk_guard|__cxa_finalize|_ITM_deregisterTMCloneTable|_ITM_registerTMCloneTable|__gmon_start__|dyld_stub_binder)$'

log=$(mktemp)
trap 'rm -f "$log"' EXIT

# Builds and checks the cdylib with the given features ("" for none).
check() {
    local features=$1
    if ! cargo rustc -p libdd-profiling-ffi --profile release-profiling --crate-type cdylib \
        --no-default-features --features "$features" \
        -- "${cfgs[@]}" "${link[@]}" >"$log" 2>&1; then
        if grep -q "__ddog_prof_panic_is_reachable" "$log"; then
            echo "FAIL [$features]: a panic path survived optimization:" >&2
            grep -B2 -A6 "__ddog_prof_panic_is_reachable" "$log" | head -40 >&2
            return 1
        fi
        cat "$log" >&2
        echo "FAIL [$features]: build error" >&2
        return 2
    fi
    local artifact="target/release-profiling/$lib"
    local undefined unexpected exports
    undefined=$("${nm_undefined[@]}" "$artifact" | awk '{print $NF}' | sed 's/@.*//' | sort -u)
    if grep -q "__ddog_prof_panic_is_reachable" <<<"$undefined"; then
        echo "FAIL [$features]: the panic sentinel is still referenced" >&2
        return 1
    fi
    unexpected=$(grep -Ev "$allowed" <<<"$undefined" || true)
    if [[ -n "$unexpected" ]]; then
        echo "FAIL [$features]: imports outside the allowlist:" >&2
        echo "$unexpected" >&2
        return 2
    fi
    exports=$(nm -g "$artifact" | grep -c " T _\?ddog_prof_" || true)
    echo "OK [${features:-no formats}]: no reachable panic; $exports ddog_prof_* exports; imports:"
    echo "  $(tr '\n' ' ' <<<"$undefined")"
}

if [[ $canary == 1 ]]; then
    # The negative control: the default build plus a deliberate panic.
    if check "pprof,otel,zstd"; then
        echo "canary: expected a failure, got none" >&2
        exit 1
    fi
    echo "canary: the check caught the deliberate panic"
    exit 1
fi

# Every feature combination is its own artifact: check each one.
status=0
# `otel` implies `gzip`, so gzip only varies on its own without `otel`.
for features in "pprof,otel,zstd" "pprof,otel" "pprof,zstd" "otel,zstd" "pprof" "otel" "zstd" "" \
    "pprof,gzip,zstd" "pprof,gzip" "gzip,zstd" "gzip"; do
    check "$features" || status=1
done
exit $status

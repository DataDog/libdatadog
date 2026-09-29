#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Pin and run ast-grep the same way CI does. Downloads the official
# release zip into a user cache on first use — no cargo/npm install.
#
# Usage (from the repo root, or via this script's path):
#   ./scripts/run-ast-grep.sh              # rule tests + repository scan
#   ./scripts/run-ast-grep.sh test         # tests only (forwards extra args)
#   ./scripts/run-ast-grep.sh scan         # scan only
#   ./scripts/run-ast-grep.sh -- <args>    # pass through to ast-grep

set -euo pipefail

# Bump this and the checksums together when upgrading.
AST_GREP_VERSION="0.45.3"

# sha256 of each GitHub release zip (not the unpacked binary).
# Source: https://github.com/ast-grep/ast-grep/releases/tag/${AST_GREP_VERSION}
SHA256_AARCH64_APPLE_DARWIN="6d2279dea5bea2ad79c66ea93f5fe54ba926e398a8a26de76c56db68fe59eac6"
SHA256_X86_64_APPLE_DARWIN="b2ffd26f42810340326a9e8a084bdc3647a8795c1a3f21fc06bd7bef3c7c5b2c"
SHA256_X86_64_UNKNOWN_LINUX_GNU="f8ac830881339d1edee6b2652f54798c0f4da5a827f2db38a08ee31117783ce8"
SHA256_AARCH64_UNKNOWN_LINUX_GNU="b39cfbc58da4b869a88b8a4bc57bd5deb0d24541e704cf7c257da7b53ec81c8f"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

die() {
    echo "ERROR: $*" >&2
    exit 2
}

target_and_sha() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "${os}-${arch}" in
        Darwin-arm64)
            echo "aarch64-apple-darwin ${SHA256_AARCH64_APPLE_DARWIN}"
            ;;
        Darwin-x86_64)
            echo "x86_64-apple-darwin ${SHA256_X86_64_APPLE_DARWIN}"
            ;;
        Linux-x86_64)
            echo "x86_64-unknown-linux-gnu ${SHA256_X86_64_UNKNOWN_LINUX_GNU}"
            ;;
        Linux-aarch64 | Linux-arm64)
            echo "aarch64-unknown-linux-gnu ${SHA256_AARCH64_UNKNOWN_LINUX_GNU}"
            ;;
        *)
            die "unsupported platform ${os}-${arch}. Install ast-grep ${AST_GREP_VERSION} yourself and re-run."
            ;;
    esac
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "need sha256sum or shasum to verify the ast-grep download"
    fi
}

ensure_ast_grep() {
    local target sha cache_dir bin url zip actual
    read -r target sha <<<"$(target_and_sha)"
    cache_dir="${XDG_CACHE_HOME:-${HOME}/.cache}/libdatadog/ast-grep-${AST_GREP_VERSION}/${target}"
    bin="${cache_dir}/ast-grep"
    if [[ -x "$bin" ]]; then
        echo "$bin"
        return
    fi

    command -v curl >/dev/null 2>&1 || die "curl is required to download ast-grep"
    command -v unzip >/dev/null 2>&1 || die "unzip is required to unpack ast-grep"

    mkdir -p "$cache_dir"
    url="https://github.com/ast-grep/ast-grep/releases/download/${AST_GREP_VERSION}/app-${target}.zip"
    zip="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$zip'" RETURN

    echo "Downloading ast-grep ${AST_GREP_VERSION} (${target})..." >&2
    curl -fsSL -o "$zip" "$url"
    actual="$(sha256_of "$zip")"
    if [[ "$actual" != "$sha" ]]; then
        die "checksum mismatch for ${url}: got ${actual}, expected ${sha}"
    fi

    unzip -qo "$zip" ast-grep -d "$cache_dir"
    chmod +x "$bin"
    echo "$bin"
}

run_default() {
    local bin="$1"
    shift
    "$bin" test
    local scan_args=()
    if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
        scan_args+=(--format github)
    fi
    # Hundreds of existing FFI accessors match this rule. Whole-repo
    # scan leaves it off; scripts/run-ffi-panic-lint.sh fails only on
    # newly added signatures.
    scan_args+=(--off=ffi-extern-c-panic-containment)
    "$bin" scan "${scan_args[@]}" "$@"
    bash "${SCRIPT_DIR}/run-ffi-panic-lint.sh"
}

main() {
    cd "$ROOT_DIR"
    local bin
    bin="$(ensure_ast_grep)"

    if [[ $# -eq 0 ]]; then
        run_default "$bin"
        return
    fi
    if [[ "$1" == "--" ]]; then
        shift
        "$bin" "$@"
        return
    fi
    if [[ "$1" == "scan" && "${GITHUB_ACTIONS:-}" == "true" ]]; then
        # Insert --format github unless the caller already chose a format.
        local has_format=0
        local arg
        for arg in "$@"; do
            if [[ "$arg" == "--format" || "$arg" == --format=* ]]; then
                has_format=1
                break
            fi
        done
        if [[ "$has_format" -eq 0 ]]; then
            shift
            "$bin" scan --format github "$@"
            return
        fi
    fi
    "$bin" "$@"
}

main "$@"

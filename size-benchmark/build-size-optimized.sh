#!/usr/bin/env bash
# Build the size-benchmark binary with the same aggressive size optimizations
# that our most critical users apply, so the measured size is representative.
#
# Requires:
#   - rustup with nightly toolchain
#   - aarch64-unknown-linux-musl target installed
#   - aarch64-linux-musl-gcc (or equivalent cross linker) on PATH
#
# Usage: ./size-benchmark/build-size-optimized.sh [extra cargo args]
# Output: prints the binary size in bytes on stdout (last line)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET=aarch64-unknown-linux-musl

RUSTFLAGS="\
  -Zunstable-options \
  -Cpanic=immediate-abort \
  -Zlocation-detail=none \
  -Zfmt-debug=none \
" \
cargo +nightly build \
  -Z build-std=std,panic_abort \
  -Z build-std-features= \
  --target "$TARGET" \
  --release \
  -p size-benchmark \
  --manifest-path "$WORKSPACE_ROOT/Cargo.toml" \
  "$@"

BINARY="$WORKSPACE_ROOT/target/$TARGET/release/size-benchmark"
wc -c < "$BINARY"

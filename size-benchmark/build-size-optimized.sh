#!/usr/bin/env bash
# Build the size-benchmark binary with the same aggressive size optimizations
# that our most critical users apply, so the measured size is representative.
#
# On Linux  → builds for {host-arch}-unknown-linux-musl (static, musl libc)
# On macOS  → builds for the native Darwin target (no musl available on macOS)
#
# Requires: rustup with nightly toolchain + the resolved target installed.
# On Linux the musl target also needs a musl C toolchain (e.g. musl-tools package).
#
# Usage: ./size-benchmark/build-size-optimized.sh [extra cargo args]
# Output: binary size in bytes on stdout (last line)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="${WORKSPACE_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"

ARCH="$(uname -m | sed 's/arm64/aarch64/')"
OS="$(uname -s)"

case "$OS" in
  Linux)  TARGET="${ARCH}-unknown-linux-musl" ;;
  Darwin) TARGET="${ARCH}-apple-darwin" ;;
  *)      echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

rustup target add "$TARGET" --toolchain nightly >/dev/null 2>&1 || true

RUSTFLAGS="\
  -Zunstable-options \
  -Cpanic=immediate-abort \
  -Zlocation-detail=none \
  -Zfmt-debug=none \
" \
cargo +nightly build \
  -Z build-std=std,panic_abort \
  -Z build-std-features=optimize_for_size \
  --target "$TARGET" \
  --profile release-size \
  -p size-benchmark \
  --manifest-path "$WORKSPACE_ROOT/Cargo.toml" \
  "$@"

TARGET_DIR="${CARGO_TARGET_DIR:-$WORKSPACE_ROOT/target}"
BINARY="$TARGET_DIR/$TARGET/release-size/size-benchmark"
wc -c < "$BINARY"

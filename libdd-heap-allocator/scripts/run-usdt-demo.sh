#!/usr/bin/env bash
# Build and run libdd-heap-allocator's usdt_demo in the background, then
# attach bpftrace in the foreground to show `usdt:*:ddheap:alloc` events
# as they fire. Ctrl+C tears down both.
#
# Designed to be invoked from inside a Lima VM via the allocator crate's
# `lima-demo-arm64` / `lima-demo-amd64` Makefile targets.
#
# Required env:
#   WORKSPACE_ROOT    absolute path to the libdatadog workspace root
#   CARGO_TARGET_DIR  cargo target dir (kept off the macOS mount)

set -euo pipefail

: "${WORKSPACE_ROOT:?must be set}"
: "${CARGO_TARGET_DIR:?must be set}"
export CARGO_TARGET_DIR

if ! command -v bpftrace >/dev/null; then
    echo "bpftrace not found; installing..." >&2
    sudo apt-get update -qq
    sudo apt-get install -y -q bpftrace
fi

cd "$WORKSPACE_ROOT"
cargo build --example usdt_demo -p libdd-heap-allocator

BIN="$CARGO_TARGET_DIR/debug/examples/usdt_demo"
LOG=/tmp/usdt_demo.log

"$BIN" >"$LOG" 2>&1 &
DEMO_PID=$!

cleanup() {
    kill "$DEMO_PID" 2>/dev/null || true
    wait "$DEMO_PID" 2>/dev/null || true
    echo
    echo "-- demo log tail --"
    tail -n 5 "$LOG" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# Let the demo reach its first allocation and print its banner.
sleep 1

echo "demo pid=$DEMO_PID  (stdout/stderr → $LOG)"
echo "attaching bpftrace on usdt:*:ddheap:alloc (Ctrl+C to stop)..."
echo

sudo bpftrace -p "$DEMO_PID" -e '
  usdt:*:ddheap:alloc {
    printf("alloc ptr=%lx size=%d weight=%d\n", arg0, arg1, arg2);
  }
  usdt:*:ddheap:free {
    printf("free  ptr=%lx\n", arg0);
  }
'

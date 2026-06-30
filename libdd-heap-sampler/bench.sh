#!/usr/bin/env bash
# Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail
set -x

# Run from inside heap-profiling-amd64. This script assumes the repo is mounted at
# /workspaces/libdatadog, but also works if launched from the repo root.
if [ -d /workspaces/libdatadog ]; then
  cd /workspaces/libdatadog
fi

OUT_DIR="libdd-heap-sampler"
mkdir -p "$OUT_DIR"

# Rebuild the benchmark so build.rs changes such as -fcf-protection=none are used.
cargo bench -p libdd-heap-allocator --bench sampler_overhead --no-run

# Pick the x86-64 bench binary; the shared target dir may contain stale arm64 bins.
BENCH_BIN=$(
  for f in target/release/deps/sampler_overhead-*; do
    [ -x "$f" ] || continue
    file "$f" | grep -q 'x86-64' && { echo "$f"; break; }
  done
)

echo "BENCH_BIN=$BENCH_BIN"
file "$BENCH_BIN"

# Check whether endbr64 is gone from the sampler entrypoints.
objdump -d --demangle "$BENCH_BIN" \
  | grep -A12 '<dd_allocation_requested__extern>:' \
  > "$OUT_DIR/objdump-cet-none-snippet.txt" || true
objdump -d --demangle "$BENCH_BIN" \
  | grep -A12 '<dd_allocation_created__extern>:' \
  >> "$OUT_DIR/objdump-cet-none-snippet.txt" || true
objdump -d --demangle "$BENCH_BIN" \
  | grep -A16 '<dd_allocation_freed__extern>:' \
  >> "$OUT_DIR/objdump-cet-none-snippet.txt" || true
cat "$OUT_DIR/objdump-cet-none-snippet.txt"

# Focused timing comparison.
cargo bench -p libdd-heap-allocator --bench sampler_overhead -- \
  --warm-up-time 0.3 \
  --measurement-time 0.3 \
  --sample-size 10 \
  'alloc_free/noop/64|alloc_free/sampled_noop_fast_path/64|sampler_only/fast_path/64|alloc_free/system/64|alloc_free/sampled_system_fast_path/64' \
  2>&1 | tee "$OUT_DIR/bench-amd64-cet-none"

# Optional perf profile of the fast path, if perf is available/allowed.
if command -v perf >/dev/null 2>&1; then
  sudo perf record -o "$OUT_DIR/perf-sampled-noop-fast-path-cet-none.data" \
    -F 997 \
    -e cpu-clock \
    -g --call-graph dwarf \
    "$BENCH_BIN" \
    --bench 'alloc_free/sampled_noop_fast_path/64' \
    --profile-time 10

  sudo perf report -f \
    -i "$OUT_DIR/perf-sampled-noop-fast-path-cet-none.data" \
    --stdio \
    --no-children \
    --sort symbol,dso \
    > "$OUT_DIR/perf-sampled-noop-fast-path-cet-none.report.txt"

  for sym in dd_allocation_requested__extern dd_allocation_created__extern dd_allocation_freed__extern; do
    sudo perf annotate -f \
      -i "$OUT_DIR/perf-sampled-noop-fast-path-cet-none.data" \
      --stdio \
      --symbol "$sym" \
      > "$OUT_DIR/perf-annotate-$sym-cet-none.txt" || true
  done
fi

ls -lh "$OUT_DIR"/*cet-none* "$OUT_DIR"/objdump-cet-none-snippet.txt 2>/dev/null || true

#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# This scans whole rlibs, not a call graph rooted at the signal handler. Init-time code is held to
# the same symbol bar deliberately; reviewed exceptions belong in this script, not in source attrs.
target_dir="${CARGO_TARGET_DIR:-target/signal-safe-guard}"
CARGO_TARGET_DIR="${target_dir}" cargo build -p libdd-crashtracker --no-default-features --features collector_signal-safe --lib

artifacts=()
while IFS= read -r artifact; do
  artifacts+=("${artifact}")
done < <(find "${target_dir}/debug/deps" -maxdepth 1 \( \
  -name 'liblibdd_crashtracker*.rlib' -o \
  -name 'librustix*.rlib' -o \
  -name 'libheapless*.rlib' -o \
  -name 'libserde_json_core*.rlib' \
\) -print)

if [[ "${#artifacts[@]}" -eq 0 ]]; then
  echo "signal-safe rlib artifacts not found" >&2
  exit 1
fi

banned='(^|[^[:alnum:]_])(malloc|calloc|realloc|free|posix_memalign|mmap|pthread_mutex_lock|pthread_mutex_unlock|pthread_cond_[[:alnum:]_]+|__rust_alloc|getenv|dlsym|getauxval|fork|posix_spawn|pthread_atfork|syslog|abort|__libc_[[:alnum:]_]+)([^[:alnum:]_]|$)'

for artifact in "${artifacts[@]}"; do
  if nm -u "${artifact}" 2>/dev/null | grep -E "${banned}"; then
    echo "signal-safe crash-path artifact references banned symbols in ${artifact}" >&2
    exit 1
  fi
done

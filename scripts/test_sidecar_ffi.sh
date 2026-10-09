#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Compile, link, and run the sidecar C smoke fixture on Linux or macOS.
# Requires cbindgen 0.29.0 and a C compiler. Respects RUSTUP_TOOLCHAIN,
# CARGO_TARGET_DIR, CARGO_BUILD_PROFILE (default: dev), CC, and CBINDGEN.
set -euo pipefail

case "$(uname -s)" in
  Linux|Darwin) ;;
  *) echo "This smoke check supports native Linux and macOS builds." >&2; exit 1 ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
cbindgen="${CBINDGEN:-cbindgen}"
if [[ "$("$cbindgen" --version)" != "cbindgen 0.29.0" ]]; then
  echo "Install the pinned generator: cargo install cbindgen --version 0.29.0 --locked" >&2
  exit 1
fi

profile="${CARGO_BUILD_PROFILE:-dev}"
profile_dir="$profile"
if [[ "$profile" == dev ]]; then
  profile_dir=debug
fi
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
mkdir -p "$target_dir"
target_dir="$(cd "$target_dir" && pwd)"
smoke_dir="$target_dir/sidecar-ffi-smoke/$profile_dir"
mkdir -p "$smoke_dir"

# PHP generates telemetry alongside sidecar before deduplication. Its header
# supplies shared types, including the unconditional Option<u64> declaration.
"$cbindgen" --config libdd-common-ffi/cbindgen.toml --crate libdd-common-ffi \
  --output "$smoke_dir/common.h"
"$cbindgen" --config libdd-telemetry-ffi/cbindgen.toml --crate libdd-telemetry-ffi \
  --output "$smoke_dir/telemetry.h"
"$cbindgen" --config datadog-sidecar-ffi/cbindgen.toml --crate datadog-sidecar-ffi \
  --output "$smoke_dir/sidecar.h"
cargo run --locked --profile "$profile" -p tools --bin dedup_headers -- \
  "$smoke_dir/common.h" "$smoke_dir/telemetry.h" "$smoke_dir/sidecar.h"
cargo build --locked --profile "$profile" -p datadog-sidecar-ffi

"${CC:-cc}" -std=c11 -Wall -Wextra -Werror \
  -I"$smoke_dir" datadog-sidecar-ffi/tests/ffe_submission.c \
  -L"$target_dir/$profile_dir" -ldatadog_sidecar_ffi \
  -Wl,-rpath,"$target_dir/$profile_dir" -o "$smoke_dir/ffe-submission-smoke"
"$smoke_dir/ffe-submission-smoke"
echo "Sidecar C smoke check passed (generated headers, linked library, runtime call)."

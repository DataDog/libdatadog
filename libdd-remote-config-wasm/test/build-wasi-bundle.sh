# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

artifact_dir=$1
# SHA-2 dominates large updates. Keep the size profile for the rest of the guest.
CARGO_PROFILE_RELEASE_STRIP=symbols \
  CC_wasm32_wasip1="${CC_wasm32_wasip1:-clang}" \
  AR_wasm32_wasip1="${AR_wasm32_wasip1:-llvm-ar}" \
  cargo --config profile.release.package.sha2.opt-level=2 \
    build --locked --release -p libdd-remote-config-wasm --target wasm32-wasip1

mkdir -p "$artifact_dir"
cp "${CARGO_TARGET_DIR:-target}/wasm32-wasip1/release/libdd_remote_config_wasm.wasm" \
  libdd-remote-config-wasm/abi.json \
  libdd-remote-config-wasm/test/agentless-fixture.mjs \
  "$artifact_dir/"
git rev-parse HEAD > "$artifact_dir/SOURCE_COMMIT"
(
  cd "$artifact_dir"
  sha256sum libdd_remote_config_wasm.wasm abi.json agentless-fixture.mjs SOURCE_COMMIT > SHA256SUMS
)

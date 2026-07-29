#!/usr/bin/env bash
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Tags the current CI pipeline as excluded from Green CI (green_ci.excluded:true).
#
# Downloads the pinned, checksum-verified datadog-ci binary instead of `npm install -g`,
# so no third-party install-time code runs with the Datadog API key in its environment.
#
# Requires DATADOG_SITE and DATADOG_API_KEY in the environment.

set -euo pipefail

URL="https://github.com/DataDog/datadog-ci/releases/download/v5.21.0/datadog-ci_linux-x64"
OUTPUT="datadog-ci"
EXPECTED_CHECKSUM="be4a6473fc451fec967ff277df3856060814b9a54d707d055a9c1542ae2869f0"

echo "Downloading datadog-ci from $URL"
curl -L --fail --retry 3 -o "$OUTPUT" "$URL"
chmod +x "$OUTPUT"

ACTUAL_CHECKSUM=$(sha256sum "$OUTPUT" | cut -d' ' -f1)
if [ "$ACTUAL_CHECKSUM" != "$EXPECTED_CHECKSUM" ]; then
  echo "Checksum verification failed! expected=$EXPECTED_CHECKSUM actual=$ACTUAL_CHECKSUM"
  exit 1
fi
echo "Checksum verification passed"

./"$OUTPUT" tag --level pipeline --tags green_ci.excluded:true

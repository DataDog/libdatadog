#!/usr/bin/env bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0. This product includes software developed
# at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

set -eu

destdir="$1"
mkdir -v -p "$destdir/include/datadog"

echo "Generating $destdir/include/libdatadog headers..."
cbindgen --crate ddcommon-ffi \
    --config ddcommon-ffi/cbindgen.toml \
    --output "$destdir/include/datadog/common.h"

if cargo +nightly &> /dev/null; then 
    cargo +nightly run --bin ddtelemetry-ffi-header > "$destdir/include/datadog/telemetry.h"
else
    cargo run --bin ddtelemetry-ffi-header > "$destdir/include/datadog/telemetry.h"
fi

cbindgen --crate "datadog-profiling-ffi" \
    --config profiling-ffi/cbindgen.toml \
    --output "$destdir/include/datadog/profiling.h"

cargo run --bin dedup_headers -- "$destdir/include/datadog/common.h" "$destdir/include/datadog/telemetry.h" "$destdir/include/datadog/profiling.h"

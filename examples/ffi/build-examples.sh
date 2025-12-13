#!/bin/bash
# Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Build FFI libraries and headers for examples, then build the example executables
set -e

# Ensure we're in the project root directory
if [[ ! -f "Cargo.toml" || ! -f "builder/Cargo.toml" ]]; then
    echo "Error: Please run this script from the project root directory"
    echo "Usage: ./examples/ffi/build-examples.sh"
    exit 1
fi

# Add features here when new FFI examples are added
FEATURES=(
    "profiling"
    "telemetry"
    "data-pipeline"
    "symbolizer"
    "crashtracker"
    "library-config"
    "log"
    "ddsketch"
    "ffe"
)

FEATURE_LIST=$(IFS=,; echo "${FEATURES[*]}")
echo "Building FFI libraries with features: $FEATURE_LIST"

cargo run --bin release --features "$FEATURE_LIST" --release -- --out

echo "Configuring example build..."
cmake -S examples/ffi -B examples/ffi/build -D Datadog_ROOT=./release

echo "Building examples..."
cmake --build ./examples/ffi/build --target profiles

echo "Done! Example executables are available in examples/ffi/build/"

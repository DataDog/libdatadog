#!/bin/bash
# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Generic script to build and run CXX examples
# Usage: ./build-and-run.sh <crate-name> <example-name>
# Example: ./build-and-run.sh libdd-profiling profiling

set -e

if [ $# -ne 2 ]; then
    echo "Usage: $0 <crate-name> <example-name>"
    echo "Example: $0 libdd-profiling profiling"
    exit 1
fi

CRATE_NAME="$1"
EXAMPLE_NAME="$2"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

echo "🔨 Building $CRATE_NAME with cxx feature..."
cargo build -p "$CRATE_NAME" --features cxx --release

echo "🔍 Finding CXX bridge headers..."
CXX_BRIDGE_INCLUDE=$(find target/release/build/${CRATE_NAME}-*/out/cxxbridge/include -type d 2>/dev/null | head -n 1)
CXX_BRIDGE_CRATE=$(find target/release/build/${CRATE_NAME}-*/out/cxxbridge/crate -type d 2>/dev/null | head -n 1)
RUST_CXX_INCLUDE=$(find target/release/build/cxx-*/out -type d 2>/dev/null | head -n 1)

if [ -z "$CXX_BRIDGE_INCLUDE" ] || [ -z "$CXX_BRIDGE_CRATE" ] || [ -z "$RUST_CXX_INCLUDE" ]; then
    echo "❌ Error: Could not find CXX bridge directories"
    exit 1
fi

echo "📁 CXX include: $CXX_BRIDGE_INCLUDE"
echo "📁 CXX crate: $CXX_BRIDGE_CRATE"
echo "📁 Rust CXX: $RUST_CXX_INCLUDE"

echo "🔨 Finding libraries..."
# Convert crate name with dashes to underscores for library name
LIB_NAME=$(echo "$CRATE_NAME" | tr '-' '_')
CRATE_LIB="$PROJECT_ROOT/target/release/lib${LIB_NAME}.a"
CXX_BRIDGE_LIB=$(find target/release/build/${CRATE_NAME}-*/out -name "lib${CRATE_NAME}-cxx.a" | head -n 1)

if [ ! -f "$CRATE_LIB" ]; then
    echo "❌ Error: Could not find $CRATE_NAME library at $CRATE_LIB"
    exit 1
fi

if [ ! -f "$CXX_BRIDGE_LIB" ]; then
    echo "❌ Error: Could not find CXX bridge library"
    exit 1
fi

echo "📚 Crate library: $CRATE_LIB"
echo "📚 CXX bridge library: $CXX_BRIDGE_LIB"

echo "🔨 Compiling C++ example..."
# Platform-specific linker flags
if [[ "$OSTYPE" == "darwin"* ]]; then
    PLATFORM_LIBS="-framework Security -framework CoreFoundation"
else
    PLATFORM_LIBS=""
fi

c++ -std=c++20 \
    -I"$CXX_BRIDGE_INCLUDE" \
    -I"$CXX_BRIDGE_CRATE" \
    -I"$RUST_CXX_INCLUDE" \
    -I"$PROJECT_ROOT" \
    "examples/cxx/${EXAMPLE_NAME}.cpp" \
    "$CRATE_LIB" \
    "$CXX_BRIDGE_LIB" \
    -lpthread -ldl $PLATFORM_LIBS \
    -o "examples/cxx/${EXAMPLE_NAME}"

echo "🚀 Running example..."
"./examples/cxx/${EXAMPLE_NAME}"

echo ""
echo "✅ Success!"



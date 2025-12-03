#!/bin/bash
# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Build and run the CXX crashinfo example
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

echo "🔨 Building libdd-crashtracker with cxx feature..."
cargo build -p libdd-crashtracker --features cxx --release

echo "🔍 Finding CXX bridge headers..."
CXX_BRIDGE_INCLUDE=$(find target/release/build/libdd-crashtracker-*/out/cxxbridge/include -type d 2>/dev/null | head -n 1)
CXX_BRIDGE_CRATE=$(find target/release/build/libdd-crashtracker-*/out/cxxbridge/crate -type d 2>/dev/null | head -n 1)
RUST_CXX_INCLUDE=$(find target/release/build/cxx-*/out -type d 2>/dev/null | head -n 1)

if [ -z "$CXX_BRIDGE_INCLUDE" ] || [ -z "$CXX_BRIDGE_CRATE" ] || [ -z "$RUST_CXX_INCLUDE" ]; then
    echo "❌ Error: Could not find CXX bridge directories"
    exit 1
fi

echo "📁 CXX include: $CXX_BRIDGE_INCLUDE"
echo "📁 CXX crate: $CXX_BRIDGE_CRATE"
echo "📁 Rust CXX: $RUST_CXX_INCLUDE"

echo "🔨 Finding libraries..."
CRASHTRACKER_LIB="$PROJECT_ROOT/target/release/liblibdd_crashtracker.a"
CXX_BRIDGE_LIB=$(find target/release/build/libdd-crashtracker-*/out -name "liblibdd-crashtracker-cxx.a" | head -n 1)

if [ ! -f "$CRASHTRACKER_LIB" ]; then
    echo "❌ Error: Could not find libdd-crashtracker library at $CRASHTRACKER_LIB"
    exit 1
fi

if [ ! -f "$CXX_BRIDGE_LIB" ]; then
    echo "❌ Error: Could not find CXX bridge library"
    exit 1
fi

echo "📚 Crashtracker library: $CRASHTRACKER_LIB"
echo "📚 CXX bridge library: $CXX_BRIDGE_LIB"

echo "🔨 Compiling C++ example..."
c++ -std=c++14 \
    -I"$CXX_BRIDGE_INCLUDE" \
    -I"$CXX_BRIDGE_CRATE" \
    -I"$RUST_CXX_INCLUDE" \
    -I"$PROJECT_ROOT" \
    examples/cxx/crashinfo.cpp \
    "$CRASHTRACKER_LIB" \
    "$CXX_BRIDGE_LIB" \
    -lpthread -ldl -framework Security -framework CoreFoundation \
    -o examples/cxx/crashinfo

echo "🚀 Running example..."
./examples/cxx/crashinfo

echo ""
echo "✅ Success!"

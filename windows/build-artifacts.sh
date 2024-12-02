#!/bin/bash

set -e

output_dir=$1
target=$2

if [ -z "$output_dir" ]; then
    echo "You must specify an output directory. Ex: $0 my_rust_project/ bin"
    exit 1
fi

targets=("i686-pc-windows-msvc" "x86_64-pc-windows-msvc")
if [ -n "$target" ]; then
    targets=("$target")
fi

if [[ "$output_dir" != /* ]]; then
    output_dir=$(pwd)/"$output_dir"
fi

echo -e "Building project into $output_dir"

features="data-pipeline-ffi,datadog-profiling-ffi/crashtracker-collector,datadog-profiling-ffi/crashtracker-receiver,datadog-profiling-ffi/ddtelemetry-ffi,datadog-profiling-ffi/demangler"

echo -e "Building for features: $features"

pushd profiling-ffi > /dev/null
for target in "${targets[@]}"; do
    cargo build --features "$features" --target "$target" --release --target-dir "$output_dir"
    cargo build --features "$features" --target "$target" --target-dir "$output_dir"
done
popd > /dev/null

echo -e "Building tools"
cd tools
cargo build --release
cd ..

echo -e "Generating headers"
cbindgen --crate ddcommon-ffi --config ddcommon-ffi/cbindgen.toml --output "$output_dir/common.h"
cbindgen --crate datadog-profiling-ffi --config profiling-ffi/cbindgen.toml --output "$output_dir/profiling.h"
cbindgen --crate ddtelemetry-ffi --config ddtelemetry-ffi/cbindgen.toml --output "$output_dir/telemetry.h"
cbindgen --crate data-pipeline-ffi --config data-pipeline-ffi/cbindgen.toml --output "$output_dir/data-pipeline.h"
cbindgen --crate datadog-crashtracker-ffi --config crashtracker-ffi/cbindgen.toml --output "$output_dir/crashtracker.h"
./target/release/dedup_headers "$output_dir/common.h" "$output_dir/profiling.h" "$output_dir/telemetry.h" "$output_dir/data-pipeline.h" "$output_dir/crashtracker.h"

echo -e "Build finished"

#!/bin/bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Default values
output_dir=""
targets=(
    # Uncomment or add targets as needed
    "aarch64-apple-darwin"
    "x86_64-apple-darwin"
    # "aarch64-unknown-linux-gnu"
    # "x86_64-unknown-linux-gnu"
    # "i686-pc-windows-msvc"
    # "x86_64-pc-windows-msvc"
)

# Parse named parameters
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        -o|--output)
            output_dir="$2"
            shift 2
            ;;
        -t|--target)
            targets=()
            shift
            while [[ "$1" && ! "$1" =~ ^- ]]; do
                targets+=("$1")
                shift
            done
            ;;
        *)
            echo "Unknown parameter: $1"
            exit 1
            ;;
    esac
done

# Check if output directory is set
if [ -z "$output_dir" ]; then
    echo "You must specify an output directory with -o or --output. Example: ./build_script.sh -o bin"
    exit 1
fi

# Make output_dir an absolute path if it's not already
if [[ "$output_dir" != /* ]]; then
    output_dir="$(pwd)/$output_dir"
fi

echo -e "Building project into $output_dir"

# Function to invoke a command and exit if it fails
invoke_call() {
    "$@"
    if [ $? -ne 0 ]; then
        exit $?
    fi
}

# Function to build project with given target, features, and release flag
build_project() {
    local target="$1"
    local release_flag="$2"

    features=(
        "data-pipeline-ffi"
        "datadog-profiling-ffi/ddtelemetry-ffi"
        "datadog-profiling-ffi/crashtracker-receiver"
        "datadog-profiling-ffi/crashtracker-collector"
        "datadog-profiling-ffi/demangler"
    )

    if [ "$release_flag" = "--release" ]; then
        invoke_call cargo build --features "$(IFS=,; echo "${features[*]}")" --target "$target" --release --target-dir "$output_dir"
    else
        invoke_call cargo build --features "$(IFS=,; echo "${features[*]}")" --target "$target" --target-dir "$output_dir"
    fi
}

# Function to generate header files using cbindgen
generate_header() {
    local crate_name="$1"
    local config_path="$2"
    local output_path="$3"

    invoke_call cbindgen --crate "$crate_name" --config "$config_path" --output "$output_path"
}

# Build project for multiple targets
pushd profiling-ffi || exit
for target in "${targets[@]}"; do
    build_project "$target" "--release"
    build_project "$target" ""
done
popd || exit

echo -e "Building tools"
pushd tools || exit
invoke_call cargo build --release
popd || exit

echo -e "Generating headers"

# Generate headers for each FFI crate
generate_header "ddcommon-ffi" "ddcommon-ffi/cbindgen.toml" "$output_dir/common.h"
generate_header "datadog-profiling-ffi" "profiling-ffi/cbindgen.toml" "$output_dir/profiling.h"
generate_header "ddtelemetry-ffi" "ddtelemetry-ffi/cbindgen.toml" "$output_dir/telemetry.h"
generate_header "data-pipeline-ffi" "data-pipeline-ffi/cbindgen.toml" "$output_dir/data-pipeline.h"
generate_header "datadog-crashtracker-ffi" "crashtracker-ffi/cbindgen.toml" "$output_dir/crashtracker.h"

# Deduplicate headers
invoke_call ./target/release/dedup_headers "$output_dir/common.h" "$output_dir/profiling.h" "$output_dir/telemetry.h" "$output_dir/data-pipeline.h" "$output_dir/crashtracker.h"

echo -e "Build finished"

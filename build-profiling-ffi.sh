#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

get_abs_filename() {
  # $1 : relative filename
  echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
}

# Location to place all artifacts
if [ -z $CARGO_TARGET_DIR ] ; then
    export CARGO_TARGET_DIR=$PWD/target
fi

set -eu

ARG_FEATURES=""
run_tests=true

usage() {
    echo "Usage: `basename "$0"` [-h] [-f FEATURES] [-T] dest-dir"
    echo
    echo "Options:"
    echo "  -h          This help text"
    echo "  -f FEATURES Enable specified features (comma separated if more than one)"
    echo "  -T          Skip checks after building"
    exit $1
}

while getopts f:hT flag
do
    case "${flag}" in
        f)
            ARG_FEATURES=${OPTARG}
            shift
            shift
            ;;
        h)
            usage 0
            ;;
        T)
            run_tests=false
            shift
            ;;
    esac
done

if test -z "${1:-}"; then
    usage 1
fi
destdir="$1"

if [ $CARGO_TARGET_DIR = $destdir ]; then
    echo "Error: CARGO_TARGET_DIR and destdir cannot be the same"
    exit 1
fi

mkdir -v -p "$destdir/include/datadog" "$destdir/lib/pkgconfig" "$destdir/cmake"

version=$(awk -F\" '$1 ~ /^version/ { print $2 }' < profiling-ffi/Cargo.toml)
target="$(rustc -vV | awk '/^host:/ { print $2 }')"
shared_library_suffix=".so"
static_library_suffix=".a"
library_prefix="lib"
remove_rpath=0
fix_macos_rpath=0
symbolizer=0

# Rust provides this note about the link libraries:
# note: Link against the following native artifacts when linking against this
# static library. The order and any duplication can be significant on some
# platforms.
#
# We've decided to strip out -lgcc_s because if it's provided then it will
# always make it into the final runtime dependencies, even if -static-libgcc is
# provided. At least on Alpine, libgcc_s may not even exist in the users'
# images, so -static-libgcc is recommended there.
case "$target" in
    "x86_64-alpine-linux-musl"|"aarch64-alpine-linux-musl")
        expected_native_static_libs=" -lssp_nonshared -lgcc_s -lc"
        native_static_libs=" -lssp_nonshared -lc"
        # on alpine musl, Rust adds some weird runpath to cdylibs
        remove_rpath=1
        symbolizer=1
        ;;

    "x86_64-apple-darwin"|"aarch64-apple-darwin")
        expected_native_static_libs=" -framework Security -framework CoreFoundation -liconv -lSystem -lresolv -lc -lm -liconv"
        native_static_libs="${expected_native_static_libs}"

        shared_library_suffix=".dylib"
        # fix usage of library in macos via rpath
        fix_macos_rpath=1
        ;;

    "x86_64-unknown-linux-gnu"|"aarch64-unknown-linux-gnu")
        expected_native_static_libs=" -ldl -lrt -lpthread -lgcc_s -lc -lm -lrt -lpthread -lutil -ldl -lutil"
        native_static_libs=" -ldl -lrt -lpthread -lc -lm -lrt -lpthread -lutil -ldl -lutil"
        symbolizer=1
        ;;

    "x86_64-pc-windows-msvc")
        expected_native_static_libs="" # I don't know what to expect
        native_static_libs="" # I don't know what to expect
        shared_library_suffix=".dll"
        static_library_suffix=".lib"
        library_prefix=""
        ;;

    *)
        >&2 echo "Unknown platform '${target}'"
        exit 1
        ;;
esac

echo "Recognized platform '${target}'. Adding libs: ${native_static_libs}"
sed < profiling-ffi/datadog_profiling.pc.in "s/@Datadog_VERSION@/${version}/g" \
    > "$destdir/lib/pkgconfig/datadog_profiling.pc"

sed < profiling-ffi/datadog_profiling_with_rpath.pc.in "s/@Datadog_VERSION@/${version}/g" \
    > "$destdir/lib/pkgconfig/datadog_profiling_with_rpath.pc"

sed < profiling-ffi/datadog_profiling-static.pc.in "s/@Datadog_VERSION@/${version}/g" \
    | sed "s/@Datadog_LIBRARIES@/${native_static_libs}/g" \
    > "$destdir/lib/pkgconfig/datadog_profiling-static.pc"

# strip leading white space as per CMake policy CMP0004.
ffi_libraries="$(echo "${native_static_libs}" | sed -e 's/^[[:space:]]*//')"

sed < cmake/DatadogConfig.cmake.in \
    > "$destdir/cmake/DatadogConfig.cmake" \
    "s/@Datadog_LIBRARIES@/${ffi_libraries}/g"

cp -v LICENSE LICENSE-3rdparty.yml NOTICE "$destdir/"


datadog_profiling_ffi="datadog-profiling-ffi"
FEATURES=(
    "cbindgen"
    "crashtracker-collector"
    "crashtracker-receiver"
    "data-pipeline-ffi"
    "datadog-profiling-ffi/ddtelemetry-ffi"
    "datadog-profiling-ffi/demangler"
)
if [[ "$symbolizer" -eq 1 ]]; then
    FEATURES+=("symbolizer")
fi

if [[ ! -z ${ARG_FEATURES} ]]; then
    FEATURES+=($ARG_FEATURES)
fi

FEATURES=$(IFS=, ; echo "${FEATURES[*]}")
echo "Building for features: $FEATURES"

# build inside the crate to use the config.toml file
( cd profiling-ffi && DESTDIR="$destdir" cargo build --features $FEATURES --release --target "${target}" )

# Remove _ffi suffix when copying
shared_library_name="${library_prefix}datadog_profiling_ffi${shared_library_suffix}"
shared_library_rename="${library_prefix}datadog_profiling${shared_library_suffix}"

static_library_name="${library_prefix}datadog_profiling_ffi${static_library_suffix}"
static_library_rename="${library_prefix}datadog_profiling${static_library_suffix}"

cp -v "$CARGO_TARGET_DIR/${target}/release/${shared_library_name}" "$destdir/lib/${shared_library_rename}"
cp -v "$CARGO_TARGET_DIR/${target}/release/${static_library_name}" "$destdir/lib/${static_library_rename}"

shared_library_name="${shared_library_rename}"
static_library_name="${static_library_rename}"

if [[ "$remove_rpath" -eq 1 ]]; then
    patchelf --remove-rpath "$destdir/lib/${shared_library_name}"
fi

if [[ "$fix_macos_rpath" -eq 1 ]]; then
    install_name_tool -id @rpath/${shared_library_name} "$destdir/lib/${shared_library_name}"
fi

if command -v patchelf > /dev/null && [[ "$target" != "x86_64-pc-windows-msvc" ]]; then
    patchelf --set-soname ${shared_library_name}  "$destdir/lib/${shared_library_rename}"
fi

# objcopy might not be available on macOS
if command -v objcopy > /dev/null && [[ "$target" != "x86_64-pc-windows-msvc" ]]; then
    # Remove .llvmbc section which is not useful for clients
    objcopy --remove-section .llvmbc "$destdir/lib/${static_library_name}"

    # Ship debug information separate from shared library, so that downstream packages can selectively include it
    # https://sourceware.org/gdb/onlinedocs/gdb/Separate-Debug-Files.html
    objcopy --only-keep-debug "$destdir/lib/$shared_library_name" "$destdir/lib/$shared_library_name.debug"
    strip -S "$destdir/lib/$shared_library_name"
    objcopy --add-gnu-debuglink="$destdir/lib/$shared_library_name.debug" "$destdir/lib/$shared_library_name"
fi

if $run_tests; then
    echo "Checking that native-static-libs are as expected for this platform..."
    cd profiling-ffi
    actual_native_static_libs="$(cargo rustc --release --target "${target}" -- --print=native-static-libs 2>&1 | awk -F ':' '/note: native-static-libs:/ { print $3 }')"
    echo "Actual native-static-libs:${actual_native_static_libs}"
    echo "Expected native-static-libs:${expected_native_static_libs}"

    # Compare unique elements between expected and actual native static libs.
    # If actual libs is different from expected libs but still a subset of expected libs
    # (ie. we will overlink compared to what is actually needed), this is not considered as an error.
    # Raise an error only if some libs are in actual libs but not in expected libs.

    # trim leading and trailing spaces, then split the string on " -" by inserting new lines and sort lines while removing duplicates
    unique_expected_libs=$(echo "$expected_native_static_libs "| awk '{ gsub(/^[ \t]+|[ \t]+$/, "");gsub(/ +-/,"\n-")};1' | sort -u)
    unique_libs=$(echo "$actual_native_static_libs "| awk '{ gsub(/^[ \t]+|[ \t]+$/, "");gsub(/ +-/,"\n-")};1' | sort -u)

    unexpected_native_libs=$(comm -13 <(echo "$unique_expected_libs") <(echo "$unique_libs"))
    if [ -n "$unexpected_native_libs" ]; then
        echo "Error - More native static libraries are required for linking than expected:" 1>&2
        echo "$unexpected_native_libs" 1>&2
        exit 1
    fi
    cd -
fi

echo "Building tools"
DESTDIR=$destdir cargo build --package tools --bins

echo "Generating $destdir/include/libdatadog headers..."
# ADD headers based on selected features.
HEADERS="$destdir/include/datadog/common.h $destdir/include/datadog/profiling.h $destdir/include/datadog/telemetry.h $destdir/include/datadog/crashtracker.h"
case $ARG_FEATURES in
    *data-pipeline-ffi*)
        HEADERS="$HEADERS $destdir/include/datadog/data-pipeline.h"
        ;;
esac

"$CARGO_TARGET_DIR"/debug/dedup_headers $HEADERS

# Don't build the crashtracker on windows
if [[ "$target" != "x86_64-pc-windows-msvc" ]]; then
    echo "Building binaries"
    # $destdir might be relative. Get an absolute path that will work when we cd
    export ABS_DESTDIR=$(get_abs_filename $destdir)
    export CRASHTRACKER_BUILD_DIR=$CARGO_TARGET_DIR/build/crashtracker-receiver
    export CRASHTRACKER_SRC_DIR=$PWD/crashtracker
    # Always start with a clean directory
    [ -d $CRASHTRACKER_BUILD_DIR ] && rm -r $CRASHTRACKER_BUILD_DIR
    mkdir -p $CRASHTRACKER_BUILD_DIR
    cd $CRASHTRACKER_BUILD_DIR
    cmake -S $CRASHTRACKER_SRC_DIR -DDatadog_ROOT=$ABS_DESTDIR
    cmake --build .
    mkdir -p $ABS_DESTDIR/bin
    cp libdatadog-crashtracking-receiver $ABS_DESTDIR/bin
fi

echo "Done."

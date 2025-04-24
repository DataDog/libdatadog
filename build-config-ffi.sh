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

destdir="$1"

if [ $CARGO_TARGET_DIR = $destdir ]; then
    echo "Error: CARGO_TARGET_DIR and destdir cannot be the same"
    exit 1
fi

mkdir -v -p "$destdir/include/datadog" "$destdir/lib/pkgconfig" "$destdir/cmake"

version=$(awk -F\" '$1 ~ /^version/ { print $2 }' < crates/datadog-profiling-ffi/Cargo.toml)
target="$(rustc -vV | awk '/^host:/ { print $2 }')"
shared_library_suffix=".so"
static_library_suffix=".a"
library_prefix="lib"
remove_rpath=0
fix_macos_rpath=0

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
        # on alpine musl, Rust adds some weird runpath to cdylibs
        remove_rpath=1
        ;;

    "x86_64-apple-darwin"|"aarch64-apple-darwin")

        shared_library_suffix=".dylib"
        # fix usage of library in macos via rpath
        fix_macos_rpath=1
        ;;

    "x86_64-unknown-linux-gnu"|"aarch64-unknown-linux-gnu")
        ;;

    "x86_64-pc-windows-msvc")
        shared_library_suffix=".dll"
        static_library_suffix=".lib"
        library_prefix=""
        ;;

    *)
        >&2 echo "Unknown platform '${target}'"
        exit 1
        ;;
esac


cp -v LICENSE LICENSE-3rdparty.yml NOTICE "$destdir/"


crate_dir="datadog-library-config-ffi"
crate="datadog-library-config-ffi"


FEATURES=(
    "cbindgen"
)

FEATURES=$(IFS=, ; echo "${FEATURES[*]}")
echo "Building for features: $FEATURES"

# build inside the crate to use the config.toml file
( cd "$crate_dir" && cargo build --features $FEATURES --release --target "${target}" )

# Remove _ffi suffix when copying
crate_name_underscore=$(echo "$crate" | sed 's/-/_/g')
renamed_stem=$(echo "$crate_name_underscore" | sed 's/_ffi//g')

shared_library_name="${library_prefix}${crate_name_underscore}${shared_library_suffix}"
shared_library_rename="${library_prefix}${renamed_stem}${shared_library_suffix}"

static_library_name="${library_prefix}${crate_name_underscore}${static_library_suffix}"
static_library_rename="${library_prefix}${renamed_stem}${static_library_suffix}"

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

echo "Building tools"
cargo build --package tools --bins

echo "Generating $destdir/include/libdatadog headers..."
rm -r $destdir/include/datadog/
mkdir $destdir/include/datadog/

CBINDGEN_HEADERS="common.h library-config.h"

CBINDGEN_HEADERS_DESTS=""
for header in $CBINDGEN_HEADERS; do
    HEADER_DEST="$destdir/include/datadog/$header"
    cp "$CARGO_TARGET_DIR/include/datadog/$header" "$HEADER_DEST"
    CBINDGEN_HEADERS_DESTS="$CBINDGEN_HEADERS_DESTS $HEADER_DEST"
done

"$CARGO_TARGET_DIR"/debug/dedup_headers $CBINDGEN_HEADERS_DESTS

echo "Done."

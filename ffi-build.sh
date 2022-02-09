#!/usr/bin/env bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0. This product includes software developed
# at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

set -eu

destdir="$1"

mkdir -v -p "$destdir/include/ddprof" "$destdir/lib/pkgconfig" "$destdir/cmake"

version=$(awk -F\" '$1 ~ /^version/ { print $2 }' < ddprof-ffi/Cargo.toml)
target="$(rustc -vV | awk '/^host:/ { print $2 }')"

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
    "x86_64-alpine-linux-musl")
        expected_native_static_libs=" -lssp_nonshared -lgcc_s -lc"
        native_static_libs=" -lssp_nonshared -lc"
        ;;
    "x86_64-apple-darwin")
        expected_native_static_libs=" -framework Security -liconv -lSystem -lresolv -lc -lm -liconv"
        native_static_libs="${expected_native_static_libs}"
        ;;
    "x86_64-unknown-linux-gnu")
        expected_native_static_libs=" -ldl -lrt -lpthread -lgcc_s -lc -lm -lrt -lpthread -lutil -ldl -lutil"
        native_static_libs=" -ldl -lrt -lpthread -lc -lm -lrt -lpthread -lutil -ldl -lutil"
        ;;
    *)
        >&2 echo "Unknown platform '${target}'"
        exit 1
        ;;
esac

echo "Recognized platform '${target}'. Adding libs: ${native_static_libs}"
sed < ddprof_ffi.pc.in "s/@DDProf_FFI_VERSION@/${version}/g" \
    | sed "s/@DDProf_FFI_LIBRARIES@/${native_static_libs}/g" \
    > "$destdir/lib/pkgconfig/ddprof_ffi.pc"

sed < cmake/DDProfConfig.cmake.in \
    > "$destdir/cmake/DDProfConfig.cmake" \
    "s/@DDProf_FFI_LIBRARIES@/${native_static_libs}/g"

cp -v LICENSE LICENSE-3rdparty.yml NOTICE "$destdir/"

export RUSTFLAGS="${RUSTFLAGS:- -C relocation-model=pic}"

echo "Building the libddprof_ffi.a library (may take some time)..."
cargo build --release --target "${target}"
cp -v "target/${target}/release/libddprof_ffi.a" "$destdir/lib/"

echo "Checking that native-static-libs are as expected for this platform..."
cd ddprof-ffi
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
    echo $unexpected_native_libs 1>&2
    exit 1
fi
cd -

echo "Generating the ddprof/ffi.h header..."
cbindgen --crate ddprof-ffi --config ddprof-ffi/cbindgen.toml --output "$destdir/include/ddprof/ffi.h"

# CI doesn't have any clang tooling
# clang-format -i "$destdir/include/ddprof/ffi.h"

echo "Done."

#/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

set -eu

destdir="$1"

mkdir -v -p "$destdir/include/ddprof" "$destdir/lib/pkgconfig" "$destdir/cmake"

version=$(awk -F\" '$1 ~ /^version/ { print $2 }' < ddprof-ffi/Cargo.toml)
sed "s/@version@/${version}/g" < ddprof_ffi.pc.in > "$destdir/lib/pkgconfig/ddprof_ffi.pc"
cp -v cmake/DDProfConfig.cmake "$destdir/cmake/"
cp -v LICENSE LICENSE-3rdparty.yml NOTICE "$destdir/"

RUSTFLAGS="${RUSTFLAGS:- -C relocation-model=pic}" cargo build --release
cp -v target/release/libddprof_ffi.a "$destdir/lib/"

cbindgen --crate ddprof-ffi --config ddprof-ffi/cbindgen.toml --output "$destdir/include/ddprof/ffi.h"

# CI doesn't have any clang tooling
# clang-format -i "$destdir/include/ddprof/ffi.h"


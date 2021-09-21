#/bin/bash

set -eu

out_dir="$1"

mkdir -v -p "$out_dir/include/ddprof" "$out_dir/lib" "$out_dir/cmake"

cp -v cmake/DDProfConfig.cmake "$out_dir/cmake/"

RUSTFLAGS="${RUSTFLAGS:- -C relocation-model=pic}" cargo build --release
cp -v target/release/libddprof_ffi.a "$out_dir/lib/"

cbindgen --crate ddprof-ffi --config ddprof-ffi/cbindgen.toml --output "$out_dir/include/ddprof/ffi.h"

# CI doesn't have any clang tooling
# clang-format -i "$out_dir/include/ddprof/ffi.h"


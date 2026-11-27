# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

check_glibc_baseline() {
    local versions newest
    versions=$(LC_ALL=C readelf --version-info "$1" | awk '$2 == "Name:" && $3 ~ /^GLIBC_/ {print $3}')
    newest=$(printf '%s\n' GLIBC_2.17 "$versions" | LC_ALL=C sort -V | tail -n 1)
    if [[ -z "$versions" || "$newest" != GLIBC_2.17 ]]; then
        printf 'Expected glibc imports no newer than 2.17 in %s\n%s\n' "$1" "$versions" >&2
        return 1
    fi
}

target=$1
output=$2
source_commit=$3
arch=$(uname -m)
compiler=cc
cargo_env=(env)
case "$(uname -s)-$arch" in
    Darwin-arm64|Darwin-x86_64)
        if [[ "$arch" == arm64 ]]; then
            arch=aarch64
        fi
        test "$target" = "$arch-apple-darwin"
        library=liblibdd_remote_config_wasm.dylib
        rpath=@loader_path/lib
        checksum=(shasum -a 256)
        ;;
    Linux-x86_64|Linux-aarch64)
        if [[ "$target" == "$arch-unknown-linux-musl" ]]; then
            test -e "/lib/ld-musl-$arch.so.1"
            export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=-crt-static"
        else
            test "$target" = "$arch-unknown-linux-gnu"
            case "$arch" in
                x86_64) zig_sha=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239 ;;
                aarch64) zig_sha=958ed7d1e00d0ea76590d27666efbf7a932281b3d7ba0c6b01b0ff26498f667f ;;
            esac
            zig_version=0.15.2
            zig_archive="zig-$arch-linux-$zig_version"
            zig_dir=$(mktemp -d "$output/remote-config-gnu-toolchain.XXXXXX")
            curl --proto '=https' --proto-redir '=https' -fsSL --retry 3 \
                "https://ziglang.org/download/$zig_version/$zig_archive.tar.xz" -o "$zig_dir/$zig_archive.tar.xz"
            (
                cd "$zig_dir"
                printf '%s  %s.tar.xz\n' "$zig_sha" "$zig_archive" | sha256sum -c --strict
                tar -xJf "$zig_archive.tar.xz"
            )
            export RC_NATIVE_ZIG="$zig_dir/$zig_archive/zig"
            export RC_NATIVE_ZIG_TARGET="$arch-linux-gnu.2.17"
            export ZIG_GLOBAL_CACHE_DIR="$zig_dir/cache" ZIG_LOCAL_CACHE_DIR="$zig_dir/local-cache"
            compiler="$PWD/libdd-remote-config-wasm/test/native-gnu-cc.sh"
            compiler_key=${target//-/_}
            linker_key=$(printf '%s' "$compiler_key" | tr '[:lower:]' '[:upper:]')
            cargo_env+=("CC_$compiler_key=$compiler" "CARGO_TARGET_${linker_key}_LINKER=$compiler")
        fi
        library=liblibdd_remote_config_wasm.so
        rpath="\$ORIGIN/lib"
        checksum=(sha256sum)
        ;;
    *) printf 'Unsupported Remote Config integration runner\n' >&2; exit 1 ;;
esac

build_dir=$(mktemp -d "$output/remote-config-native-build.XXXXXX")
"${cargo_env[@]}" CARGO_TARGET_DIR="$build_dir" cargo build --locked --release -p libdd-remote-config-wasm --target "$target"
release_dir="$build_dir/$target/release"
if [[ "$target" == *-linux-gnu ]]; then
    check_glibc_baseline "$release_dir/$library"
fi
headers=()
while IFS= read -r -d '' header; do
    headers+=("$header")
done < <(find "$release_dir/build" -type f -name ddog_remote_config.h -print0)
if [[ ${#headers[@]} -ne 1 ]]; then
    printf 'Expected one generated Remote Config header\n' >&2
    exit 1
fi
stage=$(mktemp -d "$output/remote-config-native-stage.XXXXXX")
mkdir "$stage/include" "$stage/lib"
cp "${headers[0]}" "$stage/include/"
cp "$release_dir/$library" "$stage/lib/"
printf '%s\n' "$source_commit" > "$stage/SOURCE_COMMIT"
printf '%s\n' "$target" > "$stage/TARGET"
(cd "$stage" && "${checksum[@]}" "lib/$library" include/ddog_remote_config.h SOURCE_COMMIT TARGET > SHA256SUMS)
archive="$output/remote-config-native-$target.tar.gz"
tar -czf "$archive" -C "$stage" include lib SOURCE_COMMIT TARGET SHA256SUMS
mv "$build_dir" "$build_dir.parked"
test ! -e "$build_dir"

installed="$output/remote-config-native-installed"
mkdir "$installed"
tar -xzf "$archive" -C "$installed"
(cd "$installed" && "${checksum[@]}" -c SHA256SUMS)
"$compiler" -std=c11 -O2 -UNDEBUG -Wall -Wextra -Werror -pthread -I "$installed/include" \
    libdd-remote-config-wasm/test/native.c "$installed/lib/$library" \
    "-Wl,-rpath,$rpath" -o "$installed/native-check"
if [[ "$target" == *-linux-gnu ]]; then
    check_glibc_baseline "$installed/native-check"
fi
mv "$installed" "$installed.moved"
env -u LD_LIBRARY_PATH -u DYLD_LIBRARY_PATH -u DYLD_FALLBACK_LIBRARY_PATH "$installed.moved/native-check"
if [[ -n ${GITHUB_ENV:-} ]]; then
    printf 'DD_RC_TEST_NATIVE=%s\n' "$installed.moved/lib/$library" >> "$GITHUB_ENV"
fi

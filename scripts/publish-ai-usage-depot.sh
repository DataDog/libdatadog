#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Packages libdd-ai-usage and libdd-ai-usage-ffi and uploads them to Datadog's
# internal first-party Rust registry (Depot), so internal Cargo builds can
# depend on them by version instead of pinning a libdatadog git revision.
# This does not publish anything to crates.io.
#
# Usage: scripts/publish-ai-usage-depot.sh [--package-only]
#
#   --package-only  Patch the versions and run `cargo package` (which builds
#                   the packaged crates against registry dependencies), but
#                   upload nothing.
#
# Version: <manifest version>-<YYYYMMDD>-<12-char sha>, e.g.
# 0.1.0-20261117-8d94b6d7d0a1, where the date is the commit date in UTC. A
# pre-release never collides with a future crates.io release, and re-running
# the job for the same commit produces the same version, which is skipped if
# it is already in the registry. libdd-ai-usage-ffi depends on exactly the
# libdd-ai-usage version published with it.
#
# Destination: adms routes uploads from the default branch to
# datadog/first-party and from any other branch to datadog/first-party-testing.
# DEPOT_REPOSITORY must name the expected one; the upload is refused if adms
# would route it elsewhere.

set -euo pipefail

MODE=publish
case "${1:-}" in
  "") ;;
  --package-only) MODE=package ;;
  *) echo "usage: $0 [--package-only]" >&2; exit 2 ;;
esac

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CORE=libdd-ai-usage
FFI=libdd-ai-usage-ffi
CORE_MANIFEST="$REPO_ROOT/$CORE/Cargo.toml"
FFI_MANIFEST="$REPO_ROOT/$FFI/Cargo.toml"
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
INDEX_BASE="${DEPOT_RUST_INDEX_BASE:-https://depot-read-api-rust.us1.ddbuild.io}"

if [ "$MODE" = publish ] && [ -z "${DEPOT_REPOSITORY:-}" ]; then
  echo "DEPOT_REPOSITORY must be set (datadog/first-party or datadog/first-party-testing)" >&2
  exit 2
fi

sha="${CI_COMMIT_SHA:-$(git rev-parse HEAD)}"
sha12="${sha:0:12}"
if [ -n "${CI_COMMIT_TIMESTAMP:-}" ]; then
  date_stamp="$(date -u -d "$CI_COMMIT_TIMESTAMP" +%Y%m%d)"
else
  commit_epoch="$(git show -s --format=%ct "$sha")"
  date_stamp="$(date -u -d "@$commit_epoch" +%Y%m%d 2>/dev/null || date -u -r "$commit_epoch" +%Y%m%d)"
fi

# The first `version = "..."` line of a manifest is its [package] version.
manifest_version() {
  awk -F'"' '/^version = "/ { print $2; exit }' "$1"
}

set_package_version() {
  local manifest="$1" version="$2" tmp
  tmp="$(mktemp)"
  awk -v v="$version" '
    !done && /^version = "/ { print "version = \"" v "\""; done = 1; next }
    { print }
  ' "$manifest" > "$tmp"
  cat "$tmp" > "$manifest"
  rm -f "$tmp"
}

core_base="$(manifest_version "$CORE_MANIFEST")"
ffi_base="$(manifest_version "$FFI_MANIFEST")"
for base in "$core_base" "$ffi_base"; do
  if ! [[ "$base" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "expected a plain MAJOR.MINOR.PATCH manifest version, found '$base'" >&2
    exit 1
  fi
done
CORE_VERSION="${core_base}-${date_stamp}-${sha12}"
FFI_VERSION="${ffi_base}-${date_stamp}-${sha12}"

echo "==> $CORE $CORE_VERSION, $FFI $FFI_VERSION"

set_package_version "$CORE_MANIFEST" "$CORE_VERSION"
set_package_version "$FFI_MANIFEST" "$FFI_VERSION"

# Pin the FFI crate's dependency to the exact libdd-ai-usage version published
# with it.
tmp="$(mktemp)"
awk -v v="$CORE_VERSION" -v name="$CORE" '
  index($0, name " = {") == 1 {
    if (!sub(/version = "[^"]*"/, "version = \"=" v "\"")) { exit 3 }
    found = 1
  }
  { print }
  END { if (!found) exit 3 }
' "$FFI_MANIFEST" > "$tmp" || {
  rm -f "$tmp"
  echo "could not find the $CORE dependency version in $FFI_MANIFEST" >&2
  exit 1
}
cat "$tmp" > "$FFI_MANIFEST"
rm -f "$tmp"

# Packaging both crates in one invocation lets cargo verify libdd-ai-usage-ffi
# against the freshly packaged libdd-ai-usage before either is in a registry.
# Verification builds each crate from its .crate file, with every other
# dependency (such as libdd-ddsketch) resolved from crates.io, as consumers will.
# --allow-dirty covers the version patches above and the Cargo.lock update they
# cause; no other dependency is re-resolved.
echo "==> cargo package"
cargo package -p "$CORE" -p "$FFI" --allow-dirty

CORE_CRATE="$TARGET_DIR/package/${CORE}-${CORE_VERSION}.crate"
FFI_CRATE="$TARGET_DIR/package/${FFI}-${FFI_VERSION}.crate"
ls -l "$CORE_CRATE" "$FFI_CRATE"

if [ "$MODE" = package ]; then
  echo "==> Packaged only; nothing uploaded."
  exit 0
fi

# Sparse index path of a crate (crate names here are always 4+ characters).
index_url() {
  local name="$1"
  echo "$INDEX_BASE/$DEPOT_REPOSITORY/${name:0:2}/${name:2:2}/$name"
}

in_index() {
  local name="$1" version="$2"
  curl -fsS --retry 3 "$(index_url "$name")" 2>/dev/null | grep -qF "\"vers\":\"$version\""
}

wait_for_index() {
  local name="$1" version="$2" _
  echo "    waiting for $name $version in $DEPOT_REPOSITORY"
  for _ in $(seq 1 60); do
    if in_index "$name" "$version"; then
      echo "    indexed: $(index_url "$name")"
      return 0
    fi
    sleep 5
  done
  echo "$name $version did not appear in $DEPOT_REPOSITORY within 5 minutes" >&2
  return 1
}

# Dependency order: libdd-ai-usage-ffi needs its libdd-ai-usage in the registry.
upload() {
  local name="$1" version="$2" crate="$3"
  if in_index "$name" "$version"; then
    echo "==> $name $version is already in $DEPOT_REPOSITORY; skipping"
    return 0
  fi
  echo "==> Uploading $name $version to $DEPOT_REPOSITORY"
  adms first-party rust upload --repository "$DEPOT_REPOSITORY" "$crate"
  wait_for_index "$name" "$version"
}

upload "$CORE" "$CORE_VERSION" "$CORE_CRATE"
upload "$FFI" "$FFI_VERSION" "$FFI_CRATE"

echo "==> Published to $DEPOT_REPOSITORY:"
echo "    $CORE = \"=$CORE_VERSION\""
echo "    $FFI = \"=$FFI_VERSION\""

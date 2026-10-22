#!/usr/bin/env bash
# 
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Splits one combined lcov file into lcov-<crate>.info files, one per
# workspace crate that has coverage data.
# 
# Usage: split_lcov.sh <combined.lcov.info> <output-dir> [allowlist-json]
# Emits <output-dir>/lcov-<crate>.info. Source files that match no crate
# (e.g. generated code) are dropped with a ::warning:: annotation.
#
# With an allowlist (a JSON array of crate names), records for crates
# outside the list are excluded: they were only measured through the
# selected crates' tests, so their data is undercounted.

set -euo pipefail

LCOV=$1
OUT_DIR=$2
ALLOW_JSON=${3:-}
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

# Both `cargo metadata` manifest paths and llvm-cov `SF:` paths are absolute
# and refer to the same build tree, so plain prefix comparison is exact.
crate_map=$(cd "$ROOT_DIR" && cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | (.manifest_path | rtrimstr("/Cargo.toml")) + "\t" + .name')

allow_names=""
if [[ -n "$ALLOW_JSON" ]]; then
  allow_names=$(jq -r '.[]' <<<"$ALLOW_JSON" | tr '\n' ' ')
fi

cd "$OUT_DIR"

awk -v allowlist="$allow_names" '
  BEGIN {
    if (allowlist != "") {
      nallow=split(allowlist, t, " ")
      for (i=1; i<=nallow; i++) allowed[t[i]]=1
    }
  }
  NR==FNR { dirs[++ndirs]=$1; crateof[$1]=$2; next }
  /^SF:/ {
    sf=substr($0, 4)
    best=""
    for (i=1; i<=ndirs; i++)
      if (index(sf, dirs[i] "/")==1 && length(dirs[i])>length(best)) best=dirs[i]
    out=""
    if (best=="") {
      if (!(sf in warned)) {
        printf "::warning::%s matches no crate, dropping its coverage\n", sf
        warned[sf]=1
      }
      dropped++
    } else {
      crate=crateof[best]
      if (allowlist != "" && !(crate in allowed)) {
        excluded[crate]=1
        nexcluded++
      } else {
        out="lcov-" crate ".info"
        if (!(out in seen)) { seen[out]=1; nfiles++ }
        # Lines seen before SF: (TN:) were buffered until the crate was known.
        for (j=1; j<=npending; j++) print pending[j] > out
        print > out
      }
    }
    npending=0
    next
  }
  /^end_of_record/ {
    if (out!="") print > out
    out=""
    npending=0
    next
  }
  out=="" { pending[++npending]=$0; next }
  { print > out }
  END {
    if (dropped>0)
      printf "::warning::%d source files matched no crate and were dropped\n", dropped
    if (nexcluded>0) {
      msg=""
      for (c in excluded) msg=msg (msg==""?"":", ") c
      printf "excluded %d record(s) for crates outside the allowlist: %s\n", nexcluded, msg
    }
    if (nfiles==0) {
      if (allowlist != "") {
        print "::warning::no coverage data for any crate in the allowlist"
      } else {
        print "::error::lcov split produced no per-crate files"
        exit 1
      }
    }
  }
' <(printf '%s\n' "$crate_map") "$LCOV"

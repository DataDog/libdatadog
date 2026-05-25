#!/bin/bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Determine which crates flagged with breaking_change=true and level != "major"
# in the api-changes JSON actually need a major bump.
#
# A single commit can touch multiple crates while only breaking the public
# behavior of one of them. The conventional-commit `type!:` marker on the
# subject line is therefore not sufficient on its own to decide whether a given
# crate must be majored — this script is responsible for that per-crate
# verification.
#
# Usage: breaking-change-bumps.sh API_CHANGES_JSON
# Writes a JSON array of crates needing a major bump (verified breaking
# changes) to stdout. Each entry should at minimum echo back {name, prev_tag,
# version, level} from the input row. An empty array means no major bumps are
# required.

set -euo pipefail

usage() {
  echo "Usage: $0 API_CHANGES_JSON" >&2
  exit 2
}

[[ ${1:-} ]] || usage
API_JSON=$1
[[ -f "$API_JSON" ]] || { echo "Not a file: $API_JSON" >&2; exit 2; }

# TODO: implement per-crate verification.
# For each crate in $API_JSON where .breaking_change == true and .level != "major"
# and .initial_release != "true": inspect the commits in .commits whose subject
# matches '^[a-zA-Z]+(\([^)]+\))?!:' and confirm whether the change actually
# breaks that crate's public API/behavior. Only include the crate in the output
# array when verification confirms a breaking change.
echo '[]'

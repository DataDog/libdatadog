#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Run the release-script test suite.
#
#   ./scripts/tests/run.sh                          # everything
#   ./scripts/tests/run.sh semver-level             # one suite (by file stem)
#   ./scripts/tests/run.sh -f "merge-base"          # tests whose name matches a regex
#
# Requires bats-core >= 1.5.0 on PATH:
#   curl -sSfL https://github.com/bats-core/bats-core/archive/refs/tags/v1.11.1.tar.gz \
#     | tar xz && ./bats-core-1.11.1/install.sh "$HOME/.local"

set -euo pipefail

TESTS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd -- "${TESTS_DIR}/../.." >/dev/null 2>&1 && pwd)"

missing=()
for tool in bats jq git cargo python3; do
  command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "Error: missing required tool(s): ${missing[*]}" >&2
  if [[ " ${missing[*]} " == *" bats "* ]]; then
    echo "Install bats-core >= 1.5.0; see the header of this script." >&2
  fi
  exit 1
fi

if ! python3 -c 'import yaml' >/dev/null 2>&1; then
  echo "Error: python3 is missing the PyYAML module (needed to parse the workflow YAML)." >&2
  echo "Install it with: python3 -m pip install pyyaml" >&2
  exit 1
fi

# Suites resolve paths relative to the repository root.
cd "$REPO_ROOT"

declare -a suites=()
declare -a bats_args=()
for arg in "$@"; do
  if [[ "$arg" == -* || "${#bats_args[@]}" -gt 0 && "${bats_args[-1]}" == "-f" ]]; then
    bats_args+=("$arg")
  else
    suites+=("${TESTS_DIR}/${arg%.bats}.bats")
  fi
done
if [ "${#suites[@]}" -eq 0 ]; then
  mapfile -t suites < <(find "$TESTS_DIR" -maxdepth 1 -name '*.bats' | sort)
fi

exec bats --print-output-on-failure "${bats_args[@]}" "${suites[@]}"

#!/usr/bin/env bash
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Compares workspace crate versions between two git refs by running `cargo metadata` at each ref.
# Prints crate names suitable for `cargo package -p` when the crate's *own* package version changed.
#
# Important: this intentionally ignores dependency version bumps unless they also changed the crate
# version. (Cargo metadata reports package versions, not dependency requirements.)
#
# Excludes are determined from crate metadata: crates marked `publish = false` are excluded.
# (Crates with publish unset or publish = [...] are considered publishable.)

set -euo pipefail

GIT_VERSION_BASE="${GIT_VERSION_BASE:-origin/main}"
GIT_VERSION_HEAD="${GIT_VERSION_HEAD:-HEAD}"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." >/dev/null 2>&1 && pwd)"

tmp_root="$(mktemp -d)"
base_dir="${tmp_root}/base"
head_dir="${tmp_root}/head"

cleanup() {
	# best-effort cleanup
	git -C "${REPO_ROOT}" worktree remove --force "${base_dir}" >/dev/null 2>&1 || true
	git -C "${REPO_ROOT}" worktree remove --force "${head_dir}" >/dev/null 2>&1 || true
	rm -rf "${tmp_root}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# Ensure refs exist locally for worktree add.
git -C "${REPO_ROOT}" fetch --no-tags origin >/dev/null 2>&1 || true

git -C "${REPO_ROOT}" worktree add --detach "${base_dir}" "${GIT_VERSION_BASE}" >/dev/null
git -C "${REPO_ROOT}" worktree add --detach "${head_dir}" "${GIT_VERSION_HEAD}" >/dev/null

BASE_METADATA="$(cargo metadata --manifest-path "${base_dir}/Cargo.toml" --format-version=1 --no-deps)"
HEAD_METADATA="$(cargo metadata --manifest-path "${head_dir}/Cargo.toml" --format-version=1 --no-deps)"

# Build a name -> {version, publishable} map for workspace members.
#
# publishable:
# - publish unset (null) => publishable
# - publish array (registries) => publishable if non-empty
# - publish = false => NOT publishable
map_filter='
  . as $m
  | ($m.packages | map({id, name, version, publish})) as $pkgs
  | ($m.workspace_members) as $members
  | [ $members[]
      | . as $id
      | ($pkgs[] | select(.id == $id))
      | {
          name,
          version,
          publishable: (
            if (.publish == null) then true
            elif (.publish | type) == "array" then ((.publish | length) > 0)
            else false
            end
          )
        }
    ]
  | map({key: .name, value: {version: .version, publishable: .publishable}})
  | from_entries
'

BASE_MAP="$(echo "${BASE_METADATA}" | jq -c "${map_filter}")"
HEAD_MAP="$(echo "${HEAD_METADATA}" | jq -c "${map_filter}")"

# Emit publishable crates whose version changed between base and head.
#
# - If a crate is new in head (missing in base), include it.
# - If a crate disappeared in head, ignore it.
TO_PACKAGE="$(jq -nr --argjson base "${BASE_MAP}" --argjson head "${HEAD_MAP}" '
  ($head | keys_unsorted) as $names
  | [ $names[]
      | . as $n
      | ($head[$n]) as $h
      | select($h.publishable == true)
      | ($base[$n].version // null) as $bv
      | ($h.version) as $hv
      | select($bv != $hv)
      | $n
    ]
  | sort
  | .[]
')"

if [ -z "${TO_PACKAGE}" ]; then
	echo "crates-to-package.sh: no publishable workspace crates had a version change between refs (nothing to emit)." >&2
	exit 0
fi

echo "${TO_PACKAGE}"

#!/usr/bin/env bash
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Compares every workspace package's effective [package] version between GIT_VERSION_BASE
# and GIT_VERSION_HEAD. Prints package names suitable for `cargo package -p` when the version
# changed — not when only dependency versions changed.
# Uses git for both sides so results match the refs, not uncommitted working tree edits.
#
# CI should set GIT_VERSION_BASE / GIT_VERSION_HEAD to the commits being compared (e.g. PR base
# and head). Defaults below are for local use; comparing to origin/main vs HEAD is not the same
# as "what this PR changed" once main has moved.

set -euo pipefail

GIT_VERSION_BASE="${GIT_VERSION_BASE:-origin/main}"
GIT_VERSION_HEAD="${GIT_VERSION_HEAD:-HEAD}"

# [workspace.package].version from workspace root Cargo.toml (stdin).
workspace_package_version() {
	awk '
		/^\[workspace\.package\]/ { w = 1; next }
		w && /^\[/ { w = 0 }
		w && /^version[[:space:]]*=/ {
			line = $0
			sub(/^[^"]*"/, "", line)
			sub(/".*/, "", line)
			print line
			exit
		}
	'
}

# Effective [package] version: explicit "x.y.z" or "__WS__" if version.workspace = true.
package_version_kind() {
	awk '
		/^\[package\]/ { p = 1; next }
		p && /^\[/ { p = 0 }
		p && /^version\.workspace[[:space:]]*=[[:space:]]*true/ { print "__WS__"; exit }
		p && /^version[[:space:]]*=/ && $0 !~ /version\.workspace/ {
			line = $0
			sub(/^[^"]*"/, "", line)
			sub(/".*/, "", line)
			print line
			exit
		}
	'
}

effective_package_version() {
	local manifest_content="$1"
	local workspace_content="$2"
	local kind
	kind=$(printf '%s' "$manifest_content" | package_version_kind)
	if [ "$kind" = "__WS__" ]; then
		printf '%s' "$workspace_content" | workspace_package_version
		return
	fi
	printf '%s' "$kind"
}

excluded_crate() {
	local n="$1"
	case "$n" in
	libdd-*-ffi) return 0 ;;
	datadog-*) return 0 ;;
	bin_tests | tools | sidecar_mockgen | cc_utils | spawn_worker | symbolizer-ffi | test_spawn_from_lib | build_common | build-common | builder)
		return 0
		;;
	esac
	return 1
}

workspace_old=$(git show "${GIT_VERSION_BASE}:Cargo.toml" 2>/dev/null || true)
workspace_new=$(git show "${GIT_VERSION_HEAD}:Cargo.toml" 2>/dev/null || { cat Cargo.toml; })

METADATA=$(cargo metadata --format-version=1 --no-deps)
WORKSPACE_ROOT=$(echo "$METADATA" | jq -r '.workspace_root')

declare -A emit=()

while IFS=$'\t' read -r name mpath; do
	[ -z "$name" ] && continue
	relpath=${mpath#"$WORKSPACE_ROOT/"}

	old_m=$(git show "${GIT_VERSION_BASE}:${relpath}" 2>/dev/null || true)
	new_m=$(git show "${GIT_VERSION_HEAD}:${relpath}" 2>/dev/null || true)
	[ -n "$new_m" ] || continue

	v_old=""
	if [ -n "$old_m" ]; then
		v_old=$(effective_package_version "$old_m" "$workspace_old")
	fi
	v_new=$(effective_package_version "$new_m" "$workspace_new")

	if [ "$v_old" = "$v_new" ]; then
		continue
	fi

	if excluded_crate "$name"; then
		continue
	fi
	emit["$name"]=1
done < <(echo "$METADATA" | jq -r '.packages[] | "\(.name)\t\(.manifest_path)"')

if [ "${#emit[@]}" -eq 0 ]; then
	echo "crates-to-package.sh: no packages with a changed [package] version between refs (nothing to emit)." >&2
	exit 0
fi

for n in "${!emit[@]}"; do
	echo "$n"
done | sort -u

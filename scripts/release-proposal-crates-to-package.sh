#!/usr/bin/env bash
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Reads relative paths to changed Cargo.toml files (stdin, one per line).
# Prints package names to pass to `cargo package -p` only when that crate's own
# [package] version (including version.workspace inheritance) changed vs the base ref.
# Dependency-only version bumps in [dependencies] do not qualify.

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

# [package].name (stdin).
package_name_from_manifest() {
	awk '
		/^\[package\]/ { p = 1; next }
		p && /^\[/ { p = 0 }
		p && /^name[[:space:]]*=/ {
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
	libdd-crashtracker) return 0 ;;
	datadog-*) return 0 ;;
	bin_tests | tools | sidecar_mockgen | cc_utils | spawn_worker | symbolizer-ffi | test_spawn_from_lib | build_common | build-common | builder)
		return 0
		;;
	esac
	return 1
}

workspace_old=$(git show "${GIT_VERSION_BASE}:Cargo.toml" 2>/dev/null || true)
workspace_new=$(git show "${GIT_VERSION_HEAD}:Cargo.toml" 2>/dev/null || { cat Cargo.toml; })

wp_old=""
wp_new=""
[ -n "$workspace_old" ] && wp_old=$(printf '%s' "$workspace_old" | workspace_package_version)
wp_new=$(printf '%s' "$workspace_new" | workspace_package_version)

declare -A emit=()

# Root [workspace.package].version bump: every member with version.workspace = true gets a new effective version.
if [ -n "$workspace_old" ] && [ "$wp_old" != "$wp_new" ] && [ -n "$wp_new" ]; then
	WORKSPACE_ROOT=$(cargo metadata --format-version=1 --no-deps | jq -r '.workspace_root')
	while IFS=$'\t' read -r name mpath; do
		[ -z "$name" ] && continue
		relpath=${mpath#"$WORKSPACE_ROOT/"}
		[ -f "$relpath" ] || continue
		if ! grep -q '^version\.workspace[[:space:]]*=[[:space:]]*true' "$relpath" 2>/dev/null; then
			continue
		fi
		if excluded_crate "$name"; then
			continue
		fi
		emit["$name"]=1
	done < <(cargo metadata --format-version=1 --no-deps | jq -r '.packages[] | "\(.name)\t\(.manifest_path)"')
fi

while IFS= read -r rel; do
	[ -z "$rel" ] && continue
	[ "$rel" = "Cargo.toml" ] && continue
	case "$rel" in
	*/Cargo.toml) ;;
	*) continue ;;
	esac

	if [ ! -f "$rel" ]; then
		continue
	fi

	old_m=$(git show "${GIT_VERSION_BASE}:${rel}" 2>/dev/null || true)
	new_m=$(cat "$rel")

	v_old=""
	v_new=""
	if [ -n "$old_m" ]; then
		v_old=$(effective_package_version "$old_m" "$workspace_old")
	fi
	v_new=$(effective_package_version "$new_m" "$workspace_new")

	if [ "$v_old" = "$v_new" ]; then
		continue
	fi

	name=$(printf '%s' "$new_m" | package_name_from_manifest)
	[ -n "$name" ] || continue
	if excluded_crate "$name"; then
		continue
	fi
	emit["$name"]=1
done

if [ "${#emit[@]}" -eq 0 ]; then
	echo "release-proposal-crates-to-package.sh: no packages with a changed [package] version." >&2
	exit 1
fi

for n in "${!emit[@]}"; do
	echo "$n"
done | sort -u

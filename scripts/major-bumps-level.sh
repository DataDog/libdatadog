#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# For each crate in an api-changes.json array, compare direct libdd-* dependency
# version requirements between the current tree and prev_tag. If any dependency's
# major version (first digit in the req string) increased, record details in `major_bumps`.
#
# Usage: major-bumps-level.sh API_CHANGES_JSON
# Writes enriched JSON (same shape + major_bumps) to stdout.

set -euo pipefail

usage() {
  echo "Usage: $0 API_CHANGES_JSON" >&2
  exit 2
}

[[ ${1:-} ]] || usage
API_JSON=$1
[[ -f "$API_JSON" ]] || { echo "Not a file: $API_JSON" >&2; exit 2; }

level_rank() {
  case "${1:-}" in
    patch) echo 0 ;;
    minor) echo 1 ;;
    major) echo 2 ;;
    *) echo -1 ;;
  esac
}

libdd_deps_for_crate() {
  local manifest crate
  manifest=$1
  crate=$2
  jq --arg crate "$crate" '
    .packages[]
    | select(.name == $crate)
    | [.dependencies[]
       | select(.name | startswith("libdd-"))
       | {key: .name, value: .req}]
    | from_entries
  ' <(cargo metadata --manifest-path "$manifest" --format-version=1 --no-deps)
}

compare_libdd_bumps() {
  local path_prev path_curr
  path_prev=$1
  path_curr=$2
  jq -n --slurpfile prev "$path_prev" --slurpfile curr "$path_curr" '
    ($prev[0]) as $p
    | ($curr[0]) as $c
    | def first_num(s):
        try (
          (s | tostring | match("[0-9]+") | .string | tonumber)
        ) catch null
      ;
    [
      ($c | keys_unsorted[]) as $k
      | select($p[$k] != null)
      | first_num($p[$k]) as $pm
      | first_num($c[$k]) as $cm
      | select($pm != null and $cm != null and $cm > $pm)
      | {
          dependency: $k,
          previous_req: $p[$k],
          current_req: $c[$k]
        }
    ]
  '
}

WORKTREES=()
cleanup() {
  local d
  for d in "${WORKTREES[@]:-}"; do
    git worktree remove --force "$d" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

n=$(jq length "$API_JSON")
OUT=$(mktemp)
echo '[]' >"$OUT"
FAIL=0

for ((i = 0; i < n; i++)); do
  row=$(jq -c ".[$i]" "$API_JSON")
  name=$(echo "$row" | jq -r .name)
  prev_tag=$(echo "$row" | jq -r .prev_tag)
  initial=$(echo "$row" | jq -r .initial_release)
  lev=$(echo "$row" | jq -r .level)

  bumps_json='[]'
  new_lev=$lev

  if [[ "$initial" == "true" ]] || [[ -z "$prev_tag" ]] || [[ "$prev_tag" == "null" ]]; then
    :
  else
    cur_mf="${name}/Cargo.toml"
    if [[ ! -f "$cur_mf" ]]; then
      echo "ERROR: missing manifest $cur_mf" >&2
      exit 2
    fi
    wt=$(mktemp -d)
    WORKTREES+=("$wt")
    git worktree add --detach "$wt" "$prev_tag" >/dev/null
    prev_mf="${wt}/${name}/Cargo.toml"
    if [[ ! -f "$prev_mf" ]]; then
      echo "ERROR: missing manifest $prev_mf at $prev_tag" >&2
      exit 2
    fi
    prev_json=$(mktemp)
    curr_json=$(mktemp)
    libdd_deps_for_crate "$prev_mf" "$name" >"$prev_json"
    libdd_deps_for_crate "$cur_mf" "$name" >"$curr_json"
    bumps_json=$(compare_libdd_bumps "$prev_json" "$curr_json")
    rm -f "$prev_json" "$curr_json"
    if [[ $(echo "$bumps_json" | jq 'length') -gt 0 ]]; then
      echo "libdd-* direct dependency major bump for crate ${name} (prev_tag=${prev_tag}):" >&2
      echo "$bumps_json" | jq -r '.[] | "  - \(.dependency): \(.previous_req) -> \(.current_req)"' >&2
    fi
  fi

  enriched=$(echo "$row" | jq -c --argjson bumps "$bumps_json" --arg nl "$new_lev" \
    '. + {level: $nl, major_bumps: $bumps}')
  jq --argjson enriched "$enriched" '. + [$enriched]' "$OUT" >"${OUT}.new"
  mv "${OUT}.new" "$OUT"
done

jq . "$OUT"
rm -f "$OUT"

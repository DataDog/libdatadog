#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Release Excluded Crates Hints Script
# Reports the publishable workspace crates left out of a release proposal that are
# affected by the major bumps the proposal produced.
#
# Usage: ./release-excluded-crates-hints.sh --api-changes FILE [--out-markdown FILE]
#
# Release candidates only ever come from publication-order.sh, which walks downward:
# the selected crates plus the libdd-* crates they depend on. Dependents outside that
# closure are never candidates, yet `cargo release version` still rewrote their
# dependency requirement to the new major in this proposal. Their published version
# still requires the old major, and their next proposal will be force-bumped to major
# by major-bumps-level.sh. This script surfaces that so the operator can decide to
# include them now instead of finding out on the next release.
#
# A crate is reported when, in the proposal tree, it has a direct (non-dev,
# non-build) dependency on a crate released here at level "major" whose requirement
# now names the new major version. That is the same dependency-extraction rule
# major-bumps-level.sh applies, so the hints cannot disagree with the audit that
# will run next time. Path-only dependencies (req "*") are never rewritten and never
# force a bump, so they are not reported.
#
# Results are grouped by the major-bumped dependency: one major usually affects many
# crates, and the dependency is what the operator acts on.
#
# The check is informational: it never fails and it never touches the tree. Diagnostics
# go to stdout (the caller's job log); --out-markdown receives the same result rendered
# for the step summary and the PR body, and is written even when nothing is affected (as
# an empty file), so callers can consume it unconditionally.

set -euo pipefail

API_CHANGES=""
OUT_MARKDOWN=""

usage() {
    echo "Usage: $0 --api-changes FILE [--out-markdown FILE]"
    echo ""
    echo "Options:"
    echo "  --api-changes FILE    Audited release set from release-version-major-bumps.sh (required)"
    echo "  --out-markdown FILE   Where to write the hints as a markdown section"
    echo "                        (empty file when nothing is affected)"
    echo "  --help, -h            Show this message"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --api-changes)  API_CHANGES="${2:?--api-changes needs a value}"; shift 2 ;;
        --out-markdown) OUT_MARKDOWN="${2:?--out-markdown needs a value}"; shift 2 ;;
        --help|-h)      usage; exit 0 ;;
        *)              echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

[ -n "$API_CHANGES" ] || { echo "ERROR: --api-changes is required" >&2; exit 1; }
[ -f "$API_CHANGES" ] || { echo "ERROR: not a file: $API_CHANGES" >&2; exit 1; }
jq -e 'type == "array"' "$API_CHANGES" >/dev/null \
    || { echo "ERROR: $API_CHANGES is not a JSON array" >&2; exit 1; }

write_markdown() {
    # write_markdown HINTS_JSON — renders the result to --out-markdown, if requested.
    local hints="$1"
    [ -n "$OUT_MARKDOWN" ] || return 0
    if [ "$(printf '%s' "$hints" | jq 'length')" -eq 0 ]; then
        : > "$OUT_MARKDOWN"
        return 0
    fi
    printf '%s' "$hints" | jq -r '
        [
          "### :warning: Crates left out of this proposal affected by its major bumps",
          "",
          "These publishable workspace crates are not part of this release, but a crate they depend on directly went to a new major version here. Their dependency requirement was rewritten on this branch while their published version still requires the old major, so their next release proposal will be force-bumped to `major`. Include them in this release if that is not what you want.",
          "",
          (.[] | "- `\(.dependency)` `\(.previous_version)` → `\(.new_version)` affects: "
                 + (.affected | map("`\(.name)`") | join(", ")))
        ] | join("\n")
    ' > "$OUT_MARKDOWN"
}

MAJORS=$(jq -c '[.[] | select(.level == "major" and .initial_release != "true") | .name]' "$API_CHANGES")
if [ "$MAJORS" = "[]" ]; then
    echo "No major bumps in this proposal; no crates left out can be affected."
    write_markdown '[]'
    exit 0
fi

echo "Checking crates left out of the proposal against major bumps: $(printf '%s' "$MAJORS" | jq -r 'join(", ")')"

# One workspace-wide `cargo metadata` pass. The tree is the proposal branch tip, so the
# requirements read here already include this run's cargo-release rewrites.
read -r -d '' JQ_HINTS << 'EOF' || true
def first_num(s): ((s | tostring | capture("(?<n>[0-9]+)") | .n | tonumber)? // null);

($api[0] | map(.name)) as $released
| ( $api[0]
    | map(select(.level == "major" and .initial_release != "true"))
    | map(. as $r
          | { name: $r.name,
              new_version: $r.version,
              # prev_tag is "<crate>-v<version>"; empty/null only for initial releases,
              # which are filtered out above.
              previous_version: (($r.prev_tag // "") | ltrimstr($r.name + "-v")) })
  ) as $majors
| [ .packages[]
    | select(.publish == null or (.publish | type == "array" and length > 0))
    | select((.name | IN($released[])) | not)
    | { name: .name,
        version: .version,
        reqs: ( [ .dependencies[]
                  | select(.kind != "dev" and .kind != "build")
                  | {key: .name, value: .req} ]
                # Target-specific duplicates: prefer a real requirement over a path-only
                # "*", matching major-bumps-level.sh.
                | group_by(.key)
                | map(sort_by(if .value == "*" then 1 else 0 end) | .[0])
                | from_entries ) } ] as $outsiders
| [ $majors[]
    | . as $m
    | select(first_num($m.new_version) != first_num($m.previous_version))
    | [ $outsiders[]
        | . as $o
        | ($o.reqs[$m.name] // "") as $req
        | select($req != "" and $req != "*")
        # Only when the requirement now names the new major: anything else was not
        # rewritten by this proposal and will not force a bump next time.
        | select(first_num($req) == first_num($m.new_version))
        | { name: $o.name, version: $o.version, req: $req } ] as $affected
    | select(($affected | length) > 0)
    | { dependency: $m.name,
        previous_version: $m.previous_version,
        new_version: $m.new_version,
        affected: ($affected | sort_by(.name)) } ]
| sort_by(.dependency)
EOF

HINTS=$(cargo metadata --format-version=1 --no-deps \
    | jq -c --slurpfile api "$API_CHANGES" "$JQ_HINTS")

COUNT=$(printf '%s' "$HINTS" | jq '[.[].affected[].name] | unique | length')
if [ "$COUNT" -eq 0 ]; then
    echo "No crate left out of the proposal is affected by its major bumps."
else
    echo "$COUNT crate(s) left out of the proposal are affected by its major bumps:"
    printf '%s' "$HINTS" | jq -r '
        .[] | "  - \(.dependency) \(.previous_version) -> \(.new_version) affects "
              + "\(.affected | length) crate(s): "
              + (.affected | map(.name) | join(", "))'
    echo ""
    echo "Their dependency requirements were rewritten on this branch but they are not being"
    echo "released, so their next release proposal will be force-bumped to major. Consider"
    echo "including them in this release."
fi

write_markdown "$HINTS"

#!/bin/bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0


VERBOSE=false

# Use GITHUB_OUTPUT from environment or default to /dev/stdout for local testing
if [ -z "$GITHUB_OUTPUT" ]; then
    GITHUB_OUTPUT=/dev/stdout
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        -v|--verbose)
            VERBOSE=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [-v] [-h] CRATE BASE_REF CURRENT_REF"
            exit 0
            ;;
        -*)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
        *)
            # Stop parsing flags, rest are positional
            break
            ;;
    esac
done

CRATE="${1:?ERROR: CRATE is required}"
BASE_REF="${2:-main}"
CURRENT_REF="${3:-HEAD}"

log_verbose() {
    if [ "$VERBOSE" = true ]; then
        echo "$@" >&2
    fi
}

# Echo the higher of two semver levels. Order: major > minor > patch > none.
max_level() {
    local a=$1 b=$2
    local ra rb
    case "$a" in
        major) ra=3 ;;
        minor) ra=2 ;;
        patch) ra=1 ;;
        *)     ra=0 ;;
    esac
    case "$b" in
        major) rb=3 ;;
        minor) rb=2 ;;
        patch) rb=1 ;;
        *)     rb=0 ;;
    esac
    if (( ra >= rb )); then
        echo "$a"
    else
        echo "$b"
    fi
}

# Normalize a cargo-public-api signature line so that only semver-significant
# differences remain. Reads lines on stdin and writes normalized lines out.
# It drops the leading +/- diff marker, removes attribute tokens (`#[...]`) and
# the `const`/`async`/`unsafe` qualifiers, and collapses whitespace. Those
# qualifier/attribute deltas are either non-breaking (adding `#[repr(C)]`,
# making a fn `const`) or already covered by cargo-semver-checks' own lints, so
# stripping them lets us tell a real signature change (e.g. a parameter or
# return type change) from cosmetic churn under "Changed items".
normalize_api_line() {
    sed -E 's/^[+-]//; s/#\[[^]]*\]//g; s/\b(const|async|unsafe)\b//g; s/[[:space:]]+/ /g; s/^ //; s/ $//'
}

compute_semver_results() {
    local crate=$1
    local baseline=$2
    local current=$3

    # If current is not provided set it to the tip of the branch
    if [ -z "$current" ]; then
        current="HEAD"
    fi

    # Fetch base commit
    git fetch origin "$baseline" --quiet

    # Ensure baseline has origin/ prefix if it doesn't already (skip for tags: refs/tags/...)
    if [[ ! "$baseline" =~ ^origin/ ]] && [[ "$baseline" != *"refs/tags"* ]]; then
        baseline="origin/$baseline"
    fi

    log_verbose "========================================"
    log_verbose "Checking semver for: $crate"
    log_verbose "Using baseline ref: $baseline"
    log_verbose "========================================"

    # ----------------------------------------------------------------
    # 1) cargo-semver-checks (type-signature lints)
    # ----------------------------------------------------------------
    local semver_level="none"
    local semver_reason=""
    local semver_details=""
    local crate_is_new=false

    SEMVER_OUTPUT=$(cargo semver-checks -p "$crate" --color=never --all-features --baseline-rev "$baseline" 2>&1)
    SEMVER_EXIT_CODE=$?

    if [[ $SEMVER_EXIT_CODE -eq 0 ]]; then
        log_verbose "cargo-semver-checks: no violations"
        semver_level="none"
    elif [[ $SEMVER_EXIT_CODE -eq 1 ]]; then
        if grep -qE "Summary semver requires new major version" <<< "$SEMVER_OUTPUT"; then
            semver_level="major"
            semver_reason="cargo-semver-checks detected breaking changes"
            semver_details=$(grep -A 1000 "^--- failure" <<< "$SEMVER_OUTPUT" | head -100 || tail -50 <<< "$SEMVER_OUTPUT")
            log_verbose "cargo-semver-checks: major"
        elif grep -qF "package \`$crate\` not found" <<< "$SEMVER_OUTPUT"; then
            # The crate doesn't exist in the baseline — it's a new crate being added
            semver_level="minor"
            semver_reason="New crate (not present in baseline)"
            crate_is_new=true
            log_verbose "cargo-semver-checks: new crate, treat as minor"
        elif grep -qE "Summary semver requires new minor version" <<< "$SEMVER_OUTPUT"; then
            semver_level="minor"
            semver_reason="cargo-semver-checks detected minor breaking changes"
            semver_details=$(grep -A 1000 "^--- failure" <<< "$SEMVER_OUTPUT" | head -100 || tail -50 <<< "$SEMVER_OUTPUT")
            log_verbose "cargo-semver-checks: minor"
        else
            echo "Error running cargo-semver-checks: $SEMVER_OUTPUT" >&2
            exit $SEMVER_EXIT_CODE
        fi
    else
        echo "Unexpected exit code from cargo-semver-checks: $SEMVER_EXIT_CODE" >&2
        echo "$SEMVER_OUTPUT" >&2
        exit $SEMVER_EXIT_CODE
    fi

    # ----------------------------------------------------------------
    # 2) cargo-public-api diff
    #
    # cargo-semver-checks has known false-negatives at signature level — most
    # notably, parameter type changes on non-generic functions are not detected
    # (the function_parameter_type_changed lint is not implemented). Such a change
    # keeps the item's path, so cargo-public-api reports it under "Changed items"
    # as a "-old / +new" signature pair (not as a Removed + Added pair). We
    # therefore run cargo-public-api unconditionally and, for changed items,
    # normalize and compare the old vs new signatures (see the Changed handling
    # below) before combining the result with semver-checks via max_level. Skip
    # only when there is no baseline (new crate) or when semver-checks already
    # flagged major (cannot go higher).
    #
    # Requires cargo-public-api >= 0.52.0: earlier versions include function
    # parameter names in signatures, so a non-breaking parameter *rename* also
    # surfaces under "Changed items" (-old / +new differing only by the name) and
    # would be falsely promoted to major. From 0.52.0 parameter names are omitted
    # by default, so a rename produces no diff and only signature-meaningful
    # changes (e.g. parameter or return type changes) surface there.
    # ----------------------------------------------------------------
    local public_api_level="none"
    local public_api_reason=""
    local public_api_details=""

    if $crate_is_new; then
        log_verbose "Skipping cargo-public-api: new crate (no baseline)"
    elif [[ "$semver_level" == "major" ]]; then
        log_verbose "Skipping cargo-public-api: cargo-semver-checks already at major"
    else
        PUBLIC_API_OUTPUT=$(cargo public-api --package "$crate" --color=never diff "$baseline..$current" 2>&1)
        EXIT_CODE=$?

        if [[ $EXIT_CODE -ne 0 ]]; then
          echo "Unexpected error from cargo-public-api for $crate (exit code: $EXIT_CODE)" >&2
          echo "$PUBLIC_API_OUTPUT" >&2
          exit $EXIT_CODE
        fi

        log_verbose "$PUBLIC_API_OUTPUT"

        # Removed public items → major.
        local removed_breaking=false
        if grep -q "Removed items from the public API$" <<< "$PUBLIC_API_OUTPUT" \
           && ! grep -A 2 "^Removed items from the public API$" <<< "$PUBLIC_API_OUTPUT" | grep -q "^(none)$"; then
            removed_breaking=true
        fi

        # Changed public items → breaking only if a semver-significant delta
        # survives normalization. This is the case cargo-semver-checks misses:
        # a parameter/return type change on a non-generic fn renders here as a
        # "-old / +new" signature pair rather than as Removed+Added. We compare
        # the normalized old vs new signatures; if they still differ after
        # stripping non-breaking churn (#[repr(C)] additions, const/async/unsafe
        # qualifiers — see normalize_api_line), the change is breaking → major.
        local changed_breaking=false
        local changed_section
        changed_section=$(sed -n '/^Changed items in the public API$/,/^Added items to the public API$/p' <<< "$PUBLIC_API_OUTPUT")
        if [[ -n "$changed_section" ]] \
           && ! grep -A 2 "^Changed items in the public API$" <<< "$PUBLIC_API_OUTPUT" | grep -q "^(none)$"; then
            local changed_old changed_new
            changed_old=$(grep '^-' <<< "$changed_section" | normalize_api_line | sort)
            changed_new=$(grep '^+' <<< "$changed_section" | normalize_api_line | sort)
            if [[ "$changed_old" != "$changed_new" ]]; then
                changed_breaking=true
            else
                log_verbose "cargo-public-api: changed items are non-breaking (attribute/qualifier only)"
            fi
        fi

        # Added public items → minor.
        local added=false
        if grep -q "Added items to the public API$" <<< "$PUBLIC_API_OUTPUT" \
           && ! grep -A 2 "^Added items to the public API$" <<< "$PUBLIC_API_OUTPUT" | grep -q "^(none)"; then
            added=true
        fi

        if $removed_breaking; then
            public_api_level="major"
            public_api_reason="cargo-public-api detected removed public API items"
            public_api_details=$(grep -A 50 "^Removed items from the public API$" <<< "$PUBLIC_API_OUTPUT" | head -50)
            log_verbose "cargo-public-api: major (removed items)"
        elif $changed_breaking; then
            public_api_level="major"
            public_api_reason="cargo-public-api detected breaking signature changes"
            public_api_details=$(head -50 <<< "$changed_section")
            log_verbose "cargo-public-api: major (changed signatures)"
        elif $added; then
            public_api_level="minor"
            public_api_reason="cargo-public-api detected new public API items"
            public_api_details=$(grep -A 50 "^Added items to the public API$" <<< "$PUBLIC_API_OUTPUT" | head -50)
            log_verbose "cargo-public-api: minor (added items)"
        fi
    fi

    # ----------------------------------------------------------------
    # 3) Combine signals: take the higher of cargo-semver-checks and cargo-public-api.
    # ----------------------------------------------------------------
    LEVEL=$(max_level "$semver_level" "$public_api_level")
    if [[ "$LEVEL" == "$public_api_level" && "$public_api_level" != "$semver_level" ]]; then
        REASON="$public_api_reason"
        DETAILS="$public_api_details"
    else
        REASON="$semver_reason"
        DETAILS="$semver_details"
    fi

    if [[ "$LEVEL" == "none" ]]; then
        LEVEL="patch"
        REASON="No public API changes detected"
    fi

    echo "$(jq -n \
        --arg name "$crate" \
        --arg level "$LEVEL" \
        --arg reason "$REASON" \
        --arg details "$DETAILS" \
        '{"name": $name, "level": $level, "reason": $reason, "details": $details}')"
}

# Run the computation and capture JSON output
RESULT_JSON=$(compute_semver_results "$CRATE" "$BASE_REF" "$CURRENT_REF")

# Output JSON to stdout (captured by workflow)
echo "$RESULT_JSON"

# Extract values from JSON for backwards compatibility / local testing
NAME=$(echo "$RESULT_JSON" | jq -r '.name')
LEVEL=$(echo "$RESULT_JSON" | jq -r '.level')

# For local testing, also output individual values
if [[ "$GITHUB_OUTPUT" == "/dev/stdout" ]]; then
  echo "---" >&2
  echo "crate=$NAME" >&2
  echo "semver_level=$LEVEL" >&2
fi

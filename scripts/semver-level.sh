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

compute_semver_results() {
    local crate=$1
    local baseline=$2
    local current=$3

    # If current is not provided set it to the tip of the branch
    if [ -z "$current" ]; then
        current="HEAD"
    fi

    # Fetch base commit
    git fetch origin "$baseline" --depth=50 --quiet

    # Ensure baseline has origin/ prefix if it doesn't already
    if [[ ! "$baseline" =~ ^origin/ ]]; then
        baseline="origin/$baseline"
    fi

    log_verbose "========================================" >&2
    log_verbose "Checking semver for: $crate" >&2
    log_verbose "Using baseline ref: $baseline" >&2
    log_verbose "========================================" >&2

    LEVEL="none"

    SEMVER_OUTPUT=$(cargo semver-checks -p "$crate" --color=never --all-features --baseline-rev "$baseline" 2>&1)
    SEMVER_EXIT_CODE=$?

    if [[ $SEMVER_EXIT_CODE -eq 0 ]]; then
        log_verbose "No semver violations detected" >&2
        LEVEL="none"
    elif [[ $SEMVER_EXIT_CODE -eq 1 ]]; then
        # Check if it's an error or actual semver violation
        if echo "$SEMVER_OUTPUT" | grep -qE "Summary semver requires new major version"; then
            # It's a semver violation - this is a major change
            LEVEL="major"
            log_verbose "Detected semver violations (major change)" >&2
            log_verbose "$SEMVER_OUTPUT" >&2
        else
            echo "Error running cargo-semver-checks: $SEMVER_OUTPUT" >&2
            exit $SEMVER_EXIT_CODE
        fi
    else
        echo "Unexpected exit code from cargo-semver-checks: $SEMVER_EXIT_CODE" >&2
        echo "$SEMVER_OUTPUT" >&2
        exit $SEMVER_EXIT_CODE
    fi


    if [[ "$LEVEL" == "none" ]]; then
        # Try to run cargo-public-api diff against base branch
        PUBLIC_API_OUTPUT=$(cargo public-api --package "$crate" --color=never diff "$baseline..$current" 2>&1)
        EXIT_CODE=$?

        if [[ $EXIT_CODE -ne 0 ]]; then
          echo "Unexpected error for $crate (exit code: $EXIT_CODE)" >&2
          echo "$PUBLIC_API_OUTPUT" >&2
          exit $EXIT_CODE
        fi

        log_verbose "$PUBLIC_API_OUTPUT"

        # Check for removed items (major change)
        if echo "$PUBLIC_API_OUTPUT" | grep -q "Removed items from the public API$"; then
          if ! echo "$PUBLIC_API_OUTPUT" | grep -A 2 "^Removed items from the public API$" | grep -q "^(none)$"; then
            LEVEL="major"
            log_verbose "Detected removed items (major change)" >&2
          fi
        fi

        # Check for changed items (major change)
        if echo "$PUBLIC_API_OUTPUT" | grep -q "^Changed items in the public API$"; then
          if ! echo "$PUBLIC_API_OUTPUT" | grep -A 2 "^Changed items in the public API$" | grep -q "^(none)$"; then
            LEVEL="major"
            log_verbose "Detected changed items (major change)" >&2
          fi
        fi

        # Check for added items (minor change) - only if not already major
        if [[ "$LEVEL" != "major" ]]; then
          if echo "$PUBLIC_API_OUTPUT" | grep -q "Added items to the public API$"; then
            if ! echo "$PUBLIC_API_OUTPUT" | grep -A 2 "^Added items to the public API$" | grep -q "^(none)"; then
              LEVEL="minor"
              log_verbose "Detected added items (minor change)" >&2
            fi
          fi
        fi
    fi

    if [[ "$LEVEL" == "none" ]]; then
      # No API changes detected, assume patch level
      LEVEL="patch"
    fi
    
    echo "$(jq -n \
        --arg name "$crate" \
        --arg level "$LEVEL" \
        '{"name": $name, "level": $level}')"
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

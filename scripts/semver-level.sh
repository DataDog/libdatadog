#!/bin/bash

# Accept arguments from command line
CRATES_JSON="${1:-[]}"
BASE_REF="${2:-main}"
CURRENT_REF="${3:-HEAD}"

# Use GITHUB_OUTPUT from environment or default to /dev/stdout for local testing
if [ -z "$GITHUB_OUTPUT" ]; then
    GITHUB_OUTPUT=/dev/stdout
fi

compute_semver_results() {
    local crates=$1
    local baseline=$2
    local current=$3
    local highest_level="none"
    local -a crates_checked=()

    # If current is not provided set it to the tip of the branch
    if [ -z "$current" ]; then
        current="HEAD"
    fi

    # Fetch base commit
    git fetch origin "$baseline" --depth=50

    # Parse JSON array
    readarray -t CRATES < <(echo "$crates" | jq -r '.[]')


    for CRATE_NAME in "${CRATES[@]}"; do
    echo "========================================" >&2
    echo "Checking semver for: $CRATE_NAME" >&2
    echo "========================================" >&2

    LEVEL="none"

    SEMVER_OUTPUT=$(cargo semver-checks -p "$CRATE_NAME" --all-features --baseline-rev "$baseline" 2>&1)
    SEMVER_EXIT_CODE=$?

    if [[ $SEMVER_EXIT_CODE -eq 0 ]]; then
        echo "No semver violations detected" >&2
        LEVEL="none"
    elif [[ $SEMVER_EXIT_CODE -eq 1 ]]; then
        # Check if it's an error or actual semver violation
        if echo "$SEMVER_OUTPUT" | grep -qE "(error:|Error|failed to|could not|unable to)"; then
            echo "Error running cargo-semver-checks: $SEMVER_OUTPUT" >&2
            exit $SEMVER_EXIT_CODE
        else
            # It's a semver violation - this is a major change
            LEVEL="major"
            echo "Detected semver violations (major change)" >&2
            echo "$SEMVER_OUTPUT" >&2
        fi
    else
        echo "Unexpected exit code from cargo-semver-checks: $SEMVER_EXIT_CODE" >&2
        exit $SEMVER_EXIT_CODE
    fi


    if [[ "$LEVEL" == "none" ]]; then
        # Try to run cargo-public-api diff against base branch
        PUBLIC_API_OUTPUT=$(cargo public-api --package "$CRATE_NAME" diff "$baseline..$current" 2>&1)
        EXIT_CODE=$?

        if [[ $EXIT_CODE -ne 0 ]]; then
          echo "Unexpected error for $CRATE_NAME (exit code: $EXIT_CODE)" >&2
          exit $EXIT_CODE
        fi


        # Check for removed items (major change)
        if echo "$PUBLIC_API_OUTPUT" | grep -q "Removed items from the public API$"; then
          if ! echo "$PUBLIC_API_OUTPUT" | grep -A 2 "^Removed items from the public API$" | grep -q "^(none)$"; then
            LEVEL="major"
            echo "Detected removed items (major change)" >&2
          fi
        fi

        # Check for changed items (major change)
        if echo "$PUBLIC_API_OUTPUT" | grep -q "^Changed items in the public API$"; then
          if ! echo "$PUBLIC_API_OUTPUT" | grep -A 2 "^Changed items in the public API$" | grep -q "^(none)$"; then
            LEVEL="major"
            echo "Detected changed items (major change)" >&2
          fi
        fi

        # Check for added items (minor change) - only if not already major
        if [[ "$LEVEL" != "major" ]]; then
          if echo "$PUBLIC_API_OUTPUT" | grep -q "Added items to the public API$"; then
            if ! echo "$PUBLIC_API_OUTPUT" | grep -A 2 "^Added items to the public API$" | grep -q "^(none)"; then
              LEVEL="minor"
              echo "Detected added items (minor change)" >&2
            fi
          fi
        fi
    fi

    # If we detected changes, update the highest level
    if [[ "$LEVEL" != "none" ]]; then
      crates_checked+=("$CRATE_NAME:$LEVEL")

      # Update highest level
      if [[ "$LEVEL" == "major" ]]; then
        highest_level="major"
      elif [[ "$LEVEL" == "minor" ]] && [[ "$highest_level" != "major" ]]; then
        highest_level="minor"
      elif [[ "$highest_level" == "none" ]]; then
        highest_level="patch"
      fi
    else
      # No API changes detected, assume patch level
      crates_checked+=("$CRATE_NAME:patch")
      if [[ "$highest_level" == "none" ]]; then
        highest_level="patch"
      fi
    fi
    done

    # Build JSON output
    local crates_json="[]"
    for crate_entry in "${crates_checked[@]}"; do
        IFS=':' read -r name level <<< "$crate_entry"
        crates_json=$(echo "$crates_json" | jq --arg name "$name" --arg level "$level" '. += [{"name": $name, "level": $level}]')
    done

    # Create final JSON object
    jq -n \
        --arg highest_level "$highest_level" \
        --argjson crates "$crates_json" \
        '{highest_level: $highest_level, crates: $crates}'
}

# Run the computation and capture JSON output
RESULT_JSON=$(compute_semver_results "$CRATES_JSON" "$BASE_REF" "$CURRENT_REF")

# Output JSON to stdout (captured by workflow)
echo "$RESULT_JSON"

# Extract values from JSON for backwards compatibility / local testing
HIGHEST_LEVEL=$(echo "$RESULT_JSON" | jq -r '.highest_level')
CRATES_CHECKED=$(echo "$RESULT_JSON" | jq -r '.crates | map("\(.name):\(.level)") | join(" ")')

# For local testing, also output individual values
if [[ "$GITHUB_OUTPUT" == "/dev/stdout" ]]; then
  echo "---" >&2
  echo "semver_level=$HIGHEST_LEVEL" >&2
  echo "crates_checked=$CRATES_CHECKED" >&2
fi


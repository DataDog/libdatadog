#!/bin/bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Publication Order Script
# Determines the correct order to publish workspace crates based on their dependencies
# Usage: ./publication-order.sh [OPTIONS] [CRATE...]
#
# If crate names are provided, shows only those crates and their dependencies in publication order.

set -euo pipefail

# Parse arguments
FORMAT="json"
INCLUDE_UNPUBLISHABLE=false
declare -a TARGET_CRATES=()

for arg in "$@"; do
    case "$arg" in
        --format=*)
            FORMAT="${arg#--format=}"
            ;;
        --include-unpublishable|--all)
            INCLUDE_UNPUBLISHABLE=true
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS] [CRATE...]"
            echo ""
            echo "Options:"
            echo "  --format=FORMAT           Output format: list, json, simple (default: json)"
            echo "  --include-unpublishable   Include crates marked with publish=false"
            echo "  --all                     Alias for --include-unpublishable"
            echo "  --help, -h                Show this help message"
            echo ""
            echo "Arguments:"
            echo "  CRATE...                  Optional list of crate names to filter by."
            echo "                            If provided, shows only these crates and their"
            echo "                            dependencies in publication order."
            echo ""
            echo "Examples:"
            echo "  $0 --format=list"
            echo "  $0 --format=simple --include-unpublishable"
            echo "  $0 libdd-common libdd-telemetry"
            echo "  $0 --format=json datadog-sidecar"
            exit 0
            ;;
        -*)
            echo "Unknown option: $arg" >&2
            echo "Use --help for usage information" >&2
            exit 1
            ;;
        *)
            # Positional argument - treat as crate name
            TARGET_CRATES+=("$arg")
            ;;
    esac
done

# Get workspace metadata as JSON
METADATA=$(cargo metadata --format-version=1 --no-deps)

# Extract workspace members (package IDs)
if [ "$INCLUDE_UNPUBLISHABLE" = true ]; then
    # Include all workspace members
    WORKSPACE_MEMBERS=$(echo "$METADATA" | jq -r '.workspace_members[]')
else
    # Exclude packages with publish = false
    WORKSPACE_MEMBERS=$(echo "$METADATA" | jq -r '
      .workspace_members[] as $id |
      .packages[] | 
      select(.id == $id) |
      select(.publish == null or (.publish | type == "array" and length > 0)) |
      .id
    ')
fi

# Create associative arrays for package info
declare -A PKG_NAME_TO_ID
declare -A PKG_ID_TO_NAME
declare -A PKG_DEPS

# Build package mapping
while IFS= read -r pkg_id; do
    pkg_name=$(echo "$METADATA" | jq -r --arg id "$pkg_id" \
        '.packages[] | select(.id == $id) | .name')
    PKG_NAME_TO_ID["$pkg_name"]="$pkg_id"
    PKG_ID_TO_NAME["$pkg_id"]="$pkg_name"
done <<< "$WORKSPACE_MEMBERS"

# Extract dependencies for each workspace member
while IFS= read -r pkg_id; do
    pkg_name="${PKG_ID_TO_NAME[$pkg_id]}"
    
    # Get all dependencies that are workspace members (excluding dev-dependencies and self-references)
    # Also filter to only include dependencies that are publishable
    deps=$(echo "$METADATA" | jq -r --arg id "$pkg_id" --arg pkg_name "$pkg_name" \
        '.packages[] | select(.id == $id) | 
        .dependencies[] | 
        select(.path != null and .kind != "dev" and .name != $pkg_name) | 
        .name' | sort | uniq | while IFS= read -r dep; do
            # Only include if dependency is in our publishable list
            if [ -n "${PKG_NAME_TO_ID[$dep]+x}" ]; then
                echo "$dep"
            fi
        done)
    
    # Store dependencies
    PKG_DEPS["$pkg_name"]="$deps"
done <<< "$WORKSPACE_MEMBERS"

# If target crates were specified, filter to only include them and their dependencies
if [ ${#TARGET_CRATES[@]} -gt 0 ]; then
    # Validate that all target crates exist
    for crate in "${TARGET_CRATES[@]}"; do
        if [ -z "${PKG_NAME_TO_ID[$crate]+x}" ]; then
            echo "ERROR: Unknown crate '$crate'" >&2
            echo "Available crates:" >&2
            for name in "${!PKG_NAME_TO_ID[@]}"; do
                echo "  - $name" >&2
            done | sort >&2
            exit 1
        fi
    done
    
    # Recursively collect all dependencies of target crates
    declare -A INCLUDED_CRATES
    declare -a TO_PROCESS=("${TARGET_CRATES[@]}")
    
    while [ ${#TO_PROCESS[@]} -gt 0 ]; do
        current="${TO_PROCESS[0]}"
        TO_PROCESS=("${TO_PROCESS[@]:1}")
        
        # Skip if already processed
        if [ -n "${INCLUDED_CRATES[$current]+x}" ]; then
            continue
        fi
        
        INCLUDED_CRATES["$current"]=1
        
        # Add dependencies to process queue
        deps="${PKG_DEPS[$current]}"
        if [ -n "$deps" ]; then
            while IFS= read -r dep; do
                if [ -n "$dep" ] && [ -z "${INCLUDED_CRATES[$dep]+x}" ]; then
                    TO_PROCESS+=("$dep")
                fi
            done <<< "$deps"
        fi
    done
    
    # Filter PKG_DEPS to only include the selected crates
    declare -A FILTERED_PKG_DEPS
    # Sort for deterministic order
    for crate in $(printf '%s\n' "${!INCLUDED_CRATES[@]}" | sort); do
        # Filter dependencies to only include other selected crates
        deps="${PKG_DEPS[$crate]}"
        filtered_deps=""
        if [ -n "$deps" ]; then
            while IFS= read -r dep; do
                if [ -n "$dep" ] && [ -n "${INCLUDED_CRATES[$dep]+x}" ]; then
                    if [ -n "$filtered_deps" ]; then
                        filtered_deps="$filtered_deps"$'\n'"$dep"
                    else
                        filtered_deps="$dep"
                    fi
                fi
            done <<< "$deps"
        fi
        FILTERED_PKG_DEPS["$crate"]="$filtered_deps"
    done
    
    # Replace PKG_DEPS with filtered version
    unset PKG_DEPS
    declare -A PKG_DEPS
    # Sort for deterministic order
    for crate in $(printf '%s\n' "${!FILTERED_PKG_DEPS[@]}" | sort); do
        PKG_DEPS["$crate"]="${FILTERED_PKG_DEPS[$crate]}"
    done
fi

# Topological sort using Kahn's algorithm
declare -A IN_DEGREE
declare -a SORTED_ORDER
declare -a QUEUE

# Calculate in-degrees (number of workspace dependencies each package has)
# Sort for consistent ordering (though order doesn't matter here)
for pkg_name in $(printf '%s\n' "${!PKG_DEPS[@]}" | sort); do
    deps="${PKG_DEPS[$pkg_name]}"
    count=0
    if [ -n "$deps" ]; then
        while IFS= read -r dep; do
            if [ -n "$dep" ]; then
                ((count++)) || true
            fi
        done <<< "$deps"
    fi
    IN_DEGREE["$pkg_name"]=$count
done

# Find all packages with no dependencies (in-degree = 0)
# Sort to ensure deterministic order
for pkg_name in $(printf '%s\n' "${!IN_DEGREE[@]}" | sort); do
    if [ "${IN_DEGREE[$pkg_name]}" -eq 0 ]; then
        QUEUE+=("$pkg_name")
    fi
done

# Process queue
while [ ${#QUEUE[@]} -gt 0 ]; do
    # Pop from queue
    current="${QUEUE[0]}"
    QUEUE=("${QUEUE[@]:1}")
    
    SORTED_ORDER+=("$current")
    
    # For each package that depends on current, reduce its in-degree
    # Sort to ensure deterministic order when multiple packages become ready
    for pkg_name in $(printf '%s\n' "${!PKG_DEPS[@]}" | sort); do
        deps="${PKG_DEPS[$pkg_name]}"
        if [ -n "$deps" ] && echo "$deps" | grep -qx "$current"; then
            ((IN_DEGREE["$pkg_name"]--)) || true
            if [ "${IN_DEGREE[$pkg_name]}" -eq 0 ]; then
                QUEUE+=("$pkg_name")
            fi
        fi
    done
done

# Check for cycles
if [ ${#SORTED_ORDER[@]} -ne ${#PKG_DEPS[@]} ]; then
    echo "ERROR: Circular dependency detected!" >&2
    echo "Processed: ${#SORTED_ORDER[@]} packages" >&2
    echo "Total: ${#PKG_DEPS[@]} packages" >&2
    exit 1
fi

# Output in requested format
case "$FORMAT" in
    list)
        if [ ${#TARGET_CRATES[@]} -gt 0 ]; then
            echo "Publication order for: ${TARGET_CRATES[*]}"
            echo "(including dependencies)"
        else
            echo "Publication order (dependencies first):"
        fi
        echo "========================================"
        for i in "${!SORTED_ORDER[@]}"; do
            pkg_name="${SORTED_ORDER[$i]}"
            deps="${PKG_DEPS[$pkg_name]}"
            pkg_id="${PKG_NAME_TO_ID[$pkg_name]}"
            
            # Get package version and publishable status
            pkg_info=$(echo "$METADATA" | jq -r --arg id "$pkg_id" \
                '.packages[] | select(.id == $id) | 
                {version: .version, publishable: (if .publish == null or (.publish | type == "array" and length > 0) then "true" else "false" end)} | 
                "\(.version)|\(.publishable)"')
            
            version=$(echo "$pkg_info" | cut -d'|' -f1)
            is_publishable=$(echo "$pkg_info" | cut -d'|' -f2)
            
            if [ "$is_publishable" = "false" ]; then
                echo "$((i+1)). $pkg_name ($version) [unpublishable]"
            else
                echo "$((i+1)). $pkg_name ($version)"
            fi
            
            if [ -n "$deps" ]; then
                # Add versions to dependencies
                deps_with_versions=""
                while IFS= read -r dep; do
                    if [ -n "$dep" ]; then
                        dep_id="${PKG_NAME_TO_ID[$dep]}"
                        dep_version=$(echo "$METADATA" | jq -r --arg id "$dep_id" \
                            '.packages[] | select(.id == $id) | .version')
                        deps_with_versions="$deps_with_versions $dep ($dep_version)"
                    fi
                done <<< "$deps"
                echo "   Dependencies:$deps_with_versions"
            fi
        done
        ;;
    
    json)
        # Output as JSON array with name and version
        printf '['
        for i in "${!SORTED_ORDER[@]}"; do
            [ $i -gt 0 ] && printf ','
            pkg_name="${SORTED_ORDER[$i]}"
            pkg_id="${PKG_NAME_TO_ID[$pkg_name]}"
            version=$(echo "$METADATA" | jq -r --arg id "$pkg_id" \
                '.packages[] | select(.id == $id) | .version')
            printf '{"name":"%s","version":"%s"}' "$pkg_name" "$version"
        done
        printf ']\n'
        ;;
    
    simple)
        # Just the names, one per line
        for pkg_name in "${SORTED_ORDER[@]}"; do
            echo "$pkg_name"
        done
        ;;
    
    *)
        echo "Unknown format: $FORMAT" >&2
        echo "Available formats: list, json, simple" >&2
        exit 1
        ;;
esac


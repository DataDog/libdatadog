#!/bin/bash
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0
#
# Validates Cargo.toml metadata for publishable crates:
# - Checks that description, repository, and homepage are present
# - Checks that dependencies on libdd-* crates include both path and version

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

errors=0
checked=0
skipped=0
VERBOSE=0
CRATE_PATHS=()

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -v|--verbose)
            VERBOSE=1
            shift
            ;;
        -h|--help)
            echo "Usage: $(basename "$0") [-v|--verbose] [-h|--help] [CRATE_PATH...]"
            echo ""
            echo "Validates Cargo.toml metadata for publishable crates."
            echo ""
            echo "Arguments:"
            echo "  CRATE_PATH     Path to crate directory (can specify multiple)."
            echo "                 If none specified, checks all crates in workspace."
            echo ""
            echo "Options:"
            echo "  -v, --verbose  Show all crates, including skipped and passing ones"
            echo "  -h, --help     Show this help message"
            exit 0
            ;;
        -*)
            echo "Unknown option: $1"
            echo "Use -h or --help for usage information"
            exit 1
            ;;
        *)
            # Positional argument - treat as crate path
            CRATE_PATHS+=("$1")
            shift
            ;;
    esac
done

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

log_error() {
    echo -e "${RED}ERROR:${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}WARNING:${NC} $1"
}

log_success() {
    echo -e "${GREEN}OK:${NC} $1"
}

# Check if a Cargo.toml has publish = false
is_unpublished() {
    local file="$1"
    grep -qE '^\s*publish\s*=\s*false' "$file"
}

# Check if a field exists in the [package] section
check_package_field() {
    local file="$1"
    local field="$2"
    
    # Extract the [package] section and check for the field
    # This handles both direct values and workspace references
    # Use awk to get everything from [package] until the next section header
    if awk '/^\[package\]/{flag=1; next} /^\[/{flag=0} flag' "$file" | grep -qE "^\s*${field}\s*="; then
        return 0
    fi
    return 1
}

# Check dependencies on libdd-* crates
# Only checks [dependencies] and [build-dependencies], skips [dev-dependencies]
check_internal_dependencies() {
    local file="$1"
    local crate_name="$2"
    local has_error=0
    
    # Get the actual package name from the Cargo.toml
    local package_name
    package_name=$(awk '/^\[package\]/{flag=1; next} /^\[/{flag=0} flag && /^\s*name\s*=/ {gsub(/.*=\s*"/, ""); gsub(/".*/, ""); print; exit}' "$file")
    
    # Use awk to parse the file and extract dependencies, excluding dev-dependencies
    # This handles [dependencies], [build-dependencies], and target-specific deps
    # but skips [dev-dependencies] sections
    while IFS= read -r line; do
        # Skip empty lines
        [[ -z "$line" ]] && continue
        
        # Extract the dependency name
        dep_name=$(echo "$line" | sed -E 's/^\s*([a-zA-Z0-9_-]+)\s*=.*/\1/')
        
        # Skip self-references (crates that depend on themselves for testing)
        if [[ "$dep_name" == "$package_name" ]]; then
            continue
        fi
        
        # Check if it's an internal crate (libdd-*)
        if [[ "$dep_name" =~ ^(libdd-) ]]; then
            # Check if it has path
            if echo "$line" | grep -qE 'path\s*='; then
                # Check if it also has version
                if ! echo "$line" | grep -qE 'version\s*='; then
                    log_error "$crate_name: dependency '$dep_name' has 'path' but missing 'version'"
                    has_error=1
                fi
            fi
        fi
    done < <(awk '
        # Track which section we are in
        /^\[.*dev-dependencies\]/ { in_dev_deps = 1; next }
        /^\[.*dependencies\]/ && !/dev-dependencies/ { in_dev_deps = 0 }
        /^\[/ && !/dependencies/ { in_dev_deps = 0 }
        
        # Only print lines matching internal deps when NOT in dev-dependencies
        !in_dev_deps && /^\s*(libdd-)[a-zA-Z0-9_-]+\s*=/ { print }
    ' "$file" 2>/dev/null || true)
    
    return $has_error
}

# Main validation function
validate_cargo_toml() {
    local file="$1"
    local crate_dir=$(dirname "$file")
    local crate_name=$(basename "$crate_dir")
    local has_error=0
    
    # Skip the root workspace Cargo.toml
    if [[ "$file" == "$ROOT_DIR/Cargo.toml" ]]; then
        return 0
    fi
    
    # Skip unpublished crates
    if is_unpublished "$file"; then
        skipped=$((skipped + 1))
        [[ $VERBOSE -eq 1 ]] && echo "Skipping $crate_name (publish = false)"
        return 0
    fi
    
    checked=$((checked + 1))
    [[ $VERBOSE -eq 1 ]] && echo "Checking $crate_name..."
    
    # Check required metadata fields
    if ! check_package_field "$file" "description"; then
        log_error "$crate_name: missing 'description' in [package]"
        has_error=1
    fi
    
    if ! check_package_field "$file" "repository"; then
        log_error "$crate_name: missing 'repository' in [package]"
        has_error=1
    fi
    
    if ! check_package_field "$file" "homepage"; then
        log_error "$crate_name: missing 'homepage' in [package]"
        has_error=1
    fi
    
    # Check internal dependencies
    if ! check_internal_dependencies "$file" "$crate_name"; then
        has_error=1
    fi
    
    if [[ $has_error -eq 0 ]]; then
        [[ $VERBOSE -eq 1 ]] && log_success "$crate_name passed all checks"
    else
        errors=$((errors + 1))
    fi
    
    return 0
}

echo "========================================"
echo "Checking Cargo.toml metadata..."
echo "========================================"
echo ""

if [[ ${#CRATE_PATHS[@]} -gt 0 ]]; then
    # Check specific crates provided as arguments
    for crate_path in "${CRATE_PATHS[@]}"; do
        # Handle both absolute and relative paths
        if [[ "$crate_path" = /* ]]; then
            cargo_file="$crate_path/Cargo.toml"
        else
            cargo_file="$ROOT_DIR/$crate_path/Cargo.toml"
        fi
        
        if [[ -f "$cargo_file" ]]; then
            validate_cargo_toml "$cargo_file"
        else
            log_error "Cargo.toml not found at: $cargo_file"
            errors=$((errors + 1))
        fi
    done
else
    # Find all Cargo.toml files in the workspace
    while IFS= read -r cargo_file; do
        validate_cargo_toml "$cargo_file"
    done < <(find "$ROOT_DIR" -name "Cargo.toml" -not -path "*/target/*" -not -path "*/.git/*" | sort)
fi

echo ""
echo "========================================"
echo "Summary:"
echo "  Checked: $checked crates"
echo "  Skipped: $skipped crates (publish = false)"
echo "  Errors:  $errors crates with issues"
echo "========================================"

if [[ $errors -gt 0 ]]; then
    echo ""
    log_error "Validation failed! Please fix the issues above."
    exit 1
else
    echo ""
    log_success "All publishable crates have correct metadata!"
    exit 0
fi


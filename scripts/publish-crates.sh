#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -e
set -u

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Allowed crate names for publication (in dependency order)
ALLOWED_CRATES=(
    "libdd-common"
    "libdd-ddsketch"
    "libdd-tinybytes"
    "libdd-log"
    "libdd-telemetry"
    "libdd-trace-protobuf"
    "libdd-trace-normalization"
    "libdd-trace-stats"
    "libdd-trace-utils"
    "libdd-dogstatsd-client"
    "libdd-library-config"
    "libdd-crashtracker"
    "libdd-data-pipeline"
    "libdd-alloc"
    "libdd-profiling-protobuf"
    "libdd-profiling"
)

usage() {
    cat << EOF
Usage: $(basename "$0") [OPTIONS] <tag1> [tag2] [tag3] ...

Publishes Rust crates to crates.io based on git tags.

Arguments:
    <tag>               One or more git tags in format: {crate-name}-v{version}
                        Example: libdd-common-v1.0.0

Options:
    -h, --help              Show this help message
    -d, --dry-run           Perform a dry run without actually publishing
    -c, --check-published   Only check if crates are already published (no build/test/publish)
    -t, --token TOKEN       Cargo registry token (defaults to CARGO_REGISTRY_TOKEN env var)
    -v, --verbose           Enable verbose output

Environment Variables:
    CARGO_REGISTRY_TOKEN    Token for publishing to crates.io (required unless --token provided)

Examples:
    $(basename "$0") libdd-common-v1.0.0 libdd-telemetry-v2.0.0
    $(basename "$0") --dry-run libdd-common-v1.0.0
    $(basename "$0") --check-published libdd-common-v1.0.0 libdd-telemetry-v2.0.0
    $(basename "$0") --token "my-token" libdd-common-v1.0.0

EOF
}

check_command() {
    local cmd=$1
    if ! command -v "$cmd" &> /dev/null; then
        echo -e "${RED}❌ ERROR: Required command '$cmd' is not installed${NC}" >&2
        return 1
    fi
}

check_dependencies() {
    local check_only=${1:-false}
    local -a required_commands
    
    if [ "$check_only" = true ]; then
        required_commands=("curl" "jq")
    else
        required_commands=("curl" "jq" "cargo" "git")
    fi
    
    local missing_commands=()
    
    for cmd in "${required_commands[@]}"; do
        if ! command -v "$cmd" &> /dev/null; then
            missing_commands+=("$cmd")
        fi
    done
    
    if [ ${#missing_commands[@]} -gt 0 ]; then
        echo -e "${RED}❌ ERROR: Missing required dependencies:${NC}" >&2
        for cmd in "${missing_commands[@]}"; do
            echo -e "  ${RED}✗${NC} $cmd" >&2
        done
        echo "" >&2
        echo "Please install the missing dependencies and try again." >&2
        exit 1
    fi
}

validate_tag() {
    local tag=$1
    
    # Check format: {crate-name}-v{semver}
    if [[ ! "$tag" =~ ^(.+)-v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
        echo -e "${RED}❌ Invalid tag format: $tag${NC}" >&2
        echo "   Expected format: {crate-name}-v{version} (e.g., libdd-common-v1.0.0)" >&2
        return 1
    fi
    
    local crate_name="${BASH_REMATCH[1]}"
    
    # Check if crate is in allowed list
    local found=0
    for allowed in "${ALLOWED_CRATES[@]}"; do
        if [ "$crate_name" = "$allowed" ]; then
            found=1
            break
        fi
    done
    
    if [ $found -eq 0 ]; then
        echo -e "${YELLOW}⚠️  WARNING: Crate '$crate_name' is not in the allowed list${NC}" >&2
        return 1
    fi
    
    return 0
}

# Sort tags according to ALLOWED_CRATES order
sort_tags() {
    local -a input_tags=("$@")
    local -a sorted_tags=()
    local -a invalid_tags=()
    
    for tag in "${input_tags[@]}"; do
        if validate_tag "$tag"; then
            sorted_tags+=("$tag")
        else
            invalid_tags+=("$tag")
        fi
    done
    
    # Sort valid tags by ALLOWED_CRATES order
    local -a final_sorted=()
    for allowed in "${ALLOWED_CRATES[@]}"; do
        for tag in "${sorted_tags[@]}"; do
            if [[ "$tag" =~ ^${allowed}-v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
                final_sorted+=("$tag")
            fi
        done
    done
    
    # Report results (to stderr)
    if [ ${#invalid_tags[@]} -gt 0 ]; then
        echo -e "${YELLOW}=== Invalid tags (will be skipped): ===${NC}" >&2
        for tag in "${invalid_tags[@]}"; do
            echo -e "${RED}  ❌ $tag${NC}" >&2
        done
        echo "" >&2
    fi
    
    if [ ${#final_sorted[@]} -eq 0 ]; then
        echo -e "${RED}❌ No valid tags to publish${NC}" >&2
        exit 1
    fi
    
    echo "=== Tags to publish (in dependency order): ===" >&2
    local i=1
    for tag in "${final_sorted[@]}"; do
        echo -e "${BLUE}  $i. $tag${NC}" >&2
        ((i++))
    done
    echo "" >&2
    
    # Return sorted tags via stdout (capture with command substitution)
    echo "${final_sorted[@]}"
}

# Check if a crate version is already published on crates.io
check_already_published() {
    local crate_name=$1
    local version=$2
    
    echo "--- Checking if $crate_name v$version is already published ---" >&2
    
    local response
    response=$(curl -s "https://crates.io/api/v1/crates/$crate_name")
    
    if echo "$response" | jq -e '.errors' > /dev/null 2>&1; then
        # Crate doesn't exist yet on crates.io
        echo -e "${GREEN}✓ Crate $crate_name not found on crates.io - ready for first publication${NC}" >&2
        return 1  # Not published
    else
        # Crate exists, check if this specific version is published
        local published_version
        published_version=$(echo "$response" | jq -r --arg version "$version" '.versions[] | select(.num == $version) | .num')
        
        if [ -n "$published_version" ]; then
            echo -e "${YELLOW}⚠️  Version $version of $crate_name is already published on crates.io${NC}" >&2
            return 0  # Already published
        else
            echo -e "${GREEN}✓ Version $version is not yet published - ready for publication${NC}" >&2
            return 1  # Not published
        fi
    fi
}

publish_crate() {
    local tag=$1
    local dry_run=$2
    local token=$3
    
    echo "Processing tag: $tag" >&2
    
    # Extract crate name and version
    if [[ "$tag" =~ ^(.+)-v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
        local crate_name="${BASH_REMATCH[1]}"
        local crate_version="${BASH_REMATCH[2]}"
        echo "Crate: $crate_name" >&2
        echo "Version: $crate_version" >&2
    else
        echo -e "${YELLOW}⚠️  WARNING: Tag $tag does not match expected format - skipping${NC}" >&2
        return 1
    fi
    
    # Validate version from tag corresponds to the one in the Cargo.toml
    echo "--- Validating Cargo.toml version ---" >&2
    local manifest_version
    manifest_version=$(cargo metadata --format-version=1 --no-deps | jq -e -r '.packages[] | select(.name == "'"$crate_name"'") | .version')
    
    if [ -z "$manifest_version" ]; then
        echo -e "${RED}❌ ERROR: Crate $crate_name not found in workspace${NC}" >&2
        return 1
    fi
    
    if [ "$crate_version" != "$manifest_version" ]; then
        echo -e "${RED}❌ ERROR: Version mismatch!${NC}" >&2
        echo "   Tag version: $crate_version" >&2
        echo "   Cargo.toml version: $manifest_version" >&2
        return 1
    fi
    
    echo -e "${GREEN}✓ Version matches Cargo.toml${NC}" >&2
    echo "" >&2
    
    # Check if already published
    if check_already_published "$crate_name" "$crate_version"; then
        echo "Skipping publication (already published)..." >&2
        echo "" >&2
        return 0
    fi
    echo "" >&2
    
    # Run tests with different feature configurations
    echo "--- Running tests before publication ---" >&2
    
    # Test with no default features
    echo "Testing with --no-default-features..." >&2
    if cargo test --package "$crate_name" --no-default-features --quiet; then
        echo -e "${GREEN}✓ Tests passed with --no-default-features${NC}" >&2
    else
        echo -e "${YELLOW}⚠️  Warning: Tests with --no-default-features failed${NC}" >&2
        echo "   (This is non-blocking - continuing with publication)" >&2
    fi
    echo "" >&2
    
    # Test with all features
    echo "Testing with --all-features..." >&2
    if cargo test --package "$crate_name" --all-features --quiet; then
        echo -e "${GREEN}✓ Tests passed with --all-features${NC}" >&2
    else
        echo -e "${RED}❌ ERROR: Tests failed with --all-features${NC}" >&2
        echo "   Cannot publish a crate that fails tests with --all-features" >&2
        return 1
    fi
    echo "" >&2
    
    # Publish to crates.io
    echo "--- Publishing $crate_name v$crate_version to crates.io ---" >&2
    
    if [ -z "$token" ]; then
        echo -e "${RED}❌ ERROR: CARGO_REGISTRY_TOKEN is not set${NC}" >&2
        return 1
    fi
    
    local publish_cmd="cargo publish --package $crate_name --token $token --all-features"
    
    if [ "$dry_run" = "true" ]; then
        publish_cmd="$publish_cmd --dry-run"
        echo -e "${BLUE}[DRY RUN]${NC} $publish_cmd" >&2
    fi
    
    if $publish_cmd; then
        echo -e "${GREEN}✓ Published $crate_name v$crate_version successfully${NC}" >&2
        echo "" >&2
        return 0
    else
        echo -e "${RED}❌ Failed to publish $crate_name v$crate_version${NC}" >&2
        return 1
    fi
}

check_crate_only() {
    local tag=$1
    
    echo "Processing tag: $tag" >&2
    
    # Extract crate name and version
    if [[ "$tag" =~ ^(.+)-v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
        local crate_name="${BASH_REMATCH[1]}"
        local crate_version="${BASH_REMATCH[2]}"
        echo "Crate: $crate_name" >&2
        echo "Version: $crate_version" >&2
    else
        echo -e "${YELLOW}⚠️  WARNING: Tag $tag does not match expected format - skipping${NC}" >&2
        return 1
    fi
    
    # Validate crate is in allowed list
    local found=0
    for allowed in "${ALLOWED_CRATES[@]}"; do
        if [ "$crate_name" = "$allowed" ]; then
            found=1
            break
        fi
    done
    
    if [ $found -eq 0 ]; then
        echo -e "${YELLOW}⚠️  WARNING: Crate '$crate_name' is not in the allowed list${NC}" >&2
        return 1
    fi
    
    echo "" >&2
    
    # Check if already published
    if check_already_published "$crate_name" "$crate_version"; then
        echo -e "${GREEN}✓ Crate is published on crates.io${NC}" >&2
        echo "" >&2
        return 0
    else
        echo -e "${YELLOW}⚠️  Crate is NOT published on crates.io${NC}" >&2
        echo "" >&2
        return 1
    fi
}

check_publication_status() {
    local -a sorted_tags=("$@")
    
    echo "=== Checking publication status ===" >&2
    echo "" >&2
    
    local published=0
    local not_published=0
    local failed=0
    
    for tag in "${sorted_tags[@]}"; do
        if check_crate_only "$tag"; then
            ((published++))
        else
            if [[ "$tag" =~ ^(.+)-v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
                ((not_published++))
            else
                ((failed++))
            fi
        fi
    done
    
    echo "=========================================" >&2
    echo "=== Publication Check Summary ===" >&2
    echo "=========================================" >&2
    echo "Total tags: ${#sorted_tags[@]}" >&2
    echo -e "${GREEN}Published: $published${NC}" >&2
    echo -e "${YELLOW}Not published: $not_published${NC}" >&2
    if [ $failed -gt 0 ]; then
        echo -e "${RED}Invalid: $failed${NC}" >&2
    fi
    echo "" >&2
    
    if [ $not_published -gt 0 ]; then
        exit 1
    else
        echo -e "${GREEN}✓ All crates are published on crates.io!${NC}" >&2
        exit 0
    fi
}

main() {
    local dry_run=false
    local check_only=false
    local token="${CARGO_REGISTRY_TOKEN:-}"
    local verbose=false
    local -a tags=()
    
    while [[ $# -gt 0 ]]; do
        case $1 in
            -h|--help)
                usage
                exit 0
                ;;
            -d|--dry-run)
                dry_run=true
                shift
                ;;
            -c|--check-published)
                check_only=true
                shift
                ;;
            -t|--token)
                token="$2"
                shift 2
                ;;
            -v|--verbose)
                verbose=true
                set -x
                shift
                ;;
            -*)
                echo -e "${RED}Unknown option: $1${NC}" >&2
                usage
                exit 1
                ;;
            *)
                tags+=("$1")
                shift
                ;;
        esac
    done
    
    if [ ${#tags[@]} -eq 0 ]; then
        echo -e "${RED}❌ ERROR: No tags provided${NC}" >&2
        echo ""
        usage
        exit 1
    fi
    
    check_dependencies "$check_only"
    
    local sorted_tags
    sorted_tags=($(sort_tags "${tags[@]}"))
    
    echo "" >&2
    
    # Check-only mode: just check publication status
    if [ "$check_only" = true ]; then
        check_publication_status "${sorted_tags[@]}"
    fi
    
    # Normal publication mode
    local failed=0
    for tag in "${sorted_tags[@]}"; do
        if ! publish_crate "$tag" "$dry_run" "$token"; then
            ((failed++))
        fi
    done
    
    # Summary
    echo "=========================================" >&2
    echo "=== Publication Summary ===" >&2
    echo "=========================================" >&2
    echo "Total tags: ${#sorted_tags[@]}" >&2
    echo -e "${GREEN}Successful: $((${#sorted_tags[@]} - failed))${NC}" >&2
    if [ $failed -gt 0 ]; then
        echo -e "${RED}Failed: $failed${NC}" >&2
        exit 1
    else
        echo -e "${GREEN}✓ All crates published successfully!${NC}" >&2
    fi
}

# Run main function
main "$@"


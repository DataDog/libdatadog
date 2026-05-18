#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Keep in sync with: scripts/Dockerfile.license (ARG TOOL_VERSION) and .github/workflows/lint.yml (cache key + install step)
TOOL_VERSION="1.0.6"
INSTALL_CMD="cargo install dd-rust-license-tool --version \"${TOOL_VERSION}\" --locked"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && cd .. && pwd)"

run_native() {
    cd "${ROOT_DIR}"
    echo "Running dd-rust-license-tool dump..."
    dd-rust-license-tool dump > LICENSE-3rdparty.csv
}

run_docker() {
    if ! command -v docker &> /dev/null || ! docker info &> /dev/null; then
        echo "ERROR: Docker is not running. Please start the Docker daemon and try again."
        exit 1
    fi
    export DOCKER_BUILDKIT=1
    echo "Building license tool container..."
    docker build \
        --progress=plain \
        -t libdatadog-dd-license-tool \
        -f "${ROOT_DIR}/scripts/Dockerfile.license" \
        "${ROOT_DIR}"
    echo "Generating LICENSE-3rdparty.csv..."
    docker run --rm libdatadog-dd-license-tool > "${ROOT_DIR}/LICENSE-3rdparty.csv"
}

if cargo install --list 2>/dev/null | grep -qF "dd-rust-license-tool v${TOOL_VERSION}"; then
    run_native
else
    INSTALLED_VERSION=$(cargo install --list 2>/dev/null | grep "^dd-rust-license-tool v" | awk '{print $2}' | tr -d ':' || true)

    echo "dd-rust-license-tool v${TOOL_VERSION} is not installed."
    if [ -n "${INSTALLED_VERSION}" ]; then
        echo "Found installed version: ${INSTALLED_VERSION}"
    fi
    echo ""
    echo "To install v${TOOL_VERSION} locally, run:"
    echo "  ${INSTALL_CMD}"
    echo ""
    echo "How would you like to proceed?"
    echo "  1) Install dd-rust-license-tool v${TOOL_VERSION} locally and run"
    echo "  2) Use Docker (requires Docker daemon to be running)"
    if [ -n "${INSTALLED_VERSION}" ]; then
        echo "  3) Run with the installed version (${INSTALLED_VERSION})"
        read -rp "Enter 1, 2, or 3: " choice
    else
        read -rp "Enter 1 or 2: " choice
    fi

    case "${choice}" in
        1)
            echo "Installing dd-rust-license-tool v${TOOL_VERSION}..."
            eval "${INSTALL_CMD}"
            run_native
            ;;
        2)
            run_docker
            ;;
        3)
            if [ -n "${INSTALLED_VERSION}" ]; then
                run_native
            else
                echo "Invalid choice. Exiting."
                exit 1
            fi
            ;;
        *)
            echo "Invalid choice. Exiting."
            exit 1
            ;;
    esac
fi

echo ""
echo "Successfully generated LICENSE-3rdparty.csv."
echo "Please review and commit the changes."

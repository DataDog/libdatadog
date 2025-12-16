#!/bin/bash
# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Build and run the CXX crashinfo example
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Note: Extra libraries for crashtracker on Unix/Linux/macOS are typically not needed
# as the equivalent functionality is provided by the standard system libraries
exec "$SCRIPT_DIR/build-and-run.sh" libdd-crashtracker crashinfo

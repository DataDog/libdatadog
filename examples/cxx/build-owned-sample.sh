#!/bin/bash
# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Build and run the CXX owned_sample example
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/build-and-run.sh" libdd-profiling owned_sample


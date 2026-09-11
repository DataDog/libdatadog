# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Build and run the CXX profiling api2 example
$SCRIPT_DIR = Split-Path -Parent $MyInvocation.MyCommand.Path
& "$SCRIPT_DIR\build-and-run.ps1" -CrateName "libdd-profiling" -ExampleName "profiling_api2"
exit $LASTEXITCODE

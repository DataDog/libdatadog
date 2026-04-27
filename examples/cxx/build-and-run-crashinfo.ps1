# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Build and run the CXX crashinfo example
$SCRIPT_DIR = Split-Path -Parent $MyInvocation.MyCommand.Path

& "$SCRIPT_DIR\build-and-run.ps1" `
    -CrateName "libdd-crashtracker" `
    -ExampleName "crashinfo" `
    -ExtraMsvcLibs "dbghelp.lib psapi.lib powrprof.lib user32.lib oleaut32.lib secur32.lib ncrypt.lib runtimeobject.lib" `
    -ExtraGnuLibs "-ldbghelp -lpsapi -lole32 -lpowrprof"

exit $LASTEXITCODE

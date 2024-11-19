param (
    [string]$output_dir,
    [string[]]$targets = @(
        # "aarch64-apple-darwin"
        # "x86_64-apple-darwin"
        # "aarch64-unknown-linux-gnu",
        # "x86_64-unknown-linux-gnu"
        "i686-pc-windows-msvc",
        "x86_64-pc-windows-msvc"
    )
)

# Check if output directory is set
if (-not $output_dir) {
    Write-Host "You must specify an output directory with -output. Example: .\build_script.ps1 -output bin"
    exit 1
}

# Make output_dir an absolute path if it's not already
if (-not [System.IO.Path]::IsPathRooted($output_dir)) {
    $output_dir = Join-Path -Path (Get-Location) -ChildPath $output_dir
}

Write-Host "Building project into $output_dir" -ForegroundColor Magenta

# Function to invoke a command and exit if it fails
function Invoke-Call {
    param (
        [scriptblock]$ScriptBlock
    )
    & $ScriptBlock
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}

# Function to build project with given target, features, and release flag
function Build-Project {
    param (
        [string]$target,
        [bool]$release = $false
    )

    Invoke-Call -ScriptBlock {
        $featues = @(
            "data-pipeline-ffi",
            "datadog-profiling-ffi/ddtelemetry-ffi",
            "datadog-profiling-ffi/crashtracker-receiver",
            "datadog-profiling-ffi/crashtracker-collector",
            "datadog-profiling-ffi/demangler"
        )

        # cargo has a bug when passing "" as configuration, so branch for debug and release
        if ($release) {
            cargo build --features $($featues -join ",") --target $target --release --target-dir $output_dir
        } else {
            cargo build --features $($featues -join ",") --target $target --target-dir $output_dir
        }
    }
}

# Function to generate header files using cbindgen
function Generate-Header {
    param (
        [string]$crateName,
        [string]$configPath,
        [string]$outputPath
    )

    Invoke-Call -ScriptBlock {
        cbindgen --crate $crateName --config $configPath --output $outputPath
    }
}

# Build project for multiple targets

try {
    Push-Location "profiling-ffi"
    foreach ($target in $targets) {
        Build-Project -target $target -release $true
        Build-Project -target $target
    }
}
finally {
    Pop-Location
}

Write-Host "Building tools" -ForegroundColor Magenta
try {
    Push-Location "tools"
    Invoke-Call -ScriptBlock { cargo build --release }
}
finally {
    Pop-Location
}

Write-Host "Generating headers" -ForegroundColor Magenta

# Generate headers for each FFI crate
Generate-Header -crateName "ddcommon-ffi" -configPath "ddcommon-ffi/cbindgen.toml" -outputPath "$output_dir\common.h"
Generate-Header -crateName "datadog-profiling-ffi" -configPath "profiling-ffi/cbindgen.toml" -outputPath "$output_dir\profiling.h"
Generate-Header -crateName "ddtelemetry-ffi" -configPath "ddtelemetry-ffi/cbindgen.toml" -outputPath "$output_dir\telemetry.h"
Generate-Header -crateName "data-pipeline-ffi" -configPath "data-pipeline-ffi/cbindgen.toml" -outputPath "$output_dir\data-pipeline.h"
Generate-Header -crateName "datadog-crashtracker-ffi" -configPath "crashtracker-ffi/cbindgen.toml" -outputPath "$output_dir\crashtracker.h"

# Deduplicate headers
Invoke-Call -ScriptBlock { .\target\release\dedup_headers "$output_dir\common.h" "$output_dir\profiling.h" "$output_dir\telemetry.h" "$output_dir\data-pipeline.h" "$output_dir\crashtracker.h" }

Write-Host "Build finished" -ForegroundColor Magenta

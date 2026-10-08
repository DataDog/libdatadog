param(
    [Parameter(Mandatory = $true)][string]$LibdatadogRoot,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
$target = "x86_64-pc-windows-msvc"
$builderRoot = Join-Path $OutputDirectory "builder"
$builderOutput = Join-Path $OutputDirectory "builder-output"
$cargoTarget = Join-Path $OutputDirectory "target"
$packageRoot = Join-Path $OutputDirectory "package"

if (Test-Path $OutputDirectory) {
    Remove-Item -Path $OutputDirectory -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null

$env:RUSTUP_TOOLCHAIN = "1.87.0"
$metadata = cargo metadata `
    --manifest-path (Join-Path $LibdatadogRoot "Cargo.toml") `
    --format-version 1 `
    --no-deps | ConvertFrom-Json
$version = ($metadata.packages | Where-Object name -eq "builder").version
if (-not $version) {
    throw "Could not determine the libdatadog builder version"
}

cargo install `
    --path (Join-Path $LibdatadogRoot "builder") `
    --bin release `
    --root $builderRoot `
    --no-default-features `
    --features "profiling,crashtracker,symbolizer,library-config" `
    --locked `
    --force
if ($LASTEXITCODE -ne 0) { throw "cargo install failed" }

$hostTriple = (rustc -vV | Where-Object { $_ -match "^host:" }) -replace "^host:\s*", ""
$env:PROFILE = "release"
$env:TARGET = $hostTriple
$env:CARGO_PKG_VERSION = $version
$env:CARGO_TARGET_DIR = $cargoTarget
& (Join-Path $builderRoot "bin/release.exe") `
    --out $builderOutput `
    --target $target
if ($LASTEXITCODE -ne 0) { throw "libdatadog release builder failed" }

New-Item -ItemType Directory -Force -Path $packageRoot | Out-Null
Copy-Item -Path (Join-Path $builderOutput "include") -Destination $packageRoot -Recurse
Copy-Item -Path (Join-Path $builderOutput "cmake") -Destination $packageRoot -Recurse
Copy-Item -Path (Join-Path $builderOutput "LICENSE") -Destination $packageRoot
$staticDirectory = Join-Path $packageRoot "release/static"
New-Item -ItemType Directory -Force -Path $staticDirectory | Out-Null
Copy-Item `
    -Path (Join-Path $cargoTarget "$target/release/datadog_profiling_ffi.lib") `
    -Destination (Join-Path $staticDirectory "datadog_profiling_ffi.lib")

if (-not (Test-Path (Join-Path $packageRoot "include/datadog/profiling.h"))) {
    throw "profiling header missing from Windows package"
}
if (-not (Test-Path (Join-Path $staticDirectory "datadog_profiling_ffi.lib"))) {
    throw "static profiling library missing from Windows package"
}

Get-ChildItem -Path $packageRoot -Recurse -File | ForEach-Object {
    Write-Host $_.FullName
}

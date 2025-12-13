# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Generic script to build and run CXX examples
# Usage: .\build-and-run.ps1 <crate-name> <example-name>
# Example: .\build-and-run.ps1 libdd-profiling profiling

param(
    [Parameter(Mandatory=$true)]
    [string]$CrateName,
    
    [Parameter(Mandatory=$true)]
    [string]$ExampleName
)

$ErrorActionPreference = "Stop"

$SCRIPT_DIR = Split-Path -Parent $MyInvocation.MyCommand.Path
$PROJECT_ROOT = (Get-Item (Join-Path $SCRIPT_DIR ".." "..")).FullName
Set-Location $PROJECT_ROOT

Write-Host "🔨 Building $CrateName with cxx feature..." -ForegroundColor Cyan
cargo build -p $CrateName --features cxx --release

Write-Host "🔍 Finding CXX bridge headers..." -ForegroundColor Cyan
$CXX_BRIDGE_INCLUDE = Get-ChildItem -Path "target\release\build\$CrateName-*\out\cxxbridge\include" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
$CXX_BRIDGE_CRATE = Get-ChildItem -Path "target\release\build\$CrateName-*\out\cxxbridge\crate" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
$RUST_CXX_INCLUDE = Get-ChildItem -Path "target\release\build\cxx-*\out" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName

if (-not $CXX_BRIDGE_INCLUDE -or -not $CXX_BRIDGE_CRATE -or -not $RUST_CXX_INCLUDE) {
    Write-Host "❌ Error: Could not find CXX bridge directories" -ForegroundColor Red
    exit 1
}

Write-Host "📁 CXX include: $CXX_BRIDGE_INCLUDE" -ForegroundColor Green
Write-Host "📁 CXX crate: $CXX_BRIDGE_CRATE" -ForegroundColor Green
Write-Host "📁 Rust CXX: $RUST_CXX_INCLUDE" -ForegroundColor Green

# Check if we have MSVC (cl.exe) or MinGW (g++/clang++)
$MSVC = Get-Command cl.exe -ErrorAction SilentlyContinue
$GPP = Get-Command g++.exe -ErrorAction SilentlyContinue
$CLANGPP = Get-Command clang++.exe -ErrorAction SilentlyContinue

# Convert crate name with dashes to underscores for library name
$LibName = $CrateName -replace '-', '_'

# Auto-detect which toolchain Rust used by checking which library exists
$HAS_MSVC_LIB = Test-Path (Join-Path $PROJECT_ROOT "target\release\${LibName}.lib")
$HAS_GNU_LIB = (Test-Path (Join-Path $PROJECT_ROOT "target\release\${LibName}.a")) -or `
               (Test-Path (Join-Path $PROJECT_ROOT "target\release\lib${LibName}.a"))

if ($HAS_MSVC_LIB -and $MSVC) {
    $USE_MSVC = $true
    Write-Host "Detected MSVC Rust toolchain" -ForegroundColor Cyan
} elseif ($HAS_GNU_LIB -and ($GPP -or $CLANGPP)) {
    $USE_MSVC = $false
    Write-Host "Detected GNU Rust toolchain" -ForegroundColor Cyan
} elseif ($MSVC) {
    $USE_MSVC = $true
    Write-Host "Defaulting to MSVC (library not found yet, will check after)" -ForegroundColor Yellow
} elseif ($GPP -or $CLANGPP) {
    $USE_MSVC = $false
    Write-Host "Defaulting to GNU toolchain (library not found yet, will check after)" -ForegroundColor Yellow
} else {
    Write-Host "❌ Error: No C++ compiler found. Please install MSVC (via Visual Studio) or MinGW/LLVM" -ForegroundColor Red
    exit 1
}

Write-Host "🔨 Finding libraries..." -ForegroundColor Cyan
if ($USE_MSVC) {
    # MSVC naming
    $CRATE_LIB = Join-Path $PROJECT_ROOT "target\release\${LibName}.lib"
    $CXX_BRIDGE_LIB = Get-ChildItem -Path "target\release\build\$CrateName-*\out" -Filter "$CrateName-cxx.lib" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
} else {
    # MinGW naming - try both patterns
    $CRATE_LIB_1 = Join-Path $PROJECT_ROOT "target\release\${LibName}.a"
    $CRATE_LIB_2 = Join-Path $PROJECT_ROOT "target\release\lib${LibName}.a"
    
    if (Test-Path $CRATE_LIB_1) {
        $CRATE_LIB = $CRATE_LIB_1
    } elseif (Test-Path $CRATE_LIB_2) {
        $CRATE_LIB = $CRATE_LIB_2
    } else {
        $CRATE_LIB = $CRATE_LIB_1
    }
    
    # Try both naming patterns for CXX bridge
    $CXX_BRIDGE_LIB = Get-ChildItem -Path "target\release\build\$CrateName-*\out" -Filter "$CrateName-cxx.a" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
    if (-not $CXX_BRIDGE_LIB) {
        $CXX_BRIDGE_LIB = Get-ChildItem -Path "target\release\build\$CrateName-*\out" -Filter "lib$CrateName-cxx.a" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
    }
}

if (-not (Test-Path $CRATE_LIB)) {
    Write-Host "❌ Error: Could not find $CrateName library at $CRATE_LIB" -ForegroundColor Red
    if (-not $USE_MSVC) {
        Write-Host "Searched for: ${LibName}.a and lib${LibName}.a" -ForegroundColor Yellow
    }
    exit 1
}

if (-not $CXX_BRIDGE_LIB) {
    Write-Host "❌ Error: Could not find CXX bridge library for $CrateName" -ForegroundColor Red
    exit 1
}

Write-Host "📚 Crate library: $CRATE_LIB" -ForegroundColor Green
Write-Host "📚 CXX bridge library: $CXX_BRIDGE_LIB" -ForegroundColor Green

Write-Host "🔨 Compiling C++ example..." -ForegroundColor Cyan

$ExampleCpp = "examples\cxx\$ExampleName.cpp"
$ExampleExe = "examples\cxx\$ExampleName.exe"

if ($USE_MSVC) {
    Write-Host "Using MSVC compiler" -ForegroundColor Yellow
    
    # Use /MD (dynamic CRT) to match the default Rust build
    cl.exe /std:c++20 /EHsc /MD `
        /I"$CXX_BRIDGE_INCLUDE" `
        /I"$CXX_BRIDGE_CRATE" `
        /I"$RUST_CXX_INCLUDE" `
        /I"$PROJECT_ROOT" `
        $ExampleCpp `
        "$CRATE_LIB" `
        "$CXX_BRIDGE_LIB" `
        ws2_32.lib advapi32.lib userenv.lib ntdll.lib bcrypt.lib `
        dbghelp.lib psapi.lib ole32.lib powrprof.lib `
        /Fe:$ExampleExe
    
    if ($LASTEXITCODE -ne 0) {
        Write-Host "❌ Compilation failed" -ForegroundColor Red
        exit 1
    }
} elseif ($GPP -or $CLANGPP) {
    $COMPILER = if ($GPP) { "g++" } else { "clang++" }
    Write-Host "Using $COMPILER compiler" -ForegroundColor Yellow
    
    & $COMPILER -std=c++20 `
        -I"$CXX_BRIDGE_INCLUDE" `
        -I"$CXX_BRIDGE_CRATE" `
        -I"$RUST_CXX_INCLUDE" `
        -I"$PROJECT_ROOT" `
        $ExampleCpp `
        "$CXX_BRIDGE_LIB" `
        "$CRATE_LIB" `
        -lws2_32 -ladvapi32 -luserenv -lntdll -lbcrypt `
        -ldbghelp -lpsapi -lole32 -lpowrprof `
        -lgcc_eh -lpthread `
        -o $ExampleExe
    
    if ($LASTEXITCODE -ne 0) {
        Write-Host "❌ Compilation failed" -ForegroundColor Red
        exit 1
    }
}

Write-Host "🚀 Running example..." -ForegroundColor Cyan
& ".\$ExampleExe"

if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Example failed with exit code $LASTEXITCODE" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "✅ Success!" -ForegroundColor Green



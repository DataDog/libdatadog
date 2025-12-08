# Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Build and run the CXX crashinfo example on Windows
$ErrorActionPreference = "Stop"

$SCRIPT_DIR = Split-Path -Parent $MyInvocation.MyCommand.Path
$PROJECT_ROOT = (Get-Item (Join-Path $SCRIPT_DIR ".." "..")).FullName
Set-Location $PROJECT_ROOT

Write-Host "🔨 Building libdd-crashtracker with cxx feature..." -ForegroundColor Cyan
cargo build -p libdd-crashtracker --features cxx --release

Write-Host "🔍 Finding CXX bridge headers..." -ForegroundColor Cyan
$CXX_BRIDGE_INCLUDE = Get-ChildItem -Path "target\release\build\libdd-crashtracker-*\out\cxxbridge\include" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
$CXX_BRIDGE_CRATE = Get-ChildItem -Path "target\release\build\libdd-crashtracker-*\out\cxxbridge\crate" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
$RUST_CXX_INCLUDE = Get-ChildItem -Path "target\release\build\cxx-*\out" -Directory -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName

if (-not $CXX_BRIDGE_INCLUDE -or -not $CXX_BRIDGE_CRATE -or -not $RUST_CXX_INCLUDE) {
    Write-Host "❌ Error: Could not find CXX bridge directories" -ForegroundColor Red
    exit 1
}

Write-Host "📁 CXX include: $CXX_BRIDGE_INCLUDE" -ForegroundColor Green
Write-Host "📁 CXX crate: $CXX_BRIDGE_CRATE" -ForegroundColor Green
Write-Host "📁 Rust CXX: $RUST_CXX_INCLUDE" -ForegroundColor Green

# Check if we have MSVC (cl.exe) or MinGW (g++/clang++)
# Note: Prefer MSVC on Windows as it's the default Rust toolchain
$MSVC = Get-Command cl.exe -ErrorAction SilentlyContinue
$GPP = Get-Command g++.exe -ErrorAction SilentlyContinue
$CLANGPP = Get-Command clang++.exe -ErrorAction SilentlyContinue

# Auto-detect which toolchain Rust used by checking which library exists
# Note: On Windows, Rust still uses 'lib' prefix even for MSVC .lib files
$HAS_MSVC_LIB = Test-Path (Join-Path $PROJECT_ROOT "target\release\libdd_crashtracker.lib")
$HAS_GNU_LIB = (Test-Path (Join-Path $PROJECT_ROOT "target\release\libdd_crashtracker.a")) -or `
               (Test-Path (Join-Path $PROJECT_ROOT "target\release\liblibdd_crashtracker.a"))

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
# Note: Rust library naming varies by platform and toolchain
if ($USE_MSVC) {
    # MSVC: libdd_crashtracker.lib (Rust keeps the lib prefix even on Windows)
    $CRASHTRACKER_LIB = Join-Path $PROJECT_ROOT "target\release\libdd_crashtracker.lib"
    $CXX_BRIDGE_LIB = Get-ChildItem -Path "target\release\build\libdd-crashtracker-*\out" -Filter "libdd-crashtracker-cxx.lib" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
} else {
    # MinGW: Try both possible naming patterns
    $CRASHTRACKER_LIB_1 = Join-Path $PROJECT_ROOT "target\release\libdd_crashtracker.a"
    $CRASHTRACKER_LIB_2 = Join-Path $PROJECT_ROOT "target\release\liblibdd_crashtracker.a"
    
    if (Test-Path $CRASHTRACKER_LIB_1) {
        $CRASHTRACKER_LIB = $CRASHTRACKER_LIB_1
    } elseif (Test-Path $CRASHTRACKER_LIB_2) {
        $CRASHTRACKER_LIB = $CRASHTRACKER_LIB_2
    } else {
        $CRASHTRACKER_LIB = $CRASHTRACKER_LIB_1  # Use this for error message
    }
    
    # Try both naming patterns for CXX bridge
    $CXX_BRIDGE_LIB = Get-ChildItem -Path "target\release\build\libdd-crashtracker-*\out" -Filter "libdd-crashtracker-cxx.a" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
    if (-not $CXX_BRIDGE_LIB) {
        $CXX_BRIDGE_LIB = Get-ChildItem -Path "target\release\build\libdd-crashtracker-*\out" -Filter "liblibdd-crashtracker-cxx.a" -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName
    }
}

if (-not (Test-Path $CRASHTRACKER_LIB)) {
    Write-Host "❌ Error: Could not find libdd-crashtracker library at $CRASHTRACKER_LIB" -ForegroundColor Red
    if (-not $MSVC) {
        Write-Host "Searched for: libdd_crashtracker.a and liblibdd_crashtracker.a" -ForegroundColor Yellow
        Write-Host "Files in target/release/:" -ForegroundColor Yellow
        Get-ChildItem -Path "target\release" -Filter "*crashtracker*" | Select-Object -First 10 | ForEach-Object { Write-Host "  $_" -ForegroundColor Gray }
    }
    exit 1
}

if (-not $CXX_BRIDGE_LIB) {
    if ($USE_MSVC) {
        Write-Host "❌ Error: Could not find CXX bridge library (looking for libdd-crashtracker-cxx.lib)" -ForegroundColor Red
    } else {
        Write-Host "❌ Error: Could not find CXX bridge library" -ForegroundColor Red
        Write-Host "Searched for: libdd-crashtracker-cxx.a and liblibdd-crashtracker-cxx.a" -ForegroundColor Yellow
    }
    exit 1
}

Write-Host "📚 Crashtracker library: $CRASHTRACKER_LIB" -ForegroundColor Green
Write-Host "📚 CXX bridge library: $CXX_BRIDGE_LIB" -ForegroundColor Green

Write-Host "🔨 Compiling C++ example..." -ForegroundColor Cyan

if ($USE_MSVC) {
    Write-Host "Using MSVC compiler" -ForegroundColor Yellow
    
    # MSVC compilation
    # Use /MD (dynamic CRT) to match the default Rust build
    cl.exe /std:c++20 /EHsc /MD `
        /I"$CXX_BRIDGE_INCLUDE" `
        /I"$CXX_BRIDGE_CRATE" `
        /I"$RUST_CXX_INCLUDE" `
        /I"$PROJECT_ROOT" `
        examples\cxx\crashinfo.cpp `
        "$CRASHTRACKER_LIB" `
        "$CXX_BRIDGE_LIB" `
        ws2_32.lib advapi32.lib userenv.lib ntdll.lib bcrypt.lib `
        dbghelp.lib psapi.lib ole32.lib powrprof.lib `
        /Fe:examples\cxx\crashinfo.exe
    
    if ($LASTEXITCODE -ne 0) {
        Write-Host "❌ Compilation failed" -ForegroundColor Red
        exit 1
    }
} elseif ($GPP -or $CLANGPP) {
    $COMPILER = if ($GPP) { "g++" } else { "clang++" }
    Write-Host "Using $COMPILER compiler" -ForegroundColor Yellow
    
    # MinGW/Clang compilation - needs proper library ordering and Rust std lib
    & $COMPILER -std=c++20 `
        -I"$CXX_BRIDGE_INCLUDE" `
        -I"$CXX_BRIDGE_CRATE" `
        -I"$RUST_CXX_INCLUDE" `
        -I"$PROJECT_ROOT" `
        examples/cxx/crashinfo.cpp `
        "$CXX_BRIDGE_LIB" `
        "$CRASHTRACKER_LIB" `
        -lws2_32 -ladvapi32 -luserenv -lntdll -lbcrypt `
        -ldbghelp -lpsapi -lole32 -lpowrprof `
        -lgcc_eh -lpthread `
        -o examples/cxx/crashinfo.exe
    
    if ($LASTEXITCODE -ne 0) {
        Write-Host "❌ Compilation failed" -ForegroundColor Red
        exit 1
    }
}

Write-Host "🚀 Running example..." -ForegroundColor Cyan
& ".\examples\cxx\crashinfo.exe"

if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ Example failed with exit code $LASTEXITCODE" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "✅ Success!" -ForegroundColor Green


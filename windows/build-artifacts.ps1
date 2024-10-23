function Invoke-Call {
    param (
        [scriptblock]$ScriptBlock
    )
    & @ScriptBlock
    if ($lastexitcode -ne 0) {
        exit $lastexitcode
    }
}

$output_dir = $args[0]

if ([string]::IsNullOrEmpty($output_dir)) {
    throw "You must specify an output directory. Ex: $($myInvocation.InvocationName) my_rust_project/ bin"
}

if (![System.IO.Path]::IsPathRooted($output_dir)) {
    $output_dir = [System.IO.Path]::Combine($(Get-Location), $output_dir)
}

$x86_release_dir = "$($output_dir)-x86-release"
$x86_debug_dir = "$($output_dir)-x86-debug"
$x86_64_release_dir = "$($output_dir)-x86_64-release"
$x86_64_debug_dir = "$($output_dir)-x86_64-debug"

Remove-Item -Recurse -Force -ErrorAction Ignore $output_dir
Remove-Item -Recurse -Force -ErrorAction Ignore $x86_release_dir
Remove-Item -Recurse -Force -ErrorAction Ignore $x86_debug_dir
Remove-Item -Recurse -Force -ErrorAction Ignore $x86_64_release_dir
Remove-Item -Recurse -Force -ErrorAction Ignore $x86_64_debug_dir

# build inside the crate to use the config.toml file
#pushd profiling-ffi

# cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker --release -- --out $LIBDD_OUTPUT_FOLDER
Write-Host "Building project into $($x86_64_release_dir)" -ForegroundColor Magenta
Invoke-Call -ScriptBlock { cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker --release  --target x86_64-pc-windows-msvc -- --out $x86_64_release_dir }

Write-Host "Building project into $($x86_64_debug_dir)" -ForegroundColor Magenta
Invoke-Call -ScriptBlock { cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker --target x86_64-pc-windows-msvc -- --out $x86_64_debug_dir }

Write-Host "Building project into $($x86_release_dir)" -ForegroundColor Magenta
Invoke-Call -ScriptBlock { cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker --release --target i686-pc-windows-msvc -- --out $x86_release_dir }

Write-Host "Building project into $($x86_debug_dir)" -ForegroundColor Magenta
Invoke-Call -ScriptBlock { cargo run --bin release --features profiling,telemetry,data-pipeline,symbolizer,crashtracker --target i686-pc-windows-msvc -- --out $x86_debug_dir }

#Invoke-Call -ScriptBlock { cargo build --features datadog-profiling-ffi/ddtelemetry-ffi,datadog-profiling-ffi/crashtracker-receiver,datadog-profiling-ffi/crashtracker-collector,datadog-profiling-ffi/demangler --target i686-pc-windows-msvc --release --target-dir $output_dir }
#Invoke-Call -ScriptBlock { cargo build --features datadog-profiling-ffi/ddtelemetry-ffi,datadog-profiling-ffi/crashtracker-receiver,datadog-profiling-ffi/crashtracker-collector,datadog-profiling-ffi/demangler --target i686-pc-windows-msvc --target-dir $output_dir }
#Invoke-Call -ScriptBlock { cargo build --features datadog-profiling-ffi/ddtelemetry-ffi,datadog-profiling-ffi/crashtracker-receiver,datadog-profiling-ffi/crashtracker-collector,datadog-profiling-ffi/demangler --target x86_64-pc-windows-msvc --release --target-dir $output_dir }
#Invoke-Call -ScriptBlock { cargo build --features datadog-profiling-ffi/ddtelemetry-ffi,datadog-profiling-ffi/crashtracker-receiver,datadog-profiling-ffi/crashtracker-collector,datadog-profiling-ffi/demangler --target x86_64-pc-windows-msvc --target-dir $output_dir }
#popd

# Write-Host "Building tools" -ForegroundColor Magenta
# Set-Location tools
# Invoke-Call -ScriptBlock { cargo build --release }
# Set-Location ..

Write-Host "Copying artifacts" -ForegroundColor Magenta
Copy-Item -Path $x86_64_release_dir $output_dir\x64\release\ -Recurse
Copy-Item -Path $x86_64_debug_dir $output_dir\x64\debug\ -Recurse
Copy-Item -Path $x86_release_dir $output_dir\x86\release -Recurse
Copy-Item -Path $x86_debug_dir $output_dir\x86\debug\ -Recurse


# Write-Host "Generating headers" -ForegroundColor Magenta
# Invoke-Call -ScriptBlock { cbindgen --crate ddcommon-ffi --config ddcommon-ffi/cbindgen.toml --output $output_dir\common.h }
# Invoke-Call -ScriptBlock { cbindgen --crate datadog-profiling-ffi --config profiling-ffi/cbindgen.toml --output $output_dir\profiling.h }
# Invoke-Call -ScriptBlock { cbindgen --crate ddtelemetry-ffi --config ddtelemetry-ffi/cbindgen.toml --output $output_dir\telemetry.h }
# Invoke-Call -ScriptBlock { cbindgen --crate data-pipeline-ffi --config data-pipeline-ffi/cbindgen.toml --output $output_dir"\data-pipeline.h" }
# Invoke-Call -ScriptBlock { cbindgen --crate datadog-crashtracker-ffi --config crashtracker-ffi/cbindgen.toml --output $output_dir"\crashtracker.h" }
# Invoke-Call -ScriptBlock { .\target\release\dedup_headers $output_dir"\common.h"  $output_dir"\profiling.h" $output_dir"\telemetry.h" $output_dir"\data-pipeline.h" $output_dir"\crashtracker.h"}

Write-Host "Build finished"  -ForegroundColor Magenta

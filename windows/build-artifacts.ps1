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
$target = $args[1]

if ([string]::IsNullOrEmpty($output_dir)) {
    throw "You must specify an output directory. Ex: $($myInvocation.InvocationName) my_rust_project/ bin"
}

$targets = @("i686-pc-windows-msvc", "x86_64-pc-windows-msvc")
if (![string]::IsNullOrEmpty($target)) {
    $targets = @($target)
}

if (![System.IO.Path]::IsPathRooted($output_dir)) {
    $output_dir = [System.IO.Path]::Combine($(Get-Location), $output_dir)
}

Write-Host "Building project into $($output_dir)" -ForegroundColor Magenta

# build inside the crate to use the config.toml file
$features = @(
    "data-pipeline-ffi",
    "datadog-profiling-ffi/crashtracker-collector",
    "datadog-profiling-ffi/crashtracker-receiver",
    "datadog-profiling-ffi/ddtelemetry-ffi",
    "datadog-profiling-ffi/demangler"
) -join ","

Write-Host "Building for features: $features" -ForegroundColor Magenta

pushd profiling-ffi
foreach ($target in $targets) {
    Invoke-Call -ScriptBlock { cargo build --features $features --target $target --release --target-dir $output_dir }
    Invoke-Call -ScriptBlock { cargo build --features $features --target $target --target-dir $output_dir }
}
popd

Write-Host "Building tools" -ForegroundColor Magenta
Set-Location tools
Invoke-Call -ScriptBlock { cargo build --release }
Set-Location ..

Write-Host "Generating headers" -ForegroundColor Magenta
Invoke-Call -ScriptBlock { cbindgen --crate ddcommon-ffi --config ddcommon-ffi/cbindgen.toml --output $output_dir\common.h }
Invoke-Call -ScriptBlock { cbindgen --crate datadog-profiling-ffi --config profiling-ffi/cbindgen.toml --output $output_dir\profiling.h }
Invoke-Call -ScriptBlock { cbindgen --crate ddtelemetry-ffi --config ddtelemetry-ffi/cbindgen.toml --output $output_dir\telemetry.h }
Invoke-Call -ScriptBlock { cbindgen --crate data-pipeline-ffi --config data-pipeline-ffi/cbindgen.toml --output $output_dir"\data-pipeline.h" }
Invoke-Call -ScriptBlock { cbindgen --crate datadog-crashtracker-ffi --config crashtracker-ffi/cbindgen.toml --output $output_dir"\crashtracker.h" }
Invoke-Call -ScriptBlock { .\target\release\dedup_headers $output_dir"\common.h"  $output_dir"\profiling.h" $output_dir"\telemetry.h" $output_dir"\data-pipeline.h" $output_dir"\crashtracker.h"}

Write-Host "Build finished"  -ForegroundColor Magenta

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

Write-Host "Building project into $($output_dir)" -ForegroundColor Magenta

# build inside the crate to use the config.toml file
$features = @(
    "data-pipeline-ffi",
    "datadog-profiling-ffi/crashtracker-collector",
    "datadog-profiling-ffi/crashtracker-receiver",
    "datadog-profiling-ffi/ddtelemetry-ffi",
    "datadog-profiling-ffi/demangler",
    "datadog-library-config-ffi",
    "datadog-log-ffi"
) -join ","

Write-Host "Building for features: $features" -ForegroundColor Magenta

pushd libdd-profiling-ffi
#i686 Release
Invoke-Call -ScriptBlock { cargo rustc --features $features --target i686-pc-windows-msvc --release --target-dir $output_dir --crate-type cdylib }
Invoke-Call -ScriptBlock { cargo rustc --features $features --target i686-pc-windows-msvc --release --target-dir $output_dir --crate-type staticlib }
#i686 Debug
Invoke-Call -ScriptBlock { cargo rustc --features $features --target i686-pc-windows-msvc --target-dir $output_dir --crate-type cdylib }
Invoke-Call -ScriptBlock { cargo rustc --features $features --target i686-pc-windows-msvc --target-dir $output_dir --crate-type staticlib }
#x86_64 Release
Invoke-Call -ScriptBlock { cargo rustc --features $features --target x86_64-pc-windows-msvc --release --target-dir $output_dir --crate-type cdylib}
Invoke-Call -ScriptBlock { cargo rustc --features $features --target x86_64-pc-windows-msvc --release --target-dir $output_dir --crate-type staticlib}
#x86_64 Debug
Invoke-Call -ScriptBlock { cargo rustc --features $features --target x86_64-pc-windows-msvc --target-dir $output_dir --crate-type cdylib}
Invoke-Call -ScriptBlock { cargo rustc --features $features --target x86_64-pc-windows-msvc --target-dir $output_dir --crate-type staticlib}
popd

Write-Host "Building tools" -ForegroundColor Magenta
Set-Location tools
Invoke-Call -ScriptBlock { cargo build --release }
Set-Location ..

Write-Host "Generating headers" -ForegroundColor Magenta
Invoke-Call -ScriptBlock { cbindgen --crate libdd-common-ffi --config libdd-common-ffi/cbindgen.toml --output $output_dir\common.h }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-profiling-ffi --config libd-profiling-ffi/cbindgen.toml --output $output_dir\profiling.h }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-telemetry-ffi --config libdd-telemetry-ffi/cbindgen.toml --output $output_dir\telemetry.h }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-data-pipeline-ffi --config libdd-data-pipeline-ffi/cbindgen.toml --output $output_dir"\data-pipeline.h" }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-crashtracker-ffi --config libdd-crashtracker-ffi/cbindgen.toml --output $output_dir"\crashtracker.h" }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-library-config-ffi --config libdd-library-config-ffi/cbindgen.toml --output $output_dir"\library-config.h" }
Invoke-Call -ScriptBlock { .\target\release\dedup_headers $output_dir"\common.h"  $output_dir"\profiling.h" $output_dir"\telemetry.h" $output_dir"\data-pipeline.h" $output_dir"\crashtracker.h" $output_dir"\library-config.h"}

Write-Host "Build finished"  -ForegroundColor Magenta

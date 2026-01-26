function Invoke-Call {
    param (
        [scriptblock]$ScriptBlock
    )
    & @ScriptBlock
    if ($lastexitcode -ne 0) {
        exit $lastexitcode
    }
}

function Add-DllImportToGlobals {
    param (
        [string]$HeaderPath
    )
    $content = [System.IO.File]::ReadAllText($HeaderPath)
    $pattern = '(?m)^(\s*)extern\s+(?!\"C\")(?!.*__declspec\(dllimport\))(?!.*\()(.+;)$'
    $updated = [System.Text.RegularExpressions.Regex]::Replace(
        $content,
        $pattern,
        '$1extern __declspec(dllimport) $2'
    )
    if ($updated -ne $content) {
        $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($HeaderPath, $updated, $utf8NoBom)
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
    "crashtracker-collector",
    "crashtracker-receiver",
    "ddtelemetry-ffi",
    "demangler",
    "datadog-library-config-ffi",
    "datadog-ffe-ffi",
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
Invoke-Call -ScriptBlock { cbindgen --crate libdd-profiling-ffi --config libdd-profiling-ffi/cbindgen.toml --output $output_dir\profiling.h }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-telemetry-ffi --config libdd-telemetry-ffi/cbindgen.toml --output $output_dir\telemetry.h }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-data-pipeline-ffi --config libdd-data-pipeline-ffi/cbindgen.toml --output $output_dir"\data-pipeline.h" }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-crashtracker-ffi --config libdd-crashtracker-ffi/cbindgen.toml --output $output_dir"\crashtracker.h" }
Invoke-Call -ScriptBlock { cbindgen --crate libdd-library-config-ffi --config libdd-library-config-ffi/cbindgen.toml --output $output_dir"\library-config.h" }
Add-DllImportToGlobals $output_dir"\common.h"
Add-DllImportToGlobals $output_dir"\profiling.h"
Add-DllImportToGlobals $output_dir"\telemetry.h"
Add-DllImportToGlobals $output_dir"\data-pipeline.h"
Add-DllImportToGlobals $output_dir"\crashtracker.h"
Add-DllImportToGlobals $output_dir"\library-config.h"
Invoke-Call -ScriptBlock { .\target\release\dedup_headers $output_dir"\common.h"  $output_dir"\profiling.h" $output_dir"\telemetry.h" $output_dir"\data-pipeline.h" $output_dir"\crashtracker.h" $output_dir"\library-config.h"}

Write-Host "Build finished"  -ForegroundColor Magenta

param (
    [Parameter(Mandatory=$true)][string]$ArtifactsPath
)

function Invoke-Call {
    param (
        [scriptblock]$ScriptBlock
    )
    & @ScriptBlock
    if ($lastexitcode -ne 0) {
        exit $lastexitcode
    }
}

function Extract-Nupkg {
    param (
        [Parameter(Mandatory=$true)][string]$Package,
        [Parameter(Mandatory=$true)][string]$OutDir
    )
    mkdir -p $OutDir 
    pushd $OutDir
    Invoke-Call -ScriptBlock { 7z x $Package }
    popd
}

function GetRuntimeLibrary {
    param (
        [Parameter(Mandatory=$true)][string]$RT_LinkType,
        [Parameter(Mandatory=$true)][string]$Configuration
    )

    if ( "$RT_LinkType" -ieq "static" ) {
        if ( "$Configuration" -ieq "debug" ) {
            return "MultiThreadedDebug"
        }
        else {
            return "MultiThreaded"
        }
    }
    else {
        if ( "$Configuration" -ieq "debug" ) {
            return "MultiThreadedDebugDLL"
        }
        else {
            return "MultiThreadedDLL"
        }
    }
}

function BuildProject {
    param (
        [Parameter(Mandatory=$true)][string]$RT_LinkType,
        [Parameter(Mandatory=$true)][string]$Configuration,
        [Parameter(Mandatory=$true)][string]$Platform
    )

    if ("$RT_LinkType" -ine "static" -and "$RT_LinkType" -ine "dll") {
        Write-Host "Incorrect Runtime Library Type: 'static' or 'dll'" -ForegroundColor Red
        exit 1
    }

    if ("$Configuration" -ine "debug" -and "$Configuration" -ine "release") {
        Write-Host "Incorrect Configuration: 'debug' or 'release'" -ForegroundColor Red
        exit 1
    }

    if ("$Platform" -ine "x86" -and "$Platform" -ine "x64") {
        Write-Host "Incorrect Platform: 'x86' or 'x64'" -ForegroundColor Red
        exit 1
    }

    pushd tests\windows_package
    $RuntimeLibrary=GetRuntimeLibrary -RT_LinkType $RT_LinkType -Configuration $Configuration
    Invoke-Call -ScriptBlock { &$msbuild windows_package.vcxproj /p:RuntimeLibrary=$RuntimeLibrary /p:Configuration=$Configuration /p:Platform=$Platform }
    popd
}

$msbuild = &"${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -prerelease  -products * -requires Microsoft.Component.MSBuild -find MSBuild\**\amd64\MSBuild.exe

if ( [string]::IsNullOrEmpty($msbuild) )
{
    Write-Host "Failed to locate MSBuild" -ForegroundColor Red
    exit 1
}

$libdatadog_nupkg = Get-ChildItem -Path $ArtifactsPath -Filter libdatadog*.nupkg -Recurse | %{ $_.FullName }

if ( [string]::IsNullOrEmpty($libdatadog_nupkg) )
{
    Write-Host "Failed to locate libdatadog nuget package file in the artifact folder $ArtifactsPath" -ForegroundColor Red
    exit 1
}

Extract-Nupkg -Package $libdatadog_nupkg -OutDir packages\libdatadog

# Runtime library DLL
BuildProject -RT_LinkType "dll" -Configuration "release" -Platform "x86"
BuildProject -RT_LinkType "dll" -Configuration "debug" -Platform "x86"
BuildProject -RT_LinkType "dll" -Configuration "release" -Platform "x64"
BuildProject -RT_LinkType "dll" -Configuration "debug" -Platform "x64"


# Runtime Library Static
BuildProject -RT_LinkType "static" -Configuration "release" -Platform "x86"
BuildProject -RT_LinkType "static" -Configuration "debug" -Platform "x86"
BuildProject -RT_LinkType "static" -Configuration "release" -Platform "x64"
BuildProject -RT_LinkType "static" -Configuration "debug" -Platform "x64"

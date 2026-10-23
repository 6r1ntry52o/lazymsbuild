<#
.SYNOPSIS
    Installs msbuild-extractor-sample, the design-time extractor `lazymsbuild sync` runs.

.DESCRIPTION
    Downloads a pinned release of Microsoft's msbuild-extractor-sample
    (https://github.com/microsoft/msbuild-extractor-sample) from its GitHub Releases, checks its
    SHA256, and puts it in %LOCALAPPDATA%\msbuild-extractor\, one of the places lazymsbuild looks
    (after PATH and <repository>\.tools\). Running it again changes nothing when the same version
    is in place.

    It also puts the .NET hostfxr.dll next to the exe. The extractor is a self-contained .NET
    program; when it cannot find Visual Studio's MSBuild it falls back to the .NET SDK and then
    fails with DllNotFoundException unless hostfxr.dll is next to it (--vs-path and --msbuild-path
    do not help, because MSBuild is registered before the arguments are read).

    Works in Windows PowerShell 5.1 and PowerShell 7. Needs no administrator rights.

.PARAMETER Version
    The release tag to install.

.PARAMETER Sha256
    The SHA256 of that release's msbuild-extractor-sample.exe. For another version, take it from
    the asset's "digest" in https://api.github.com/repos/microsoft/msbuild-extractor-sample/releases.

.PARAMETER Destination
    Where to put it. Default: %LOCALAPPDATA%\msbuild-extractor.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\install-extractor.ps1
#>
[CmdletBinding()]
param(
    [string]$Version = 'v0.3.0',
    [string]$Sha256 = '543c5cc6b57a1b3eb46b11e56b8f35a9ca8676106426bde6041ca2dc2e06f13c',
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'msbuild-extractor')
)

$ErrorActionPreference = 'Stop'
$expected = $Sha256.ToUpperInvariant()
$exe = Join-Path $Destination 'msbuild-extractor-sample.exe'
New-Item -ItemType Directory -Path $Destination -Force | Out-Null

if ((Test-Path $exe) -and (Get-FileHash $exe -Algorithm SHA256).Hash -eq $expected) {
    Write-Host "ok       $exe ($Version)"
} else {
    $url = "https://github.com/microsoft/msbuild-extractor-sample/releases/download/$Version/msbuild-extractor-sample.exe"
    $tmp = "$exe.download"
    Write-Host "download $url"
    # The progress bar makes Invoke-WebRequest very slow on a 76 MB file in Windows PowerShell.
    $progress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
    } finally {
        $ProgressPreference = $progress
    }
    $actual = (Get-FileHash $tmp -Algorithm SHA256).Hash
    if ($actual -ne $expected) {
        Remove-Item $tmp -Force
        throw "SHA256 mismatch for $url`n  expected $expected`n  actual   $actual"
    }
    Move-Item $tmp $exe -Force
    Write-Host "install  $exe ($Version)"
}

$hostfxr = Join-Path $Destination 'hostfxr.dll'
if (Test-Path $hostfxr) {
    Write-Host "ok       $hostfxr"
} else {
    $source = Get-ChildItem (Join-Path $env:ProgramFiles 'dotnet\host\fxr\*\hostfxr.dll') -ErrorAction SilentlyContinue |
        Sort-Object { [version]$_.Directory.Name } | Select-Object -Last 1
    if ($null -eq $source) {
        Write-Warning 'No .NET runtime found (%ProgramFiles%\dotnet\host\fxr). Install the .NET SDK or runtime and run this again; without hostfxr.dll the extractor can fail with DllNotFoundException.'
    } else {
        Copy-Item $source.FullName $hostfxr -Force
        Write-Host "copy     $($source.FullName) -> $hostfxr"
    }
}

Write-Host ''
& $exe --version
Write-Host 'lazymsbuild finds it there; check with: lazymsbuild doctor'

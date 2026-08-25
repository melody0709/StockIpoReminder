[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version = '0.2.2',
    [switch]$SkipTests,
    [switch]$KeepStaging
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $workspace 'Cargo.toml'
$targetDirectory = Join-Path $workspace 'target\release'
$builtExecutable = Join-Path $targetDirectory 'StockIpoReminder.exe'
$releaseParent = Join-Path $workspace 'artifacts\release'
$releaseDirectory = Join-Path $releaseParent $Version
$stagingParent = Join-Path $workspace 'artifacts\.release-staging'
$stagingDirectory = Join-Path $stagingParent ("$Version-" + [Guid]::NewGuid().ToString('N'))
$portableDirectory = Join-Path $stagingDirectory 'portable'

function Assert-SafeDescendant {
    param([string]$Path, [string]$Parent)
    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    if (-not $fullPath.StartsWith($fullParent + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing operation outside expected directory: $fullPath"
    }
}

function Invoke-Cargo {
    param([string[]]$Arguments)
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo failed with exit code $LASTEXITCODE" }
}

function Get-Sha256Hex {
    param([string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose(); $stream.Dispose() }
}

function New-ZipFromDirectory {
    param([string]$Source, [string]$Destination)
    if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
    [System.IO.Compression.ZipFile]::CreateFromDirectory($Source, $Destination, [System.IO.Compression.CompressionLevel]::Optimal, $false)
}

$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath
$configuredVersion = [regex]::Match($cargoText, '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ($configuredVersion -ne $Version) { throw "Version mismatch: Cargo.toml=$configuredVersion, requested=$Version" }

New-Item -ItemType Directory -Path $releaseParent -Force | Out-Null
New-Item -ItemType Directory -Path $stagingParent -Force | Out-Null
if (Test-Path -LiteralPath $releaseDirectory) {
    Assert-SafeDescendant -Path $releaseDirectory -Parent $releaseParent
    Remove-Item -LiteralPath $releaseDirectory -Recurse -Force
}
New-Item -ItemType Directory -Path $releaseDirectory -Force | Out-Null
New-Item -ItemType Directory -Path $portableDirectory -Force | Out-Null

try {
    if (-not $SkipTests) {
        Invoke-Cargo @('test', '--locked', '--manifest-path', $manifestPath)
    }
    Invoke-Cargo @('build', '--release', '--locked', '--manifest-path', $manifestPath)
    if (-not (Test-Path -LiteralPath $builtExecutable -PathType Leaf)) { throw 'Rust release executable is missing.' }

    Copy-Item -LiteralPath $builtExecutable -Destination (Join-Path $portableDirectory 'StockIpoReminder.exe') -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $portableDirectory -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination $portableDirectory -Force

    $portableName = "StockIpoReminder-$Version-win-x64-portable.zip"
    $setupName = "StockIpoReminder-Setup-$Version-win-x64.exe"
    $portablePath = Join-Path $releaseDirectory $portableName
    $setupPath = Join-Path $releaseDirectory $setupName
    New-ZipFromDirectory -Source $portableDirectory -Destination $portablePath
    Copy-Item -LiteralPath $builtExecutable -Destination $setupPath -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination $releaseDirectory -Force

    $artifacts = foreach ($path in @($portablePath, $setupPath)) {
        $item = Get-Item -LiteralPath $path
        [ordered]@{ name = $item.Name; sizeBytes = $item.Length; sha256 = Get-Sha256Hex $path }
    }
    $manifest = [ordered]@{
        product = 'StockIpoReminder'
        displayName = 'Stock IPO Reminder'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        runtime = 'win-x64-native'
        implementationLanguage = 'rust'
        ui = 'Slint'
        dotnetRuntimeRequired = $false
        minimumWindowsBuild = 19041
        signed = $false
        testsExecuted = -not $SkipTests
        artifacts = @($artifacts)
    }
    $manifestFile = Join-Path $releaseDirectory 'release-manifest.json'
    [System.IO.File]::WriteAllText($manifestFile, ($manifest | ConvertTo-Json -Depth 6), [System.Text.UTF8Encoding]::new($false))

    $hashLines = foreach ($file in Get-ChildItem -LiteralPath $releaseDirectory -File | Sort-Object Name) {
        "$(Get-Sha256Hex $file.FullName)  $($file.Name)"
    }
    [System.IO.File]::WriteAllLines((Join-Path $releaseDirectory 'SHA256SUMS.txt'), $hashLines, [System.Text.UTF8Encoding]::new($false))
    Write-Host "Rust release created: $releaseDirectory"
}
finally {
    if (-not $KeepStaging -and (Test-Path -LiteralPath $stagingDirectory)) {
        Assert-SafeDescendant -Path $stagingDirectory -Parent $stagingParent
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
    }
}

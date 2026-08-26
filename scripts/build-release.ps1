[CmdletBinding()]
param(
    [string]$Version,
    [ValidateSet('Runtime', 'Portable', 'Msi', 'All')]
    [string]$PackageMode = 'All',
    [switch]$SkipTests,
    [switch]$KeepStaging
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $workspace 'Cargo.toml'
$buildRoot = Join-Path $workspace 'build'
$cargoRoot = Join-Path $buildRoot 'cargo'
$builtExecutable = Join-Path $cargoRoot 'release\StockIpoReminder.exe'
$runtimeDirectory = Join-Path $buildRoot 'run\x64-release'
$testArtifactsRoot = Join-Path $buildRoot 'artifacts\tests'
$diagnosticArtifactsRoot = Join-Path $buildRoot 'artifacts\diagnostics'
$logsRoot = Join-Path $buildRoot 'logs'
$packagesRoot = Join-Path $buildRoot 'packages'
$stagingParent = Join-Path $cargoRoot 'package-staging'
$wixProject = Join-Path $workspace 'packaging\windows\StockIpoReminder.Installer.wixproj'
$productIdentity = Join-Path $workspace 'packaging\windows\ProductIdentity.wxi'
$productInstanceGenerator = Join-Path $workspace 'scripts\generate-wix-product-instance.ps1'
$stopRuntime = Join-Path $workspace 'scripts\stop-runtime-process.ps1'
$layoutValidator = Join-Path $workspace 'scripts\validate-build-layout.ps1'

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

function Invoke-DotNet {
    param([string[]]$Arguments)
    & dotnet @Arguments
    if ($LASTEXITCODE -ne 0) { throw "dotnet failed with exit code $LASTEXITCODE" }
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
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $Source,
        $Destination,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false)
}

$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath
$configuredVersion = [regex]::Match(
    $cargoText,
    '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $configuredVersion }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Invalid version: $Version" }
if ($configuredVersion -ne $Version) {
    throw "Version mismatch: Cargo.toml=$configuredVersion, requested=$Version"
}

$releaseDirectory = Join-Path $packagesRoot $Version
$stagingDirectory = Join-Path $stagingParent ("$Version-" + [Guid]::NewGuid().ToString('N'))
$portableDirectory = Join-Path $stagingDirectory 'portable'
$generatedWixDirectory = Join-Path $stagingDirectory 'wix-generated'
$msiOutputDirectory = Join-Path $stagingDirectory 'msi-output'
$wixIntermediateDirectory = Join-Path $stagingDirectory 'wix-intermediate'

New-Item -ItemType Directory -Path `
    $cargoRoot, $testArtifactsRoot, $diagnosticArtifactsRoot, $logsRoot, $packagesRoot `
    -Force | Out-Null

try {
    if (-not $SkipTests) {
        Invoke-Cargo @('test', '--locked', '--manifest-path', $manifestPath)
    }
    Invoke-Cargo @('build', '--release', '--locked', '--manifest-path', $manifestPath)
    if (-not (Test-Path -LiteralPath $builtExecutable -PathType Leaf)) {
        throw "Rust release executable is missing: $builtExecutable"
    }

    & $stopRuntime -ExecutablePath (Join-Path $runtimeDirectory 'StockIpoReminder.exe')
    if (Test-Path -LiteralPath $runtimeDirectory) {
        Assert-SafeDescendant -Path $runtimeDirectory -Parent $buildRoot
        Remove-Item -LiteralPath $runtimeDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $runtimeDirectory -Force | Out-Null
    Copy-Item -LiteralPath $builtExecutable -Destination (Join-Path $runtimeDirectory 'StockIpoReminder.exe') -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $runtimeDirectory -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination $runtimeDirectory -Force

    $runtimeFiles = foreach ($name in @('StockIpoReminder.exe', 'README.md', 'RELEASE_NOTES.md')) {
        $path = Join-Path $runtimeDirectory $name
        $item = Get-Item -LiteralPath $path
        [ordered]@{ name = $name; sizeBytes = $item.Length; sha256 = Get-Sha256Hex $path }
    }
    $runtimeManifest = [ordered]@{
        schemaVersion = '1'
        product = 'StockIpoReminder'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        implementation = 'rust'
        architecture = 'x64'
        files = @($runtimeFiles)
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $runtimeDirectory 'runtime-manifest.json'),
        ($runtimeManifest | ConvertTo-Json -Depth 6),
        [System.Text.UTF8Encoding]::new($false))

    & $layoutValidator -BuildRoot $buildRoot -RuntimeDirectory $runtimeDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Build layout validation failed after runtime install.' }

    if ($PackageMode -eq 'Runtime') {
        Write-Host "Rust runtime installed: $runtimeDirectory"
        return
    }

    if (Test-Path -LiteralPath $releaseDirectory) {
        Assert-SafeDescendant -Path $releaseDirectory -Parent $packagesRoot
        Remove-Item -LiteralPath $releaseDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $releaseDirectory -Force | Out-Null
    New-Item -ItemType Directory -Path $stagingDirectory -Force | Out-Null

    $artifactPaths = [System.Collections.Generic.List[string]]::new()
    if ($PackageMode -in @('Portable', 'All')) {
        New-Item -ItemType Directory -Path $portableDirectory -Force | Out-Null
        Copy-Item -Path (Join-Path $runtimeDirectory '*') -Destination $portableDirectory -Force
        $portablePath = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64-portable.zip"
        New-ZipFromDirectory -Source $portableDirectory -Destination $portablePath
        $artifactPaths.Add($portablePath)
    }

    if ($PackageMode -in @('Msi', 'All')) {
        New-Item -ItemType Directory -Path $generatedWixDirectory, $msiOutputDirectory, $wixIntermediateDirectory -Force | Out-Null
        $msiName = "StockIpoReminder-$Version-win-x64.msi"
        $msiOutputName = [System.IO.Path]::GetFileNameWithoutExtension($msiName)
        & $productInstanceGenerator `
            -ProductIdentityPath $productIdentity `
            -ProductVersion $Version `
            -OutputPath (Join-Path $generatedWixDirectory 'ProductInstance.generated.wxi')
        if ($LASTEXITCODE -ne 0) {
            throw "WiX product identity generation failed with exit code $LASTEXITCODE"
        }
        Invoke-DotNet @(
            'build', $wixProject,
            '--configuration', 'Release',
            '--nologo',
            "-p:GeneratedWixDirectory=$generatedWixDirectory",
            "-p:PayloadRoot=$runtimeDirectory",
            "-p:StockIpoReminderMsiOutputName=$msiOutputName",
            "-p:StockIpoReminderMsiOutputDirectory=$msiOutputDirectory",
            "-p:StockIpoReminderWixIntermediateDirectory=$wixIntermediateDirectory",
            "-p:BaseIntermediateOutputPath=$wixIntermediateDirectory\",
            "-p:MSBuildProjectExtensionsPath=$wixIntermediateDirectory\"
        )
        $builtMsi = Get-ChildItem -LiteralPath $msiOutputDirectory -Filter $msiName -File -Recurse |
            Select-Object -First 1
        if ($null -eq $builtMsi) { throw 'WiX MSI output is missing.' }
        $msiPath = Join-Path $releaseDirectory $msiName
        Copy-Item -LiteralPath $builtMsi.FullName -Destination $msiPath -Force
        $artifactPaths.Add($msiPath)
    }

    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination $releaseDirectory -Force

    $artifacts = foreach ($path in $artifactPaths) {
        $item = Get-Item -LiteralPath $path
        [ordered]@{ name = $item.Name; sizeBytes = $item.Length; sha256 = Get-Sha256Hex $path }
    }
    $releaseManifest = [ordered]@{
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
        installer = 'msi-per-machine-selectable-directory'
        packageMode = $PackageMode
        artifacts = @($artifacts)
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $releaseDirectory 'release-manifest.json'),
        ($releaseManifest | ConvertTo-Json -Depth 6),
        [System.Text.UTF8Encoding]::new($false))

    $hashLines = foreach ($file in Get-ChildItem -LiteralPath $releaseDirectory -File | Sort-Object Name) {
        "$(Get-Sha256Hex $file.FullName)  $($file.Name)"
    }
    [System.IO.File]::WriteAllLines(
        (Join-Path $releaseDirectory 'SHA256SUMS.txt'),
        $hashLines,
        [System.Text.UTF8Encoding]::new($false))

    & $layoutValidator -BuildRoot $buildRoot -RuntimeDirectory $runtimeDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Build layout validation failed after packaging.' }
    Write-Host "Rust packages created: $releaseDirectory"
}
finally {
    if (-not $KeepStaging -and (Test-Path -LiteralPath $stagingDirectory)) {
        Assert-SafeDescendant -Path $stagingDirectory -Parent $stagingParent
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
    }
}

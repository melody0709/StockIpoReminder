[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version = '0.2.2',
    [string]$SmokeReportPath,
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$releaseDirectory = Join-Path $workspace "build\packages\$Version"
$portablePath = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64-portable.zip"
$msiPath = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64.msi"
$manifestPath = Join-Path $releaseDirectory 'release-manifest.json'
$hashPath = Join-Path $releaseDirectory 'SHA256SUMS.txt'
$auditParent = Join-Path $workspace 'build\cargo\audit-staging'
$auditRoot = Join-Path $auditParent ([Guid]::NewGuid().ToString('N'))
$extractRoot = Join-Path $auditRoot 'portable'
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $workspace ("build\artifacts\tests\audit\release-rust-$Version-" + [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmss') + '.json')
}

function Assert-Condition { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }
function Get-Sha256Hex {
    param([string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose(); $stream.Dispose() }
}
function Get-PeMachine {
    param([string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $reader = [System.IO.BinaryReader]::new($stream)
    try {
        $stream.Position = 0x3c
        $peOffset = $reader.ReadInt32()
        $stream.Position = $peOffset
        Assert-Condition ($reader.ReadUInt32() -eq 0x00004550) "Invalid PE signature: $Path"
        $reader.ReadUInt16()
    }
    finally { $reader.Dispose(); $stream.Dispose() }
}
function Test-MsiSignature {
    param([string]$Path)
    $expected = [byte[]](0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1)
    $actual = [System.IO.File]::ReadAllBytes($Path)[0..7]
    for ($index = 0; $index -lt $expected.Length; $index++) {
        if ($actual[$index] -ne $expected[$index]) { return $false }
    }
    $true
}
function Get-WorkspaceRelativePath {
    param([string]$Path)
    $fullPath = [System.IO.Path]::GetFullPath($Path)
    $prefix = $workspace.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    Assert-Condition ($fullPath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) "Path is outside workspace: $fullPath"
    $fullPath.Substring($prefix.Length).Replace('\', '/')
}

$checks = [System.Collections.Generic.List[object]]::new()
function Add-Check { param([string]$Name, [string]$Detail) $checks.Add([ordered]@{ name = $Name; passed = $true; detail = $Detail }) }

New-Item -ItemType Directory -Path $auditRoot -Force | Out-Null
try {
    foreach ($path in @($portablePath, $msiPath, $manifestPath, $hashPath)) {
        Assert-Condition (Test-Path -LiteralPath $path -PathType Leaf) "Missing release file: $path"
    }
    Add-Check 'release.files' 'Portable ZIP, MSI, manifest, and SHA256SUMS exist.'

    $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
    Assert-Condition ([string]$manifest.version -eq $Version) 'Manifest version mismatch.'
    Assert-Condition ([string]$manifest.implementationLanguage -eq 'rust') 'Manifest implementation is not Rust.'
    Assert-Condition (-not [bool]$manifest.dotnetRuntimeRequired) 'Manifest still requires .NET.'
    Assert-Condition ([string]$manifest.ui -eq 'Slint') 'Manifest UI is not Slint.'
    Assert-Condition ([string]$manifest.installer -eq 'msi-per-machine-selectable-directory') 'Manifest installer contract is not MSI with a selectable directory.'
    foreach ($artifact in $manifest.artifacts) {
        $path = Join-Path $releaseDirectory ([string]$artifact.name)
        Assert-Condition ((Get-Item -LiteralPath $path).Length -eq [long]$artifact.sizeBytes) "Artifact size mismatch: $path"
        Assert-Condition ((Get-Sha256Hex $path) -eq [string]$artifact.sha256) "Artifact hash mismatch: $path"
    }
    Add-Check 'manifest.rust-native' 'Manifest declares Rust + Slint and no .NET runtime dependency.'

    [System.IO.Compression.ZipFile]::ExtractToDirectory($portablePath, $extractRoot)
    $entries = @(Get-ChildItem -LiteralPath $extractRoot -Recurse -File)
    $appPath = Join-Path $extractRoot 'StockIpoReminder.exe'
    Assert-Condition (Test-Path -LiteralPath $appPath -PathType Leaf) 'Portable app executable is missing.'
    $forbidden = @($entries | Where-Object { $_.Name -match '(?i)\.(?:dll|pdb|deps\.json|runtimeconfig\.json)$' -or $_.Name -match '(?i)^(?:coreclr|hostfxr|hostpolicy|clrjit)' })
    $forbiddenNames = @($forbidden | ForEach-Object { $_.Name })
    Assert-Condition ($forbidden.Count -eq 0) ('Portable package contains .NET/runtime files: ' + ($forbiddenNames -join ', '))
    Assert-Condition ($entries.Count -le 4) "Portable package unexpectedly contains $($entries.Count) files."
    Add-Check 'portable.single-native-exe' "Portable archive contains one Rust EXE and documentation only: $($entries.Count) files."

    Assert-Condition ((Get-PeMachine $appPath) -eq 0x8664) 'Portable app is not AMD64 PE.'
    Assert-Condition (Test-MsiSignature $msiPath) 'Installer is not a Windows Installer compound file.'
    Add-Check 'binary.formats' 'App is AMD64 PE and installer is an MSI compound file.'

    $cargoManifest = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'Cargo.toml')
    Assert-Condition ($cargoManifest -match '(?m)^name\s*=\s*"stock-ipo-reminder"') 'Cargo package is not formal stock-ipo-reminder.'
    Assert-Condition ($cargoManifest -match ('(?m)^version\s*=\s*"' + [regex]::Escape($Version) + '"')) 'Cargo version mismatch.'
    $releaseScripts = (Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'scripts\build-release.ps1')) + (Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'scripts\smoke-release.ps1'))
    Assert-Condition ($releaseScripts -notmatch '(?i)\.tools[\\/]dotnet|\.csproj|StockIpoReminder\.App') 'Formal build/smoke scripts still invoke legacy .NET application inputs.'
    Assert-Condition ($releaseScripts -match 'StockIpoReminder\.Installer\.wixproj') 'Formal build does not invoke the WiX MSI project.'
    Add-Check 'source.release-entrypoints' 'Cargo builds the application and WiX builds the MSI; no .NET application runtime is packaged.'

    $legacyExtensions = @('.cs', '.xaml', '.csproj', '.sln')
    $sourceRoots = @('src', 'ui', 'tests', 'scripts', 'assets') | ForEach-Object { Join-Path $workspace $_ }
    $legacySources = @(
        Get-ChildItem -LiteralPath $workspace -File | Where-Object { $legacyExtensions -contains $_.Extension.ToLowerInvariant() }
        foreach ($sourceRoot in $sourceRoots) {
            if (Test-Path -LiteralPath $sourceRoot -PathType Container) {
                Get-ChildItem -LiteralPath $sourceRoot -File -Recurse | Where-Object { $legacyExtensions -contains $_.Extension.ToLowerInvariant() }
            }
        }
    )
    Assert-Condition ($legacySources.Count -eq 0) ('Legacy .NET/WPF sources remain: ' + (($legacySources | ForEach-Object FullName) -join ', '))
    Add-Check 'source.rust-only' 'Active source tree contains no C#, WPF, project, or solution files.'

    if ([string]::IsNullOrWhiteSpace($SmokeReportPath)) {
        $SmokeReportPath = Get-ChildItem -LiteralPath (Join-Path $workspace 'build\artifacts\tests\smoke') -Filter "windows-rust-$Version-*.json" -File -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1 -ExpandProperty FullName
    }
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($SmokeReportPath)) 'Rust Windows smoke report was not found.'
    $smoke = Get-Content -Raw -Encoding UTF8 -LiteralPath $SmokeReportPath | ConvertFrom-Json
    Assert-Condition ([bool]$smoke.success) 'Rust Windows smoke failed.'
    Assert-Condition ([string]$smoke.implementation -eq 'rust') 'Smoke report is not Rust.'
    Assert-Condition ([long]$smoke.memory.privateBytes -lt [long]$smoke.memory.limitBytes) 'Idle memory gate failed.'
    Assert-Condition ([bool]$smoke.checks.msiAdministrativeExtract -and [bool]$smoke.checks.msiPayloadSelfTest -and [bool]$smoke.checks.selectableInstallDirectoryAuthoring) 'MSI smoke gates failed.'
    Add-Check 'evidence.windows-smoke' 'Rust UI, SQLite, MSI administrative extraction, selectable directory authoring, and sub-100MB idle memory smoke passed.'

    $report = [ordered]@{
        schemaVersion = '2'
        success = $true
        product = 'StockIpoReminder'
        implementation = 'rust'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        releaseDirectory = "build/packages/$Version"
        smokeReport = Get-WorkspaceRelativePath $SmokeReportPath
        checks = @($checks)
    }
    $parent = Split-Path -Parent $OutputPath
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    [System.IO.File]::WriteAllText($OutputPath, ($report | ConvertTo-Json -Depth 8), [System.Text.UTF8Encoding]::new($false))
    Write-Host "Rust release audit report: $OutputPath"
}
finally {
    if (Test-Path -LiteralPath $auditRoot) { Remove-Item -LiteralPath $auditRoot -Recurse -Force }
}

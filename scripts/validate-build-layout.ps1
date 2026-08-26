[CmdletBinding()]
param(
    [string]$BuildRoot,
    [string]$RuntimeDirectory,
    [switch]$SkipRuntime,
    [switch]$AllowIncompleteRuntime
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
    $BuildRoot = Join-Path (Split-Path $PSScriptRoot -Parent) 'build'
}
if ([string]::IsNullOrWhiteSpace($RuntimeDirectory)) {
    $RuntimeDirectory = Join-Path $BuildRoot 'run\x64-release'
}

function Get-AbsolutePath([string]$Path) {
    [System.IO.Path]::GetFullPath($Path)
}

function Get-Sha256Hex([string]$Path) {
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose(); $stream.Dispose() }
}

function Test-ReparsePoint([System.IO.FileSystemInfo]$Item) {
    ($Item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
}

$buildRootPath = Get-AbsolutePath $BuildRoot
$runtimeRootPath = Get-AbsolutePath $RuntimeDirectory
$workspace = Get-AbsolutePath (Split-Path $PSScriptRoot -Parent)
$issues = [System.Collections.Generic.List[string]]::new()

if (-not (Test-Path -LiteralPath $buildRootPath -PathType Container)) {
    throw "Build root does not exist: $buildRootPath"
}

$allowedRootEntries = @{
    'cargo' = 'Container'
    'run' = 'Container'
    'artifacts' = 'Container'
    'logs' = 'Container'
    'packages' = 'Container'
    'README.txt' = 'Leaf'
}
foreach ($entry in Get-ChildItem -LiteralPath $buildRootPath -Force) {
    if (-not $allowedRootEntries.ContainsKey($entry.Name)) {
        $issues.Add("Unexpected build root entry: $($entry.FullName)")
        continue
    }
    $expectedType = $allowedRootEntries[$entry.Name]
    if ($expectedType -eq 'Container' -and -not $entry.PSIsContainer) {
        $issues.Add("Expected directory but found file: $($entry.FullName)")
    }
    if ($expectedType -eq 'Leaf' -and $entry.PSIsContainer) {
        $issues.Add("Expected file but found directory: $($entry.FullName)")
    }
    if (Test-ReparsePoint $entry) {
        $issues.Add("Build root reparse point is forbidden: $($entry.FullName)")
    }
}

foreach ($legacyName in @('target', 'artifacts', 'publish')) {
    $legacyPath = Join-Path $workspace $legacyName
    if (Test-Path -LiteralPath $legacyPath) {
        $issues.Add("Legacy generated root must be migrated into build/: $legacyPath")
    }
}

foreach ($generatedSourcePath in @(
    (Join-Path $workspace 'packaging\windows\obj'),
    (Join-Path $workspace 'packaging\windows\bin'))) {
    if (Test-Path -LiteralPath $generatedSourcePath) {
        $issues.Add("Generated installer output is forbidden in the source tree: $generatedSourcePath")
    }
}

$runRoot = Join-Path $buildRootPath 'run'
if (Test-Path -LiteralPath $runRoot -PathType Container) {
    foreach ($entry in Get-ChildItem -LiteralPath $runRoot -Force) {
        if ($entry.Name -ne 'x64-release' -or -not $entry.PSIsContainer -or (Test-ReparsePoint $entry)) {
            $issues.Add("Unexpected run layout entry: $($entry.FullName)")
        }
    }
}

$artifactsRoot = Join-Path $buildRootPath 'artifacts'
if (Test-Path -LiteralPath $artifactsRoot -PathType Container) {
    foreach ($entry in Get-ChildItem -LiteralPath $artifactsRoot -Force) {
        if ($entry.Name -notin @('tests', 'diagnostics') -or -not $entry.PSIsContainer -or (Test-ReparsePoint $entry)) {
            $issues.Add("Unexpected artifacts layout entry: $($entry.FullName)")
        }
    }
    $testsRoot = Join-Path $artifactsRoot 'tests'
    if (Test-Path -LiteralPath $testsRoot -PathType Container) {
        foreach ($entry in Get-ChildItem -LiteralPath $testsRoot -Force) {
            if ($entry.Name -notin @('smoke', 'audit') -or -not $entry.PSIsContainer -or (Test-ReparsePoint $entry)) {
                $issues.Add("Unexpected test artifact entry: $($entry.FullName)")
            }
        }
    }
    $diagnosticsRoot = Join-Path $artifactsRoot 'diagnostics'
    if (Test-Path -LiteralPath $diagnosticsRoot -PathType Container) {
        foreach ($entry in Get-ChildItem -LiteralPath $diagnosticsRoot -Force) {
            if ($entry.Name -ne 'memory' -or -not $entry.PSIsContainer -or (Test-ReparsePoint $entry)) {
                $issues.Add("Unexpected diagnostic artifact entry: $($entry.FullName)")
            }
        }
    }
}

$packagesRoot = Join-Path $buildRootPath 'packages'
if (Test-Path -LiteralPath $packagesRoot -PathType Container) {
    foreach ($versionDirectory in Get-ChildItem -LiteralPath $packagesRoot -Force) {
        if (-not $versionDirectory.PSIsContainer -or $versionDirectory.Name -notmatch '^\d+\.\d+\.\d+$' -or (Test-ReparsePoint $versionDirectory)) {
            $issues.Add("Unexpected packages layout entry: $($versionDirectory.FullName)")
            continue
        }
        $version = $versionDirectory.Name
        $allowedNames = @(
            "StockIpoReminder-$version-win-x64-portable.zip",
            "StockIpoReminder-$version-win-x64.msi",
            'README.md',
            'RELEASE_NOTES.md',
            'release-manifest.json',
            'SHA256SUMS.txt'
        )
        foreach ($entry in Get-ChildItem -LiteralPath $versionDirectory.FullName -Force) {
            if ($entry.PSIsContainer -or $entry.Name -notin $allowedNames -or (Test-ReparsePoint $entry)) {
                $issues.Add("Unexpected package file: $($entry.FullName)")
            }
        }
    }
}

if (-not $SkipRuntime) {
    if (-not (Test-Path -LiteralPath $runtimeRootPath -PathType Container)) {
        if (-not $AllowIncompleteRuntime) {
            $issues.Add("Runtime directory is missing: $runtimeRootPath")
        }
    } else {
        $runtimeItem = Get-Item -LiteralPath $runtimeRootPath -Force
        if (Test-ReparsePoint $runtimeItem) {
            $issues.Add("Runtime reparse point is forbidden: $runtimeRootPath")
        } else {
            $expectedNames = @(
                'StockIpoReminder.exe',
                'README.md',
                'RELEASE_NOTES.md',
                'runtime-manifest.json'
            )
            foreach ($entry in Get-ChildItem -LiteralPath $runtimeRootPath -Force) {
                if ($entry.PSIsContainer -or $entry.Name -notin $expectedNames -or (Test-ReparsePoint $entry)) {
                    $issues.Add("Unexpected runtime entry: $($entry.FullName)")
                }
            }
            foreach ($name in $expectedNames) {
                $path = Join-Path $runtimeRootPath $name
                if (-not $AllowIncompleteRuntime -and -not (Test-Path -LiteralPath $path -PathType Leaf)) {
                    $issues.Add("Expected runtime file is missing: $path")
                }
            }

            $runtimeManifestPath = Join-Path $runtimeRootPath 'runtime-manifest.json'
            if (Test-Path -LiteralPath $runtimeManifestPath -PathType Leaf) {
                try {
                    $manifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $runtimeManifestPath | ConvertFrom-Json
                    foreach ($file in $manifest.files) {
                        $path = Join-Path $runtimeRootPath ([string]$file.name)
                        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { continue }
                        if ((Get-Item -LiteralPath $path).Length -ne [long]$file.sizeBytes) {
                            $issues.Add("Runtime size mismatch: $path")
                        }
                        if ((Get-Sha256Hex $path) -ne [string]$file.sha256) {
                            $issues.Add("Runtime hash mismatch: $path")
                        }
                    }
                } catch {
                    $issues.Add("Invalid runtime manifest: $($_.Exception.Message)")
                }
            }
        }
    }
}

if ($issues.Count -gt 0) {
    foreach ($issue in $issues) { [Console]::Error.WriteLine("ERROR: $issue") }
    exit 1
}

$runtimeFileCount = 0
if (-not $SkipRuntime -and (Test-Path -LiteralPath $runtimeRootPath -PathType Container)) {
    $runtimeFileCount = @(Get-ChildItem -LiteralPath $runtimeRootPath -File -Force).Count
}
Write-Host "Build layout valid: root='$buildRootPath', runtimeFiles=$runtimeFileCount"

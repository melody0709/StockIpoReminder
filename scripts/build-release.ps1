[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version = '0.1.0',
    [switch]$SkipTests,
    [switch]$KeepStaging
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$dotnet = Join-Path $workspace '.tools\dotnet\dotnet.exe'
if (-not (Test-Path -LiteralPath $dotnet -PathType Leaf)) {
    $dotnet = (Get-Command dotnet -ErrorAction Stop).Source
}

$releaseParent = Join-Path $workspace 'artifacts\release'
$releaseDirectory = Join-Path $releaseParent $Version
$stagingParent = Join-Path $workspace 'artifacts\.release-staging'
$stagingDirectory = Join-Path $stagingParent ("$Version-" + [Guid]::NewGuid().ToString('N'))

function Assert-SafeDescendant {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$Parent
    )

    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $prefix = $fullParent + [System.IO.Path]::DirectorySeparatorChar
    if ($fullPath -eq $fullParent -or -not $fullPath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing destructive operation outside expected directory: $fullPath"
    }
}

function Invoke-DotNet {
    param([Parameter(Mandatory)] [string[]]$Arguments)

    & $dotnet @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw ('dotnet command failed with exit code {0}: {1}' -f $LASTEXITCODE, ($Arguments -join ' '))
    }
}

function Copy-DirectoryContents {
    param(
        [Parameter(Mandatory)] [string]$Source,
        [Parameter(Mandatory)] [string]$Destination
    )

    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    Copy-Item -Path (Join-Path $Source '*') -Destination $Destination -Recurse -Force
}

function New-ZipFromDirectory {
    param(
        [Parameter(Mandatory)] [string]$Source,
        [Parameter(Mandatory)] [string]$Destination
    )

    if (Test-Path -LiteralPath $Destination) {
        Remove-Item -LiteralPath $Destination -Force
    }

    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $Source,
        $Destination,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false)
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)] [string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hashBytes = $sha256.ComputeHash($stream)
        return [System.BitConverter]::ToString($hashBytes).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
}

[xml]$buildProps = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'Directory.Build.props')
$configuredVersion = [string]($buildProps.Project.PropertyGroup.Version | Select-Object -First 1)
if ($configuredVersion -ne $Version) {
    throw "Version mismatch: Directory.Build.props=$configuredVersion, requested=$Version"
}

New-Item -ItemType Directory -Path $releaseParent -Force | Out-Null
New-Item -ItemType Directory -Path $stagingParent -Force | Out-Null
if (Test-Path -LiteralPath $releaseDirectory) {
    Assert-SafeDescendant -Path $releaseDirectory -Parent $releaseParent
    Remove-Item -LiteralPath $releaseDirectory -Recurse -Force
}

New-Item -ItemType Directory -Path $releaseDirectory -Force | Out-Null
New-Item -ItemType Directory -Path $stagingDirectory -Force | Out-Null

$appProject = Join-Path $workspace 'src\StockIpoReminder.App\StockIpoReminder.App.csproj'
$setupProject = Join-Path $workspace 'tools\StockIpoReminder.Setup\StockIpoReminder.Setup.csproj'
$solution = Join-Path $workspace 'StockIpoReminder.sln'
$appPublish = Join-Path $stagingDirectory 'app-publish'
$portableDirectory = Join-Path $stagingDirectory 'portable'
$uninstallerPublish = Join-Path $stagingDirectory 'uninstaller-publish'
$installerPayload = Join-Path $stagingDirectory 'installer-payload'
$payloadZip = Join-Path $stagingDirectory 'installer-payload.zip'
$setupPublish = Join-Path $stagingDirectory 'setup-publish'

try {
    Invoke-DotNet @('restore', $solution, '--verbosity', 'minimal')
    if (-not $SkipTests) {
        Invoke-DotNet @('test', $solution, '--configuration', 'Release', '--no-restore', '--verbosity', 'minimal')
    }

    Invoke-DotNet @(
        'publish', $appProject,
        '--configuration', 'Release',
        '--runtime', 'win-x64',
        '--self-contained', 'true',
        '--no-restore',
        '--output', $appPublish,
        '-p:ContinuousIntegrationBuild=true',
        '-p:DebugType=None',
        '-p:DebugSymbols=false',
        "-p:Version=$Version"
    )

    $publishedExecutable = Join-Path $appPublish 'StockIpoReminder.exe'
    if (-not (Test-Path -LiteralPath $publishedExecutable -PathType Leaf)) {
        throw 'Published application executable is missing.'
    }

    $fileVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($publishedExecutable).FileVersion
    if (-not $fileVersion.StartsWith($Version, [StringComparison]::Ordinal)) {
        throw "Published application version mismatch: $fileVersion"
    }

    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $appPublish -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination $appPublish -Force
    Copy-DirectoryContents -Source $appPublish -Destination $portableDirectory

    Invoke-DotNet @(
        'publish', $setupProject,
        '--configuration', 'Release',
        '--runtime', 'win-x64',
        '--self-contained', 'true',
        '--no-restore',
        '--output', $uninstallerPublish,
        '-p:ContinuousIntegrationBuild=true',
        '-p:DebugType=None',
        '-p:DebugSymbols=false',
        '-p:AssemblyName=StockIpoReminder.Uninstaller',
        "-p:Version=$Version"
    )

    $uninstallerExecutable = Join-Path $uninstallerPublish 'StockIpoReminder.Uninstaller.exe'
    if (-not (Test-Path -LiteralPath $uninstallerExecutable -PathType Leaf)) {
        throw 'Published uninstaller executable is missing.'
    }

    Copy-DirectoryContents -Source $appPublish -Destination $installerPayload
    Copy-Item -LiteralPath $uninstallerExecutable -Destination (Join-Path $installerPayload 'StockIpoReminder.Uninstaller.exe') -Force
    New-ZipFromDirectory -Source $installerPayload -Destination $payloadZip

    Invoke-DotNet @(
        'publish', $setupProject,
        '--configuration', 'Release',
        '--runtime', 'win-x64',
        '--self-contained', 'true',
        '--no-restore',
        '--output', $setupPublish,
        '-p:ContinuousIntegrationBuild=true',
        '-p:DebugType=None',
        '-p:DebugSymbols=false',
        '-p:AssemblyName=StockIpoReminder.Setup',
        "-p:PayloadZip=$payloadZip",
        "-p:Version=$Version"
    )

    $setupExecutable = Join-Path $setupPublish 'StockIpoReminder.Setup.exe'
    if (-not (Test-Path -LiteralPath $setupExecutable -PathType Leaf)) {
        throw 'Published setup executable is missing.'
    }

    $portableName = "StockIpoReminder-$Version-win-x64-portable.zip"
    $setupName = "StockIpoReminder-Setup-$Version-win-x64.exe"
    $portablePath = Join-Path $releaseDirectory $portableName
    $setupPath = Join-Path $releaseDirectory $setupName
    New-ZipFromDirectory -Source $portableDirectory -Destination $portablePath
    Copy-Item -LiteralPath $setupExecutable -Destination $setupPath -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination (Join-Path $releaseDirectory 'README.md') -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination (Join-Path $releaseDirectory 'RELEASE_NOTES.md') -Force

    $primaryArtifacts = @($portablePath, $setupPath)
    $artifactEntries = foreach ($artifact in $primaryArtifacts) {
        $item = Get-Item -LiteralPath $artifact
        [ordered]@{
            name = $item.Name
            sizeBytes = $item.Length
            sha256 = Get-Sha256Hex -Path $item.FullName
        }
    }

    $manifest = [ordered]@{
        product = 'StockIpoReminder'
        displayName = 'Stock IPO Reminder'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        runtime = 'win-x64'
        selfContained = $true
        minimumWindowsBuild = 19041
        signed = $false
        testsExecuted = -not $SkipTests
        artifacts = $artifactEntries
    }
    $manifestPath = Join-Path $releaseDirectory 'release-manifest.json'
    [System.IO.File]::WriteAllText(
        $manifestPath,
        ($manifest | ConvertTo-Json -Depth 6),
        [System.Text.UTF8Encoding]::new($false))

    $hashFiles = Get-ChildItem -LiteralPath $releaseDirectory -File | Sort-Object Name
    $hashLines = foreach ($file in $hashFiles) {
        $hash = Get-Sha256Hex -Path $file.FullName
        "$hash  $($file.Name)"
    }
    [System.IO.File]::WriteAllLines(
        (Join-Path $releaseDirectory 'SHA256SUMS.txt'),
        $hashLines,
        [System.Text.UTF8Encoding]::new($false))

    Write-Host "Release created: $releaseDirectory"
}
finally {
    if (-not $KeepStaging -and (Test-Path -LiteralPath $stagingDirectory)) {
        Assert-SafeDescendant -Path $stagingDirectory -Parent $stagingParent
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
    }
}

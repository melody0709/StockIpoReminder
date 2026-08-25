[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version = '0.2.2',
    [switch]$KeepSandbox
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$releaseDirectory = Join-Path $workspace "artifacts\release\$Version"
$portableZip = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64-portable.zip"
$setupExecutable = Join-Path $releaseDirectory "StockIpoReminder-Setup-$Version-win-x64.exe"
$smokeParent = Join-Path ([System.IO.Path]::GetTempPath()) 'StockIpoReminder-rust-smoke'
$smokeRoot = Join-Path $smokeParent ("$Version-" + [Guid]::NewGuid().ToString('N'))
$portableDirectory = Join-Path $smokeRoot 'portable'
$installDirectory = Join-Path $smokeRoot 'install\StockIpoReminder'
$dataRoot = Join-Path $smokeRoot 'data\StockIpoReminder'
$reports = Join-Path $smokeRoot 'reports'
$evidenceDirectory = Join-Path $workspace 'artifacts\smoke'
$outputPath = Join-Path $evidenceDirectory ("windows-rust-$Version-" + [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmss') + '.json')

function Assert-SafeDescendant {
    param([string]$Path, [string]$Parent, [switch]$AllowParent)
    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    if ((-not $AllowParent -and $fullPath -eq $fullParent) -or (-not $fullPath.StartsWith($fullParent + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase))) {
        throw "Unsafe path: $fullPath"
    }
}

function Assert-Condition { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }

function Invoke-Product {
    param([string]$Executable, [string[]]$Arguments, [int]$TimeoutSeconds = 60)
    $process = Start-Process -FilePath $Executable -ArgumentList $Arguments -PassThru -WindowStyle Hidden
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) { try { $process.Kill() } catch {}; throw "Process timed out: $Executable" }
    if ($process.ExitCode -ne 0) { throw "Process failed: $Executable exit=$($process.ExitCode)" }
}

Assert-Condition (Test-Path -LiteralPath $portableZip -PathType Leaf) 'Portable ZIP is missing.'
Assert-Condition (Test-Path -LiteralPath $setupExecutable -PathType Leaf) 'Rust setup executable is missing.'
New-Item -ItemType Directory -Path $smokeRoot -Force | Out-Null
New-Item -ItemType Directory -Path $reports -Force | Out-Null
New-Item -ItemType Directory -Path $evidenceDirectory -Force | Out-Null

$checks = [ordered]@{}
$process = $null
try {
    [System.IO.Compression.ZipFile]::ExtractToDirectory($portableZip, $portableDirectory)
    $portableExecutable = Join-Path $portableDirectory 'StockIpoReminder.exe'
    Assert-Condition (Test-Path -LiteralPath $portableExecutable -PathType Leaf) 'Portable executable is missing.'
    $checks.portableExtracted = $true

    $selfTestReport = Join-Path $reports 'portable-self-test.json'
    Invoke-Product $portableExecutable @('--data-root', $dataRoot, '--self-test-report', $selfTestReport)
    $selfTest = Get-Content -Raw -Encoding UTF8 -LiteralPath $selfTestReport | ConvertFrom-Json
    Assert-Condition ([bool]$selfTest.success) 'Portable self-test failed.'
    Assert-Condition ([string]$selfTest.implementation -eq 'rust') 'Self-test is not the Rust implementation.'
    Assert-Condition ([string]$selfTest.databaseIntegrity -eq 'ok') 'SQLite integrity check failed.'
    $checks.selfTest = $true

    $process = Start-Process -FilePath $portableExecutable -ArgumentList @('--data-root', $dataRoot, '--background', '--skip-startup-sync', '--exit-after-seconds', '8') -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 3
    $sample = Get-Process -Id $process.Id -ErrorAction Stop
    $privateBytes = [long]$sample.PrivateMemorySize64
    $workingSet = [long]$sample.WorkingSet64
    Assert-Condition ($privateBytes -lt 100MB) "Idle Private Bytes exceeded 100MB: $privateBytes"

    Assert-Condition ($process.WaitForExit(20000)) 'Background UI smoke did not exit.'
    Assert-Condition ($process.ExitCode -eq 0) "Background UI smoke failed: $($process.ExitCode)"
    $checks.backgroundUi = $true
    $checks.privateBytesUnder100Mb = $true

    $installReport = Join-Path $reports 'install.json'
    Invoke-Product $setupExecutable @('--install', '--install-root', $installDirectory, '--data-root', $dataRoot, '--no-launch', '--report', $installReport)
    $install = Get-Content -Raw -Encoding UTF8 -LiteralPath $installReport | ConvertFrom-Json
    Assert-Condition ([bool]$install.success) 'Rust per-user installer failed.'
    $installedExecutable = Join-Path $installDirectory 'StockIpoReminder.exe'
    Assert-Condition (Test-Path -LiteralPath $installedExecutable -PathType Leaf) 'Installed executable is missing.'
    Assert-Condition (Test-Path -LiteralPath (Join-Path $installDirectory 'StockIpoReminder.Uninstaller.exe') -PathType Leaf) 'Installed uninstaller is missing.'
    $checks.install = $true

    $installedSelfTestReport = Join-Path $reports 'installed-self-test.json'
    Invoke-Product $installedExecutable @('--data-root', $dataRoot, '--self-test-report', $installedSelfTestReport)
    $installedSelfTest = Get-Content -Raw -Encoding UTF8 -LiteralPath $installedSelfTestReport | ConvertFrom-Json
    Assert-Condition ([bool]$installedSelfTest.success) 'Installed self-test failed.'
    $checks.installedSelfTest = $true

    $uninstallReport = Join-Path $reports 'uninstall.json'
    Invoke-Product $setupExecutable @('--uninstall', '--install-root', $installDirectory, '--data-root', $dataRoot, '--report', $uninstallReport)
    $uninstall = Get-Content -Raw -Encoding UTF8 -LiteralPath $uninstallReport | ConvertFrom-Json
    Assert-Condition ([bool]$uninstall.success) 'Rust uninstaller failed.'
    Assert-Condition (-not (Test-Path -LiteralPath $installDirectory)) 'Install directory remains after uninstall.'
    Assert-Condition (Test-Path -LiteralPath $dataRoot -PathType Container) 'Normal uninstall removed user data.'
    $checks.uninstallPreservesData = $true

    $report = [ordered]@{
        schemaVersion = '2'
        success = $true
        implementation = 'rust'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        checks = $checks
        memory = [ordered]@{ privateBytes = $privateBytes; workingSetBytes = $workingSet; limitBytes = 100MB }
        evidence = [ordered]@{
            portableSelfTest = 'portable-self-test.json'
            installedSelfTest = 'installed-self-test.json'
            install = 'install.json'
            uninstall = 'uninstall.json'
        }
    }
    [System.IO.File]::WriteAllText($outputPath, ($report | ConvertTo-Json -Depth 8), [System.Text.UTF8Encoding]::new($false))
    Write-Host "Rust Windows smoke report: $outputPath"
}
catch {
    $report = [ordered]@{ schemaVersion = '2'; success = $false; implementation = 'rust'; version = $Version; generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O'); checks = $checks; error = $_.Exception.Message }
    [System.IO.File]::WriteAllText($outputPath, ($report | ConvertTo-Json -Depth 8), [System.Text.UTF8Encoding]::new($false))
    throw
}
finally {
    if ($null -ne $process -and -not $process.HasExited) {
        try { $process.Kill(); $process.WaitForExit() } catch {}
    }
    if (-not $KeepSandbox -and (Test-Path -LiteralPath $smokeRoot)) {
        Assert-SafeDescendant -Path $smokeRoot -Parent $smokeParent
        Remove-Item -LiteralPath $smokeRoot -Recurse -Force
    }
}

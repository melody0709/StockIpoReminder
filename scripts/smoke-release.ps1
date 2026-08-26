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
$releaseDirectory = Join-Path $workspace "build\packages\$Version"
$portableZip = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64-portable.zip"
$msiPath = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64.msi"
$smokeParent = Join-Path ([System.IO.Path]::GetTempPath()) 'StockIpoReminder-rust-smoke'
$smokeRoot = Join-Path $smokeParent ("$Version-" + [Guid]::NewGuid().ToString('N'))
$portableDirectory = Join-Path $smokeRoot 'portable'
$msiAdminDirectory = Join-Path $smokeRoot 'msi-admin'
$dataRoot = Join-Path $smokeRoot 'data\StockIpoReminder'
$reports = Join-Path $smokeRoot 'reports'
$evidenceDirectory = Join-Path $workspace 'build\artifacts\tests\smoke'
$outputPath = Join-Path $evidenceDirectory ("windows-rust-$Version-" + [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmss') + '.json')

function Assert-SafeDescendant {
    param([string]$Path, [string]$Parent)
    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    if (-not $fullPath.StartsWith($fullParent + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Unsafe path: $fullPath"
    }
}

function Assert-Condition { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }

function Invoke-Product {
    param([string]$Executable, [string[]]$Arguments, [int]$TimeoutSeconds = 60)
    $process = Start-Process -FilePath $Executable -ArgumentList $Arguments -PassThru -WindowStyle Hidden
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        try { $process.Kill() } catch {}
        throw "Process timed out: $Executable"
    }
    if ($process.ExitCode -ne 0) { throw "Process failed: $Executable exit=$($process.ExitCode)" }
}

function Invoke-MsiAdministrativeExtraction {
    param([string]$Package, [string]$Destination)
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    $arguments = @('/a', "`"$Package`"", '/qn', "TARGETDIR=`"$Destination`"")
    $process = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\msiexec.exe') `
        -ArgumentList $arguments -PassThru -Wait -WindowStyle Hidden
    if ($process.ExitCode -ne 0) { throw "MSI administrative extraction failed: exit=$($process.ExitCode)" }
}

Assert-Condition (Test-Path -LiteralPath $portableZip -PathType Leaf) 'Portable ZIP is missing.'
Assert-Condition (Test-Path -LiteralPath $msiPath -PathType Leaf) 'MSI package is missing.'
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

    $process = Start-Process -FilePath $portableExecutable -ArgumentList @('--data-root', $dataRoot, '--background', '--skip-startup-sync', '--skip-auto-start-registration', '--exit-after-seconds', '8') -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 3
    $sample = Get-Process -Id $process.Id -ErrorAction Stop
    $privateBytes = [long]$sample.PrivateMemorySize64
    $workingSet = [long]$sample.WorkingSet64
    Assert-Condition ($privateBytes -lt 100MB) "Idle Private Bytes exceeded 100MB: $privateBytes"
    Assert-Condition ($process.WaitForExit(20000)) 'Background UI smoke did not exit.'
    Assert-Condition ($process.ExitCode -eq 0) "Background UI smoke failed: $($process.ExitCode)"
    $checks.backgroundUi = $true
    $checks.privateBytesUnder100Mb = $true

    Invoke-MsiAdministrativeExtraction $msiPath $msiAdminDirectory
    $msiExecutables = @(Get-ChildItem -LiteralPath $msiAdminDirectory -Filter 'StockIpoReminder.exe' -File -Recurse)
    Assert-Condition ($msiExecutables.Count -eq 1) "MSI administrative image contains $($msiExecutables.Count) application executables."
    $checks.msiAdministrativeExtract = $true

    $msiSelfTestReport = Join-Path $reports 'msi-payload-self-test.json'
    Invoke-Product $msiExecutables[0].FullName @('--data-root', $dataRoot, '--self-test-report', $msiSelfTestReport)
    $msiSelfTest = Get-Content -Raw -Encoding UTF8 -LiteralPath $msiSelfTestReport | ConvertFrom-Json
    Assert-Condition ([bool]$msiSelfTest.success) 'MSI payload self-test failed.'
    $checks.msiPayloadSelfTest = $true

    $packageSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'packaging\windows\Package.wxs')
    $uiSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'packaging\windows\InstallerUi.wxs')
    Assert-Condition ($packageSource -match 'ProgramFiles64Folder') 'MSI does not default to 64-bit Program Files.'
    Assert-Condition ($packageSource -match 'RegistrySearch') 'MSI does not remember the selected install directory.'
    Assert-Condition ($uiSource -match 'InstallDirDlg') 'MSI does not expose an install-directory picker.'
    $checks.selectableInstallDirectoryAuthoring = $true

    $report = [ordered]@{
        schemaVersion = '3'
        success = $true
        implementation = 'rust'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        checks = $checks
        memory = [ordered]@{ privateBytes = $privateBytes; workingSetBytes = $workingSet; limitBytes = 100MB }
        evidence = [ordered]@{
            portableSelfTest = 'portable-self-test.json'
            msiPayloadSelfTest = 'msi-payload-self-test.json'
        }
    }
    [System.IO.File]::WriteAllText($outputPath, ($report | ConvertTo-Json -Depth 8), [System.Text.UTF8Encoding]::new($false))
    Write-Host "Rust Windows smoke report: $outputPath"
}
catch {
    $report = [ordered]@{
        schemaVersion = '3'
        success = $false
        implementation = 'rust'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        checks = $checks
        error = $_.Exception.Message
    }
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

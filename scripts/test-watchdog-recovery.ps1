[CmdletBinding()]
param(
    [string]$Version,
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'Cargo.toml')
$configuredVersion = [regex]::Match(
    $cargoText,
    '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $configuredVersion }
if ($configuredVersion -ne $Version) {
    throw "Version mismatch: Cargo.toml=$configuredVersion, requested=$Version"
}

$executable = Join-Path $workspace 'build\run\x64-release\StockIpoReminder.exe'
$stamp = [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmss')
$evidenceDirectory = Join-Path $workspace 'build\artifacts\tests\watchdog'
$testRoot = Join-Path $evidenceDirectory ("data\watchdog-matrix-$Version-$stamp")
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $evidenceDirectory "watchdog-matrix-$Version-$stamp.json"
}

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Wait-ChildProcess {
    param(
        [int]$ParentProcessId,
        [string]$ExpectedExecutable,
        [int[]]$ExcludedIds = @(),
        [int]$TimeoutSeconds = 15
    )
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $child = Get-CimInstance Win32_Process -Filter ("ParentProcessId = " + $ParentProcessId) -ErrorAction SilentlyContinue |
            Where-Object {
                $_.ExecutablePath -eq $ExpectedExecutable -and
                $ExcludedIds -notcontains [int]$_.ProcessId
            } |
            Select-Object -First 1
        if ($null -ne $child) { return $child }
        Start-Sleep -Milliseconds 200
    } while ([DateTimeOffset]::UtcNow -lt $deadline)
    return $null
}

function Start-Supervisor {
    param([string]$DataRoot, [int]$ExitAfterSeconds = 0)
    New-Item -ItemType Directory -Path $DataRoot -Force | Out-Null
    $arguments = @(
        '--data-root', $DataRoot,
        '--background',
        '--skip-startup-sync',
        '--skip-auto-start-registration')
    if ($ExitAfterSeconds -gt 0) {
        $arguments += @('--exit-after-seconds', $ExitAfterSeconds.ToString())
    }
    return Start-Process -FilePath $executable -ArgumentList $arguments -PassThru -WindowStyle Hidden
}

function Stop-ScopedProcesses {
    param([string]$DataRoot)
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.ExecutablePath -eq $executable -and
            $_.CommandLine -like ('*' + $DataRoot + '*')
        } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
}

Assert-Condition (Test-Path -LiteralPath $executable -PathType Leaf) 'Release executable is missing.'
New-Item -ItemType Directory -Path $testRoot, $evidenceDirectory -Force | Out-Null

$singleRoot = Join-Path $testRoot 'single\StockIpoReminder'
$loopRoot = Join-Path $testRoot 'loop\StockIpoReminder'
$orphanRoot = Join-Path $testRoot 'supervisor-exit\StockIpoReminder'
$singleSupervisor = $null
$loopSupervisor = $null
$orphanSupervisor = $null
$orphanChild = $null

try {
    $singleSupervisor = Start-Supervisor -DataRoot $singleRoot -ExitAfterSeconds 6
    $singleChild = Wait-ChildProcess -ParentProcessId $singleSupervisor.Id -ExpectedExecutable $executable
    Assert-Condition ($null -ne $singleChild) 'Single-recovery child was not created.'
    Stop-Process -Id $singleChild.ProcessId -Force
    $singleReplacement = Wait-ChildProcess -ParentProcessId $singleSupervisor.Id -ExpectedExecutable $executable -ExcludedIds @([int]$singleChild.ProcessId) -TimeoutSeconds 10
    Assert-Condition ($null -ne $singleReplacement) 'Watchdog did not restart after one forced termination.'
    Assert-Condition ($singleSupervisor.WaitForExit(20000)) 'Single-recovery supervisor did not exit normally.'
    Assert-Condition ($singleSupervisor.ExitCode -eq 0) "Single-recovery supervisor exit=$($singleSupervisor.ExitCode)"

    $singleReports = @(Get-ChildItem -LiteralPath (Join-Path $singleRoot 'diagnostics\crashes') -Filter 'crash-recovery-*.json' -File)
    Assert-Condition ($singleReports.Count -eq 1) "Single recovery created $($singleReports.Count) reports."
    $singleReport = Get-Content -Raw -Encoding UTF8 -LiteralPath $singleReports[0].FullName | ConvertFrom-Json
    Assert-Condition ([bool]$singleReport.restartScheduled) 'Single recovery report did not schedule restart.'

    $loopSupervisor = Start-Supervisor -DataRoot $loopRoot
    $seenIds = [System.Collections.Generic.List[int]]::new()
    $restartTimeouts = @(10, 10, 18, 38)
    for ($attempt = 0; $attempt -lt 4; $attempt++) {
        $loopChild = Wait-ChildProcess `
            -ParentProcessId $loopSupervisor.Id `
            -ExpectedExecutable $executable `
            -ExcludedIds @($seenIds) `
            -TimeoutSeconds $restartTimeouts[$attempt]
        Assert-Condition ($null -ne $loopChild) "Crash-loop child $($attempt + 1) was not created."
        $seenIds.Add([int]$loopChild.ProcessId)
        Stop-Process -Id $loopChild.ProcessId -Force
    }
    Assert-Condition ($loopSupervisor.WaitForExit(15000)) 'Crash-loop supervisor did not stop after the restart limit.'
    Assert-Condition ($loopSupervisor.ExitCode -ne 0) 'Crash-loop supervisor unexpectedly reported success.'

    $loopReports = @(Get-ChildItem -LiteralPath (Join-Path $loopRoot 'diagnostics\crashes') -Filter 'crash-recovery-*.json' -File | Sort-Object Name)
    Assert-Condition ($loopReports.Count -eq 4) "Crash loop created $($loopReports.Count) reports instead of 4."
    $finalLoopReport = Get-Content -Raw -Encoding UTF8 -LiteralPath $loopReports[-1].FullName | ConvertFrom-Json
    Assert-Condition (-not [bool]$finalLoopReport.restartScheduled) 'Fourth crash still scheduled another restart.'
    Assert-Condition ([int]$finalLoopReport.crashesInTenMinuteWindow -eq 4) 'Crash-loop report did not record four crashes.'

    $orphanSupervisor = Start-Supervisor -DataRoot $orphanRoot -ExitAfterSeconds 6
    $orphanChild = Wait-ChildProcess -ParentProcessId $orphanSupervisor.Id -ExpectedExecutable $executable
    Assert-Condition ($null -ne $orphanChild) 'Supervisor-exit child was not created.'
    Stop-Process -Id $orphanSupervisor.Id -Force
    $orphanSupervisor.WaitForExit()
    Start-Sleep -Seconds 1
    $childAfterSupervisorExit = Get-Process -Id $orphanChild.ProcessId -ErrorAction SilentlyContinue
    Assert-Condition ($null -ne $childAfterSupervisorExit) 'Main process was incorrectly terminated with the supervisor.'
    Assert-Condition ($childAfterSupervisorExit.WaitForExit(12000)) 'Orphaned main process did not complete its normal exit timer.'

    $report = [ordered]@{
        schemaVersion = '1'
        success = $true
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        singleRecovery = [ordered]@{
            killedProcessId = [int]$singleChild.ProcessId
            restartedProcessId = [int]$singleReplacement.ProcessId
            restartDelaySeconds = [int]$singleReport.restartDelaySeconds
            supervisorExitCode = [int]$singleSupervisor.ExitCode
        }
        crashLoop = [ordered]@{
            killedProcessIds = @($seenIds)
            crashReportCount = $loopReports.Count
            finalRestartScheduled = [bool]$finalLoopReport.restartScheduled
            crashesInTenMinuteWindow = [int]$finalLoopReport.crashesInTenMinuteWindow
            supervisorExitCode = [int]$loopSupervisor.ExitCode
        }
        supervisorExit = [ordered]@{
            supervisorWasForcedToExit = $true
            mainProcessContinued = $true
            mainProcessExitedNormallyAfterward = $true
        }
    }
    [System.IO.File]::WriteAllText(
        $OutputPath,
        ($report | ConvertTo-Json -Depth 8),
        [System.Text.UTF8Encoding]::new($false))
    Write-Host "Watchdog matrix report: $OutputPath"
}
finally {
    foreach ($root in @($singleRoot, $loopRoot, $orphanRoot)) {
        Stop-ScopedProcesses -DataRoot $root
    }
}

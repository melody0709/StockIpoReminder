[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version = '0.1.0',
    [switch]$KeepSandbox
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$releaseDirectory = Join-Path $workspace "artifacts\release\$Version"
$setupExecutable = Join-Path $releaseDirectory "StockIpoReminder-Setup-$Version-win-x64.exe"
$portableZip = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64-portable.zip"
$smokeParent = Join-Path ([System.IO.Path]::GetTempPath()) 'StockIpoReminder-smoke'
$smokeRoot = Join-Path $smokeParent ("$Version-" + [Guid]::NewGuid().ToString('N'))
$installDirectory = Join-Path $smokeRoot 'install\StockIpoReminder'
$dataRoot = Join-Path $smokeRoot 'data\StockIpoReminder'
$portableDirectory = Join-Path $smokeRoot 'portable'
$reportDirectory = Join-Path $smokeRoot 'reports'
$evidenceParent = Join-Path $workspace 'artifacts\smoke'
$evidenceDate = [DateTimeOffset]::Now.ToString('yyyyMMdd')
$uiEvidenceDirectory = Join-Path $evidenceParent "ui-$Version-$evidenceDate"
$processEvidenceDirectory = Join-Path $evidenceParent "process-$Version-$evidenceDate"
$recoveryEvidenceDirectory = Join-Path $evidenceParent "recovery-$Version-$evidenceDate"
$instanceId = 'smoke'
$uninstallRegistryKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\StockIpoReminder-smoke'
$startMenuShortcut = $null

function Assert-SafeDescendant {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$Parent
    )

    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $prefix = $fullParent + [System.IO.Path]::DirectorySeparatorChar
    if ($fullPath -eq $fullParent -or -not $fullPath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing destructive operation outside smoke sandbox: $fullPath"
    }
}

function Get-WorkspaceRelativePath {
    param([Parameter(Mandatory)] [string]$Path)

    $fullPath = [System.IO.Path]::GetFullPath($Path)
    Assert-SafeDescendant -Path $fullPath -Parent $workspace
    $basePath = $workspace.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    $baseUri = [Uri]::new($basePath)
    $pathUri = [Uri]::new($fullPath)
    return [Uri]::UnescapeDataString($baseUri.MakeRelativeUri($pathUri).ToString())
}

function Invoke-CheckedProcess {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [Parameter(Mandatory)] [string[]]$Arguments,
        [int[]]$AllowedExitCodes = @(0)
    )

    $process = Start-Process -FilePath $FilePath -ArgumentList $Arguments -Wait -PassThru -WindowStyle Hidden
    if ($AllowedExitCodes -notcontains $process.ExitCode) {
        throw "Process failed with exit code $($process.ExitCode): $FilePath $($Arguments -join ' ')"
    }

    return $process.ExitCode
}

function Wait-ForFile {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [int]$TimeoutSeconds = 30
    )

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            return
        }

        Start-Sleep -Milliseconds 200
    }

    throw "Timed out waiting for file: $Path"
}

function Reset-SafeDirectory {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$Parent
    )

    if (Test-Path -LiteralPath $Path) {
        Assert-SafeDescendant -Path $Path -Parent $Parent
        Remove-Item -LiteralPath $Path -Recurse -Force
    }

    New-Item -ItemType Directory -Path $Path -Force | Out-Null
}

function Test-ReportRedaction {
    param([Parameter(Mandatory)] [string]$Path)

    $content = Get-Content -Raw -Encoding UTF8 -LiteralPath $Path
    if ($content.IndexOf($workspace, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
        $content.IndexOf($smokeRoot, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
        $content.IndexOf('Authorization', [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
        $content.IndexOf('Cookie', [StringComparison]::OrdinalIgnoreCase) -ge 0) {
        return $false
    }

    return $content -notmatch 'https?://[^"\s]+\?'
}

function Read-JsonFile {
    param([Parameter(Mandatory)] [string]$Path)
    return Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json
}

function Get-TaskName {
    param([Parameter(Mandatory)] [string]$Root)

    $normalized = [System.IO.Path]::GetFullPath($Root).TrimEnd([System.IO.Path]::DirectorySeparatorChar).ToUpperInvariant()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($normalized)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = [System.BitConverter]::ToString($sha256.ComputeHash($bytes)).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
    }

    return "StockIpoReminder-$($hash.Substring(0, 12))"
}

function Test-SqliteIntegrity {
    param([Parameter(Mandatory)] [string]$DatabasePath)

    $sqlite = (Get-Command sqlite3.exe -ErrorAction Stop).Source
    $output = & $sqlite $DatabasePath 'PRAGMA integrity_check;'
    if ($LASTEXITCODE -ne 0) {
        throw "sqlite3 integrity_check failed for $DatabasePath"
    }

    $result = ($output | Select-Object -First 1).Trim()
    if ($result -ne 'ok') {
        throw "SQLite integrity_check returned: $result"
    }

    return $result
}

if (-not (Test-Path -LiteralPath $setupExecutable -PathType Leaf)) {
    throw "Setup artifact not found: $setupExecutable"
}
if (-not (Test-Path -LiteralPath $portableZip -PathType Leaf)) {
    throw "Portable artifact not found: $portableZip"
}

New-Item -ItemType Directory -Path $smokeParent -Force | Out-Null
New-Item -ItemType Directory -Path $smokeRoot -Force | Out-Null
New-Item -ItemType Directory -Path $reportDirectory -Force | Out-Null
New-Item -ItemType Directory -Path $evidenceParent -Force | Out-Null
Reset-SafeDirectory -Path $uiEvidenceDirectory -Parent $evidenceParent
Reset-SafeDirectory -Path $processEvidenceDirectory -Parent $evidenceParent
Reset-SafeDirectory -Path $recoveryEvidenceDirectory -Parent $evidenceParent
$taskName = Get-TaskName -Root $dataRoot
$startedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$checks = [ordered]@{}

try {
    $installReport = Join-Path $reportDirectory 'install.json'
    Invoke-CheckedProcess -FilePath $setupExecutable -Arguments @(
        '--quiet', '--no-launch',
        '--install-dir', $installDirectory,
        '--data-root', $dataRoot,
        '--instance-id', $instanceId,
        '--report-file', $installReport
    ) | Out-Null
    $installResult = Read-JsonFile -Path $installReport
    $checks.installSucceeded = [bool]$installResult.success
    $checks.installedExecutableExists = Test-Path -LiteralPath (Join-Path $installDirectory 'StockIpoReminder.exe') -PathType Leaf
    $checks.uninstallerExists = Test-Path -LiteralPath (Join-Path $installDirectory 'StockIpoReminder.Uninstaller.exe') -PathType Leaf
    $checks.installManifestExists = Test-Path -LiteralPath (Join-Path $installDirectory 'install-manifest.json') -PathType Leaf
    $installedManifest = Read-JsonFile -Path (Join-Path $installDirectory 'install-manifest.json')
    $startMenuShortcut = [string]$installedManifest.startMenuShortcutPath
    $checks.uninstallRegistryExists = Test-Path -LiteralPath $uninstallRegistryKey
    $checks.startMenuShortcutExists = Test-Path -LiteralPath $startMenuShortcut -PathType Leaf
    $checks.dataMarkerExists = Test-Path -LiteralPath (Join-Path $dataRoot '.stock-ipo-reminder-data.json') -PathType Leaf

    $instanceSwitchReport = Join-Path $reportDirectory 'reject-instance-switch.json'
    Invoke-CheckedProcess -FilePath $setupExecutable -Arguments @(
        '--quiet', '--no-launch',
        '--install-dir', $installDirectory,
        '--data-root', $dataRoot,
        '--instance-id', 'smoke-other',
        '--report-file', $instanceSwitchReport
    ) -AllowedExitCodes @(1) | Out-Null
    $instanceSwitchResult = Read-JsonFile -Path $instanceSwitchReport
    $checks.installRejectsInstanceSwitch = (-not [bool]$instanceSwitchResult.success) -and ([string]$instanceSwitchResult.message -like '*不能切换到不同实例标识*')

    $otherDataRoot = Join-Path $smokeRoot 'other-data\StockIpoReminder'
    $dataRootSwitchReport = Join-Path $reportDirectory 'reject-data-root-switch.json'
    Invoke-CheckedProcess -FilePath $setupExecutable -Arguments @(
        '--quiet', '--no-launch',
        '--install-dir', $installDirectory,
        '--data-root', $otherDataRoot,
        '--instance-id', $instanceId,
        '--report-file', $dataRootSwitchReport
    ) -AllowedExitCodes @(1) | Out-Null
    $dataRootSwitchResult = Read-JsonFile -Path $dataRootSwitchReport
    $checks.installRejectsDataRootSwitch = (-not [bool]$dataRootSwitchResult.success) -and ([string]$dataRootSwitchResult.message -like '*不能切换到不同数据目录*')
    $manifestAfterRejectedSwitches = Read-JsonFile -Path (Join-Path $installDirectory 'install-manifest.json')
    $checks.rejectedSwitchesPreserveManifest = ([string]$manifestAfterRejectedSwitches.instanceId -eq $instanceId) -and ([System.IO.Path]::GetFullPath([string]$manifestAfterRejectedSwitches.dataRoot) -eq [System.IO.Path]::GetFullPath($dataRoot))

    $appExecutable = Join-Path $installDirectory 'StockIpoReminder.exe'
    $readyFile = Join-Path $reportDirectory 'installed-ready.json'
    $appArguments = @(
        '--background', '--smoke-mode', '--smoke-enable-autostart',
        '--data-root', $dataRoot,
        '--ready-file', $readyFile,
        '--exit-after-seconds', '15'
    )
    $appProcess = Start-Process -FilePath $appExecutable -ArgumentList $appArguments -PassThru -WindowStyle Hidden
    $startedProcesses.Add($appProcess)
    Wait-ForFile -Path $readyFile -TimeoutSeconds 30
    $ready = Read-JsonFile -Path $readyFile
    $checks.installedAppReady = $ready.status -eq 'ready'
    $checks.installedAppBackground = [bool]$ready.background
    $checks.installedAppSmokeMode = [bool]$ready.smokeMode
    $checks.autoStartConfiguredByApp = [bool]$ready.autoStartConfigured
    $checks.installedTrayIconVisible = [bool]$ready.trayIconVisible
    $checks.installedTrayStatusPresent = -not [string]::IsNullOrWhiteSpace([string]$ready.trayStatusText)
    $checks.installedAppUsesIsolatedData = [System.IO.Path]::GetFullPath([string]$ready.dataRoot) -eq [System.IO.Path]::GetFullPath($dataRoot)
    $checks.installedAppVersion = [string]$ready.version
    $checks.noConsoleWindow = $appProcess.MainWindowHandle -eq [IntPtr]::Zero
    $checks.databaseCreated = Test-Path -LiteralPath (Join-Path $dataRoot 'stock-ipo-reminder.db') -PathType Leaf

    $scheduledTask = Get-ScheduledTask -TaskName $taskName -ErrorAction Stop
    $taskAction = @($scheduledTask.Actions)[0]
    $taskTrigger = @($scheduledTask.Triggers)[0]
    $checks.autoStartTaskExists = $null -ne $scheduledTask
    $checks.autoStartExecutableMatches = [System.IO.Path]::GetFullPath([string]$taskAction.Execute).TrimEnd([System.IO.Path]::DirectorySeparatorChar) -eq [System.IO.Path]::GetFullPath($appExecutable).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $checks.autoStartArgumentsContainBackground = ([string]$taskAction.Arguments).IndexOf('--background', [StringComparison]::OrdinalIgnoreCase) -ge 0
    $checks.autoStartArgumentsContainDataRoot = ([string]$taskAction.Arguments).IndexOf($dataRoot, [StringComparison]::OrdinalIgnoreCase) -ge 0
    $checks.autoStartWorkingDirectoryMatches = [System.IO.Path]::GetFullPath([string]$taskAction.WorkingDirectory).TrimEnd([System.IO.Path]::DirectorySeparatorChar) -eq [System.IO.Path]::GetFullPath($installDirectory).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $checks.autoStartHasLogonTrigger = $taskTrigger.CimClass.CimClassName -eq 'MSFT_TaskLogonTrigger' -and [bool]$taskTrigger.Enabled
    $checks.autoStartLeastPrivilege = [string]$scheduledTask.Principal.RunLevel -eq 'Limited'

    $secondReadyFile = Join-Path $reportDirectory 'second-ready.json'
    $secondProcess = Start-Process -FilePath $appExecutable -ArgumentList @(
        '--background', '--smoke-mode',
        '--data-root', $dataRoot,
        '--ready-file', $secondReadyFile
    ) -Wait -PassThru -WindowStyle Hidden
    Wait-ForFile -Path $secondReadyFile -TimeoutSeconds 10
    $secondReady = Read-JsonFile -Path $secondReadyFile
    $checks.singleInstanceRejected = $secondProcess.ExitCode -eq 3 -and $secondReady.status -eq 'already-running'

    if (-not $appProcess.WaitForExit(30000)) {
        throw 'Installed application smoke process did not exit on schedule.'
    }
    $checks.installedAppCleanExit = $appProcess.ExitCode -eq 0

    $uiDataRoot = Join-Path $smokeRoot 'ui-data'
    $uiReport = Join-Path $uiEvidenceDirectory 'report.json'
    $uiProcess = Start-Process -FilePath $appExecutable -ArgumentList @(
        '--background', '--smoke-mode', '--smoke-seed-scenarios',
        '--ui-smoke-report', $uiReport,
        '--data-root', $uiDataRoot
    ) -PassThru
    $startedProcesses.Add($uiProcess)
    if (-not $uiProcess.WaitForExit(90000)) {
        Stop-Process -Id $uiProcess.Id -Force
        throw 'Installed application UI smoke timed out.'
    }
    $uiResult = Read-JsonFile -Path $uiReport
    $uiCheckCount = @($uiResult.checks.PSObject.Properties).Count
    $requiredUiCheckNames = @(
        'trayIconVisible',
        'trayStatusShowsDataWarning',
        'missingApplyCodeTriggersVisibleReviewWarning',
        'futureCalendarShowsPostponedStatus',
        'futureCalendarShowsSuspendedStatus',
        'futureCalendarShowsTerminatedStatus',
        'rescheduledEventInvalidatesOldAcknowledgement',
        'changedEventCanBeAcknowledgedAgain',
        'reminderShowsHourlyLevel',
        'reminderEscalatesToFifteenMinutes',
        'reminderEscalatesToFiveMinutes',
        'reminderEscalatesToTwoMinutes',
        'manualOverrideChangesEffectiveField',
        'manualOverrideAuditIsVisible',
        'manualOverrideLinksOfficialAnnouncement'
    )
    $uiCheckNames = @($uiResult.checks.PSObject.Properties.Name)
    $missingRequiredUiChecks = @($requiredUiCheckNames | Where-Object { $uiCheckNames -notcontains $_ })
    $uiScreenshotPaths = @($uiResult.screenshots.PSObject.Properties | ForEach-Object { [string]$_.Value })
    $uiMissingScreenshots = @($uiScreenshotPaths | Where-Object {
        $screenshotPath = Join-Path $uiEvidenceDirectory $_
        -not (Test-Path -LiteralPath $screenshotPath -PathType Leaf) -or (Get-Item -LiteralPath $screenshotPath).Length -eq 0
    })
    $checks.uiSmokeSucceeded = $uiProcess.ExitCode -eq 0 -and [bool]$uiResult.success
    $checks.uiSmokeHasExpectedChecks = $uiCheckCount -ge 55 -and @($uiResult.failedChecks).Count -eq 0
    $checks.uiSmokeRequiredAcceptanceChecksPresent = [string]$uiResult.scenarioVersion -eq '2' -and $missingRequiredUiChecks.Count -eq 0
    $checks.uiSmokeScreenshotsComplete = $uiScreenshotPaths.Count -ge 10 -and $uiMissingScreenshots.Count -eq 0
    $checks.uiSmokeUsesRedactedDataRoot = [string]$uiResult.dataRoot -eq '<isolated-smoke-data-root>'
    $checks.uiSmokeUsesRelativeScreenshotPaths = @($uiScreenshotPaths | Where-Object {
        [System.IO.Path]::IsPathRooted($_) -or $_.IndexOf('..', [StringComparison]::Ordinal) -ge 0
    }).Count -eq 0
    $checks.uiSmokeReportRedacted = Test-ReportRedaction -Path $uiReport

    $processDataRoot = Join-Path $smokeRoot 'process-data'
    $processPrepareReport = Join-Path $processEvidenceDirectory 'prepare.json'
    $processVerifyReport = Join-Path $processEvidenceDirectory 'verify.json'
    $prepareProcess = Start-Process -FilePath $appExecutable -ArgumentList @(
        '--background', '--smoke-mode', '--smoke-seed-scenarios',
        '--process-smoke-stage', 'prepare',
        '--process-smoke-report', $processPrepareReport,
        '--data-root', $processDataRoot
    ) -PassThru -WindowStyle Hidden
    $startedProcesses.Add($prepareProcess)
    try {
        Wait-ForFile -Path $processPrepareReport -TimeoutSeconds 30
        $processPrepareResult = Read-JsonFile -Path $processPrepareReport
        $prepareProcess.Refresh()
        $checks.processPrepareSucceeded = [bool]$processPrepareResult.success
        $checks.processPrepareRemainsAliveForForcedTermination = -not $prepareProcess.HasExited
    }
    finally {
        $prepareProcess.Refresh()
        if (-not $prepareProcess.HasExited) {
            Stop-Process -Id $prepareProcess.Id -Force
            $prepareProcess.WaitForExit(10000) | Out-Null
        }
    }

    Start-Sleep -Seconds 3
    Invoke-CheckedProcess -FilePath $appExecutable -Arguments @(
        '--background', '--smoke-mode',
        '--process-smoke-stage', 'verify',
        '--process-smoke-report', $processVerifyReport,
        '--data-root', $processDataRoot
    ) | Out-Null
    $processVerifyResult = Read-JsonFile -Path $processVerifyReport
    $processPrepareOutbox = @($processPrepareResult.persistence.outbox)[0]
    $processFinalOutbox = @($processVerifyResult.persistence.outbox)[0]
    $checks.processVerifySucceeded = [bool]$processVerifyResult.success
    $checks.processForcedTerminationRecoveredLease = ([string]$processPrepareOutbox.state -eq 'Leased') -and ([string]$processFinalOutbox.state -eq 'Delivered') -and ([int]$processFinalOutbox.AttemptCount -eq 2)
    $checks.processCompletionIsIdempotent = ([int]$processVerifyResult.persistence.ReminderLogCount -eq 1) -and ([int]$processVerifyResult.persistence.ActiveAcknowledgementCount -eq 1)
    $checks.processDatabaseIntegrity = ([string]$processPrepareResult.persistence.IntegrityResult -eq 'ok') -and ([string]$processVerifyResult.persistence.IntegrityResult -eq 'ok')
    $checks.processReportsRedacted = (Test-ReportRedaction -Path $processPrepareReport) -and (Test-ReportRedaction -Path $processVerifyReport)

    $recoveryDataRoot = Join-Path $smokeRoot 'recovery-data'
    $recoveryReport = Join-Path $recoveryEvidenceDirectory 'report.json'
    Invoke-CheckedProcess -FilePath $appExecutable -Arguments @(
        '--background', '--smoke-mode',
        '--recovery-smoke-report', $recoveryReport,
        '--data-root', $recoveryDataRoot
    ) | Out-Null
    $recoveryResult = Read-JsonFile -Path $recoveryReport
    $checks.recoverySmokeSucceeded = [bool]$recoveryResult.success -and @($recoveryResult.failedChecks).Count -eq 0
    $checks.recoverySmokeCoversAllSignals = (@($recoveryResult.syncReasons).Count -eq 3) -and (@($recoveryResult.clockCheckReasons).Count -eq 3) -and (@($recoveryResult.dispatches | Where-Object Dispatched).Count -eq 3)
    $checks.recoverySmokeUsesFiveSecondDebounce = ([double]$recoveryResult.debounceSeconds -eq 5) -and [bool]$recoveryResult.checks.burstRecoveryIsDebounced -and [bool]$recoveryResult.checks.secondBurstIsDebounced
    $checks.recoverySmokeTriggersSyncAndClockCheck = [bool]$recoveryResult.checks.acceptedEventsTriggerSyncAndClockCheck
    $checks.recoverySmokeReportRedacted = Test-ReportRedaction -Path $recoveryReport

    Start-ScheduledTask -TaskName $taskName
    $scheduledProcess = $null
    $scheduledDeadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    while ([DateTimeOffset]::UtcNow -lt $scheduledDeadline -and $null -eq $scheduledProcess) {
        $scheduledProcess = Get-Process -Name 'StockIpoReminder' -ErrorAction SilentlyContinue | Where-Object {
            try {
                [System.IO.Path]::GetFullPath($_.Path) -eq [System.IO.Path]::GetFullPath($appExecutable)
            }
            catch {
                $false
            }
        } | Select-Object -First 1
        if ($null -eq $scheduledProcess) {
            Start-Sleep -Milliseconds 250
        }
    }
    $checks.autoStartTaskLaunchesInstalledApp = $null -ne $scheduledProcess
    if ($null -ne $scheduledProcess) {
        $scheduledCommandLine = [string](Get-CimInstance Win32_Process -Filter "ProcessId = $($scheduledProcess.Id)").CommandLine
        $checks.autoStartLaunchUsesBackgroundArgument = $scheduledCommandLine.IndexOf('--background', [StringComparison]::OrdinalIgnoreCase) -ge 0
        $checks.autoStartLaunchUsesIsolatedDataRoot = $scheduledCommandLine.IndexOf($dataRoot, [StringComparison]::OrdinalIgnoreCase) -ge 0
        $checks.autoStartLaunchHasNoConsoleWindow = $scheduledProcess.MainWindowHandle -eq [IntPtr]::Zero
        Stop-Process -Id $scheduledProcess.Id -Force
        $scheduledProcess.WaitForExit(10000) | Out-Null
        $scheduledProcess.Dispose()
    }
    else {
        $checks.autoStartLaunchUsesBackgroundArgument = $false
        $checks.autoStartLaunchUsesIsolatedDataRoot = $false
        $checks.autoStartLaunchHasNoConsoleWindow = $false
    }

    Expand-Archive -LiteralPath $portableZip -DestinationPath $portableDirectory -Force
    $portableDataRoot = Join-Path $smokeRoot 'portable-data'
    $portableReadyFile = Join-Path $reportDirectory 'portable-ready.json'
    $portableExecutable = Join-Path $portableDirectory 'StockIpoReminder.exe'
    $portableProcess = Start-Process -FilePath $portableExecutable -ArgumentList @(
        '--background', '--smoke-mode',
        '--data-root', $portableDataRoot,
        '--ready-file', $portableReadyFile,
        '--exit-after-seconds', '5'
    ) -PassThru -WindowStyle Hidden
    $startedProcesses.Add($portableProcess)
    Wait-ForFile -Path $portableReadyFile -TimeoutSeconds 30
    $portableReady = Read-JsonFile -Path $portableReadyFile
    $checks.portableAppReady = $portableReady.status -eq 'ready'
    $checks.portableUsesIsolatedData = [System.IO.Path]::GetFullPath([string]$portableReady.dataRoot) -eq [System.IO.Path]::GetFullPath($portableDataRoot)
    if (-not $portableProcess.WaitForExit(20000)) {
        throw 'Portable application smoke process did not exit on schedule.'
    }
    $checks.portableAppCleanExit = $portableProcess.ExitCode -eq 0

    $upgradeReport = Join-Path $reportDirectory 'upgrade.json'
    Invoke-CheckedProcess -FilePath $setupExecutable -Arguments @(
        '--quiet', '--no-launch',
        '--install-dir', $installDirectory,
        '--data-root', $dataRoot,
        '--instance-id', $instanceId,
        '--report-file', $upgradeReport
    ) | Out-Null
    $upgradeResult = Read-JsonFile -Path $upgradeReport
    $checks.upgradeSucceeded = [bool]$upgradeResult.success
    $checks.upgradeBackupCreated = -not [string]::IsNullOrWhiteSpace([string]$upgradeResult.backupPath) -and (Test-Path -LiteralPath ([string]$upgradeResult.backupPath) -PathType Leaf)
    $checks.databasePreservedAfterUpgrade = Test-Path -LiteralPath (Join-Path $dataRoot 'stock-ipo-reminder.db') -PathType Leaf
    $checks.upgradeDatabaseIntegrity = Test-SqliteIntegrity -DatabasePath (Join-Path $dataRoot 'stock-ipo-reminder.db')

    $uninstallReport = Join-Path $reportDirectory 'uninstall-preserve.json'
    Invoke-CheckedProcess -FilePath (Join-Path $installDirectory 'StockIpoReminder.Uninstaller.exe') -Arguments @(
        '--uninstall', '--quiet',
        '--report-file', $uninstallReport
    ) | Out-Null
    Wait-ForFile -Path $uninstallReport -TimeoutSeconds 60
    $uninstallResult = Read-JsonFile -Path $uninstallReport
    $checks.uninstallPreserveSucceeded = [bool]$uninstallResult.success
    $checks.installDirectoryRemoved = -not (Test-Path -LiteralPath $installDirectory)
    $checks.dataPreservedOnNormalUninstall = Test-Path -LiteralPath $dataRoot -PathType Container
    $checks.databasePreservedOnNormalUninstall = Test-Path -LiteralPath (Join-Path $dataRoot 'stock-ipo-reminder.db') -PathType Leaf
    $checks.uninstallRegistryRemoved = -not (Test-Path -LiteralPath $uninstallRegistryKey)
    $checks.startMenuShortcutRemoved = -not (Test-Path -LiteralPath $startMenuShortcut)
    $checks.autoStartTaskAbsentOrRemoved = -not [bool](Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue)

    $reinstallReport = Join-Path $reportDirectory 'reinstall.json'
    Invoke-CheckedProcess -FilePath $setupExecutable -Arguments @(
        '--quiet', '--no-launch',
        '--install-dir', $installDirectory,
        '--data-root', $dataRoot,
        '--instance-id', $instanceId,
        '--report-file', $reinstallReport
    ) | Out-Null
    $reinstallResult = Read-JsonFile -Path $reinstallReport
    $checks.reinstallSucceeded = [bool]$reinstallResult.success
    $checks.databaseReusedAfterReinstall = Test-Path -LiteralPath (Join-Path $dataRoot 'stock-ipo-reminder.db') -PathType Leaf

    $deleteWithoutConfirmationReport = Join-Path $reportDirectory 'delete-without-confirmation.json'
    Invoke-CheckedProcess -FilePath (Join-Path $installDirectory 'StockIpoReminder.Uninstaller.exe') -Arguments @(
        '--uninstall', '--quiet', '--delete-data',
        '--report-file', $deleteWithoutConfirmationReport
    ) | Out-Null
    Wait-ForFile -Path $deleteWithoutConfirmationReport -TimeoutSeconds 60
    $deleteWithoutConfirmationResult = Read-JsonFile -Path $deleteWithoutConfirmationReport
    $checks.deleteDataRequiresSecondConfirmation = -not [bool]$deleteWithoutConfirmationResult.success -and $deleteWithoutConfirmationResult.exitCode -eq 32
    $checks.dataStillExistsAfterRejectedDelete = Test-Path -LiteralPath $dataRoot -PathType Container
    $checks.installStillExistsAfterRejectedDelete = Test-Path -LiteralPath $installDirectory -PathType Container

    $deleteConfirmedReport = Join-Path $reportDirectory 'delete-confirmed.json'
    Invoke-CheckedProcess -FilePath (Join-Path $installDirectory 'StockIpoReminder.Uninstaller.exe') -Arguments @(
        '--uninstall', '--quiet', '--delete-data', '--confirm-delete-data',
        '--report-file', $deleteConfirmedReport
    ) | Out-Null
    Wait-ForFile -Path $deleteConfirmedReport -TimeoutSeconds 60
    $deleteConfirmedResult = Read-JsonFile -Path $deleteConfirmedReport
    $checks.confirmedDeleteSucceeded = [bool]$deleteConfirmedResult.success
    $checks.dataRemovedAfterConfirmedDelete = -not (Test-Path -LiteralPath $dataRoot)
    $checks.installRemovedAfterConfirmedDelete = -not (Test-Path -LiteralPath $installDirectory)

    $failedChecks = @($checks.GetEnumerator() | Where-Object {
        $_.Value -is [bool] -and -not $_.Value
    })
    $report = [ordered]@{
        success = $failedChecks.Count -eq 0
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        os = [ordered]@{
            caption = (Get-CimInstance Win32_OperatingSystem).Caption
            build = (Get-CimInstance Win32_OperatingSystem).BuildNumber
            version = [Environment]::OSVersion.Version.ToString()
            is64Bit = [Environment]::Is64BitOperatingSystem
        }
        sandbox = '<isolated-smoke-root>'
        taskName = $taskName
        checks = $checks
        failedChecks = @($failedChecks | ForEach-Object Key)
        evidence = [ordered]@{
            uiReport = Get-WorkspaceRelativePath -Path $uiReport
            uiCheckCount = $uiCheckCount
            uiScreenshotCount = $uiScreenshotPaths.Count
            processPrepareReport = Get-WorkspaceRelativePath -Path $processPrepareReport
            processVerifyReport = Get-WorkspaceRelativePath -Path $processVerifyReport
            recoveryReport = Get-WorkspaceRelativePath -Path $recoveryReport
            recoveryCheckCount = @($recoveryResult.checks.PSObject.Properties).Count
        }
    }
    $finalReport = Join-Path $workspace "artifacts\smoke\windows-$Version-$evidenceDate.json"
    New-Item -ItemType Directory -Path (Split-Path -Parent $finalReport) -Force | Out-Null
    [System.IO.File]::WriteAllText(
        $finalReport,
        ($report | ConvertTo-Json -Depth 8),
        [System.Text.UTF8Encoding]::new($false))
    Write-Host "Smoke report: $finalReport"
    if (-not $report.success) {
        throw "Smoke checks failed: $($report.failedChecks -join ', ')"
    }
}
finally {
    foreach ($process in $startedProcesses) {
        try {
            if (-not $process.HasExited) {
                $process.Kill($true)
                $process.WaitForExit(5000) | Out-Null
            }
        }
        catch {
        }
        finally {
            $process.Dispose()
        }
    }

    try {
        Stop-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
        if (Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue) {
            Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
        }
    }
    catch {
    }

    if (Test-Path -LiteralPath $uninstallRegistryKey) {
        Remove-Item -LiteralPath $uninstallRegistryKey -Recurse -Force
    }
    if (-not [string]::IsNullOrWhiteSpace($startMenuShortcut) -and (Test-Path -LiteralPath $startMenuShortcut)) {
        Remove-Item -LiteralPath $startMenuShortcut -Force
    }
    if (-not $KeepSandbox -and (Test-Path -LiteralPath $smokeRoot)) {
        Assert-SafeDescendant -Path $smokeRoot -Parent $smokeParent
        Remove-Item -LiteralPath $smokeRoot -Recurse -Force
    }
}

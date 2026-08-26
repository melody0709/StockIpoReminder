[CmdletBinding()]
param(
    [string]$Version,
    [string]$DataRoot,
    [int]$TimeoutSeconds = 180,
    [int]$PostSyncSeconds = 5,
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoManifestPath = Join-Path $workspace 'Cargo.toml'
$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath $cargoManifestPath
$configuredVersion = [regex]::Match(
    $cargoText,
    '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $configuredVersion }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Invalid version: $Version" }
if ($configuredVersion -ne $Version) {
    throw "Version mismatch: Cargo.toml=$configuredVersion, requested=$Version"
}
$executable = Join-Path $workspace 'build\run\x64-release\StockIpoReminder.exe'
$stamp = [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmss')
if ([string]::IsNullOrWhiteSpace($DataRoot)) {
    $DataRoot = Join-Path $workspace "build\artifacts\diagnostics\memory\data\rust-$Version-$stamp"
}
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $workspace "build\artifacts\diagnostics\memory\rust-$Version-$stamp.json"
}

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Get-RelatedProcesses {
    param([int]$MainProcessId, [string]$ExecutablePath)
    $processIds = [System.Collections.Generic.List[int]]::new()
    $processIds.Add($MainProcessId)
    foreach ($child in @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $MainProcessId" -ErrorAction SilentlyContinue)) {
        if ($child.ExecutablePath -eq $ExecutablePath) {
            $processIds.Add([int]$child.ProcessId)
        }
    }
    @(Get-Process -Id $processIds.ToArray() -ErrorAction SilentlyContinue)
}

Assert-Condition (Test-Path -LiteralPath $executable -PathType Leaf) 'Rust release executable is missing.'
New-Item -ItemType Directory -Path $DataRoot -Force | Out-Null
New-Item -ItemType Directory -Path (Split-Path -Parent $OutputPath) -Force | Out-Null

$process = $null
$report = $null
$peakMainPrivate = 0L
$peakMainWorkingSet = 0L
$peakTotalPrivate = 0L
$peakTotalWorkingSet = 0L
$peakPdfWorkerPrivate = 0L
$peakPdfWorkerWorkingSet = 0L
$peakWorkerCount = 0
$sampleCount = 0
$syncCompleted = $false
$logReadContentionCount = 0
$logPattern = Join-Path $DataRoot 'logs\stock-ipo-reminder-*.log*'

try {
    $process = Start-Process -FilePath $executable -ArgumentList @('--data-root', $DataRoot, '--background', '--skip-auto-start-registration', '--no-watchdog') -PassThru -WindowStyle Hidden
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)

    do {
        if ($process.HasExited) { throw "Rust process exited before synchronization completed: $($process.ExitCode)" }
        $related = @(Get-RelatedProcesses -MainProcessId $process.Id -ExecutablePath $executable)
        $main = $related | Where-Object Id -eq $process.Id | Select-Object -First 1
        if ($null -ne $main) {
            $mainPrivate = [long]$main.PrivateMemorySize64
            $mainWorkingSet = [long]$main.WorkingSet64
            $totalPrivate = [long](($related | Measure-Object -Property PrivateMemorySize64 -Sum).Sum)
            $totalWorkingSet = [long](($related | Measure-Object -Property WorkingSet64 -Sum).Sum)
            $workerCount = [Math]::Max(0, $related.Count - 1)
            $workers = @($related | Where-Object Id -ne $process.Id)
            $workerPrivate = if ($workers.Count -gt 0) {
                [long](($workers | Measure-Object -Property PrivateMemorySize64 -Maximum).Maximum)
            } else { 0L }
            $workerWorkingSet = if ($workers.Count -gt 0) {
                [long](($workers | Measure-Object -Property WorkingSet64 -Maximum).Maximum)
            } else { 0L }
            $peakMainPrivate = [Math]::Max($peakMainPrivate, $mainPrivate)
            $peakMainWorkingSet = [Math]::Max($peakMainWorkingSet, $mainWorkingSet)
            $peakTotalPrivate = [Math]::Max($peakTotalPrivate, $totalPrivate)
            $peakTotalWorkingSet = [Math]::Max($peakTotalWorkingSet, $totalWorkingSet)
            $peakPdfWorkerPrivate = [Math]::Max($peakPdfWorkerPrivate, $workerPrivate)
            $peakPdfWorkerWorkingSet = [Math]::Max($peakPdfWorkerWorkingSet, $workerWorkingSet)
            $peakWorkerCount = [Math]::Max($peakWorkerCount, $workerCount)
            $sampleCount++
        }

        $logPath = Get-ChildItem -Path $logPattern -File -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTimeUtc -Descending |
            Select-Object -First 1 -ExpandProperty FullName
        if ($null -ne $logPath -and (Test-Path -LiteralPath $logPath -PathType Leaf)) {
            $logText = $null
            try {
                $logText = [System.IO.File]::ReadAllText($logPath, [System.Text.Encoding]::UTF8)
            }
            catch [System.IO.IOException] {
                # The application rotates and appends this file. A short exclusive window is
                # expected; keep sampling instead of turning transient contention into a failed run.
                $logReadContentionCount++
            }
            if ($null -ne $logText -and $logText -match 'events=\d+, announcements=\d+, sources=\d+, failed=\d+') {
                $syncCompleted = $true
                break
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTimeOffset]::UtcNow -lt $deadline)

    Assert-Condition $syncCompleted "Synchronization did not finish within $TimeoutSeconds seconds."

    $postSyncDeadline = [DateTimeOffset]::UtcNow.AddSeconds($PostSyncSeconds)
    do {
        $related = @(Get-RelatedProcesses -MainProcessId $process.Id -ExecutablePath $executable)
        $main = $related | Where-Object Id -eq $process.Id | Select-Object -First 1
        if ($null -ne $main) {
            $mainPrivate = [long]$main.PrivateMemorySize64
            $mainWorkingSet = [long]$main.WorkingSet64
            $totalPrivate = [long](($related | Measure-Object -Property PrivateMemorySize64 -Sum).Sum)
            $totalWorkingSet = [long](($related | Measure-Object -Property WorkingSet64 -Sum).Sum)
            $peakMainPrivate = [Math]::Max($peakMainPrivate, $mainPrivate)
            $peakMainWorkingSet = [Math]::Max($peakMainWorkingSet, $mainWorkingSet)
            $peakTotalPrivate = [Math]::Max($peakTotalPrivate, $totalPrivate)
            $peakTotalWorkingSet = [Math]::Max($peakTotalWorkingSet, $totalWorkingSet)
            $sampleCount++
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTimeOffset]::UtcNow -lt $postSyncDeadline)

    $finalMain = Get-Process -Id $process.Id -ErrorAction Stop
    $idlePrivate = [long]$finalMain.PrivateMemorySize64
    $idleWorkingSet = [long]$finalMain.WorkingSet64
    $remainingWorkers = @((Get-RelatedProcesses -MainProcessId $process.Id -ExecutablePath $executable) | Where-Object Id -ne $process.Id)
    $temporaryDirectory = Join-Path $DataRoot 'temp'
    $residualFiles = @(if (Test-Path -LiteralPath $temporaryDirectory -PathType Container) {
        Get-ChildItem -LiteralPath $temporaryDirectory -Recurse -File | Where-Object {
            $_.Name -like '*.worker-request.json' -or
            $_.Name -like '*.worker-response.json' -or
            $_.Name -like '*.download' -or
            $_.Name -like '*.tmp'
        }
    })

    Assert-Condition ($idlePrivate -lt 100MB) "Post-sync Private Bytes exceeded 100MB: $idlePrivate"
    Assert-Condition ($idleWorkingSet -lt 100MB) "Post-sync Working Set exceeded 100MB: $idleWorkingSet"
    Assert-Condition ($peakPdfWorkerPrivate -le 512MB) "PDF Worker exceeded its 512 MiB process memory limit: $peakPdfWorkerPrivate"
    Assert-Condition ($remainingWorkers.Count -eq 0) "PDF Worker processes remain after synchronization: $($remainingWorkers.Count)"
    Assert-Condition ($residualFiles.Count -eq 0) "Worker/download temporary files remain after synchronization: $($residualFiles.Count)"

    $report = [ordered]@{
        schemaVersion = '1'
        success = $true
        implementation = 'rust'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        dataRoot = [System.IO.Path]::GetFullPath($DataRoot)
        sampleCount = $sampleCount
        syncCompleted = $syncCompleted
        logReadContentionCount = $logReadContentionCount
        memory = [ordered]@{
            peakMainPrivateBytes = $peakMainPrivate
            peakMainWorkingSetBytes = $peakMainWorkingSet
            peakTotalPrivateBytes = $peakTotalPrivate
            peakTotalWorkingSetBytes = $peakTotalWorkingSet
            peakPdfWorkerPrivateBytes = $peakPdfWorkerPrivate
            peakPdfWorkerWorkingSetBytes = $peakPdfWorkerWorkingSet
            postSyncPrivateBytes = $idlePrivate
            postSyncWorkingSetBytes = $idleWorkingSet
            limitBytes = 100MB
            pdfWorkerLimitBytes = 512MB
        }
        cleanup = [ordered]@{
            peakPdfWorkerCount = $peakWorkerCount
            remainingPdfWorkerCount = $remainingWorkers.Count
            residualTemporaryFileCount = $residualFiles.Count
        }
        log = $logPath
    }
}
catch {
    $report = [ordered]@{
        schemaVersion = '1'
        success = $false
        implementation = 'rust'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        dataRoot = [System.IO.Path]::GetFullPath($DataRoot)
        sampleCount = $sampleCount
        syncCompleted = $syncCompleted
        logReadContentionCount = $logReadContentionCount
        error = $_.Exception.Message
    }
    throw
}
finally {
    if ($null -ne $report) {
        [System.IO.File]::WriteAllText($OutputPath, ($report | ConvertTo-Json -Depth 8), [System.Text.UTF8Encoding]::new($false))
        Write-Host "Rust memory report: $OutputPath"
    }
    if ($null -ne $process -and -not $process.HasExited) {
        try { $process.Kill(); $process.WaitForExit() } catch {}
    }
}

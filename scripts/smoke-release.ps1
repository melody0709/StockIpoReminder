[CmdletBinding()]
param(
    [string]$Version,
    [switch]$KeepSandbox
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class StockIpoReminderWindowProbe
{
    private delegate bool EnumWindowsCallback(IntPtr window, IntPtr parameter);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(IntPtr window);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr window, StringBuilder text, int count);

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(IntPtr window, out Rect rect);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(
        IntPtr window,
        IntPtr insertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags);

    public static IntPtr FindVisibleTitledWindow(uint processId, string expectedTitle)
    {
        var found = IntPtr.Zero;
        EnumWindows((window, parameter) =>
        {
            GetWindowThreadProcessId(window, out var ownerProcessId);
            if (ownerProcessId != processId || !IsWindowVisible(window))
            {
                return true;
            }

            var title = new StringBuilder(512);
            GetWindowText(window, title, title.Capacity);
            if (String.Equals(title.ToString(), expectedTitle, StringComparison.Ordinal))
            {
                found = window;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static bool HasVisibleTitledWindow(uint processId, string expectedTitle)
    {
        return FindVisibleTitledWindow(processId, expectedTitle) != IntPtr.Zero;
    }

    public static bool GrowWindow(IntPtr window, int widthDelta, int heightDelta)
    {
        if (!GetWindowRect(window, out var outer))
        {
            return false;
        }
        const uint NoMoveNoOrderNoActivate = 0x0002 | 0x0004 | 0x0010;
        return SetWindowPos(
            window,
            IntPtr.Zero,
            0,
            0,
            outer.Right - outer.Left + widthDelta,
            outer.Bottom - outer.Top + heightDelta,
            NoMoveNoOrderNoActivate);
    }
}
'@

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
$instanceSupervisor = $null
$portableExecutable = $null
$instanceDataRoot = Join-Path $smokeRoot 'instance-data\StockIpoReminder'
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
    Assert-Condition ([int]$selfTest.schemaMigrationVersion -ge 8) 'Secondary-notification SQLite migration is missing.'
    Assert-Condition ([int]$selfTest.secondaryNotification.pending -eq 0) 'Self-test unexpectedly created secondary-notification deliveries.'
    Assert-Condition ([bool]$selfTest.windowsTimeService.supported) 'Windows Time service diagnostics are not supported in the Windows build.'
    Assert-Condition ([bool]$selfTest.windowsTimeService.querySucceeded) 'Windows Time service status query failed.'
    Assert-Condition ([bool]$selfTest.windowsToast.supported) 'Windows Toast diagnostics are not supported in the Windows build.'
    Assert-Condition ([bool]$selfTest.windowsToast.processIdentitySet) 'The process AppUserModelID was not initialized.'
    Assert-Condition ([string]$selfTest.windowsToast.appUserModelId -eq 'StockIpoReminder.Desktop') 'Windows Toast AppUserModelID is not stable.'
    $checks.selfTest = $true
    $checks.windowsTimeServiceDiagnosed = $true
    $checks.windowsToastDiagnosed = $true

    $process = Start-Process -FilePath $portableExecutable -ArgumentList @('--data-root', $dataRoot, '--skip-startup-sync', '--skip-auto-start-registration', '--skip-update-check', '--skip-crash-upload', '--no-watchdog', '--exit-after-seconds', '8') -PassThru
    Start-Sleep -Seconds 3
    $sample = Get-Process -Id $process.Id -ErrorAction Stop
    $sample.Refresh()
    $privateBytes = [long]$sample.PrivateMemorySize64
    $workingSet = [long]$sample.WorkingSet64
    $mainWindowVisible = [StockIpoReminderWindowProbe]::HasVisibleTitledWindow(
        [uint32]$process.Id,
        'A 股新股申购提醒')
    Assert-Condition (-not $mainWindowVisible) 'Default startup unexpectedly displayed the main window.'
    Assert-Condition ($privateBytes -lt 100MB) "Idle Private Bytes exceeded 100MB: $privateBytes"
    Assert-Condition ($process.WaitForExit(20000)) 'Default tray-only startup smoke did not exit.'
    Assert-Condition ($process.ExitCode -eq 0) "Default tray-only startup smoke failed: $($process.ExitCode)"
    $checks.defaultStartupTrayOnly = $true
    $checks.backgroundUi = $true
    $checks.privateBytesUnder100Mb = $true

    $reminderWindowReport = Join-Path $reports 'reminder-window-smoke.json'
    Invoke-Product $portableExecutable @(
        '--data-root', $dataRoot,
        '--background',
        '--skip-startup-sync',
        '--skip-auto-start-registration',
        '--skip-update-check',
        '--skip-crash-upload',
        '--no-watchdog',
        '--reminder-window-smoke-report', $reminderWindowReport)
    $reminderWindowSmoke = Get-Content -Raw -Encoding UTF8 -LiteralPath $reminderWindowReport | ConvertFrom-Json
    Assert-Condition ([bool]$reminderWindowSmoke.success) 'Dedicated reminder window smoke failed.'
    Assert-Condition ([bool]$reminderWindowSmoke.visibleInWorkArea) 'Dedicated reminder window was not visible in a work area.'
    Assert-Condition ([bool]$reminderWindowSmoke.noFocusSteal) 'Dedicated reminder window became the foreground window.'
    $checks.reminderWindowVisibleNoFocusSteal = $true

    $windowsRecoveryReport = Join-Path $reports 'windows-recovery-smoke.json'
    Invoke-Product $portableExecutable @(
        '--data-root', $dataRoot,
        '--background',
        '--skip-startup-sync',
        '--skip-auto-start-registration',
        '--skip-update-check',
        '--skip-crash-upload',
        '--no-watchdog',
        '--windows-recovery-smoke-report', $windowsRecoveryReport)
    $windowsRecoverySmoke = Get-Content -Raw -Encoding UTF8 -LiteralPath $windowsRecoveryReport | ConvertFrom-Json
    Assert-Condition ([bool]$windowsRecoverySmoke.success) 'Windows recovery-message smoke failed.'
    Assert-Condition ([int]$windowsRecoverySmoke.taskbarCreated.reRegistrationSucceeded -eq 1) 'Tray icon was not re-registered after simulated taskbar loss.'
    Assert-Condition ([int]$windowsRecoverySmoke.taskbarCreated.reRegistrationFailed -eq 0) 'Tray icon re-registration reported a failure.'
    Assert-Condition ([int]$windowsRecoverySmoke.recoveryMessages.runtimeCallbacks -eq 2) 'Windows recovery messages did not produce the expected debounced callbacks.'
    Assert-Condition ([int]$windowsRecoverySmoke.recoveryMessages.suppressedByFiveSecondDebounce -eq 2) 'Windows recovery debounce did not suppress the expected duplicate messages.'
    $checks.explorerTrayReRegistration = $true
    $checks.windowsRecoveryMessagesDebounced = $true

    New-Item -ItemType Directory -Path $instanceDataRoot -Force | Out-Null
    $initialWindowWidth = 900
    $initialWindowHeight = 560
    $windowStatePath = Join-Path $instanceDataRoot 'window-state.json'
    [System.IO.File]::WriteAllText(
        $windowStatePath,
        (@{
            schemaVersion = 1
            width = $initialWindowWidth
            height = $initialWindowHeight
        } | ConvertTo-Json),
        [System.Text.UTF8Encoding]::new($false))
    $instanceSupervisor = Start-Process -FilePath $portableExecutable -ArgumentList @(
        '--data-root', $instanceDataRoot,
        '--background',
        '--skip-startup-sync',
        '--skip-auto-start-registration',
        '--skip-update-check',
        '--skip-crash-upload',
        '--exit-after-seconds', '18') -PassThru -WindowStyle Hidden
    $instanceChild = $null
    $instanceDeadline = [DateTimeOffset]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $instanceChild = Get-CimInstance Win32_Process -Filter ("ParentProcessId = " + $instanceSupervisor.Id) -ErrorAction SilentlyContinue |
            Where-Object { $_.ExecutablePath -eq $portableExecutable } |
            Select-Object -First 1
    } while ($null -eq $instanceChild -and [DateTimeOffset]::UtcNow -lt $instanceDeadline)
    Assert-Condition ($null -ne $instanceChild) 'Watchdog did not create the main process for the second-launch smoke.'

    $secondLaunch = Start-Process -FilePath $portableExecutable -ArgumentList @(
        '--data-root', $instanceDataRoot,
        '--background',
        '--skip-startup-sync',
        '--skip-auto-start-registration',
        '--skip-update-check',
        '--skip-crash-upload') -PassThru -WindowStyle Hidden
    Assert-Condition ($secondLaunch.WaitForExit(10000)) 'Second launch did not exit after contacting the existing instance.'
    Assert-Condition ($secondLaunch.ExitCode -eq 0) "Second launch failed: $($secondLaunch.ExitCode)"

    $activationConfirmed = $false
    $activationDeadline = [DateTimeOffset]::UtcNow.AddSeconds(5)
    do {
        $logPath = Get-ChildItem -LiteralPath (Join-Path $instanceDataRoot 'logs') -Filter 'stock-ipo-reminder-*.log*' -File -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTimeUtc -Descending |
            Select-Object -First 1 -ExpandProperty FullName
        if ($null -ne $logPath) {
            try {
                $activationConfirmed = [System.IO.File]::ReadAllText($logPath, [System.Text.Encoding]::UTF8) -match 'event=second_launch_window_visible'
            }
            catch [System.IO.IOException] {}
        }
        if (-not $activationConfirmed) { Start-Sleep -Milliseconds 200 }
    } while (-not $activationConfirmed -and [DateTimeOffset]::UtcNow -lt $activationDeadline)
    Assert-Condition $activationConfirmed 'Existing instance did not confirm that the main window became visible.'
    $checks.secondLaunchActivatesExistingInstance = $true

    $restoreConfirmed = $false
    $restoreDeadline = [DateTimeOffset]::UtcNow.AddSeconds(3)
    do {
        if ($null -ne $logPath) {
            try {
                $restoreConfirmed = [System.IO.File]::ReadAllText($logPath, [System.Text.Encoding]::UTF8) -match (
                    'logicalWidth=' + $initialWindowWidth +
                    '\s+logicalHeight=' + $initialWindowHeight +
                    '.*event=main_window_size_restored')
            }
            catch [System.IO.IOException] {}
        }
        if (-not $restoreConfirmed) { Start-Sleep -Milliseconds 200 }
    } while (-not $restoreConfirmed -and [DateTimeOffset]::UtcNow -lt $restoreDeadline)
    Assert-Condition $restoreConfirmed 'Saved main-window logical size was not restored.'

    $mainWindow = [StockIpoReminderWindowProbe]::FindVisibleTitledWindow(
        [uint32]$instanceChild.ProcessId,
        'A 股新股申购提醒')
    Assert-Condition ($mainWindow -ne [IntPtr]::Zero) 'Activated main window was not found.'
    Assert-Condition (
        [StockIpoReminderWindowProbe]::GrowWindow(
            $mainWindow,
            120,
            90)) `
        'Could not resize the main window for persistence smoke.'
    Start-Sleep -Seconds 3
    $savedWindowState = Get-Content -Raw -Encoding UTF8 -LiteralPath $windowStatePath | ConvertFrom-Json
    Assert-Condition ([int]$savedWindowState.width -gt $initialWindowWidth) 'Resized main-window width was not persisted.'
    Assert-Condition ([int]$savedWindowState.height -gt $initialWindowHeight) 'Resized main-window height was not persisted.'
    $checks.mainWindowSizePersistence = $true

    Assert-Condition ($instanceSupervisor.WaitForExit(30000)) 'Instance-activation smoke supervisor did not exit.'
    Assert-Condition ($instanceSupervisor.ExitCode -eq 0) "Instance-activation smoke failed: $($instanceSupervisor.ExitCode)"

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
    $productIdentitySource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'packaging\windows\ProductIdentity.wxi')
    $deploymentSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'src\deployment.rs')
    $updaterSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'src\updater.rs')
    $crashUploadSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'src\crash_upload.rs')
    $secondaryNotificationSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'src\secondary_notification.rs')
    $storageSource = @(
        Get-ChildItem -LiteralPath (Join-Path $workspace 'src\storage') -Filter '*.rs' -File |
            Sort-Object Name |
            ForEach-Object { Get-Content -Raw -Encoding UTF8 -LiteralPath $_.FullName }
    ) -join "`n"
    $buildSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'scripts\build-release.ps1')
    $mainUiSource = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'ui\main.slint')
    Assert-Condition ($packageSource -match 'ProgramFiles64Folder') 'MSI does not default to 64-bit Program Files.'
    Assert-Condition ($packageSource -match 'RegistrySearch') 'MSI does not remember the selected install directory.'
    Assert-Condition ($uiSource -match 'InstallDirDlg') 'MSI does not expose an install-directory picker.'
    Assert-Condition ($packageSource -match '<ShortcutProperty\s+Key="System\.AppUserModel\.ID"\s+Value="StockIpoReminder\.Desktop"\s*/>') 'MSI Start-menu shortcut does not register the Toast AppUserModelID.'
    $upgradeCodeMatch = [regex]::Match($productIdentitySource, 'StockIpoReminderUpgradeCode\s*=\s*"(?<code>[0-9A-Fa-f-]{36})"')
    Assert-Condition $upgradeCodeMatch.Success 'MSI UpgradeCode definition is missing.'
    $upgradeCode = $upgradeCodeMatch.Groups['code'].Value.ToUpperInvariant()
    Assert-Condition ($deploymentSource -match [regex]::Escape("{$upgradeCode}")) 'Application uninstall helper does not use the MSI UpgradeCode.'
    Assert-Condition ($deploymentSource -match 'MsiEnumRelatedProductsW') 'Application uninstall helper does not resolve the installed MSI through Windows Installer.'
    Assert-Condition ($deploymentSource -match '"/passive",\s*"/norestart"') 'Application uninstall helper does not use the expected passive, no-restart MSI contract.'
    Assert-Condition ($deploymentSource -match 'validate_current_user_data_root' -and $deploymentSource -match 'validate_purge_confirmation') 'Application uninstall helper does not enforce data-root and confirmation validation.'
    $purgePhrase = -join @([char]0x5220, [char]0x9664, [char]0x5F53, [char]0x524D, [char]0x7528, [char]0x6237, [char]0x6570, [char]0x636E)
    Assert-Condition ($mainUiSource.IndexOf($purgePhrase, [StringComparison]::Ordinal) -ge 0) 'Uninstall UI does not expose the explicit current-user data deletion confirmation phrase.'
    Assert-Condition ($updaterSource -match 'CryptVerifyDetachedMessageSignature' -and $updaterSource -match 'WinVerifyTrust') 'Updater does not verify both detached CMS and Authenticode signatures.'
    Assert-Condition ($updaterSource -match 'TRUSTED_UPDATE_SIGNER_SHA256' -and $updaterSource -match 'normalize_sha256') 'Updater does not pin a normalized signing-certificate SHA-256 fingerprint.'
    Assert-Condition ($updaterSource -match 'validated_https_url' -and $updaterSource -match 'installer\.sha256') 'Updater does not enforce HTTPS and installer SHA-256 validation.'
    Assert-Condition ($buildSource -match 'signtool' -and $buildSource -match "'/tr'" -and $buildSource -match 'SignedCms') 'Release build does not author timestamped Authenticode and detached CMS signatures.'
    Assert-Condition (
        $mainUiSource.Contains('automatic-updates-enabled') -and
        $mainUiSource.Contains('check-for-updates') -and
        $mainUiSource.Contains('install-update')) `
        'Settings UI does not expose the signed update workflow.'
    Assert-Condition (
        $crashUploadSource.Contains('STOCK_IPO_CRASH_REPORT_PRIVACY_URL') -and
        $crashUploadSource.Contains('Policy::none()') -and
        $crashUploadSource.Contains('MAX_ATTEMPTS_PER_DAY') -and
        $crashUploadSource.Contains('sensitive_key') -and
        $mainUiSource.Contains('crash-upload-enabled') -and
        $mainUiSource.Contains('upload-crash-report')) `
        'Application does not expose the consented, redacted and rate-limited crash-report workflow.'
    Assert-Condition (
        $secondaryNotificationSource.Contains('CryptProtectData') -and
        $secondaryNotificationSource.Contains('CryptUnprotectData') -and
        $secondaryNotificationSource.Contains('CRYPTPROTECT_UI_FORBIDDEN') -and
        $secondaryNotificationSource.Contains('Policy::none()') -and
        $secondaryNotificationSource.Contains('qyapi.weixin.qq.com') -and
        $secondaryNotificationSource.Contains('oapi.dingtalk.com') -and
        $secondaryNotificationSource.Contains('open.feishu.cn') -and
        $secondaryNotificationSource.Contains('www.pushplus.plus') -and
        $storageSource.Contains('SECONDARY_MAX_ATTEMPTS') -and
        $storageSource.Contains('SECONDARY_REQUESTS_PER_HOUR') -and
        $storageSource.Contains('secondary_notification_outbox') -and
        $mainUiSource.Contains('secondary-notification-enabled') -and
        $mainUiSource.Contains('test-secondary-notification')) `
        'Application does not expose the encrypted, persistent, rate-limited secondary-notification workflow.'
    Assert-Condition (-not (Test-Path -LiteralPath (Join-Path $dataRoot 'secrets\secondary-notification.dpapi.json'))) 'Self-test unexpectedly persisted a secondary-notification secret.'
    $checks.selectableInstallDirectoryAuthoring = $true
    $checks.toastAumidAuthoring = $true
    $checks.safeMsiUninstallAuthoring = $true
    $checks.signedUpdateAuthoring = $true
    $checks.crashReportPrivacyAuthoring = $true
    $checks.secondaryNotificationAuthoring = $true

    $report = [ordered]@{
        schemaVersion = '11'
        success = $true
        implementation = 'rust'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        checks = $checks
        memory = [ordered]@{ privateBytes = $privateBytes; workingSetBytes = $workingSet; limitBytes = 100MB }
        evidence = [ordered]@{
            portableSelfTest = 'portable-self-test.json'
            msiPayloadSelfTest = 'msi-payload-self-test.json'
            reminderWindowSmoke = 'reminder-window-smoke.json'
            windowsRecoverySmoke = 'windows-recovery-smoke.json'
        }
    }
    [System.IO.File]::WriteAllText($outputPath, ($report | ConvertTo-Json -Depth 8), [System.Text.UTF8Encoding]::new($false))
    Write-Host "Rust Windows smoke report: $outputPath"
}
catch {
    $report = [ordered]@{
        schemaVersion = '11'
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
    if ($null -ne $instanceSupervisor -and -not $instanceSupervisor.HasExited) {
        try { $instanceSupervisor.Kill(); $instanceSupervisor.WaitForExit() } catch {}
    }
    if (Test-Path -LiteralPath $instanceDataRoot) {
        Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
            Where-Object {
                $_.ExecutablePath -eq $portableExecutable -and
                $_.CommandLine -like ('*' + $instanceDataRoot + '*')
            } |
            ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    }
    if (-not $KeepSandbox -and (Test-Path -LiteralPath $smokeRoot)) {
        Assert-SafeDescendant -Path $smokeRoot -Parent $smokeParent
        Remove-Item -LiteralPath $smokeRoot -Recurse -Force
    }
}

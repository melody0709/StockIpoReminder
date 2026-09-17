[CmdletBinding()]
param(
    [string]$PrivateKeyPath = 'C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.key',
    [string]$PasswordFile = 'C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.password.txt',
    [switch]$KeepSandbox
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 更新安装助手的本地闭环测试（无 UAC 风险）：
# 1. 构造由生产密钥签名、指向无效 MSI 的 pending（版本 99.0.0）。
# 2. 以短命父进程运行 --update-install-helper。
# 预期：清单验签通过 -> MSI 只读锁定 -> 等待父进程退出 -> 取得 supervisor 锁
# -> msiexec 打开无效包失败（1620，不触发 UAC）-> 保留 pending -> 退出码 2。
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$executable = Join-Path $workspace 'build\run\x64-release\StockIpoReminder.exe'
$sandboxParent = Join-Path $workspace 'build\cargo\update-helper-test'
$sandbox = Join-Path $sandboxParent ([Guid]::NewGuid().ToString('N'))

function Assert-Condition { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }
function Get-Sha256Hex {
    param([string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose(); $stream.Dispose() }
}

Assert-Condition (Test-Path -LiteralPath $executable -PathType Leaf) 'Release executable is missing.'
Assert-Condition (Test-Path -LiteralPath $PrivateKeyPath -PathType Leaf) 'Minisign private key is missing.'
Assert-Condition (Test-Path -LiteralPath $PasswordFile -PathType Leaf) 'Password file is missing.'

$dataRoot = Join-Path $sandbox 'data'
$pending = Join-Path $dataRoot 'updates\pending'
New-Item -ItemType Directory -Path $pending -Force | Out-Null

try {
    # 1. 无效 MSI：1024 字节零，哈希与清单一致但不是合法安装包。
    $msiPath = Join-Path $pending 'StockIpoReminder-99.0.0-win-x64.msi'
    [System.IO.File]::WriteAllBytes($msiPath, [byte[]]::new(1024))

    # 2. 生产密钥签名的 schema v2 清单。
    $manifestPath = Join-Path $pending 'update-manifest.json'
    $manifest = [ordered]@{
        schemaVersion = 2
        product = 'StockIpoReminder'
        channel = 'stable'
        version = '99.0.0'
        publishedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        minimumWindowsBuild = 19041
        releaseNotesUrl = 'RELEASE_NOTES.md'
        installer = [ordered]@{
            url = 'StockIpoReminder-99.0.0-win-x64.msi'
            sha256 = Get-Sha256Hex $msiPath
            sizeBytes = 1024
        }
    }
    [System.IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 6), [System.Text.UTF8Encoding]::new($false))
    $passwordLine = @(Get-Content -LiteralPath $PasswordFile | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })[1]
    $passwordLine | minisign -S -s $PrivateKeyPath -m $manifestPath -x (Join-Path $pending 'update-manifest.json.minisig')
    Assert-Condition ($LASTEXITCODE -eq 0) 'Minisign signing failed.'

    # 3. 短命父进程（2 秒后退出），helper 应正常走过等待父进程与 supervisor 锁。
    $parent = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList @('-NoProfile', '-Command', 'Start-Sleep -Seconds 2') -PassThru -WindowStyle Hidden
    $helper = Start-Process -FilePath $executable -ArgumentList @(
        '--update-install-helper',
        '--parent-pid', "$($parent.Id)",
        '--data-root', $dataRoot) -PassThru -WindowStyle Hidden
    Assert-Condition ($helper.WaitForExit(120000)) 'Update helper did not exit within 120 seconds.'
    "helper-exit=$($helper.ExitCode)"

    # 4. 验证结果：安装失败被记录，pending 保留供重试。
    $resultPath = Join-Path $dataRoot 'diagnostics\update-last-result.json'
    Assert-Condition (Test-Path -LiteralPath $resultPath -PathType Leaf) 'Helper result file is missing.'
    $result = Get-Content -Raw -Encoding UTF8 -LiteralPath $resultPath | ConvertFrom-Json
    "result-success=$($result.success)"
    "result-detail=$($result.error)"
    Assert-Condition ($helper.ExitCode -eq 2) "Helper should exit with 2 on installer failure, got $($helper.ExitCode)."
    Assert-Condition (-not [bool]$result.success) 'Helper result unexpectedly reports success.'
    Assert-Condition ("$($result.error)" -match 'Windows Installer 更新失败') 'Helper result does not report the msiexec failure.'
    Assert-Condition (Test-Path -LiteralPath $msiPath -PathType Leaf) 'Pending installer was unexpectedly deleted on failure.'
    Assert-Condition (Test-Path -LiteralPath $manifestPath -PathType Leaf) 'Pending manifest was unexpectedly deleted on failure.'

    Write-Host "Update helper recovery test passed; sandbox: $sandbox"
    if (-not $KeepSandbox) {
        Remove-Item -LiteralPath $sandbox -Recurse -Force
    }
}
finally {
    if (-not $KeepSandbox -and (Test-Path -LiteralPath $sandbox)) {
        Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    }
}

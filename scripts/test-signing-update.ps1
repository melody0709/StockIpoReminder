[CmdletBinding()]
param(
    [string]$Version,
    [switch]$KeepSandbox
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 更新篡改集成测试：使用一次性 Minisign 测试密钥（无密码、仓库外沙盒生成）
# 验证客户端对正确签名、篡改清单、错误密钥、legacy 签名和安装包哈希的行为。
# 测试密钥从不被生产客户端信任；正式信任根始终是 assets/update-signing 公钥。
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'Cargo.toml')
$configuredVersion = [regex]::Match($cargoText, '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $configuredVersion }
if ($Version -ne $configuredVersion) { throw "Version mismatch: Cargo.toml=$configuredVersion requested=$Version" }

$executable = Join-Path $workspace 'build\run\x64-release\StockIpoReminder.exe'
$sourceMsi = Join-Path $workspace "build\packages\$Version\StockIpoReminder-$Version-win-x64.msi"
$repositoryPublicKey = Join-Path $workspace 'assets\update-signing\stock-ipo-update.pub'
$sandboxParent = Join-Path $workspace 'build\cargo\signing-update-test'
$sandbox = Join-Path $sandboxParent ([Guid]::NewGuid().ToString('N'))
$artifactDirectory = Join-Path $workspace 'build\artifacts\tests\signing-update'
$reportPath = Join-Path $artifactDirectory ("signing-update-$Version-" + [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmss') + '.json')

function Assert-SafeDescendant {
    param([string]$Path, [string]$Parent)
    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    if (-not $fullPath.StartsWith($fullParent + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Unsafe path: $fullPath"
    }
}

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw $Message }
}

function Get-Sha256Hex {
    param([string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose(); $stream.Dispose() }
}

function Invoke-BundleSelfTest {
    param([string]$ManifestPath, [string]$SignaturePath, [string]$InstallerPath, [string]$PublicKeyPath, [string]$ReportPath)
    $process = Start-Process -FilePath $executable -ArgumentList @(
        '--update-bundle-self-test',
        '--manifest', $ManifestPath,
        '--signature', $SignaturePath,
        '--installer', $InstallerPath,
        '--public-key', $PublicKeyPath,
        '--report', $ReportPath) -PassThru -Wait -WindowStyle Hidden
    $result = Get-Content -Raw -Encoding UTF8 -LiteralPath $ReportPath | ConvertFrom-Json
    [ordered]@{ exitCode = $process.ExitCode; success = [bool]$result.success }
}

Assert-Condition (Test-Path -LiteralPath $executable -PathType Leaf) 'Release executable is missing.'
Assert-Condition (Test-Path -LiteralPath $sourceMsi -PathType Leaf) 'Release MSI is missing.'
Assert-Condition (Test-Path -LiteralPath $repositoryPublicKey -PathType Leaf) 'Repository Minisign public key is missing.'
New-Item -ItemType Directory -Path $sandbox, $artifactDirectory -Force | Out-Null

try {
    # 1. 一次性测试密钥（无密码，仅用于本测试，绝不被生产客户端信任）。
    $testKey = Join-Path $sandbox 'test-only.key'
    $testPub = Join-Path $sandbox 'test-only.pub'
    & minisign -G -W -p $testPub -s $testKey
    Assert-Condition ($LASTEXITCODE -eq 0) 'Ephemeral Minisign test key generation failed.'

    # 2. 面向真实 MSI 的 schema v2 清单（真实大小和哈希）。
    $msiCopy = Join-Path $sandbox "StockIpoReminder-$Version-win-x64.msi"
    Copy-Item -LiteralPath $sourceMsi -Destination $msiCopy -Force
    $manifestPath = Join-Path $sandbox 'update-manifest.json'
    $manifest = [ordered]@{
        schemaVersion = 2
        product = 'StockIpoReminder'
        channel = 'stable'
        version = $Version
        publishedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        minimumWindowsBuild = 19041
        releaseNotesUrl = 'RELEASE_NOTES.md'
        installer = [ordered]@{
            url = [System.IO.Path]::GetFileName($msiCopy)
            sha256 = Get-Sha256Hex $msiCopy
            sizeBytes = (Get-Item -LiteralPath $msiCopy).Length
        }
    }
    [System.IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 6), [System.Text.UTF8Encoding]::new($false))

    # 3. 预哈希签名（客户端 allow_legacy=false 只接受这种）。
    $signaturePath = Join-Path $sandbox 'update-manifest.json.minisig'
    & minisign -S -s $testKey -m $manifestPath -x $signaturePath
    Assert-Condition ($LASTEXITCODE -eq 0) 'Ephemeral Minisign pre-hashed signing failed.'

    # 4. 正确的预哈希签名被接受。
    $valid = Invoke-BundleSelfTest -ManifestPath $manifestPath -SignaturePath $signaturePath -InstallerPath $msiCopy -PublicKeyPath $testPub -ReportPath (Join-Path $sandbox 'valid-report.json')
    Assert-Condition ($valid.exitCode -eq 0 -and $valid.success) "Valid Minisign update bundle was rejected: exit=$($valid.exitCode)"

    # 5. 清单被改动一个字节后必须被拒绝。
    $tamperedManifest = Join-Path $sandbox 'update-manifest.tampered.json'
    [System.IO.File]::WriteAllBytes($tamperedManifest, [System.IO.File]::ReadAllBytes($manifestPath))
    Add-Content -LiteralPath $tamperedManifest -Value ' ' -Encoding UTF8 -NoNewline
    $tampered = Invoke-BundleSelfTest -ManifestPath $tamperedManifest -SignaturePath $signaturePath -InstallerPath $msiCopy -PublicKeyPath $testPub -ReportPath (Join-Path $sandbox 'tampered-report.json')
    Assert-Condition ($tampered.exitCode -ne 0 -and -not $tampered.success) 'Tampered update manifest was accepted.'

    # 6. 错误的公钥（key ID 不匹配）必须被拒绝。
    $wrongKey = Join-Path $sandbox 'wrong-only.key'
    $wrongPub = Join-Path $sandbox 'wrong-only.pub'
    & minisign -G -W -p $wrongPub -s $wrongKey
    Assert-Condition ($LASTEXITCODE -eq 0) 'Second ephemeral key generation failed.'
    $wrongKeyResult = Invoke-BundleSelfTest -ManifestPath $manifestPath -SignaturePath $signaturePath -InstallerPath $msiCopy -PublicKeyPath $wrongPub -ReportPath (Join-Path $sandbox 'wrong-key-report.json')
    Assert-Condition ($wrongKeyResult.exitCode -ne 0 -and -not $wrongKeyResult.success) 'Signature made with a different key was accepted.'

    # 7. 正式信任根必须拒绝测试密钥签名。
    $productionRoot = Invoke-BundleSelfTest -ManifestPath $manifestPath -SignaturePath $signaturePath -InstallerPath $msiCopy -PublicKeyPath $repositoryPublicKey -ReportPath (Join-Path $sandbox 'production-root-report.json')
    Assert-Condition ($productionRoot.exitCode -ne 0 -and -not $productionRoot.success) 'Production trust root unexpectedly accepted a test-key signature.'

    # 8. legacy（非预哈希）签名必须被拒绝。
    $legacySignature = Join-Path $sandbox 'update-manifest.json.legacy.minisig'
    & minisign -S -l -s $testKey -m $manifestPath -x $legacySignature
    Assert-Condition ($LASTEXITCODE -eq 0) 'Legacy signing failed.'
    $legacy = Invoke-BundleSelfTest -ManifestPath $manifestPath -SignaturePath $legacySignature -InstallerPath $msiCopy -PublicKeyPath $testPub -ReportPath (Join-Path $sandbox 'legacy-report.json')
    Assert-Condition ($legacy.exitCode -ne 0 -and -not $legacy.success) 'Legacy (non pre-hashed) signature was accepted.'

    # 9. 安装包被改动后必须被拒绝（清单哈希与实际内容不一致）。
    # 文件名必须与清单一致，确保失败来自哈希校验而不是名称校验。
    $tamperedDirectory = Join-Path $sandbox 'tampered'
    New-Item -ItemType Directory -Path $tamperedDirectory -Force | Out-Null
    $tamperedMsi = Join-Path $tamperedDirectory "StockIpoReminder-$Version-win-x64.msi"
    $bytes = [System.IO.File]::ReadAllBytes($msiCopy)
    $bytes[$bytes.Length - 1] = $bytes[$bytes.Length - 1] -bxor 0x01
    [System.IO.File]::WriteAllBytes($tamperedMsi, $bytes)
    $tamperedInstaller = Invoke-BundleSelfTest -ManifestPath $manifestPath -SignaturePath $signaturePath -InstallerPath $tamperedMsi -PublicKeyPath $testPub -ReportPath (Join-Path $sandbox 'tampered-msi-report.json')
    Assert-Condition ($tamperedInstaller.exitCode -ne 0 -and -not $tamperedInstaller.success) 'Tampered installer was accepted.'

    $report = [ordered]@{
        schemaVersion = '2'
        success = $true
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        checks = [ordered]@{
            minisignPrehashedAccepted = $true
            tamperedManifestRejected = $true
            wrongKeyRejected = $true
            productionRootRejectsTestKey = $true
            legacySignatureRejected = $true
            tamperedInstallerRejected = $true
        }
    }
    [System.IO.File]::WriteAllText($reportPath, ($report | ConvertTo-Json -Depth 6), [System.Text.UTF8Encoding]::new($false))
    Write-Host "Signing/update report: $reportPath"
}
finally {
    if (-not $KeepSandbox -and (Test-Path -LiteralPath $sandbox)) {
        Assert-SafeDescendant -Path $sandbox -Parent $sandboxParent
        Remove-Item -LiteralPath $sandbox -Recurse -Force
    }
}

[CmdletBinding()]
param(
    [string]$Version,
    [string]$ReleaseDirectory,
    [string]$PrivateKeyPath = 'C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.key',
    [string]$PublicKeyPath,
    [string]$PasswordFile,
    [switch]$SkipPasswordFileCleanup
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 根据最终 MSI 生成 schema v2 更新清单，用仓库外 Minisign 私钥签名，
# 并用编译进客户端的同一公钥复核。密码只允许交互输入或从文件经标准输入
# 管道传给 minisign，绝不作为命令行参数出现。
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($PublicKeyPath)) {
    $PublicKeyPath = Join-Path $workspace 'assets\update-signing\stock-ipo-update.pub'
}
$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'Cargo.toml')
$configuredVersion = [regex]::Match(
    $cargoText,
    '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $configuredVersion }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Invalid version: $Version" }
if ($configuredVersion -ne $Version) {
    throw "Version mismatch: Cargo.toml=$configuredVersion, requested=$Version"
}
if ([string]::IsNullOrWhiteSpace($ReleaseDirectory)) {
    $ReleaseDirectory = Join-Path $workspace "build\packages\$Version"
}
if (-not (Test-Path -LiteralPath $PrivateKeyPath -PathType Leaf)) {
    throw "Minisign private key is missing: $PrivateKeyPath"
}
if (-not (Test-Path -LiteralPath $PublicKeyPath -PathType Leaf)) {
    throw "Repository public key is missing: $PublicKeyPath"
}
$msiName = "StockIpoReminder-$Version-win-x64.msi"
$msiPath = Join-Path $ReleaseDirectory $msiName
if (-not (Test-Path -LiteralPath $msiPath -PathType Leaf)) {
    throw "Release MSI is missing: $msiPath"
}
$manifestPath = Join-Path $ReleaseDirectory 'update-manifest.json'
$signaturePath = Join-Path $ReleaseDirectory 'update-manifest.json.minisig'
$releaseManifestPath = Join-Path $ReleaseDirectory 'release-manifest.json'
$hashPath = Join-Path $ReleaseDirectory 'SHA256SUMS.txt'

function Get-Sha256Hex {
    param([string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose(); $stream.Dispose() }
}

function Get-MinisignKeyId {
    param([string]$PublicKeyFile)
    # 公钥 base64 行解码后的结构：算法(2) + key id(8) + 公钥(32)。
    # minisign 在注释中以大端（字节反序）显示 key ID，这里保持同一约定。
    $lines = Get-Content -LiteralPath $PublicKeyFile
    $base64Line = @($lines | Where-Object { $_ -match '^[A-Za-z0-9+/=]+$' })[0]
    $bytes = [System.Convert]::FromBase64String($base64Line)
    if ($bytes.Length -lt 10) { throw 'Public key blob is too short.' }
    $keyId = [byte[]]$bytes[2..9]
    [array]::Reverse($keyId)
    ($keyId | ForEach-Object { $_.ToString('x2') }) -join ''
}

function Invoke-Minisign {
    param([string[]]$Arguments)
    if (-not [string]::IsNullOrWhiteSpace($PasswordFile)) {
        # 密码从文件经标准输入管道传给 minisign；不进入命令行参数。
        # generate-update-signing-key.ps1 生成的密码文件第一行是标题、
        # 第二行才是密码；纯密码文件（只有一行）也受支持。
        if (-not (Test-Path -LiteralPath $PasswordFile -PathType Leaf)) {
            throw "Password file is missing: $PasswordFile"
        }
        $lines = @(Get-Content -LiteralPath $PasswordFile | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($lines.Count -eq 0) { throw 'Password file is empty.' }
        $passwordLine = if ($lines.Count -ge 2) { $lines[1] } else { $lines[0] }
        $passwordLine | minisign @Arguments
    }
    else {
        # 无密码文件时让 minisign 在当前控制台交互式询问。
        minisign @Arguments
    }
    if ($LASTEXITCODE -ne 0) {
        throw "minisign failed with exit code ${LASTEXITCODE}: $($Arguments -join ' ')"
    }
}

$publicKeyId = Get-MinisignKeyId -PublicKeyFile $PublicKeyPath
$msiHash = Get-Sha256Hex $msiPath
$msiSize = (Get-Item -LiteralPath $msiPath).Length

# 1. 生成 schema v2 清单；签名对象是写盘后的原始 UTF-8 字节，
#    生成签名后不得重新格式化或改写清单。
$manifest = [ordered]@{
    schemaVersion = 2
    product = 'StockIpoReminder'
    channel = 'stable'
    version = $Version
    publishedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
    minimumWindowsBuild = 19041
    releaseNotesUrl = 'RELEASE_NOTES.md'
    installer = [ordered]@{
        url = $msiName
        sha256 = $msiHash
        sizeBytes = $msiSize
    }
}
$manifestJson = $manifest | ConvertTo-Json -Depth 6
[System.IO.File]::WriteAllText($manifestPath, $manifestJson, [System.Text.UTF8Encoding]::new($false))

# 2. 用仓库外有密码私钥生成 .minisig。
Invoke-Minisign @('-S', '-s', $PrivateKeyPath, '-m', $manifestPath, '-x', $signaturePath)

# 3. 用编译进客户端的同一公钥复核签名（与客户端 allow_legacy=false 一致）。
& minisign -V -q -m $manifestPath -x $signaturePath -p $PublicKeyPath
if ($LASTEXITCODE -ne 0) { throw "Minisign verification failed against the repository public key." }

# 4. 复核清单字段与最终 MSI 一致。
$verified = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath | ConvertFrom-Json
if ([string]$verified.version -ne $Version) { throw 'Manifest version mismatch after signing.' }
if ([string]$verified.installer.url -ne $msiName) { throw 'Manifest installer name mismatch after signing.' }
if ([string]$verified.installer.sha256 -ne $msiHash) { throw 'Manifest installer hash mismatch after signing.' }
if ([long]$verified.installer.sizeBytes -ne $msiSize) { throw 'Manifest installer size mismatch after signing.' }

# 5. 更新 release-manifest.json：Minisign 更新签名与可选 Authenticode 相互独立。
if (-not (Test-Path -LiteralPath $releaseManifestPath -PathType Leaf)) {
    throw "Release manifest is missing: $releaseManifestPath"
}
$releaseManifest = Get-Content -Raw -Encoding UTF8 -LiteralPath $releaseManifestPath | ConvertFrom-Json
$releaseManifest | Add-Member -NotePropertyName updateManifest -NotePropertyValue 'update-manifest.json' -Force
$releaseManifest | Add-Member -NotePropertyName updateManifestSignature -NotePropertyValue 'update-manifest.json.minisig' -Force
$releaseManifest | Add-Member -NotePropertyName updateSignatureAlgorithm -NotePropertyValue 'minisign' -Force
$releaseManifest | Add-Member -NotePropertyName updatePublicKeyId -NotePropertyValue $publicKeyId -Force
[System.IO.File]::WriteAllText(
    $releaseManifestPath,
    ($releaseManifest | ConvertTo-Json -Depth 8),
    [System.Text.UTF8Encoding]::new($false))

# 6. 全部清单和签名完成后最后重新生成 SHA256SUMS.txt（不含自身，避免自引用）。
$hashLines = foreach ($file in Get-ChildItem -LiteralPath $ReleaseDirectory -File | Sort-Object Name) {
    if ($file.Name -eq 'SHA256SUMS.txt') { continue }
    "$(Get-Sha256Hex $file.FullName)  $($file.Name)"
}
[System.IO.File]::WriteAllLines($hashPath, $hashLines, [System.Text.UTF8Encoding]::new($false))

Write-Host "Update manifest signed: $manifestPath"
Write-Host "Signature: $signaturePath (public key id: $publicKeyId)"
Write-Host "Checksums regenerated: $hashPath"

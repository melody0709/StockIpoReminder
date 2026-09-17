[CmdletBinding()]
param(
    [string]$KeyDirectory = 'C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# 生成正式 Minisign 更新签名密钥对。
# 私钥使用密码学随机强密码加密；密码写入 vault 内的临时文件，
# 由用户查看记忆后手动删除。密码不经过命令行参数或控制台输出。
$publicKeyPath = Join-Path $KeyDirectory 'stock-ipo-update.pub'
$privateKeyPath = Join-Path $KeyDirectory 'stock-ipo-update.key'
$passwordPath = Join-Path $KeyDirectory 'stock-ipo-update.password.txt'
if ((Test-Path -LiteralPath $publicKeyPath) -or (Test-Path -LiteralPath $privateKeyPath)) {
    throw "Key pair already exists in $KeyDirectory; refusing to overwrite."
}
New-Item -ItemType Directory -Path $KeyDirectory -Force | Out-Null

$alphabet = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!#%+=?@'
$random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
try {
    $bytes = [byte[]]::new(48)
    $random.GetBytes($bytes)
    $password = -join ($bytes | ForEach-Object { $alphabet[$_ % $alphabet.Length] })
}
finally { $random.Dispose() }

[System.IO.File]::WriteAllText(
    $passwordPath,
    "Minisign 私钥密码（stock-ipo-update.key）：`r`n$password`r`n`r`n" +
    "请立即把该密码记入你的密码管理器，然后删除本文件。`r`n" +
    "私钥本身已用该密码加密，两者不要存放在同一位置。`r`n",
    [System.Text.UTF8Encoding]::new($false))

$pipePath = Join-Path ([System.IO.Path]::GetTempPath()) ("minisign-keygen-" + [Guid]::NewGuid().ToString('N') + '.txt')
try {
    [System.IO.File]::WriteAllText($pipePath, "$password`r`n$password`r`n", [System.Text.UTF8Encoding]::new($false))
    Get-Content -LiteralPath $pipePath | minisign -G -p $publicKeyPath -s $privateKeyPath
    if ($LASTEXITCODE -ne 0) { throw "minisign key generation failed with exit code $LASTEXITCODE" }
}
finally { Remove-Item -LiteralPath $pipePath -Force -ErrorAction SilentlyContinue }

Write-Host "Public key: $publicKeyPath"
Write-Host "Encrypted private key: $privateKeyPath"
Write-Host "Password file (delete after memorizing): $passwordPath"

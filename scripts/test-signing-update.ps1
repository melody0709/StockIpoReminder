[CmdletBinding()]
param(
    [string]$Version,
    [switch]$KeepSandbox
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath (Join-Path $workspace 'Cargo.toml')
$configuredVersion = [regex]::Match($cargoText, '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $configuredVersion }
if ($Version -ne $configuredVersion) { throw "Version mismatch: Cargo.toml=$configuredVersion requested=$Version" }

$executable = Join-Path $workspace 'build\run\x64-release\StockIpoReminder.exe'
$sourceMsi = Join-Path $workspace "build\packages\$Version\StockIpoReminder-$Version-win-x64.msi"
$sandboxParent = Join-Path $workspace 'build\cargo\signing-update-test'
$sandbox = Join-Path $sandboxParent ([Guid]::NewGuid().ToString('N'))
$artifactDirectory = Join-Path $workspace 'build\artifacts\tests\signing-update'
$reportPath = Join-Path $artifactDirectory ("signing-update-$Version-" + [DateTimeOffset]::Now.ToString('yyyyMMdd-HHmmss') + '.json')
$certificate = $null
$certificateThumbprint = $null

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

function Get-CertificateSha256 {
    param([System.Security.Cryptography.X509Certificates.X509Certificate2]$Value)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($Value.RawData)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose() }
}

function Get-SignToolPath {
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) { return $command.Source }
    $kitsRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $candidate = Get-ChildItem -LiteralPath $kitsRoot -Filter signtool.exe -File -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($null -eq $candidate) { throw 'signtool.exe was not found.' }
    $candidate.FullName
}

function Remove-CertificateFromCurrentUserStore {
    param([string]$Thumbprint, [string]$StoreName)
    if ([string]::IsNullOrWhiteSpace($Thumbprint)) { return }
    $store = [System.Security.Cryptography.X509Certificates.X509Store]::new(
        $StoreName,
        [System.Security.Cryptography.X509Certificates.StoreLocation]::CurrentUser)
    try {
        $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
        foreach ($match in @($store.Certificates.Find(
            [System.Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
            $Thumbprint,
            $false))) {
            $store.Remove($match)
        }
    }
    finally {
        $store.Close()
        $store.Dispose()
    }
}

Assert-Condition (Test-Path -LiteralPath $executable -PathType Leaf) 'Release executable is missing.'
Assert-Condition (Test-Path -LiteralPath $sourceMsi -PathType Leaf) 'Release MSI is missing.'
New-Item -ItemType Directory -Path $sandbox, $artifactDirectory -Force | Out-Null

try {
    $securityModule = Join-Path $PSHOME 'Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1'
    Import-Module $securityModule -Force
    Import-Module PKI -Force
    $certificate = New-SelfSignedCertificate `
        -Type Custom `
        -Subject ("CN=StockIpoReminder Ephemeral Update Test " + [Guid]::NewGuid().ToString('N')) `
        -CertStoreLocation 'Cert:\CurrentUser\My' `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -HashAlgorithm SHA256 `
        -KeyExportPolicy Exportable `
        -KeyUsage DigitalSignature `
        -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3') `
        -NotAfter (Get-Date).AddDays(1)
    Assert-Condition ($null -ne $certificate -and $certificate.HasPrivateKey) 'Ephemeral code-signing certificate was not created.'
    $certificateThumbprint = $certificate.Thumbprint
    $signerSha256 = Get-CertificateSha256 $certificate

    $signedMsi = Join-Path $sandbox "StockIpoReminder-$Version-win-x64.msi"
    Copy-Item -LiteralPath $sourceMsi -Destination $signedMsi -Force
    $signTool = Get-SignToolPath
    & $signTool sign /fd SHA256 /sha1 $certificate.Thumbprint $signedMsi
    if ($LASTEXITCODE -ne 0) { throw "signtool sign failed: $LASTEXITCODE" }

    $manifestPath = Join-Path $sandbox 'update-manifest.json'
    $signaturePath = Join-Path $sandbox 'update-manifest.json.p7s'
    $manifest = [ordered]@{
        schemaVersion = 1
        product = 'StockIpoReminder'
        channel = 'stable'
        version = $Version
        publishedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        minimumWindowsBuild = 19041
        releaseNotesUrl = 'RELEASE_NOTES.md'
        installer = [ordered]@{
            url = [System.IO.Path]::GetFileName($signedMsi)
            sha256 = Get-Sha256Hex $signedMsi
            sizeBytes = (Get-Item -LiteralPath $signedMsi).Length
            signerSha256 = $signerSha256
        }
    }
    [System.IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 6), [System.Text.UTF8Encoding]::new($false))

    Add-Type -AssemblyName System.Security
    $content = [System.Security.Cryptography.Pkcs.ContentInfo]::new([System.IO.File]::ReadAllBytes($manifestPath))
    $cms = [System.Security.Cryptography.Pkcs.SignedCms]::new($content, $true)
    $cmsSigner = [System.Security.Cryptography.Pkcs.CmsSigner]::new($certificate)
    $cmsSigner.IncludeOption = [System.Security.Cryptography.X509Certificates.X509IncludeOption]::EndCertOnly
    $cmsSigner.DigestAlgorithm = [System.Security.Cryptography.Oid]::new('2.16.840.1.101.3.4.2.1')
    $cms.ComputeSignature($cmsSigner, $false)
    [System.IO.File]::WriteAllBytes($signaturePath, $cms.Encode())

    $validReport = Join-Path $sandbox 'valid-report.json'
    $valid = Start-Process -FilePath $executable -ArgumentList @(
        '--update-bundle-self-test',
        '--manifest', $manifestPath,
        '--signature', $signaturePath,
        '--installer', $signedMsi,
        '--signer', $signerSha256,
        '--allow-untrusted-test-root',
        '--report', $validReport) -PassThru -Wait -WindowStyle Hidden
    Assert-Condition ($valid.ExitCode -eq 0) "Valid signed update bundle was rejected: exit=$($valid.ExitCode)"
    $validResult = Get-Content -Raw -Encoding UTF8 -LiteralPath $validReport | ConvertFrom-Json
    Assert-Condition ([bool]$validResult.success) 'Valid signed update bundle report failed.'

    Add-Content -LiteralPath $manifestPath -Value ' ' -Encoding UTF8
    $tamperedReport = Join-Path $sandbox 'tampered-report.json'
    $tampered = Start-Process -FilePath $executable -ArgumentList @(
        '--update-bundle-self-test',
        '--manifest', $manifestPath,
        '--signature', $signaturePath,
        '--installer', $signedMsi,
        '--signer', $signerSha256,
        '--allow-untrusted-test-root',
        '--report', $tamperedReport) -PassThru -Wait -WindowStyle Hidden
    Assert-Condition ($tampered.ExitCode -ne 0) 'Tampered update manifest was accepted.'
    $tamperedResult = Get-Content -Raw -Encoding UTF8 -LiteralPath $tamperedReport | ConvertFrom-Json
    Assert-Condition (-not [bool]$tamperedResult.success) 'Tampered update report unexpectedly succeeded.'

    $report = [ordered]@{
        schemaVersion = '1'
        success = $true
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        checks = [ordered]@{
            authenticodeSignatureValidatedWithEphemeralRoot = $true
            productionSystemTrustStillRequired = $true
            detachedCmsAccepted = $true
            installerHashAccepted = $true
            signerPinAccepted = $true
            tamperedManifestRejected = $true
        }
    }
    [System.IO.File]::WriteAllText($reportPath, ($report | ConvertTo-Json -Depth 6), [System.Text.UTF8Encoding]::new($false))
    Write-Host "Signing/update report: $reportPath"
}
finally {
    Remove-CertificateFromCurrentUserStore -Thumbprint $certificateThumbprint -StoreName 'My'
    if ($null -ne $certificate) { $certificate.Dispose() }
    if (-not $KeepSandbox -and (Test-Path -LiteralPath $sandbox)) {
        Assert-SafeDescendant -Path $sandbox -Parent $sandboxParent
        Remove-Item -LiteralPath $sandbox -Recurse -Force
    }
}

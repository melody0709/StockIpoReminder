[CmdletBinding()]
param(
    [string]$Version,
    [ValidateSet('Runtime', 'Portable', 'Msi', 'All')]
    [string]$PackageMode = 'All',
    [switch]$SkipTests,
    [switch]$KeepStaging,
    [switch]$Sign,
    [string]$SigningPfxPath = $env:STOCK_IPO_SIGNING_PFX_PATH,
    [string]$SigningCertificateThumbprint = $env:STOCK_IPO_SIGNING_CERTIFICATE_THUMBPRINT,
    [string]$SigningPasswordEnvironmentVariable = 'STOCK_IPO_SIGNING_PFX_PASSWORD',
    [string]$TimestampUrl = 'https://timestamp.digicert.com',
    [string]$UpdateFeedUrl = $env:STOCK_IPO_UPDATE_FEED_URL,
    [string]$CrashReportUrl = $env:STOCK_IPO_CRASH_REPORT_URL,
    [string]$CrashReportPrivacyUrl = $env:STOCK_IPO_CRASH_REPORT_PRIVACY_URL
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifestPath = Join-Path $workspace 'Cargo.toml'
$buildRoot = Join-Path $workspace 'build'
$cargoRoot = Join-Path $buildRoot 'cargo'
$builtExecutable = Join-Path $cargoRoot 'release\StockIpoReminder.exe'
$runtimeDirectory = Join-Path $buildRoot 'run\x64-release'
$testArtifactsRoot = Join-Path $buildRoot 'artifacts\tests'
$diagnosticArtifactsRoot = Join-Path $buildRoot 'artifacts\diagnostics'
$logsRoot = Join-Path $buildRoot 'logs'
$packagesRoot = Join-Path $buildRoot 'packages'
$stagingParent = Join-Path $cargoRoot 'package-staging'
$wixProject = Join-Path $workspace 'packaging\windows\StockIpoReminder.Installer.wixproj'
$productIdentity = Join-Path $workspace 'packaging\windows\ProductIdentity.wxi'
$productInstanceGenerator = Join-Path $workspace 'scripts\generate-wix-product-instance.ps1'
$stopRuntime = Join-Path $workspace 'scripts\stop-runtime-process.ps1'
$layoutValidator = Join-Path $workspace 'scripts\validate-build-layout.ps1'

function Assert-SafeDescendant {
    param([string]$Path, [string]$Parent)
    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    if (-not $fullPath.StartsWith($fullParent + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing operation outside expected directory: $fullPath"
    }
}

function Invoke-Cargo {
    param([string[]]$Arguments)
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo failed with exit code $LASTEXITCODE" }
}

function Invoke-DotNet {
    param([string[]]$Arguments)
    & dotnet @Arguments
    if ($LASTEXITCODE -ne 0) { throw "dotnet failed with exit code $LASTEXITCODE" }
}

function Get-Sha256Hex {
    param([string]$Path)
    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose(); $stream.Dispose() }
}

function Test-CredentialFreeHttpsUrl {
    param([string]$Value)
    try {
        $uri = [Uri]$Value
        return $uri.IsAbsoluteUri -and
            $uri.Scheme -eq 'https' -and
            -not [string]::IsNullOrWhiteSpace($uri.Host) -and
            [string]::IsNullOrWhiteSpace($uri.UserInfo)
    }
    catch { return $false }
}

function New-ZipFromDirectory {
    param([string]$Source, [string]$Destination)
    if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $Source,
        $Destination,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false)
}

function Get-SignToolPath {
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) { return $command.Source }
    $kitsRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $candidate = Get-ChildItem -LiteralPath $kitsRoot -Filter signtool.exe -File -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($null -eq $candidate) { throw 'signtool.exe was not found in PATH or the Windows SDK.' }
    $candidate.FullName
}

function Get-CertificateSha256 {
    param([System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try { [System.BitConverter]::ToString($sha256.ComputeHash($Certificate.RawData)).Replace('-', '').ToLowerInvariant() }
    finally { $sha256.Dispose() }
}

function Get-CurrentUserStoreCertificate {
    param([string]$Thumbprint)
    $store = [System.Security.Cryptography.X509Certificates.X509Store]::new(
        'My',
        [System.Security.Cryptography.X509Certificates.StoreLocation]::CurrentUser)
    try {
        $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
        @($store.Certificates.Find(
            [System.Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
            $Thumbprint,
            $false) | Where-Object { $_.HasPrivateKey }) | Select-Object -First 1
    }
    finally {
        $store.Close()
        $store.Dispose()
    }
}

function Add-CurrentUserSigningCertificate {
    param([System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate)
    $store = [System.Security.Cryptography.X509Certificates.X509Store]::new(
        'My',
        [System.Security.Cryptography.X509Certificates.StoreLocation]::CurrentUser)
    try {
        $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
        $existing = $store.Certificates.Find(
            [System.Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
            $Certificate.Thumbprint,
            $false)
        $store.Add($Certificate)
        if ($existing.Count -eq 0) {
            $script:importedSigningCertificates += $Certificate
        }
    }
    finally {
        $store.Close()
        $store.Dispose()
    }
}

function Remove-CurrentUserSigningCertificate {
    param([System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate)
    $store = [System.Security.Cryptography.X509Certificates.X509Store]::new(
        'My',
        [System.Security.Cryptography.X509Certificates.StoreLocation]::CurrentUser)
    try {
        $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
        foreach ($match in @($store.Certificates.Find(
            [System.Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
            $Certificate.Thumbprint,
            $false))) {
            $store.Remove($match)
        }
    }
    finally {
        $store.Close()
        $store.Dispose()
    }
}

function Get-SigningCertificate {
    if (-not [string]::IsNullOrWhiteSpace($SigningPfxPath)) {
        $fullPfxPath = [System.IO.Path]::GetFullPath($SigningPfxPath)
        if (-not (Test-Path -LiteralPath $fullPfxPath -PathType Leaf)) { throw "Signing PFX is missing: $fullPfxPath" }
        $password = [Environment]::GetEnvironmentVariable($SigningPasswordEnvironmentVariable)
        if ($null -eq $password) { throw "Signing password environment variable is missing: $SigningPasswordEnvironmentVariable" }
        $flags = [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::UserKeySet -bor
            [System.Security.Cryptography.X509Certificates.X509KeyStorageFlags]::PersistKeySet
        $collection = [System.Security.Cryptography.X509Certificates.X509Certificate2Collection]::new()
        $collection.Import($fullPfxPath, $password, $flags)
        $certificate = @($collection | Where-Object { $_.HasPrivateKey }) | Select-Object -First 1
        if ($null -eq $certificate) { throw 'The imported signing PFX contains no certificate with a private key.' }
        Add-CurrentUserSigningCertificate -Certificate $certificate
        return $certificate
    }
    if ([string]::IsNullOrWhiteSpace($SigningCertificateThumbprint)) {
        throw 'Signing requires either -SigningPfxPath or -SigningCertificateThumbprint.'
    }
    $normalized = $SigningCertificateThumbprint.Replace(' ', '').ToUpperInvariant()
    $certificate = Get-CurrentUserStoreCertificate -Thumbprint $normalized
    if ($null -eq $certificate) { throw "Code-signing certificate with private key was not found: $normalized" }
    $certificate
}

function Invoke-SignToolSign {
    param([string]$Path)
    $arguments = [System.Collections.Generic.List[string]]::new()
    $arguments.Add('sign')
    $arguments.Add('/fd'); $arguments.Add('SHA256')
    $arguments.Add('/tr'); $arguments.Add($TimestampUrl)
    $arguments.Add('/td'); $arguments.Add('SHA256')
    $arguments.Add('/sha1'); $arguments.Add($signingCertificate.Thumbprint)
    $arguments.Add($Path)
    & $signTool @arguments
    if ($LASTEXITCODE -ne 0) { throw "signtool sign failed for $Path with exit code $LASTEXITCODE" }
    & $signTool verify /pa /all $Path
    if ($LASTEXITCODE -ne 0) { throw "signtool verify failed for $Path with exit code $LASTEXITCODE" }
}

function Write-DetachedCmsSignature {
    param([string]$ContentPath, [string]$OutputPath)
    Add-Type -AssemblyName System.Security
    $content = [System.Security.Cryptography.Pkcs.ContentInfo]::new([System.IO.File]::ReadAllBytes($ContentPath))
    $cms = [System.Security.Cryptography.Pkcs.SignedCms]::new($content, $true)
    $signer = [System.Security.Cryptography.Pkcs.CmsSigner]::new($signingCertificate)
    $signer.IncludeOption = [System.Security.Cryptography.X509Certificates.X509IncludeOption]::EndCertOnly
    $signer.DigestAlgorithm = [System.Security.Cryptography.Oid]::new('2.16.840.1.101.3.4.2.1')
    $cms.ComputeSignature($signer, $false)
    [System.IO.File]::WriteAllBytes($OutputPath, $cms.Encode())
}

$cargoText = Get-Content -Raw -Encoding UTF8 -LiteralPath $manifestPath
$configuredVersion = [regex]::Match(
    $cargoText,
    '(?m)^version\s*=\s*"(?<version>\d+\.\d+\.\d+)"').Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($Version)) { $Version = $configuredVersion }
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Invalid version: $Version" }
if ($configuredVersion -ne $Version) {
    throw "Version mismatch: Cargo.toml=$configuredVersion, requested=$Version"
}

$releaseDirectory = Join-Path $packagesRoot $Version
$stagingDirectory = Join-Path $stagingParent ("$Version-" + [Guid]::NewGuid().ToString('N'))
$portableDirectory = Join-Path $stagingDirectory 'portable'
$generatedWixDirectory = Join-Path $stagingDirectory 'wix-generated'
$msiOutputDirectory = Join-Path $stagingDirectory 'msi-output'
$wixIntermediateDirectory = Join-Path $stagingDirectory 'wix-intermediate'
$signTool = $null
$signingCertificate = $null
$importedSigningCertificates = @()
$signerSha256 = $null
$previousUpdateFeedUrl = $env:STOCK_IPO_UPDATE_FEED_URL
$previousUpdateSignerSha256 = $env:STOCK_IPO_UPDATE_SIGNER_SHA256
$previousCrashReportUrl = $env:STOCK_IPO_CRASH_REPORT_URL
$previousCrashReportPrivacyUrl = $env:STOCK_IPO_CRASH_REPORT_PRIVACY_URL
$crashReportConfigured = -not [string]::IsNullOrWhiteSpace($CrashReportUrl) -and -not [string]::IsNullOrWhiteSpace($CrashReportPrivacyUrl)
if ([string]::IsNullOrWhiteSpace($CrashReportUrl) -ne [string]::IsNullOrWhiteSpace($CrashReportPrivacyUrl)) {
    throw 'Crash reporting requires both STOCK_IPO_CRASH_REPORT_URL and STOCK_IPO_CRASH_REPORT_PRIVACY_URL.'
}
if ($crashReportConfigured) {
    if (-not (Test-CredentialFreeHttpsUrl $CrashReportUrl) -or
        -not (Test-CredentialFreeHttpsUrl $CrashReportPrivacyUrl)) {
        throw 'Crash reporting requires credential-free HTTPS receiver and privacy-policy URLs.'
    }
    $env:STOCK_IPO_CRASH_REPORT_URL = $CrashReportUrl
    $env:STOCK_IPO_CRASH_REPORT_PRIVACY_URL = $CrashReportPrivacyUrl
}
else {
    Remove-Item Env:STOCK_IPO_CRASH_REPORT_URL -ErrorAction SilentlyContinue
    Remove-Item Env:STOCK_IPO_CRASH_REPORT_PRIVACY_URL -ErrorAction SilentlyContinue
}

if ($Sign) {
    if (-not (Test-CredentialFreeHttpsUrl $TimestampUrl)) {
        throw 'Signed releases require a credential-free HTTPS RFC3161 timestamp URL.'
    }
    if (-not (Test-CredentialFreeHttpsUrl $UpdateFeedUrl)) {
        throw 'Signed releases require STOCK_IPO_UPDATE_FEED_URL or -UpdateFeedUrl with a credential-free HTTPS manifest URL.'
    }
    $signTool = Get-SignToolPath
    $signingCertificate = Get-SigningCertificate
    if (-not $signingCertificate.HasPrivateKey) { throw 'The signing certificate has no private key.' }
    $codeSigningEku = @($signingCertificate.Extensions | Where-Object {
        $_ -is [System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension] -and
        @($_.EnhancedKeyUsages | Where-Object { $_.Value -eq '1.3.6.1.5.5.7.3.3' }).Count -gt 0
    })
    if ($codeSigningEku.Count -eq 0) { throw 'The signing certificate is missing the Code Signing EKU.' }
    $signerSha256 = Get-CertificateSha256 $signingCertificate
    $env:STOCK_IPO_UPDATE_SIGNER_SHA256 = $signerSha256
    $env:STOCK_IPO_UPDATE_FEED_URL = $UpdateFeedUrl
}
else {
    Remove-Item Env:STOCK_IPO_UPDATE_SIGNER_SHA256 -ErrorAction SilentlyContinue
    Remove-Item Env:STOCK_IPO_UPDATE_FEED_URL -ErrorAction SilentlyContinue
}

New-Item -ItemType Directory -Path `
    $cargoRoot, $testArtifactsRoot, $diagnosticArtifactsRoot, $logsRoot, $packagesRoot `
    -Force | Out-Null

try {
    if (-not $SkipTests) {
        Invoke-Cargo @('test', '--locked', '--manifest-path', $manifestPath)
    }
    Invoke-Cargo @('build', '--release', '--locked', '--manifest-path', $manifestPath)
    if (-not (Test-Path -LiteralPath $builtExecutable -PathType Leaf)) {
        throw "Rust release executable is missing: $builtExecutable"
    }

    & $stopRuntime -ExecutablePath (Join-Path $runtimeDirectory 'StockIpoReminder.exe')
    if (Test-Path -LiteralPath $runtimeDirectory) {
        Assert-SafeDescendant -Path $runtimeDirectory -Parent $buildRoot
        Remove-Item -LiteralPath $runtimeDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $runtimeDirectory -Force | Out-Null
    Copy-Item -LiteralPath $builtExecutable -Destination (Join-Path $runtimeDirectory 'StockIpoReminder.exe') -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $runtimeDirectory -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination $runtimeDirectory -Force
    if ($Sign) {
        Invoke-SignToolSign (Join-Path $runtimeDirectory 'StockIpoReminder.exe')
    }

    $runtimeFiles = foreach ($name in @('StockIpoReminder.exe', 'README.md', 'RELEASE_NOTES.md')) {
        $path = Join-Path $runtimeDirectory $name
        $item = Get-Item -LiteralPath $path
        [ordered]@{ name = $name; sizeBytes = $item.Length; sha256 = Get-Sha256Hex $path }
    }
    $runtimeManifest = [ordered]@{
        schemaVersion = '1'
        product = 'StockIpoReminder'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        implementation = 'rust'
        architecture = 'x64'
        signed = [bool]$Sign
        signerSha256 = $signerSha256
        files = @($runtimeFiles)
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $runtimeDirectory 'runtime-manifest.json'),
        ($runtimeManifest | ConvertTo-Json -Depth 6),
        [System.Text.UTF8Encoding]::new($false))

    & $layoutValidator -BuildRoot $buildRoot -RuntimeDirectory $runtimeDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Build layout validation failed after runtime install.' }

    if ($PackageMode -eq 'Runtime') {
        Write-Host "Rust runtime installed: $runtimeDirectory"
        return
    }

    if (Test-Path -LiteralPath $releaseDirectory) {
        Assert-SafeDescendant -Path $releaseDirectory -Parent $packagesRoot
        Remove-Item -LiteralPath $releaseDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $releaseDirectory -Force | Out-Null
    New-Item -ItemType Directory -Path $stagingDirectory -Force | Out-Null

    $artifactPaths = [System.Collections.Generic.List[string]]::new()
    if ($PackageMode -in @('Portable', 'All')) {
        New-Item -ItemType Directory -Path $portableDirectory -Force | Out-Null
        Copy-Item -Path (Join-Path $runtimeDirectory '*') -Destination $portableDirectory -Force
        $portablePath = Join-Path $releaseDirectory "StockIpoReminder-$Version-win-x64-portable.zip"
        New-ZipFromDirectory -Source $portableDirectory -Destination $portablePath
        $artifactPaths.Add($portablePath)
    }

    if ($PackageMode -in @('Msi', 'All')) {
        New-Item -ItemType Directory -Path $generatedWixDirectory, $msiOutputDirectory, $wixIntermediateDirectory -Force | Out-Null
        $msiName = "StockIpoReminder-$Version-win-x64.msi"
        $msiOutputName = [System.IO.Path]::GetFileNameWithoutExtension($msiName)
        & $productInstanceGenerator `
            -ProductIdentityPath $productIdentity `
            -ProductVersion $Version `
            -OutputPath (Join-Path $generatedWixDirectory 'ProductInstance.generated.wxi')
        if ($LASTEXITCODE -ne 0) {
            throw "WiX product identity generation failed with exit code $LASTEXITCODE"
        }
        Invoke-DotNet @(
            'build', $wixProject,
            '--configuration', 'Release',
            '--nologo',
            "-p:GeneratedWixDirectory=$generatedWixDirectory",
            "-p:PayloadRoot=$runtimeDirectory",
            "-p:StockIpoReminderMsiOutputName=$msiOutputName",
            "-p:StockIpoReminderMsiOutputDirectory=$msiOutputDirectory",
            "-p:StockIpoReminderWixIntermediateDirectory=$wixIntermediateDirectory",
            "-p:BaseIntermediateOutputPath=$wixIntermediateDirectory\",
            "-p:MSBuildProjectExtensionsPath=$wixIntermediateDirectory\"
        )
        $builtMsi = Get-ChildItem -LiteralPath $msiOutputDirectory -Filter $msiName -File -Recurse |
            Select-Object -First 1
        if ($null -eq $builtMsi) { throw 'WiX MSI output is missing.' }
        $msiPath = Join-Path $releaseDirectory $msiName
        Copy-Item -LiteralPath $builtMsi.FullName -Destination $msiPath -Force
        if ($Sign) { Invoke-SignToolSign $msiPath }
        $artifactPaths.Add($msiPath)
    }

    Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $releaseDirectory -Force
    Copy-Item -LiteralPath (Join-Path $workspace 'RELEASE_NOTES.md') -Destination $releaseDirectory -Force

    $artifacts = foreach ($path in $artifactPaths) {
        $item = Get-Item -LiteralPath $path
        [ordered]@{ name = $item.Name; sizeBytes = $item.Length; sha256 = Get-Sha256Hex $path }
    }
    $updateManifestName = $null
    $updateSignatureName = $null
    if ($Sign -and $PackageMode -in @('Msi', 'All')) {
        $installerArtifact = @($artifacts | Where-Object { $_.name -like '*.msi' }) | Select-Object -First 1
        if ($null -eq $installerArtifact) { throw 'Signed update manifest requires an MSI artifact.' }
        $updateManifestName = 'update-manifest.json'
        $updateSignatureName = 'update-manifest.json.p7s'
        $updateManifestPath = Join-Path $releaseDirectory $updateManifestName
        $updateManifest = [ordered]@{
            schemaVersion = 1
            product = 'StockIpoReminder'
            channel = 'stable'
            version = $Version
            publishedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
            minimumWindowsBuild = 19041
            releaseNotesUrl = 'RELEASE_NOTES.md'
            installer = [ordered]@{
                url = $installerArtifact.name
                sha256 = $installerArtifact.sha256
                sizeBytes = $installerArtifact.sizeBytes
                signerSha256 = $signerSha256
            }
        }
        [System.IO.File]::WriteAllText(
            $updateManifestPath,
            ($updateManifest | ConvertTo-Json -Depth 6),
            [System.Text.UTF8Encoding]::new($false))
        Write-DetachedCmsSignature -ContentPath $updateManifestPath -OutputPath (Join-Path $releaseDirectory $updateSignatureName)
    }
    $releaseManifest = [ordered]@{
        product = 'StockIpoReminder'
        displayName = 'Stock IPO Reminder'
        version = $Version
        generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
        runtime = 'win-x64-native'
        implementationLanguage = 'rust'
        ui = 'Slint'
        dotnetRuntimeRequired = $false
        minimumWindowsBuild = 19041
        signed = [bool]$Sign
        signerSha256 = $signerSha256
        timestampUrl = $(if ($Sign) { $TimestampUrl } else { $null })
        updateFeedUrl = $(if ($Sign) { $UpdateFeedUrl } else { $null })
        updateManifest = $updateManifestName
        updateManifestSignature = $updateSignatureName
        crashReportUrl = $(if ($crashReportConfigured) { $CrashReportUrl } else { $null })
        crashReportPrivacyUrl = $(if ($crashReportConfigured) { $CrashReportPrivacyUrl } else { $null })
        testsExecuted = -not $SkipTests
        installer = 'msi-per-machine-selectable-directory'
        packageMode = $PackageMode
        artifacts = @($artifacts)
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $releaseDirectory 'release-manifest.json'),
        ($releaseManifest | ConvertTo-Json -Depth 6),
        [System.Text.UTF8Encoding]::new($false))

    $hashLines = foreach ($file in Get-ChildItem -LiteralPath $releaseDirectory -File | Sort-Object Name) {
        "$(Get-Sha256Hex $file.FullName)  $($file.Name)"
    }
    [System.IO.File]::WriteAllLines(
        (Join-Path $releaseDirectory 'SHA256SUMS.txt'),
        $hashLines,
        [System.Text.UTF8Encoding]::new($false))

    & $layoutValidator -BuildRoot $buildRoot -RuntimeDirectory $runtimeDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Build layout validation failed after packaging.' }
    Write-Host "Rust packages created: $releaseDirectory"
}
finally {
    if ($null -eq $previousUpdateFeedUrl) { Remove-Item Env:STOCK_IPO_UPDATE_FEED_URL -ErrorAction SilentlyContinue }
    else { $env:STOCK_IPO_UPDATE_FEED_URL = $previousUpdateFeedUrl }
    if ($null -eq $previousUpdateSignerSha256) { Remove-Item Env:STOCK_IPO_UPDATE_SIGNER_SHA256 -ErrorAction SilentlyContinue }
    else { $env:STOCK_IPO_UPDATE_SIGNER_SHA256 = $previousUpdateSignerSha256 }
    if ($null -eq $previousCrashReportUrl) { Remove-Item Env:STOCK_IPO_CRASH_REPORT_URL -ErrorAction SilentlyContinue }
    else { $env:STOCK_IPO_CRASH_REPORT_URL = $previousCrashReportUrl }
    if ($null -eq $previousCrashReportPrivacyUrl) { Remove-Item Env:STOCK_IPO_CRASH_REPORT_PRIVACY_URL -ErrorAction SilentlyContinue }
    else { $env:STOCK_IPO_CRASH_REPORT_PRIVACY_URL = $previousCrashReportPrivacyUrl }
    foreach ($certificate in @($importedSigningCertificates)) {
        Remove-CurrentUserSigningCertificate -Certificate $certificate
    }
    if (-not $KeepStaging -and (Test-Path -LiteralPath $stagingDirectory)) {
        Assert-SafeDescendant -Path $stagingDirectory -Parent $stagingParent
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
    }
}

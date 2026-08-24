[CmdletBinding()]
param(
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version = '0.1.0',
    [string]$SmokeReportPath,
    [string]$DiagnosticReportPath,
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$script:workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$script:releaseDirectory = Join-Path $script:workspace "artifacts\release\$Version"
$script:manifestPath = Join-Path $script:releaseDirectory 'release-manifest.json'
$script:hashListPath = Join-Path $script:releaseDirectory 'SHA256SUMS.txt'
$script:portablePath = Join-Path $script:releaseDirectory "StockIpoReminder-$Version-win-x64-portable.zip"
$script:setupPath = Join-Path $script:releaseDirectory "StockIpoReminder-Setup-$Version-win-x64.exe"
$script:auditStagingParent = Join-Path $script:workspace 'artifacts\.audit-staging'
$script:auditStagingDirectory = Join-Path $script:auditStagingParent ("$Version-" + [Guid]::NewGuid().ToString('N'))
$script:portableExtractDirectory = Join-Path $script:auditStagingDirectory 'portable'
$script:manifest = $null
$script:smokeReportResolved = $null
$script:diagnosticReportResolved = $null
$script:checks = [System.Collections.Generic.List[object]]::new()
$script:evidence = [ordered]@{}

if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $auditDirectory = Join-Path $script:workspace 'artifacts\audit'
    $OutputPath = Join-Path $auditDirectory ("release-$Version-" + [DateTimeOffset]::Now.ToString('yyyyMMdd') + '.json')
}
elseif (-not [System.IO.Path]::IsPathRooted($OutputPath)) {
    $OutputPath = Join-Path $script:workspace $OutputPath
}
$script:outputPath = [System.IO.Path]::GetFullPath($OutputPath)

function Assert-Condition {
    param(
        [Parameter(Mandatory)] [bool]$Condition,
        [Parameter(Mandatory)] [string]$Message
    )

    if (-not $Condition) {
        throw $Message
    }
}

function Assert-SafeDescendant {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$Parent,
        [switch]$AllowParent
    )

    $fullPath = [System.IO.Path]::GetFullPath($Path).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $fullParent = [System.IO.Path]::GetFullPath($Parent).TrimEnd([System.IO.Path]::DirectorySeparatorChar)
    $prefix = $fullParent + [System.IO.Path]::DirectorySeparatorChar
    $isParent = $fullPath.Equals($fullParent, [StringComparison]::OrdinalIgnoreCase)
    $isDescendant = $fullPath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)
    if ((-not $AllowParent -and $isParent) -or (-not $isParent -and -not $isDescendant)) {
        throw "Path escapes the expected parent directory: $fullPath"
    }
}

function Get-WorkspaceRelativePath {
    param([Parameter(Mandatory)] [string]$Path)

    $fullPath = [System.IO.Path]::GetFullPath($Path)
    Assert-SafeDescendant -Path $fullPath -Parent $script:workspace -AllowParent
    $basePath = $script:workspace.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    $baseUri = [Uri]::new($basePath)
    $pathUri = [Uri]::new($fullPath)
    return [Uri]::UnescapeDataString($baseUri.MakeRelativeUri($pathUri).ToString())
}

function Get-SafeDetail {
    param([AllowNull()] [object]$Value)

    $detail = if ($null -eq $Value) { '通过。' } else { [string]$Value }
    if ([string]::IsNullOrWhiteSpace($detail)) {
        $detail = '通过。'
    }

    $detail = [regex]::Replace(
        $detail,
        [regex]::Escape($script:workspace),
        '<workspace>',
        [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)
    if ($detail.Length -gt 1200) {
        $detail = $detail.Substring(0, 1200) + '…'
    }

    return $detail
}

function Add-AuditCheck {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [bool]$Passed,
        [Parameter(Mandatory)] [string]$Detail
    )

    $script:checks.Add([ordered]@{
        name = $Name
        passed = $Passed
        detail = Get-SafeDetail -Value $Detail
    })
}

function Invoke-AuditCheck {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [scriptblock]$Action
    )

    try {
        $detail = & $Action
        if ($detail -is [array]) {
            $detail = ($detail | ForEach-Object { [string]$_ }) -join '；'
        }
        Add-AuditCheck -Name $Name -Passed $true -Detail (Get-SafeDetail -Value $detail)
        return $true
    }
    catch {
        Add-AuditCheck -Name $Name -Passed $false -Detail (Get-SafeDetail -Value $_.Exception.Message)
        return $false
    }
}

function Read-JsonFile {
    param([Parameter(Mandatory)] [string]$Path)

    return Get-Content -Raw -Encoding UTF8 -LiteralPath $Path | ConvertFrom-Json -ErrorAction Stop
}

function Get-Sha256Hex {
    param([Parameter(Mandatory)] [string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hashBytes = $sha256.ComputeHash($stream)
        return [System.BitConverter]::ToString($hashBytes).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
}

function Get-PeMetadata {
    param([Parameter(Mandatory)] [string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $reader = [System.IO.BinaryReader]::new($stream)
    try {
        Assert-Condition -Condition ($stream.Length -ge 256) -Message "PE 文件过短：$([System.IO.Path]::GetFileName($Path))"
        Assert-Condition -Condition ($reader.ReadUInt16() -eq 0x5A4D) -Message "PE DOS 签名无效：$([System.IO.Path]::GetFileName($Path))"

        $stream.Position = 0x3C
        $peOffset = $reader.ReadInt32()
        Assert-Condition -Condition ($peOffset -ge 0x40 -and ([long]$peOffset + 24) -lt $stream.Length) -Message "PE 头偏移无效：$([System.IO.Path]::GetFileName($Path))"

        $stream.Position = $peOffset
        Assert-Condition -Condition ($reader.ReadUInt32() -eq 0x00004550) -Message "PE 签名无效：$([System.IO.Path]::GetFileName($Path))"
        $machine = $reader.ReadUInt16()
        $stream.Position = [long]$peOffset + 20
        $optionalHeaderSize = $reader.ReadUInt16()
        $optionalHeaderOffset = [long]$peOffset + 24
        Assert-Condition -Condition (($optionalHeaderOffset + $optionalHeaderSize) -le $stream.Length) -Message "PE Optional Header 越界：$([System.IO.Path]::GetFileName($Path))"

        $stream.Position = $optionalHeaderOffset
        $magic = $reader.ReadUInt16()
        $dataDirectoryOffset = switch ($magic) {
            0x10B { 96 }
            0x20B { 112 }
            default { throw "未知 PE Optional Header：0x$($magic.ToString('X4'))" }
        }
        $certificateDirectoryOffset = $dataDirectoryOffset + (4 * 8)
        Assert-Condition -Condition ($optionalHeaderSize -ge ($certificateDirectoryOffset + 8)) -Message "PE Certificate Table 目录缺失：$([System.IO.Path]::GetFileName($Path))"

        $stream.Position = $optionalHeaderOffset + $certificateDirectoryOffset
        $certificateFileOffset = $reader.ReadUInt32()
        $certificateSize = $reader.ReadUInt32()
        $hasCertificate = $certificateFileOffset -ne 0 -or $certificateSize -ne 0
        if ($hasCertificate) {
            Assert-Condition -Condition ($certificateFileOffset -ne 0 -and $certificateSize -ne 0) -Message "PE Certificate Table 状态不完整：$([System.IO.Path]::GetFileName($Path))"
            Assert-Condition -Condition (([long]$certificateFileOffset + [long]$certificateSize) -le $stream.Length) -Message "PE Certificate Table 越界：$([System.IO.Path]::GetFileName($Path))"
        }

        return [pscustomobject]@{
            Machine = $machine
            IsX64 = $machine -eq 0x8664
            HasEmbeddedAuthenticode = $hasCertificate
            CertificateSize = [long]$certificateSize
        }
    }
    finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

function Resolve-ReportPath {
    param(
        [AllowNull()] [string]$RequestedPath,
        [Parameter(Mandatory)] [string]$SearchDirectory,
        [Parameter(Mandatory)] [string]$Filter,
        [Parameter(Mandatory)] [string]$Description
    )

    if (-not [string]::IsNullOrWhiteSpace($RequestedPath)) {
        $candidate = if ([System.IO.Path]::IsPathRooted($RequestedPath)) {
            $RequestedPath
        }
        else {
            Join-Path $script:workspace $RequestedPath
        }
        $candidate = [System.IO.Path]::GetFullPath($candidate)
        Assert-Condition -Condition (Test-Path -LiteralPath $candidate -PathType Leaf) -Message "$Description 不存在。"
        return $candidate
    }

    Assert-Condition -Condition (Test-Path -LiteralPath $SearchDirectory -PathType Container) -Message "$Description 搜索目录不存在。"
    $match = Get-ChildItem -LiteralPath $SearchDirectory -Filter $Filter -File |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1
    Assert-Condition -Condition ($null -ne $match) -Message "没有找到$Description。"
    return $match.FullName
}

function Get-ProductionSourceFiles {
    $files = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
    foreach ($relativeRoot in @('src', 'tools')) {
        $root = Join-Path $script:workspace $relativeRoot
        if (-not (Test-Path -LiteralPath $root -PathType Container)) {
            continue
        }

        foreach ($file in Get-ChildItem -LiteralPath $root -Recurse -Filter '*.cs' -File) {
            if ($file.FullName -notmatch '[\\/](?:bin|obj)[\\/]') {
                $files.Add($file)
            }
        }
    }

    return $files.ToArray()
}

function Get-JsonStringNodes {
    param(
        [AllowNull()] [object]$Value,
        [string]$Path = '$'
    )

    if ($null -eq $Value) {
        return
    }
    if ($Value -is [string]) {
        [pscustomobject]@{ Path = $Path; Value = [string]$Value }
        return
    }
    if ($Value -is [System.Collections.IDictionary]) {
        foreach ($key in $Value.Keys) {
            Get-JsonStringNodes -Value $Value[$key] -Path "$Path.$key"
        }
        return
    }
    if ($Value -is [System.Collections.IEnumerable]) {
        $index = 0
        foreach ($item in $Value) {
            Get-JsonStringNodes -Value $item -Path "$Path[$index]"
            $index++
        }
        return
    }
    if ($Value -is [psobject]) {
        foreach ($property in $Value.PSObject.Properties) {
            Get-JsonStringNodes -Value $property.Value -Path "$Path.$($property.Name)"
        }
    }
}

function Assert-ReportRedacted {
    param([Parameter(Mandatory)] [string]$Path)

    $raw = Get-Content -Raw -Encoding UTF8 -LiteralPath $Path
    if ([regex]::IsMatch($raw, '(?i)\b(?:Cookie|Authorization)\b')) {
        throw "报告包含被禁止的凭据头名称：$([System.IO.Path]::GetFileName($Path))"
    }

    $json = $raw | ConvertFrom-Json -ErrorAction Stop
    $violations = [System.Collections.Generic.List[string]]::new()
    $workspaceSlash = $script:workspace.Replace('\', '/')
    foreach ($node in Get-JsonStringNodes -Value $json) {
        $value = [string]$node.Value
        if ($value.IndexOf($script:workspace, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
            $value.IndexOf($workspaceSlash, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            $violations.Add("$($node.Path):workspace-path")
        }
        if ([regex]::IsMatch($value, '(?i)(?:^|[\s"''(=])(?:[a-z]:\\|\\\\)[^\s"'']*')) {
            $violations.Add("$($node.Path):absolute-path")
        }
        if ([regex]::IsMatch($value, '(?i)file:///[a-z]:/')) {
            $violations.Add("$($node.Path):file-uri")
        }
        if ([regex]::IsMatch($value, '(?i)https?://[^\s"''<>]+\?')) {
            $violations.Add("$($node.Path):url-query")
        }
    }

    if ($violations.Count -gt 0) {
        throw "报告脱敏检查失败：$((@($violations | Select-Object -Unique) -join ', '))"
    }
}

function Assert-SuccessReport {
    param(
        [Parameter(Mandatory)] [object]$Report,
        [Parameter(Mandatory)] [string]$Description
    )

    Assert-Condition -Condition ([bool]$Report.success) -Message "$Description success=false。"
    if ($null -ne $Report.PSObject.Properties['failedChecks']) {
        Assert-Condition -Condition (@($Report.failedChecks).Count -eq 0) -Message "$Description 存在 failedChecks。"
    }
    if ($null -ne $Report.PSObject.Properties['checks']) {
        $failedBooleanChecks = @($Report.checks.PSObject.Properties | Where-Object {
            $_.Value -is [bool] -and -not [bool]$_.Value
        })
        Assert-Condition -Condition ($failedBooleanChecks.Count -eq 0) -Message "$Description 存在 false 检查项。"
    }
}

New-Item -ItemType Directory -Path $script:auditStagingParent -Force | Out-Null
New-Item -ItemType Directory -Path $script:auditStagingDirectory -Force | Out-Null

try {
    Invoke-AuditCheck -Name 'release.layout' -Action {
        Assert-Condition -Condition (Test-Path -LiteralPath $script:releaseDirectory -PathType Container) -Message '发布目录不存在。'
        foreach ($requiredPath in @($script:manifestPath, $script:hashListPath, $script:portablePath, $script:setupPath)) {
            Assert-Condition -Condition (Test-Path -LiteralPath $requiredPath -PathType Leaf) -Message "缺少发布文件：$([System.IO.Path]::GetFileName($requiredPath))"
        }
        '发布目录和四个必需文件存在。'
    } | Out-Null

    Invoke-AuditCheck -Name 'release.manifest' -Action {
        $script:manifest = Read-JsonFile -Path $script:manifestPath
        Assert-Condition -Condition ([string]$script:manifest.product -eq 'StockIpoReminder') -Message 'manifest product 不正确。'
        Assert-Condition -Condition ([string]$script:manifest.version -eq $Version) -Message 'manifest version 不正确。'
        Assert-Condition -Condition ([string]$script:manifest.runtime -eq 'win-x64') -Message 'manifest runtime 必须为 win-x64。'
        Assert-Condition -Condition ($script:manifest.selfContained -is [bool] -and [bool]$script:manifest.selfContained) -Message 'manifest selfContained 必须为 true。'
        Assert-Condition -Condition ([int]$script:manifest.minimumWindowsBuild -eq 19041) -Message 'manifest minimumWindowsBuild 必须为 19041。'
        Assert-Condition -Condition ($script:manifest.testsExecuted -is [bool] -and [bool]$script:manifest.testsExecuted) -Message 'manifest testsExecuted 必须为 true；最终发布禁止使用 -SkipTests。'
        Assert-Condition -Condition ($script:manifest.signed -is [bool]) -Message 'manifest signed 必须是布尔值。'
        $generatedAt = [DateTimeOffset]::MinValue
        Assert-Condition -Condition ([DateTimeOffset]::TryParse([string]$script:manifest.generatedAtUtc, [ref]$generatedAt)) -Message 'manifest generatedAtUtc 无效。'
        Assert-Condition -Condition (@($script:manifest.artifacts).Count -eq 2) -Message 'manifest 必须且只能列出两个主发布物。'
        "version=$Version；runtime=win-x64；selfContained=true；testsExecuted=true；minimumWindowsBuild=19041；signed=$([bool]$script:manifest.signed)。"
    } | Out-Null

    Invoke-AuditCheck -Name 'release.sha256-list' -Action {
        $entries = @{}
        foreach ($line in Get-Content -Encoding UTF8 -LiteralPath $script:hashListPath) {
            if ([string]::IsNullOrWhiteSpace($line)) {
                continue
            }
            $match = [regex]::Match($line, '^(?<hash>[0-9a-fA-F]{64})  (?<name>[^\\/]+)$')
            Assert-Condition -Condition $match.Success -Message 'SHA256SUMS.txt 含格式无效或带路径的条目。'
            $name = $match.Groups['name'].Value
            Assert-Condition -Condition (-not $entries.ContainsKey($name)) -Message "SHA256SUMS.txt 含重复条目：$name"
            Assert-Condition -Condition ($name -ne 'SHA256SUMS.txt') -Message 'SHA256SUMS.txt 不得列出自身。'
            $entries[$name] = $match.Groups['hash'].Value.ToLowerInvariant()
        }

        $expectedFiles = @(Get-ChildItem -LiteralPath $script:releaseDirectory -File | Where-Object Name -ne 'SHA256SUMS.txt')
        Assert-Condition -Condition ($entries.Count -eq $expectedFiles.Count) -Message 'SHA256SUMS.txt 条目数与发布目录不一致。'
        foreach ($file in $expectedFiles) {
            Assert-Condition -Condition $entries.ContainsKey($file.Name) -Message "SHA256SUMS.txt 缺少：$($file.Name)"
            $actualHash = Get-Sha256Hex -Path $file.FullName
            Assert-Condition -Condition ($entries[$file.Name] -eq $actualHash) -Message "SHA-256 不匹配：$($file.Name)"
        }
        "已逐项验证 $($expectedFiles.Count) 个发布目录文件。"
    } | Out-Null

    Invoke-AuditCheck -Name 'release.manifest-artifact-hashes' -Action {
        Assert-Condition -Condition ($null -ne $script:manifest) -Message 'manifest 未成功加载。'
        $expectedNames = @(
            "StockIpoReminder-$Version-win-x64-portable.zip",
            "StockIpoReminder-Setup-$Version-win-x64.exe"
        )
        $artifactMap = @{}
        foreach ($artifact in @($script:manifest.artifacts)) {
            $name = [string]$artifact.name
            Assert-Condition -Condition (-not [string]::IsNullOrWhiteSpace($name)) -Message 'manifest artifact name 为空。'
            Assert-Condition -Condition (-not $artifactMap.ContainsKey($name)) -Message "manifest artifact 重复：$name"
            $artifactMap[$name] = $artifact
        }
        Assert-Condition -Condition ($artifactMap.Count -eq $expectedNames.Count) -Message 'manifest artifact 数量不正确。'
        foreach ($name in $expectedNames) {
            Assert-Condition -Condition $artifactMap.ContainsKey($name) -Message "manifest 缺少 artifact：$name"
            $path = Join-Path $script:releaseDirectory $name
            $item = Get-Item -LiteralPath $path
            Assert-Condition -Condition ([long]$artifactMap[$name].sizeBytes -eq $item.Length) -Message "manifest sizeBytes 不匹配：$name"
            Assert-Condition -Condition ([string]$artifactMap[$name].sha256 -eq (Get-Sha256Hex -Path $path)) -Message "manifest sha256 不匹配：$name"
        }
        '两个主发布物的名称、大小和 SHA-256 均与 manifest 一致。'
    } | Out-Null

    Invoke-AuditCheck -Name 'portable.archive-hygiene' -Action {
        New-Item -ItemType Directory -Path $script:portableExtractDirectory -Force | Out-Null
        $archive = [System.IO.Compression.ZipFile]::OpenRead($script:portablePath)
        try {
            $seenEntries = @{}
            $forbiddenEntries = [System.Collections.Generic.List[string]]::new()
            $credentialContentEntries = [System.Collections.Generic.List[string]]::new()
            $forbiddenPatterns = @(
                '(?i)(?:^|/)(?:install-manifest\.json|\.stock-ipo-reminder-data\.json)$',
                '(?i)(?:^|/)(?:logs?|data|cache|backups?|diagnostics?|cookies?|credentials?)(?:/|$)',
                '(?i)(?:^|/)(?:settings|user-settings|cookies?|credentials?|secrets?|tokens?)(?:\.[^/]+)?$',
                '(?i)(?:\.db|\.sqlite|\.sqlite3|\.log|\.pdb|\.dmp|\.bak|\.backup|\.zip|-(?:wal|shm))$'
            )
            $textExtensions = @('.json', '.config', '.xml', '.txt', '.md', '.ini', '.yml', '.yaml')
            foreach ($entry in $archive.Entries) {
                $entryName = $entry.FullName.Replace('\', '/')
                Assert-Condition -Condition (-not [string]::IsNullOrWhiteSpace($entryName)) -Message '便携 ZIP 含空名称条目。'
                Assert-Condition -Condition (-not $seenEntries.ContainsKey($entryName)) -Message "便携 ZIP 含重复条目：$entryName"
                $seenEntries[$entryName] = $true

                $segments = @($entryName.Split('/') | Where-Object { $_.Length -gt 0 })
                Assert-Condition -Condition (-not [System.IO.Path]::IsPathRooted($entryName) -and $entryName.IndexOf(':') -lt 0 -and $segments -notcontains '..') -Message "便携 ZIP 含不安全路径：$entryName"
                $destination = [System.IO.Path]::GetFullPath((Join-Path $script:portableExtractDirectory $entryName.Replace('/', '\')))
                Assert-SafeDescendant -Path $destination -Parent $script:portableExtractDirectory -AllowParent

                foreach ($pattern in $forbiddenPatterns) {
                    if ([regex]::IsMatch($entryName, $pattern)) {
                        $forbiddenEntries.Add($entryName)
                        break
                    }
                }

                $extension = [System.IO.Path]::GetExtension($entryName).ToLowerInvariant()
                if ($entry.Length -gt 0 -and $entry.Length -le 5MB -and $textExtensions -contains $extension) {
                    $stream = $entry.Open()
                    $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8, $true)
                    try {
                        $content = $reader.ReadToEnd()
                        if ([regex]::IsMatch($content, '(?im)^\s*(?:Cookie|Authorization|Set-Cookie)\s*:') -or
                            [regex]::IsMatch($content, '(?i)"(?:password|passwd|cookie|authorization|accessToken|refreshToken|apiKey|clientSecret)"\s*:\s*"(?!\s*<redacted>\s*"|\s*")[^"]+"') -or
                            [regex]::IsMatch($content, '(?i)(?:Password|Pwd)\s*=\s*[^;\s]+')) {
                            $credentialContentEntries.Add($entryName)
                        }
                    }
                    finally {
                        $reader.Dispose()
                        $stream.Dispose()
                    }
                }
            }

            Assert-Condition -Condition ($forbiddenEntries.Count -eq 0) -Message "便携 ZIP 含数据库、日志、PDB、缓存、备份、诊断或用户数据：$((@($forbiddenEntries | Select-Object -Unique) -join ', '))"
            Assert-Condition -Condition ($credentialContentEntries.Count -eq 0) -Message "便携 ZIP 文本文件疑似含凭据：$((@($credentialContentEntries | Select-Object -Unique) -join ', '))"
        }
        finally {
            $archive.Dispose()
        }

        [System.IO.Compression.ZipFile]::ExtractToDirectory($script:portablePath, $script:portableExtractDirectory)
        $entryCount = @(Get-ChildItem -LiteralPath $script:portableExtractDirectory -Recurse -File).Count
        "ZIP 路径安全，且 $entryCount 个文件中未发现数据库/WAL/SHM、日志、PDB、Cookie、凭据、用户数据、安装 manifest、缓存、备份或诊断产物。"
    } | Out-Null

    Invoke-AuditCheck -Name 'portable.runtime-and-version' -Action {
        $requiredFiles = @(
            'StockIpoReminder.exe',
            'StockIpoReminder.deps.json',
            'StockIpoReminder.runtimeconfig.json',
            'coreclr.dll',
            'hostfxr.dll',
            'clrjit.dll'
        )
        foreach ($name in $requiredFiles) {
            Assert-Condition -Condition (Test-Path -LiteralPath (Join-Path $script:portableExtractDirectory $name) -PathType Leaf) -Message "自包含便携包缺少：$name"
        }

        $appExecutable = Join-Path $script:portableExtractDirectory 'StockIpoReminder.exe'
        $appPe = Get-PeMetadata -Path $appExecutable
        $setupPe = Get-PeMetadata -Path $script:setupPath
        Assert-Condition -Condition ([bool]$appPe.IsX64 -and [bool]$setupPe.IsX64) -Message 'App 或 Setup 不是 AMD64 PE。'

        foreach ($path in @($appExecutable, $script:setupPath)) {
            $fileVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($path).FileVersion
            $versionMatches = $fileVersion -eq $Version -or $fileVersion.StartsWith($Version + '.', [StringComparison]::Ordinal)
            Assert-Condition -Condition $versionMatches -Message "文件版本不匹配：$([System.IO.Path]::GetFileName($path))=$fileVersion"
        }
        'App/Setup 均为 AMD64，版本匹配，便携包包含完整 .NET 与 WPF 自包含运行时。'
    } | Out-Null

    Invoke-AuditCheck -Name 'portable.dependencies-allowlist' -Action {
        $depsPath = Join-Path $script:portableExtractDirectory 'StockIpoReminder.deps.json'
        $deps = Read-JsonFile -Path $depsPath
        Assert-Condition -Condition ($null -ne $deps.libraries) -Message '.deps.json 缺少 libraries。'
        $allowedNamePattern = '^(?:StockIpoReminder(?:\..*)?|runtimepack\.Microsoft\..*|Microsoft\.Data\.Sqlite(?:\..*)?|Microsoft\.Extensions\..*|Microsoft\.Web\.WebView2(?:\..*)?|Microsoft\.Windows(?:\..*)?|Microsoft\.WindowsAppSDK(?:\..*)?|PdfPig|SQLitePCLRaw\..*|System\.Numerics\.Tensors)$'
        $unexpected = [System.Collections.Generic.List[string]]::new()
        $libraryCount = 0
        foreach ($property in $deps.libraries.PSObject.Properties) {
            $libraryCount++
            $identity = [string]$property.Name
            $separator = $identity.LastIndexOf('/')
            $name = if ($separator -gt 0) { $identity.Substring(0, $separator) } else { $identity }
            if (-not [regex]::IsMatch($name, $allowedNamePattern, [System.Text.RegularExpressions.RegexOptions]::IgnoreCase)) {
                $unexpected.Add($identity)
            }
        }
        Assert-Condition -Condition ($libraryCount -gt 0) -Message '.deps.json libraries 为空。'
        Assert-Condition -Condition ($unexpected.Count -eq 0) -Message "发现依赖 allowlist 之外的库：$((@($unexpected) -join ', '))"
        "已验证 $libraryCount 个项目/包/运行时依赖，全部位于发布 allowlist。"
    } | Out-Null

    Invoke-AuditCheck -Name 'release.authenticode-state' -Action {
        Assert-Condition -Condition ($null -ne $script:manifest) -Message 'manifest 未成功加载。'
        $productExecutables = [System.Collections.Generic.List[string]]::new()
        $productExecutables.Add($script:setupPath)
        foreach ($file in Get-ChildItem -LiteralPath $script:portableExtractDirectory -Filter 'StockIpoReminder*.exe' -File) {
            $productExecutables.Add($file.FullName)
        }
        Assert-Condition -Condition ($productExecutables.Count -ge 2) -Message '未找到 App 和 Setup 两个产品可执行文件。'

        $expectedSigned = [bool]$script:manifest.signed
        $mismatches = [System.Collections.Generic.List[string]]::new()
        foreach ($path in $productExecutables) {
            $pe = Get-PeMetadata -Path $path
            if ([bool]$pe.HasEmbeddedAuthenticode -ne $expectedSigned) {
                $mismatches.Add([System.IO.Path]::GetFileName($path))
            }
        }
        Assert-Condition -Condition ($mismatches.Count -eq 0) -Message "PE Certificate Table 与 manifest signed=$expectedSigned 不一致：$((@($mismatches) -join ', '))"
        "通过 PE Optional Header Certificate Table 直接验证 $($productExecutables.Count) 个产品 EXE；状态与 manifest signed=$expectedSigned 一致。"
    } | Out-Null

    Invoke-AuditCheck -Name 'source.outbound-host-allowlist' -Action {
        $policyPath = Join-Path $script:workspace 'src\StockIpoReminder.Infrastructure\Runtime\OutboundNetworkPolicy.cs'
        $policyText = Get-Content -Raw -Encoding UTF8 -LiteralPath $policyPath
        $allowedHosts = @{}
        foreach ($match in [regex]::Matches($policyText, '(?im)^\s*"(?<host>[a-z0-9](?:[a-z0-9-]*\.)+[a-z0-9-]+)",?\s*$')) {
            $allowedHosts[$match.Groups['host'].Value.ToLowerInvariant()] = $true
        }
        Assert-Condition -Condition ($allowedHosts.Count -ge 8) -Message '无法从 OutboundNetworkPolicy 解析完整域名白名单。'

        $usedHosts = @{}
        $violations = [System.Collections.Generic.List[string]]::new()
        foreach ($file in Get-ProductionSourceFiles) {
            $content = Get-Content -Raw -Encoding UTF8 -LiteralPath $file.FullName
            $content = $content.Replace('http://schemas.microsoft.com/windows/2004/02/mit/task', '')
            foreach ($match in [regex]::Matches($content, '(?i)\b(?<scheme>https?)://(?<host>[a-z0-9](?:[a-z0-9-]*\.)*[a-z0-9-]+)(?::\d+)?')) {
                $scheme = $match.Groups['scheme'].Value.ToLowerInvariant()
                $hostName = $match.Groups['host'].Value.ToLowerInvariant()
                if ($scheme -ne 'https' -or -not $allowedHosts.ContainsKey($hostName)) {
                    $relativeFile = Get-WorkspaceRelativePath -Path $file.FullName
                    $violations.Add("$relativeFile -> ${scheme}://$hostName")
                }
                else {
                    $usedHosts[$hostName] = $true
                }
            }
        }
        Assert-Condition -Condition ($violations.Count -eq 0) -Message "源码含 HTTP 或白名单外出站域名：$((@($violations | Select-Object -Unique) -join ', '))"
        "源码中使用的 HTTPS 主机均为精确白名单项：$((@($usedHosts.Keys | Sort-Object) -join ', '))。"
    } | Out-Null

    Invoke-AuditCheck -Name 'source.no-broker-login-or-ordering' -Action {
        $forbiddenCodePatterns = @(
            '(?i)\b(?:Place|Submit|Send|Execute|Create)(?:Stock|Ipo|Trade|Subscription)?Order(?:Async)?\b',
            '(?i)\b(?:Broker|Trading|Securities)(?:Api|Sdk|Client|Session|Login|Authentication|Credential|Password|Account)(?:Async)?\b',
            '(?i)\b(?:Trade|Trading)(?:Api|Sdk|Client|Session|Order)(?:Async)?\b',
            '(?i)\b(?:Selenium|Playwright|WebDriver|PuppeteerSharp)\b'
        )
        $violations = [System.Collections.Generic.List[string]]::new()
        foreach ($file in Get-ProductionSourceFiles) {
            $content = Get-Content -Raw -Encoding UTF8 -LiteralPath $file.FullName
            foreach ($pattern in $forbiddenCodePatterns) {
                if ([regex]::IsMatch($content, $pattern)) {
                    $relativeFile = Get-WorkspaceRelativePath -Path $file.FullName
                    $violations.Add($relativeFile)
                    break
                }
            }
        }

        foreach ($project in Get-ChildItem -LiteralPath (Join-Path $script:workspace 'src') -Recurse -Filter '*.csproj' -File) {
            $projectText = Get-Content -Raw -Encoding UTF8 -LiteralPath $project.FullName
            foreach ($match in [regex]::Matches($projectText, '(?i)<PackageReference\s+Include="(?<name>[^"]+)"')) {
                $packageName = $match.Groups['name'].Value
                if ([regex]::IsMatch($packageName, '(?i)(?:broker|trading|tradeapi|securities|selenium|playwright|webdriver|puppeteer)')) {
                    $violations.Add((Get-WorkspaceRelativePath -Path $project.FullName) + " package=$packageName")
                }
            }
        }

        Assert-Condition -Condition ($violations.Count -eq 0) -Message "发现疑似券商登录、凭据、交易/下单 SDK 或浏览器自动化实现：$((@($violations | Select-Object -Unique) -join ', '))"
        '生产源码和依赖中未发现券商登录、账号凭据、交易/下单 API 或浏览器自动化实现。'
    } | Out-Null

    Invoke-AuditCheck -Name 'evidence.windows-smoke' -Action {
        $script:smokeReportResolved = Resolve-ReportPath `
            -RequestedPath $SmokeReportPath `
            -SearchDirectory (Join-Path $script:workspace 'artifacts\smoke') `
            -Filter "windows-$Version-*.json" `
            -Description 'Windows smoke 报告'
        Assert-SafeDescendant -Path $script:smokeReportResolved -Parent $script:workspace
        Assert-ReportRedacted -Path $script:smokeReportResolved
        $smoke = Read-JsonFile -Path $script:smokeReportResolved
        Assert-SuccessReport -Report $smoke -Description 'Windows smoke'
        Assert-Condition -Condition ([string]$smoke.version -eq $Version) -Message 'Windows smoke 版本不匹配。'
        Assert-Condition -Condition ($null -ne $smoke.evidence) -Message 'Windows smoke 缺少 evidence。'
        Assert-Condition -Condition ([int]$smoke.evidence.uiCheckCount -ge 55) -Message 'UI smoke 检查项少于 55。'
        Assert-Condition -Condition ([int]$smoke.evidence.uiScreenshotCount -ge 10) -Message 'UI smoke 截图少于 10 张。'
        Assert-Condition -Condition ([int]$smoke.evidence.recoveryCheckCount -ge 13) -Message '恢复事件 smoke 检查项少于 13。'

        $evidenceFields = @('uiReport', 'processPrepareReport', 'processVerifyReport', 'recoveryReport')
        foreach ($field in $evidenceFields) {
            $property = $smoke.evidence.PSObject.Properties[$field]
            Assert-Condition -Condition ($null -ne $property -and -not [string]::IsNullOrWhiteSpace([string]$property.Value)) -Message "Windows smoke 缺少证据路径：$field"
            $relativePath = [string]$property.Value
            Assert-Condition -Condition (-not [System.IO.Path]::IsPathRooted($relativePath) -and (@($relativePath.Replace('\', '/').Split('/')) -notcontains '..')) -Message "Windows smoke 证据路径必须为安全相对路径：$field"
            $fullPath = [System.IO.Path]::GetFullPath((Join-Path $script:workspace $relativePath))
            Assert-SafeDescendant -Path $fullPath -Parent $script:workspace
            Assert-Condition -Condition (Test-Path -LiteralPath $fullPath -PathType Leaf) -Message "Windows smoke 证据文件不存在：$field"
            Assert-ReportRedacted -Path $fullPath
            $subReport = Read-JsonFile -Path $fullPath
            Assert-SuccessReport -Report $subReport -Description $field
        }

        $script:evidence.windowsSmoke = Get-WorkspaceRelativePath -Path $script:smokeReportResolved
        "Windows smoke、55+ UI 检查、10+ 截图、进程强制结束恢复和 13+ 恢复事件检查均成功且报告已脱敏。"
    } | Out-Null

    Invoke-AuditCheck -Name 'evidence.strict-network-diagnostic' -Action {
        $script:diagnosticReportResolved = Resolve-ReportPath `
            -RequestedPath $DiagnosticReportPath `
            -SearchDirectory (Join-Path $script:workspace 'artifacts\diagnostics') `
            -Filter 'full-sync-*.json' `
            -Description '严格联网诊断报告'
        Assert-SafeDescendant -Path $script:diagnosticReportResolved -Parent $script:workspace
        Assert-ReportRedacted -Path $script:diagnosticReportResolved
        $diagnostic = Read-JsonFile -Path $script:diagnosticReportResolved
        Assert-Condition -Condition ([bool]$diagnostic.success) -Message '严格联网诊断 success=false。'
        Assert-Condition -Condition ([string]$diagnostic.mode -in @('sync', 'all')) -Message '严格联网诊断未执行 sync。'
        Assert-Condition -Condition (@($diagnostic.checks | Where-Object { -not [bool]$_.passed }).Count -eq 0) -Message '严格联网诊断含失败检查。'
        Assert-Condition -Condition (-not [bool]$diagnostic.dataIsolation.usedLocalApplicationData) -Message '严格联网诊断错误使用了用户应用数据目录。'
        Assert-Condition -Condition (-not [bool]$diagnostic.dataIsolation.keepRequested) -Message '最终严格联网诊断不得保留临时数据。'
        Assert-Condition -Condition ([bool]$diagnostic.dataIsolation.cleanupAttempted -and [bool]$diagnostic.dataIsolation.cleanupSucceeded) -Message '严格联网诊断临时目录未成功清理。'
        Assert-Condition -Condition ([bool]$diagnostic.synchronization.serviceSucceeded) -Message '严格联网诊断同步服务失败。'
        Assert-Condition -Condition ([string]$diagnostic.synchronization.databaseIntegrity -eq 'ok') -Message '严格联网诊断 SQLite integrity_check 未通过。'
        $script:evidence.strictNetworkDiagnostic = Get-WorkspaceRelativePath -Path $script:diagnosticReportResolved
        '严格联网诊断成功、SQLite 完整、使用隔离目录并完成清理，报告不含凭据头、URL query 或绝对路径。'
    } | Out-Null
}
finally {
    try {
        if (Test-Path -LiteralPath $script:auditStagingDirectory) {
            Assert-SafeDescendant -Path $script:auditStagingDirectory -Parent $script:auditStagingParent
            Remove-Item -LiteralPath $script:auditStagingDirectory -Recurse -Force
        }
        Add-AuditCheck -Name 'audit.staging-cleanup' -Passed $true -Detail '审计临时解压目录已删除。'
    }
    catch {
        Add-AuditCheck -Name 'audit.staging-cleanup' -Passed $false -Detail $_.Exception.Message
    }
}

$failedChecks = @($script:checks | Where-Object { -not [bool]$_.passed })
$report = [ordered]@{
    schemaVersion = '1'
    success = $failedChecks.Count -eq 0
    product = 'StockIpoReminder'
    version = $Version
    generatedAtUtc = [DateTimeOffset]::UtcNow.ToString('O')
    releaseDirectory = "artifacts/release/$Version"
    inputs = $script:evidence
    checks = @($script:checks)
    failedChecks = @($failedChecks | ForEach-Object { [string]$_.name })
}

$outputDirectory = Split-Path -Parent $script:outputPath
if (-not [string]::IsNullOrWhiteSpace($outputDirectory)) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}
[System.IO.File]::WriteAllText(
    $script:outputPath,
    ($report | ConvertTo-Json -Depth 8),
    [System.Text.UTF8Encoding]::new($false))

Write-Host "Release audit report: $script:outputPath"
if (-not $report.success) {
    throw "Release audit failed: $($report.failedChecks -join ', ')"
}

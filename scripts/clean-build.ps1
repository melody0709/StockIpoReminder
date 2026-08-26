[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildRoot = Join-Path $workspace 'build'
$stopScript = Join-Path $PSScriptRoot 'stop-runtime-process.ps1'

& $stopScript -ExecutablePath (Join-Path $buildRoot 'run\x64-release\StockIpoReminder.exe')

foreach ($relativePath in @('cargo', 'run', 'artifacts', 'logs')) {
    $path = [System.IO.Path]::GetFullPath((Join-Path $buildRoot $relativePath))
    $prefix = $buildRoot.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $path.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing clean outside build root: $path"
    }
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Recurse -Force
    }
}

Write-Host 'Clean complete. build\packages and build\README.txt were preserved.'

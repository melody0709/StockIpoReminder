[CmdletBinding()]
param(
    [string]$ExecutablePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($ExecutablePath)) {
    $ExecutablePath = Join-Path (Split-Path $PSScriptRoot -Parent) 'build\run\x64-release\StockIpoReminder.exe'
}

$target = [System.IO.Path]::GetFullPath($ExecutablePath)
$matches = @(Get-CimInstance Win32_Process -Filter "Name = 'StockIpoReminder.exe'" -ErrorAction SilentlyContinue |
    Where-Object {
        -not [string]::IsNullOrWhiteSpace($_.ExecutablePath) -and
        [string]::Equals(
            [System.IO.Path]::GetFullPath($_.ExecutablePath),
            $target,
            [System.StringComparison]::OrdinalIgnoreCase)
    })

foreach ($process in $matches) {
    Stop-Process -Id $process.ProcessId -Force -ErrorAction Stop
}

if ($matches.Count -gt 0) {
    Write-Host "Stopped repository runtime instance(s): $($matches.Count)"
}

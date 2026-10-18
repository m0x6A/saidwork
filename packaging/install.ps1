# Install saidwork from the Windows release zip: copies the binary and its shared files
# and puts saidwork on your user PATH. No administrator rights needed. Run from the
# unpacked zip directory:
#
#   powershell -ExecutionPolicy Bypass -File .\install.ps1
#   .\install.ps1 -Prefix D:\Tools\saidwork
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\saidwork')
)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$bin = Join-Path $Prefix 'bin'

New-Item -ItemType Directory -Force -Path $bin | Out-Null
Copy-Item -Force (Join-Path $here 'bin\saidwork.exe') $bin
# Shipped resources; saidwork finds them through `{share}/...` in its configuration.
$share = Join-Path $Prefix 'share'
New-Item -ItemType Directory -Force -Path $share | Out-Null
Copy-Item -Recurse -Force (Join-Path $here 'share\saidwork') $share
Write-Host "installed $bin\saidwork.exe"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = if ($userPath) { $userPath -split ';' } else { @() }
if ($entries -notcontains $bin) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $bin) -join ';').Trim(';'), 'User')
    Write-Host "added $bin to your PATH; open a new terminal to use it"
}

if (-not (Get-Command uv -ErrorAction SilentlyContinue)) {
    Write-Host 'note: the local preset runs Pianissimo with uv: winget install astral-sh.uv'
}

Write-Host 'next: saidwork config init     # or --preset whisper-ollama / openai'
Write-Host '      saidwork doctor          # says what else to install for your configuration'

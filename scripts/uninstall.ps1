[CmdletBinding()]
param(
    [switch]$KeepConfig
)

$ErrorActionPreference = 'Stop'
$installDir = Join-Path $env:LOCALAPPDATA 'CodexDiscordRelay\bin'
$exe = Join-Path $installDir 'codex-discord.exe'
if (Test-Path -LiteralPath $exe) {
    & $exe uninstall-startup
}
if (Test-Path -LiteralPath $installDir) {
    Remove-Item -LiteralPath $installDir -Recurse -Force
}
if (-not $KeepConfig) {
    $data = Join-Path $env:LOCALAPPDATA 'Codex\CodexDiscordRelay'
    if (Test-Path -LiteralPath $data) {
        Remove-Item -LiteralPath $data -Recurse -Force
    }
}
Write-Host 'Codex Discord Relay removed.'


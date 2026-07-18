[CmdletBinding()]
param(
    [switch]$Highest
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$prebuilt = Join-Path $repo 'codex-discord.exe'
$built = Join-Path $repo 'target\release\codex-discord.exe'
if (-not (Test-Path -LiteralPath $prebuilt)) {
    $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (-not (Test-Path -LiteralPath $cargo)) {
        $rustup = Join-Path $env:TEMP 'codex-relay-rustup-init.exe'
        Invoke-WebRequest 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile $rustup
        & $rustup -y --default-toolchain stable --profile minimal
        if ($LASTEXITCODE -ne 0) { throw "rustup failed with exit code $LASTEXITCODE" }
    }
    Push-Location $repo
    try {
        & $cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
    $prebuilt = $built
}

$installArgs = @('install', '--configure')
if ($Highest) { $installArgs += '--highest' }
& $prebuilt @installArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host 'Rust installer completed. Run `codex-discord doctor --deep` for live verification.' -ForegroundColor Green

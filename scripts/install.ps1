# Build bin\hertrees.exe for this checkout from source with cargo (Rust 1.88+).
#
# Herdr runs this as the plugin's Windows [[build]] step on `herdr plugin
# install` (updating is reinstalling; `plugin link` never runs it: run it
# yourself for a local checkout).
#
# The manifest's pane command names `bin/hertrees`, so bin\hertrees (no
# extension) is installed beside bin\hertrees.exe: launchers that append .exe
# find the one, launchers that take the path as written find the other.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2

$Root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $Root
$BinDir = Join-Path $Root 'bin'
$Built = Join-Path $Root 'target\release\hertrees.exe'

function Say([string]$Message) { [Console]::Error.WriteLine("hertrees: $Message") }

# rustup puts cargo in %USERPROFILE%\.cargo\bin, which a Herdr server started
# before the install may not have on PATH.
$Cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($Cargo) {
    $Cargo = $Cargo.Source
} else {
    $Cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (-not (Test-Path -LiteralPath $Cargo)) {
        Say 'cargo (Rust 1.88+) is needed to build the plugin: https://rustup.rs'
        exit 1
    }
}

# cargo reports on stderr, which passes straight through; only its stdout is
# moved there, so the build step's stdout stays clean.
& $Cargo build --release --locked | ForEach-Object { [Console]::Error.WriteLine("$_") }
$Code = $LASTEXITCODE
if ($Code -ne 0) {
    Say "cargo build failed (exit $Code; hertrees needs Rust 1.88 or newer)"
    exit 1
}

# Windows cannot overwrite a running .exe but can rename it: an open popup
# keeps its binary busy, so each one is moved aside before the new one lands.
New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
foreach ($Stale in @(Get-ChildItem -LiteralPath $BinDir -Filter 'hertrees*.old.*' -ErrorAction SilentlyContinue)) {
    Remove-Item -LiteralPath $Stale.FullName -Force -ErrorAction SilentlyContinue
}
foreach ($Name in @('hertrees.exe', 'hertrees')) {
    $Destination = Join-Path $BinDir $Name
    $Temp = "$Destination.tmp.$PID"
    Copy-Item -LiteralPath $Built -Destination $Temp -Force
    if (Test-Path -LiteralPath $Destination) {
        Move-Item -LiteralPath $Destination -Destination "$Destination.old.$PID" -Force
    }
    Move-Item -LiteralPath $Temp -Destination $Destination -Force
    # Best-effort: a copy still running is deleted on the next install.
    Remove-Item -LiteralPath "$Destination.old.$PID" -Force -ErrorAction SilentlyContinue
}
Say "built bin\hertrees.exe"

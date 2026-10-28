# Put the hertrees binary at bin\hertrees.exe for this checkout.
#
# Herdr runs this as the plugin's Windows [[build]] step on `herdr plugin
# install` (`plugin link` never runs it: run it yourself for a local checkout).
# It downloads the prebuilt binary of the manifest's version from the GitHub
# release, checks it against the release's SHA256SUMS, and falls back to
# `cargo build --release` (Rust 1.88+) when there is no binary for this machine
# or the download or the check fails.
#
# A git checkout that differs from the release (uncommitted changes, or HEAD is
# not the local v<version> tag) is built from source, so development builds are
# never replaced by the released binary.
#
#   HERTREES_BUILD=source      skip the download and build with cargo
#   HERTREES_DOWNLOAD_BASE=    release download URL to use instead of GitHub's
#
# The manifest's pane command names `bin/hertrees`, so bin\hertrees (no
# extension) is installed beside bin\hertrees.exe: launchers that append .exe
# find the one, launchers that take the path as written find the other.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2

$Root = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $Root
$BinDir = Join-Path $Root 'bin'
$Target = Join-Path $BinDir 'hertrees.exe'
$Alias = Join-Path $BinDir 'hertrees'
$RepoUrl = 'https://github.com/janooh37-hue/hertrees'

function Say([string]$Message) { [Console]::Error.WriteLine("hertrees: $Message") }

$Version = $null
foreach ($Line in Get-Content -LiteralPath (Join-Path $Root 'herdr-plugin.toml')) {
    if ($Line -match '^version\s*=\s*"([^"]*)"') { $Version = $Matches[1]; break }
}
if (-not $Version) {
    Say 'cannot read the version from herdr-plugin.toml'
    exit 1
}

# Run a native command without PowerShell 5.1 turning its stderr output into a
# terminating NativeCommandError under $ErrorActionPreference = 'Stop'.
# Returns the exit code; stdout goes to $script:NativeOutput, or with
# -ShowOutput to stderr, so the build step's stdout stays clean.
function Invoke-Native([string]$Command, [string[]]$Arguments, [switch]$ShowOutput) {
    $Saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        if ($ShowOutput) {
            & $Command @Arguments 2>&1 | ForEach-Object { [Console]::Error.WriteLine("$_") }
            $script:NativeOutput = ''
        } else {
            $script:NativeOutput = (& $Command @Arguments 2>$null | Out-String).Trim()
        }
        return $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $Saved
    }
}

# Whether this checkout is the released source of v$Version. Herdr's managed
# checkouts are shallow clones without tags, so an unknown tag counts as released.
function Test-ReleaseCheckout {
    if (-not (Test-Path -LiteralPath (Join-Path $Root '.git'))) { return $true }
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { return $true }
    if ((Invoke-Native 'git' @('status', '--porcelain')) -ne 0) { return $true }
    if ($script:NativeOutput) {
        Say 'the checkout has uncommitted changes'
        return $false
    }
    if ((Invoke-Native 'git' @('rev-parse', '-q', '--verify', "refs/tags/v$Version^{commit}")) -ne 0) { return $true }
    $Tagged = $script:NativeOutput
    if ((Invoke-Native 'git' @('rev-parse', '-q', '--verify', 'HEAD^{commit}')) -ne 0) { return $true }
    if ($Tagged -ne $script:NativeOutput) {
        Say "HEAD is not the v$Version tag"
        return $false
    }
    return $true
}

function Get-AssetName {
    $Arch = $null
    try {
        $Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    } catch {
        # Windows PowerShell on an old .NET: a 32-bit shell on 64-bit Windows
        # reports the native architecture in PROCESSOR_ARCHITEW6432.
        $Arch = $env:PROCESSOR_ARCHITEW6432
        if (-not $Arch) { $Arch = $env:PROCESSOR_ARCHITECTURE }
    }
    switch -Regex ($Arch) {
        '^(X64|AMD64)$' { return 'hertrees-windows-x86_64.exe' }
        '^ARM64$' { return 'hertrees-windows-aarch64.exe' }
        default { return $null }
    }
}

# Windows cannot overwrite a running .exe but can rename it: an open popup
# keeps its binary busy, so an existing binary is moved aside to
# <name>.old.<pid> before the new one is renamed in. The .exe and the
# extensionless alias are staged first, then swapped, and a failed swap
# restores whatever was moved aside.
function Install-Binary([string]$Source) {
    New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
    foreach ($Stale in @(Get-ChildItem -LiteralPath $BinDir -Filter 'hertrees*.old.*' -ErrorAction SilentlyContinue)) {
        Remove-Item -LiteralPath $Stale.FullName -Force -ErrorAction SilentlyContinue
    }
    $Pairs = @()
    try {
        foreach ($Destination in @($Target, $Alias)) {
            $Temp = "$Destination.tmp.$PID"
            Copy-Item -LiteralPath $Source -Destination $Temp -Force
            $Pairs += ,@($Destination, $Temp, "$Destination.old.$PID")
        }
    } catch {
        foreach ($Pair in $Pairs) { Remove-Item -LiteralPath $Pair[1] -Force -ErrorAction SilentlyContinue }
        throw
    }
    $Done = @()
    try {
        foreach ($Pair in $Pairs) {
            if (Test-Path -LiteralPath $Pair[0]) {
                Move-Item -LiteralPath $Pair[0] -Destination $Pair[2] -Force
            }
            Move-Item -LiteralPath $Pair[1] -Destination $Pair[0] -Force
            $Done += ,$Pair
        }
    } catch {
        foreach ($Pair in $Pairs) {
            Remove-Item -LiteralPath $Pair[1] -Force -ErrorAction SilentlyContinue
            if (Test-Path -LiteralPath $Pair[2]) {
                $IsDone = @($Done | Where-Object { $_[0] -eq $Pair[0] }).Count -gt 0
                if ($IsDone) {
                    Remove-Item -LiteralPath $Pair[0] -Force -ErrorAction SilentlyContinue
                }
                if (-not (Test-Path -LiteralPath $Pair[0])) {
                    Move-Item -LiteralPath $Pair[2] -Destination $Pair[0] -Force -ErrorAction SilentlyContinue
                }
            }
        }
        throw
    }
    # The old copies may still be running; deleting them is best-effort and
    # the next install retries.
    foreach ($Pair in $Pairs) { Remove-Item -LiteralPath $Pair[2] -Force -ErrorAction SilentlyContinue }
}

function Invoke-Download {
    $Asset = Get-AssetName
    if (-not $Asset) {
        Say 'no prebuilt binary for this architecture'
        return $false
    }
    $Base = $env:HERTREES_DOWNLOAD_BASE
    if (-not $Base) { $Base = "$RepoUrl/releases/download/v$Version" }
    if ($Base -notmatch '^(https|file)://') { Say "warning: HERTREES_DOWNLOAD_BASE is not https: $Base" }
    $Temp = Join-Path ([System.IO.Path]::GetTempPath()) ('hertrees-' + [System.Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $Temp | Out-Null
    try {
        Say "downloading $Asset $Version"
        try {
            # Windows PowerShell 5.1 still defaults to TLS 1.0 on some systems.
            [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        } catch {}
        $ProgressPreference = 'SilentlyContinue'
        $File = Join-Path $Temp $Asset
        $Sums = Join-Path $Temp 'SHA256SUMS'
        try {
            Invoke-WebRequest -UseBasicParsing -TimeoutSec 300 -Uri "$Base/$Asset" -OutFile $File
            Invoke-WebRequest -UseBasicParsing -TimeoutSec 300 -Uri "$Base/SHA256SUMS" -OutFile $Sums
        } catch {
            Say "download failed: $($_.Exception.Message)"
            return $false
        }
        $Expected = $null
        foreach ($Line in Get-Content -LiteralPath $Sums) {
            $Fields = $Line.Trim() -split '\s+', 2
            if ($Fields.Count -eq 2 -and ($Fields[1] -eq $Asset -or $Fields[1] -eq "*$Asset")) {
                $Expected = $Fields[0].ToLowerInvariant()
                break
            }
        }
        if (-not $Expected) {
            Say "SHA256SUMS has no entry for $Asset"
            return $false
        }
        $Actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $File).Hash.ToLowerInvariant()
        if ($Expected -ne $Actual) {
            Say "checksum mismatch for $Asset (expected $Expected, got $Actual)"
            return $false
        }
        # Run it from bin\ rather than %TEMP%, which AppLocker or similar
        # policies may block, and only keep it if it reports this version.
        New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
        $Check = Join-Path $BinDir ".hertrees.check.$PID.exe"
        Copy-Item -LiteralPath $File -Destination $Check -Force
        try {
            $Code = Invoke-Native $Check @('--version')
            if ($Code -ne 0) {
                Say "the downloaded $Asset does not run on this machine"
                return $false
            }
            if ($script:NativeOutput -ne "hertrees $Version") {
                Say "downloaded binary reports '$($script:NativeOutput)', expected 'hertrees $Version'"
                return $false
            }
            Install-Binary $Check
        } finally {
            Remove-Item -LiteralPath $Check -Force -ErrorAction SilentlyContinue
        }
        return $true
    } catch {
        Say "download failed: $($_.Exception.Message)"
        return $false
    } finally {
        Remove-Item -LiteralPath $Temp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-SourceBuild {
    # rustup puts cargo in %USERPROFILE%\.cargo\bin, which a Herdr server
    # started before the install may not have on PATH.
    $Cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if ($Cargo) {
        $Cargo = $Cargo.Source
    } else {
        $Cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
        if (-not (Test-Path -LiteralPath $Cargo)) {
            Say 'cargo is not installed (building hertrees needs Rust 1.88 or newer)'
            return $false
        }
    }
    Say 'building from source with cargo'
    try {
        # target\ is left in place: in a development clone it is the build cache.
        $Code = Invoke-Native $Cargo @('build', '--release', '--locked') -ShowOutput
        if ($Code -ne 0) {
            Say "cargo build failed (exit $Code; hertrees needs Rust 1.88 or newer)"
            return $false
        }
        Install-Binary (Join-Path $Root 'target\release\hertrees.exe')
        return $true
    } catch {
        Say "cargo build failed: $($_.Exception.Message)"
        return $false
    }
}

if ($env:HERTREES_BUILD -ne 'source' -and (Test-ReleaseCheckout) -and (Invoke-Download)) {
    Say "installed $Target"
    exit 0
}
if (Invoke-SourceBuild) {
    Say "installed $Target"
    exit 0
}
Say "could not install hertrees ${Version}: no prebuilt binary could be downloaded"
Say 'and building from source failed. Install Rust 1.88 or newer'
Say '(https://rustup.rs), then reinstall the plugin:'
Say '  herdr plugin install janooh37-hue/hertrees'
exit 1

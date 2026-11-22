<#
.SYNOPSIS
    Downloads the prebuilt rusty_v8 static library and installs it into the
    cargo target directory.

.DESCRIPTION
    The `v8` crate's build script downloads a ~230 MB prebuilt V8 from the
    denoland/rusty_v8 GitHub releases on every clean build. This script does
    that step up front:

      1. Downloads `rusty_v8<features>_release_<target>.lib.gz` for the `v8`
         version locked in Cargo.lock into the shared cargo cache
         (`$CARGO_HOME/.rusty_v8`), under the same key the build script uses.
      2. Decompresses it to `<target-dir>/<profile>/gn_out/obj/rusty_v8.lib`.
      3. Writes the `rusty_v8.sum` record next to it, so the build script
         treats the library as already fetched and skips the download.
      4. When the target directory and the cargo registry sit on different
         drives, creates `<target-dir>/<profile>/gn_root` as a directory
         junction to the `v8` crate source. The build script wants a symlink
         there and panics with "Failed to create symlink" when Windows denies
         symlink creation (no admin / Developer Mode); a junction needs no
         privilege and satisfies its check.

    Windows always links the release V8 build, whichever cargo profile is
    being built; -CargoProfile only picks which target directories receive it.

.PARAMETER Version
    rusty_v8 version. Defaults to the `v8` version in Cargo.lock.

.PARAMETER Target
    Rust target triple. Defaults to x86_64-pc-windows-msvc.

.PARAMETER CargoProfile
    Cargo profile directories to install into. Defaults to release.

.PARAMETER Features
    Prebuilt variant suffix, matching enabled `v8` crate features:
    ptrcomp (v8_enable_pointer_compression), sandbox (v8_enable_sandbox).

.PARAMETER Force
    Re-download and re-install even when the cached copy is current.

.EXAMPLE
    .\tools\fetch-v8.ps1

.EXAMPLE
    .\tools\fetch-v8.ps1 -CargoProfile release, debug
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$Target = 'x86_64-pc-windows-msvc',
    [string[]]$CargoProfile = @('release'),
    [ValidateSet('ptrcomp', 'sandbox')]
    [string[]]$Features = @(),
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is 10x slower with the progress bar.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Root = Split-Path -Parent $PSScriptRoot

if (-not $Version) {
    $lock = Get-Content (Join-Path $Root 'Cargo.lock') -Raw
    $m = [regex]::Match($lock, '(?m)^name = "v8"\r?\nversion = "([^"]+)"')
    if (-not $m.Success) { throw 'No `v8` package in Cargo.lock; pass -Version.' }
    $Version = $m.Groups[1].Value
}

# Names and URL must match the v8 crate's build.rs exactly, or it re-downloads.
$suffix   = ($Features | Sort-Object { @('ptrcomp', 'sandbox').IndexOf($_) } | ForEach-Object { "_$_" }) -join ''
$libName  = "rusty_v8${suffix}_release_$Target.lib"
$gzName   = "$libName.gz"
$tag      = "v$Version"
$url      = "https://github.com/denoland/rusty_v8/releases/download/$tag/$gzName"

$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
$cacheDir  = Join-Path $cargoHome '.rusty_v8'
$cacheFile = Join-Path $cacheDir (("$tag/$gzName") -replace '[^A-Za-z0-9]', '_')

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

Write-Host "rusty_v8 $tag ($libName)"

# 1. Download into the cargo cache.
if ($Force -or -not (Test-Path -LiteralPath $cacheFile)) {
    New-Item -ItemType Directory -Force -Path $cacheDir | Out-Null
    $tmp = "$cacheFile.tmp"
    Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    Write-Host "  downloading $url"
    $curl = Join-Path $env:SystemRoot 'System32\curl.exe'
    if (Test-Path -LiteralPath $curl) {
        & $curl -L -f --retry 3 --progress-bar -o $tmp $url
        if ($LASTEXITCODE -ne 0) { throw "curl failed ($LASTEXITCODE) fetching $url" }
    } else {
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
    }
    Move-Item -LiteralPath $tmp -Destination $cacheFile -Force
} else {
    Write-Host "  cached $cacheFile"
}
$archiveSha = Get-Sha256 $cacheFile

# 2-3. Decompress into each profile's gn_out/obj and write the .sum record.
foreach ($p in $CargoProfile) {
    $objDir  = Join-Path $targetDir "$p\gn_out\obj"
    $libPath = Join-Path $objDir 'rusty_v8.lib'
    $sumPath = Join-Path $objDir 'rusty_v8.sum'

    if (-not $Force -and (Test-Path -LiteralPath $libPath) -and (Test-Path -LiteralPath $sumPath)) {
        $sum = Get-Content -LiteralPath $sumPath
        if ($sum -contains "url $url" -and $sum -contains "archive-sha256 $archiveSha" -and
            $sum -contains "sha256 $(Get-Sha256 $libPath)") {
            Write-Host "  up to date $libPath"
            continue
        }
    }

    New-Item -ItemType Directory -Force -Path $objDir | Out-Null
    Write-Host "  extracting $libPath"
    $tmp = "$libPath.tmp"
    $in = [IO.File]::OpenRead($cacheFile)
    try {
        $gz  = New-Object IO.Compression.GZipStream($in, [IO.Compression.CompressionMode]::Decompress)
        $out = [IO.File]::Create($tmp)
        try { $gz.CopyTo($out, 1MB) } finally { $out.Dispose(); $gz.Dispose() }
    } finally { $in.Dispose() }
    Move-Item -LiteralPath $tmp -Destination $libPath -Force

    $record = "url $url`narchive-sha256 $archiveSha`nsha256 $(Get-Sha256 $libPath)`n"
    [IO.File]::WriteAllText($sumPath, $record)
}

# 4. gn_root junction for cross-drive layouts.
$crateDir = Get-Item -Path (Join-Path $cargoHome "registry\src\*\v8-$Version") -ErrorAction SilentlyContinue |
    Where-Object PSIsContainer | Select-Object -First 1 -ExpandProperty FullName
if (-not $crateDir) {
    Write-Warning "v8-$Version source not in the cargo registry yet; run 'cargo fetch' and re-run this script to create gn_root."
} else {
    foreach ($p in $CargoProfile) {
        $profileDir = Join-Path $targetDir $p
        if ([IO.Path]::GetPathRoot((Resolve-Path $profileDir).Path) -eq [IO.Path]::GetPathRoot($crateDir)) { continue }
        $link = Join-Path $profileDir 'gn_root'
        $item = Get-Item -LiteralPath $link -Force -ErrorAction SilentlyContinue
        if ($item -and $item.LinkType -and ($item.Target | Select-Object -First 1) -eq $crateDir) {
            Write-Host "  up to date $link"
            continue
        }
        if ($item) {
            if (-not $item.LinkType) { throw "$link exists and is not a link; remove it and re-run." }
            [IO.Directory]::Delete($link)   # removes the link only, never the target's contents
        }
        Write-Host "  junction $link -> $crateDir"
        New-Item -ItemType Junction -Path $link -Target $crateDir | Out-Null
    }
}

Write-Host 'done.'

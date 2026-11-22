<#
.SYNOPSIS
    Downloads the native build toolchain (LLVM, CMake, Ninja) into `.bin/`.

.DESCRIPTION
    Nothing in the default Rust build needs these. They exist for the native
    pieces that do:

      * LLVM  - `libclang.dll` for bindgen, and `clang-cl` for building Skia
                from source when no prebuilt binary matches the requested
                feature set (see crates/spherekit-skia).
      * CMake - C/C++ dependencies built through the `cmake` crate.
      * Ninja - the generator Skia's GN build and CMake both drive.

    Versions are pinned so every machine resolves the same tools. Each tool
    lands in its own directory and a stamp file records what was installed;
    re-running is a no-op unless a version changed or -Force is given.

    `.bin/` is git-ignored. Dot-source `.bin/env.ps1` afterwards to put the
    tools on PATH and set LIBCLANG_PATH for the current shell:

        . .\.bin\env.ps1

.PARAMETER Force
    Re-download everything, even when the stamp says it is current.

.PARAMETER Only
    Restrict to a subset: llvm, cmake, ninja.
#>
[CmdletBinding()]
param(
    [switch]$Force,
    [ValidateSet('llvm', 'cmake', 'ninja')]
    [string[]]$Only = @('llvm', 'cmake', 'ninja')
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is 10x slower with the progress bar.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$LlvmVersion  = '23.1.6'
$CMakeVersion = '4.5.1'
$NinjaVersion = '1.13.2'
$ZstdVersion  = '1.5.7'

$Root   = Split-Path -Parent $PSScriptRoot
$BinDir = Join-Path $Root '.bin'
$Cache  = Join-Path $BinDir '.downloads'
New-Item -ItemType Directory -Force -Path $BinDir, $Cache | Out-Null

# bsdtar ships with Windows 10 1803+. Git Bash's GNU tar may shadow it on PATH
# and mishandles drive-letter paths, so name it explicitly.
$Tar = Join-Path $env:SystemRoot 'System32\tar.exe'

$Tools = @{
    llvm = @{
        Version = $LlvmVersion
        # The .tar.zst is under a third the size of the .tar.xz.
        Url     = "https://github.com/llvm/llvm-project/releases/download/llvmorg-$LlvmVersion/clang+llvm-$LlvmVersion-x86_64-pc-windows-msvc.tar.zst"
        Strip   = 1
        Check   = 'bin\clang.exe'
    }
    cmake = @{
        Version = $CMakeVersion
        Url     = "https://github.com/Kitware/CMake/releases/download/v$CMakeVersion/cmake-$CMakeVersion-windows-x86_64.zip"
        Strip   = 1
        Check   = 'bin\cmake.exe'
    }
    ninja = @{
        Version = $NinjaVersion
        Url     = "https://github.com/ninja-build/ninja/releases/download/v$NinjaVersion/ninja-win.zip"
        Strip   = 0
        Check   = 'ninja.exe'
    }
}

# Fetched on demand into the download cache; only the LLVM archive needs it.
function Get-Zstd {
    $dir = Join-Path $Cache "zstd-v$ZstdVersion-win64"
    $exe = Join-Path $dir 'zstd.exe'
    if (-not (Test-Path $exe)) {
        $zip = "$dir.zip"
        Write-Host "  downloading zstd $ZstdVersion (needed to unpack LLVM)"
        Invoke-WebRequest -UseBasicParsing -OutFile $zip `
            -Uri "https://github.com/facebook/zstd/releases/download/v$ZstdVersion/zstd-v$ZstdVersion-win64.zip"
        & $Tar -xf $zip -C $Cache
        if (-not (Test-Path $exe)) { throw "zstd.exe not found after extracting $zip" }
    }
    return $exe
}

function Install-Tool([string]$Name, [hashtable]$Tool) {
    $dest  = Join-Path $BinDir $Name
    $stamp = Join-Path $dest '.version'
    if (-not $Force -and (Test-Path $stamp) -and ((Get-Content $stamp -Raw).Trim() -eq $Tool.Version) `
            -and (Test-Path (Join-Path $dest $Tool.Check))) {
        Write-Host "  $Name $($Tool.Version) already installed"
        return
    }

    $file    = Split-Path -Leaf $Tool.Url
    $archive = Join-Path $Cache $file
    if ($Force -or -not (Test-Path $archive)) {
        Write-Host "  downloading $file"
        $partial = "$archive.part"
        Invoke-WebRequest -Uri $Tool.Url -OutFile $partial -UseBasicParsing
        Move-Item -Force $partial $archive
    }

    Write-Host "  extracting $Name -> $dest"
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    $strip = if ($Tool.Strip -gt 0) { "--strip-components=$($Tool.Strip)" } else { '' }
    if ($archive.EndsWith('.zst')) {
        # LLVM compresses with a 2 GiB window (`--long=31`), which libarchive's
        # built-in zstd refuses ("Frame requires too much memory"). Decode with
        # the real zstd CLI and stream into tar. Through cmd, not PowerShell:
        # Windows PowerShell re-encodes bytes piped between native programs.
        $zstd = Get-Zstd
        & cmd /c "`"$zstd`" -dcq --long=31 `"$archive`" | `"$Tar`" -xf - -C `"$dest`" $strip"
    } else {
        & $Tar -xf $archive -C $dest $strip
    }
    if ($LASTEXITCODE -ne 0) { throw "tar failed extracting $file" }
    if (-not (Test-Path (Join-Path $dest $Tool.Check))) {
        throw "$Name extracted but $($Tool.Check) is missing"
    }
    Set-Content -Path $stamp -Value $Tool.Version -Encoding ascii
}

Write-Host "Toolchain -> $BinDir"
foreach ($name in $Only) { Install-Tool $name $Tools[$name] }

# An activation script, so nothing has to be added to the user's global PATH.
$envScript = @'
# Generated by tools/bootstrap.ps1. Dot-source it: . .\.bin\env.ps1
$bin = $PSScriptRoot
foreach ($dir in @("$bin\llvm\bin", "$bin\cmake\bin", "$bin\ninja")) {
    if ((Test-Path $dir) -and ($env:PATH -split ';' -notcontains $dir)) { $env:PATH = "$dir;$env:PATH" }
}
if (Test-Path "$bin\llvm\bin\libclang.dll") { $env:LIBCLANG_PATH = "$bin\llvm\bin" }
if (Test-Path "$bin\cmake\bin\cmake.exe")   { $env:CMAKE = "$bin\cmake\bin\cmake.exe" }
if (Test-Path "$bin\ninja\ninja.exe")       { $env:CMAKE_GENERATOR = 'Ninja'; $env:SKIA_NINJA_COMMAND = "$bin\ninja\ninja.exe" }
'@
Set-Content -Path (Join-Path $BinDir 'env.ps1') -Value $envScript -Encoding utf8

Write-Host ''
Write-Host 'Done. Activate for this shell with:  . .\.bin\env.ps1'

# Build entry point.
#
# Two things have to be true before cargo runs, and neither is true in a plain shell:
#
# 1. The Ninja CMake generator. Both whisper.cpp and llama.cpp build their Vulkan
#    shader compiler (vulkan-shaders-gen) as a nested CMake ExternalProject. Under the
#    Visual Studio generator that nested build fails on this machine with
#    "The system cannot find the batch label specified - VCEnd" and a missing
#    CMakeCache.txt, because MSBuild's generated custom-build batch files break on the
#    very long paths cargo's target directory produces. Ninja sidesteps the whole
#    MSBuild custom-build path and is substantially faster besides.
#
# 2. The MSVC environment. Ninja, unlike the Visual Studio generator, expects cl.exe,
#    link.exe and the Windows SDK to be on PATH already.
#
# Usage: .\scripts\build.ps1 [-Dev] [-Bundle] [cargo args...]

# -Bundle additionally produces the NSIS installer. It runs after the DLL copy below
# rather than through `cargo tauri build`, because tauri.conf.json ships those DLLs as
# bundle resources and they do not exist until cargo has run and the copy has happened.

param(
    [switch]$Dev,
    [switch]$Bundle,
    [Parameter(ValueFromRemainingArguments = $true)]
    $CargoArgs
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
if ($env:VCVARS64 -and (Test-Path $env:VCVARS64)) {
    $vcvars = $env:VCVARS64
} else {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vsPath = $null
    if (Test-Path $vswhere) {
        $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    }
    if ($vsPath) {
        $vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat'
    } else {
        $vcvars = $null
    }
    if (-not $vcvars -or -not (Test-Path $vcvars)) {
        throw 'vcvars64.bat not found. Install Visual Studio Build Tools with the "Desktop development with C++" workload, or set $env:VCVARS64 to the full path of vcvars64.bat.'
    }
}

# Pull the MSVC environment into this session. cmd is the only thing that can read
# vcvars, so run it there and import the resulting variables.
cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') {
        Set-Item -Path "env:$($matches[1])" -Value $matches[2] -ErrorAction SilentlyContinue
    }
}

$env:PATH = "$root\.tools;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:CMAKE_GENERATOR = 'Ninja'

if (-not (Get-Command cl -ErrorAction SilentlyContinue)) { throw 'cl.exe not on PATH after vcvars' }
if (-not (Get-Command ninja -ErrorAction SilentlyContinue)) { throw 'ninja.exe not found; see scripts/README or re-fetch into .tools' }
if (-not $env:VULKAN_SDK) { throw 'VULKAN_SDK is not set; the Vulkan SDK is required, see brief section 1' }

Set-Location $root
# Native-command argument arrays, not splatting: PowerShell unwraps a single-element
# array to a bare string, and @string splats one character per argument.
$cargoArgv = [System.Collections.Generic.List[string]]::new()
$cargoArgv.Add('build')
if (-not $Dev) { $cargoArgv.Add('--release') }
foreach ($a in $CargoArgs) { if ($null -ne $a) { $cargoArgv.Add([string]$a) } }

# CrispASR builds itself as a set of DLLs inside the cargo OUT_DIR, and the executable
# will not start without them on PATH or beside it. llama.cpp is linked statically
# precisely so its ggml does not produce files with the same names; see amendment A14.
$profileDir = Join-Path $root ($(if ($Dev) { 'target\debug' } else { 'target\release' }))

function Copy-CrispAsrRuntime {
    param([switch]$Quiet)

    $crispBin = Get-ChildItem -Path (Join-Path $profileDir 'build') -Directory -Filter 'crispasr-sys-*' -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName 'out\crispasr-build\bin' } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1

    if (-not $crispBin) {
        if (-not $Quiet) {
            Write-Warning 'CrispASR build output not found; the binary will not start without its DLLs.'
        }
        return
    }

    $copied = 0
    foreach ($dll in Get-ChildItem (Join-Path $crispBin '*.dll')) {
        $target = Join-Path $profileDir $dll.Name
        if (-not (Test-Path $target) -or (Get-Item $target).LastWriteTimeUtc -lt $dll.LastWriteTimeUtc) {
            Copy-Item $dll.FullName $target -Force
            $copied++
        }
    }
    if ($copied -gt 0) { Write-Host "copied $copied CrispASR runtime dll(s) to $profileDir" }
}

function Copy-MsvcRuntime {
    # Everything here is built with MSVC and imports MSVCP140, VCRUNTIME140,
    # VCRUNTIME140_1 and VCOMP140 (the OpenMP runtime ggml's CPU backend uses). Windows
    # does not ship those -- they arrive with the Visual C++ redistributable, which a
    # clean install has never seen, and without them the app dies at launch with a
    # missing-DLL box that tells the user nothing. Microsoft's redistributable licence
    # permits shipping them beside the executable, which is cheaper for the user than a
    # second installer.
    if (-not $env:VCToolsRedistDir) {
        Write-Warning 'VCToolsRedistDir is not set; the C++ runtime will not be bundled.'
        return
    }

    $desktopX64 = Join-Path $env:VCToolsRedistDir 'x64'
    $copied = 0
    foreach ($name in 'msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll', 'vcomp140.dll') {
        # Not the onecore\ variants: those target OneCore/Server, not desktop Windows.
        $src = Get-ChildItem -Path $desktopX64 -Recurse -Filter $name -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if (-not $src) {
            Write-Warning "$name not found under $desktopX64; a clean machine may not start the app."
            continue
        }
        $target = Join-Path $profileDir $src.Name
        if (-not (Test-Path $target) -or (Get-Item $target).LastWriteTimeUtc -lt $src.LastWriteTimeUtc) {
            Copy-Item $src.FullName $target -Force
            $copied++
        }
    }
    if ($copied -gt 0) { Write-Host "copied $copied C++ runtime dll(s) to $profileDir" }
}

# The DLLs have to sit beside the executable *before* the lathe crate is compiled, not
# merely before it is bundled: tauri.conf.json ships them as bundle resources, and
# tauri-build resolves that glob while the crate's build script runs. On a fresh clone
# nothing has put them there yet and the build dies with "glob pattern
# ../../target/release/*.dll path not found or didn't match any files". So build the
# crate that produces them first, stage them, then build the rest.
# tauri::generate_context! reads frontendDist while the lathe crate compiles, so the
# settings window has to be built before cargo runs at all -- not merely before
# bundling, which is the only time tauri.conf.json's beforeBuildCommand fires. A fresh
# clone has no ui/dist, and the failure surfaces as a panic inside a proc macro that
# never mentions npm.
if (-not (Get-Command npm -ErrorAction SilentlyContinue)) {
    throw 'npm not found; Node.js is required to build the settings window'
}

$uiDir = Join-Path $root 'ui'
if (-not (Test-Path (Join-Path $uiDir 'node_modules'))) {
    & npm --prefix $uiDir ci
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
& npm --prefix $uiDir run build
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$coreArgv = [System.Collections.Generic.List[string]]::new()
$coreArgv.Add('build')
$coreArgv.Add('-p')
$coreArgv.Add('lathe-core')
if (-not $Dev) { $coreArgv.Add('--release') }

& cargo $coreArgv
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Copy-CrispAsrRuntime -Quiet
Copy-MsvcRuntime

& cargo $cargoArgv
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Copy-CrispAsrRuntime
Copy-MsvcRuntime

if ($Bundle) {
    if ($Dev) { throw '-Bundle needs a release build; drop -Dev' }
    if (-not (Get-Command cargo-tauri -ErrorAction SilentlyContinue)) {
        throw 'cargo-tauri not found; install it with: cargo install tauri-cli --version "^2" --locked'
    }
    & cargo tauri bundle --bundles nsis --ci
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

exit $LASTEXITCODE

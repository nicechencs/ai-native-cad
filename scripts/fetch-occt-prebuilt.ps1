<#
.SYNOPSIS
    Idempotently fetch prebuilt OpenCASCADE (OCCT 7.9+) dynamic libraries for Windows (MSVC)
    from conda-forge into cpp/occt-prebuilt/, enforcing LGPL-2.1 dynamic linking boundaries.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/fetch-occt-prebuilt.ps1 -DryRun
    powershell -ExecutionPolicy Bypass -File scripts/fetch-occt-prebuilt.ps1 -OcctVersion 7.9
#>
[CmdletBinding()]
param(
    [Alias("dry-run")]
    [switch]$DryRun,

    [string]$OcctVersion = "7.9",

    [string]$TargetDir = "",

    [switch]$Force
)

$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($TargetDir)) {
    $TargetDir = Join-Path $RepoRoot "cpp\occt-prebuilt"
}

$IncludeDir = Join-Path $TargetDir "Library\include\opencascade"
$BinDir = Join-Path $TargetDir "Library\bin"
$LibDir = Join-Path $TargetDir "Library\lib"
$ManifestPath = Join-Path $TargetDir ".occt-prebuilt-manifest.json"
$VersionHeader = Join-Path $IncludeDir "Standard_Version.hxx"

function Find-CondaTool {
    foreach ($candidate in @("micromamba", "conda")) {
        $cmd = Get-Command $candidate -ErrorAction SilentlyContinue
        if ($null -ne $cmd) {
            return @{ Name = $candidate; Path = $cmd.Source }
        }
    }
    return $null
}

function Test-OcctInstalled {
    if (-not (Test-Path $VersionHeader)) {
        return $false
    }
    $dlls = Get-ChildItem -Path $BinDir -Filter "TKernel*.dll" -ErrorAction SilentlyContinue
    return ($null -ne $dlls -and $dlls.Count -gt 0)
}

Write-Host "=== AI-Native CAD: OCCT Prebuilt Fetcher (Windows MSVC) ==="
Write-Host "Repo Root   : $RepoRoot"
Write-Host "Target Dir  : $TargetDir"
Write-Host "Min Version : $OcctVersion (conda-forge)"
Write-Host "License Mode: LGPL-2.1 Dynamic Linking ONLY (DLLs + import libs; no static OCCT linking)"

$Tool = Find-CondaTool
$AlreadyInstalled = Test-OcctInstalled

if ($AlreadyInstalled -and -not $Force) {
    Write-Host "[OK] Prebuilt OCCT already present at: $TargetDir"
    Write-Host "     Headers : $IncludeDir"
    Write-Host "     DLLs    : $BinDir"
    Write-Host "     Libs    : $LibDir"
    Write-Host "     Pass -Force to re-fetch or upgrade."
    exit 0
}

if ($null -eq $Tool) {
    Write-Host "[WARN] Neither 'micromamba' nor 'conda' was found in PATH."
    Write-Host "       Install micromamba on Windows via:"
    Write-Host "         winget install Mamba.Micromamba"
    Write-Host "       Or Miniforge:"
    Write-Host "         winget install CondaForge.Miniforge3"
    $PlannedTool = "micromamba"
} else {
    Write-Host "[INFO] Detected package manager: $($Tool.Name) ($($Tool.Path))"
    $PlannedTool = $Tool.Name
}

$PkgSpec = "occt>=$OcctVersion"
$CreateArgs = @("create", "-y", "-p", $TargetDir, "-c", "conda-forge", $PkgSpec)

if ($DryRun) {
    Write-Host ""
    Write-Host "[DRY-RUN] Environment check complete. No files were downloaded or modified."
    Write-Host "[DRY-RUN] Planned command:"
    Write-Host "  $PlannedTool $($CreateArgs -join ' ')"
    Write-Host ""
    Write-Host "[DRY-RUN] Expected CMake / Rust cxx integration paths:"
    Write-Host "  -DCMAKE_PREFIX_PATH=`"$(Join-Path $TargetDir 'Library')`""
    Write-Host "  -DOpenCASCADE_DIR=`"$(Join-Path $TargetDir 'Library\cmake')`""
    Write-Host "  Runtime PATH must include: $BinDir"
    Write-Host "  Compliance: Ship unmodified OCCT *.dll side-by-side; do NOT statically link LGPL-2.1 OCCT."
    exit 0
}

if ($null -eq $Tool) {
    Write-Error "Cannot fetch OCCT because neither 'micromamba' nor 'conda' is available."
    exit 1
}

if ($Force -and (Test-Path $TargetDir)) {
    Write-Host "[INFO] -Force specified; cleaning existing $TargetDir ..."
    Remove-Item -Recurse -Force $TargetDir
}

$ParentDir = Split-Path -Parent $TargetDir
if (-not (Test-Path $ParentDir)) {
    New-Item -ItemType Directory -Path $ParentDir -Force | Out-Null
}

Write-Host "[RUN] $($Tool.Name) $($CreateArgs -join ' ')"
& $Tool.Path @CreateArgs
if ($LASTEXITCODE -ne 0) {
    Write-Error "$($Tool.Name) exited with code $LASTEXITCODE"
    exit $LASTEXITCODE
}

if (-not (Test-OcctInstalled)) {
    Write-Error "OCCT installation completed, but expected headers/DLLs were not found under $TargetDir\Library"
    exit 1
}

$Manifest = @{
    platform      = "win-64-msvc"
    package       = $PkgSpec
    installer     = $Tool.Name
    license       = "LGPL-2.1-only"
    linking       = "dynamic"
    include_dir   = $IncludeDir
    bin_dir       = $BinDir
    lib_dir       = $LibDir
    installed_utc = (Get-Date).ToUniversalTime().ToString("o")
} | ConvertTo-Json -Depth 3

Set-Content -Path $ManifestPath -Value $Manifest -Encoding UTF8

Write-Host ""
Write-Host "[OK] OCCT prebuilt installed successfully."
Write-Host "     Add to PATH before running occt-ffi binaries: $BinDir"
Write-Host "     Configure CMake: cmake -S cpp/occt-bridge -B cpp/occt-bridge/build -DOpenCASCADE_DIR=`"$(Join-Path $TargetDir 'Library\cmake')`""

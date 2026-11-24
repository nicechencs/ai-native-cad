# Bootstrap OCCT 7.9+ runtime DLLs, then exec target/occt-ffi/(release|debug)/cad-cli.exe.
# Every argument is forwarded unchanged. A missing prebuilt OCCT, bridge, or VC++
# runtime is a hard error: this script never launches a non-OCCT cad-cli.
#
# Hermetic checks (not cad-cli arguments):
#   AICAD_OCCT_BOOTSTRAP_ROOT    checkout to probe instead of this repo
#   AICAD_OCCT_BOOTSTRAP_DRY_RUN 1 = print the resolved argv and library dirs, do not exec

$ErrorActionPreference = "Stop"
if (Test-Path variable:PSNativeCommandUseErrorActionPreference) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$CadCliArgs = @($args)
$BootstrapOverride = -not [string]::IsNullOrWhiteSpace($env:AICAD_OCCT_BOOTSTRAP_ROOT)
if ($BootstrapOverride) {
    $RepoRoot = [System.IO.Path]::GetFullPath($env:AICAD_OCCT_BOOTSTRAP_ROOT)
} else {
    $RepoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
}

$ToolkitStems = @(
    "TKernel", "TKMath", "TKBRep", "TKGeomBase", "TKG2d", "TKG3d", "TKGeomAlgo", "TKTopAlgo",
    "TKPrim", "TKBO", "TKFillet", "TKOffset", "TKShHealing", "TKMesh", "TKHLR",
    "TKCDF", "TKLCAF", "TKXCAF", "TKXSBase", "TKDESTEP", "TKDEIGES"
)
$CrtLibraries = @("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll")
$BridgeConfigs = @("Release", "RelWithDebInfo", "MinSizeRel", "Debug")

$Missing = New-Object System.Collections.Generic.List[string]
$Fixes = New-Object System.Collections.Generic.List[string]
$LibraryDirs = New-Object System.Collections.Generic.List[string]

function Test-DynamicLibraryFile {
    param([string]$LiteralPath)
    if ([string]::IsNullOrWhiteSpace($LiteralPath)) {
        return $false
    }
    if (-not (Test-Path -LiteralPath $LiteralPath -PathType Leaf -ErrorAction SilentlyContinue)) {
        return $false
    }
    $stream = $null
    try {
        $stream = [System.IO.File]::Open(
            $LiteralPath,
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Read,
            [System.IO.FileShare]::ReadWrite)
        $buffer = New-Object byte[] 4
        $read = $stream.Read($buffer, 0, 4)
        return ($read -ge 2 -and $buffer[0] -eq 0x4D -and $buffer[1] -eq 0x5A)
    } catch {
        return $false
    } finally {
        if ($null -ne $stream) {
            $stream.Dispose()
        }
    }
}

function Test-NamedLibrary {
    param(
        [string]$Directory,
        [string]$Canonical,
        [bool]$AllowVersioned
    )
    if ([string]::IsNullOrWhiteSpace($Directory)) {
        return $false
    }
    if (-not (Test-Path -LiteralPath $Directory -PathType Container -ErrorAction SilentlyContinue)) {
        return $false
    }
    $exact = Join-Path $Directory $Canonical
    if (Test-DynamicLibraryFile -LiteralPath $exact) {
        return $true
    }
    if (-not $AllowVersioned) {
        return $false
    }
    $stem = [System.IO.Path]::GetFileNameWithoutExtension($Canonical)
    try {
        foreach ($file in [System.IO.Directory]::EnumerateFiles($Directory)) {
            $name = [System.IO.Path]::GetFileName($file)
            $dot = $stem + "."
            $dash = $stem + "-"
            $matched = $name.StartsWith($dot, [StringComparison]::OrdinalIgnoreCase) -or
                $name.StartsWith($dash, [StringComparison]::OrdinalIgnoreCase)
            if ($matched -and (Test-DynamicLibraryFile -LiteralPath $file)) {
                return $true
            }
        }
    } catch {
        return $false
    }
    return $false
}

function Add-UniqueText {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[string]]$List,
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Text
    )
    if ([string]::IsNullOrWhiteSpace($Text)) {
        return
    }
    foreach ($existing in $List) {
        if ($existing -eq $Text) {
            return
        }
    }
    [void]$List.Add($Text)
}

function Add-LibraryDir {
    param([string]$Dir)
    if ([string]::IsNullOrWhiteSpace($Dir)) {
        return
    }
    if (-not (Test-Path -LiteralPath $Dir -PathType Container -ErrorAction SilentlyContinue)) {
        return
    }
    $full = [System.IO.Path]::GetFullPath($Dir)
    foreach ($existing in $LibraryDirs) {
        if ($existing.Equals($full, [StringComparison]::OrdinalIgnoreCase)) {
            return
        }
    }
    [void]$LibraryDirs.Add($full)
}

function Format-QuotedPath {
    param([string]$Path)
    return '"' + $Path + '"'
}

if (-not $BootstrapOverride -and -not [string]::IsNullOrWhiteSpace($env:OCCT_BRIDGE_LIB_DIR)) {
    $customBridge = $env:OCCT_BRIDGE_LIB_DIR
    if (-not [System.IO.Path]::IsPathRooted($customBridge)) {
        $customBridge = Join-Path $RepoRoot $customBridge
    }
    Add-LibraryDir $customBridge
}
foreach ($config in $BridgeConfigs) {
    Add-LibraryDir (Join-Path $RepoRoot (Join-Path "build\occt-bridge" $config))
}
Add-LibraryDir (Join-Path $RepoRoot "build\occt-bridge")
$PrebuiltBin = Join-Path $RepoRoot "cpp\occt-prebuilt\Library\bin"
Add-LibraryDir $PrebuiltBin
# Conda-forge packages on Windows place transitive runtime DLLs (e.g. zlib.dll for FreeImage) in the prefix root.
Add-LibraryDir (Join-Path $RepoRoot "cpp\occt-prebuilt")

$bridgeFound = $false
foreach ($dir in $LibraryDirs) {
    if (Test-NamedLibrary -Directory $dir -Canonical "occt_bridge.dll" -AllowVersioned $true) {
        $bridgeFound = $true
        break
    }
}
if (-not $bridgeFound) {
    Add-UniqueText $Missing "occt_bridge.dll"
    $bridgeSrc = Format-QuotedPath (Join-Path $RepoRoot "cpp\occt-bridge")
    $bridgeBuild = Format-QuotedPath (Join-Path $RepoRoot "build\occt-bridge")
    Add-UniqueText $Fixes "cmake -S $bridgeSrc -B $bridgeBuild; cmake --build $bridgeBuild --config Release"
}

$occtMissing = $false
foreach ($stem in $ToolkitStems) {
    $dll = "$stem.dll"
    if (-not (Test-NamedLibrary -Directory $PrebuiltBin -Canonical $dll -AllowVersioned $true)) {
        $occtMissing = $true
        Add-UniqueText $Missing $dll
    }
}

$versionHeaders = @(
    (Join-Path $RepoRoot "cpp\occt-prebuilt\Library\include\opencascade\Standard_Version.hxx"),
    (Join-Path $RepoRoot "cpp\occt-prebuilt\include\opencascade\Standard_Version.hxx")
)
foreach ($header in $versionHeaders) {
    if (-not (Test-Path -LiteralPath $header -PathType Leaf -ErrorAction SilentlyContinue)) {
        continue
    }
    $versionText = [System.IO.File]::ReadAllText($header)
    $majorMatch = [regex]::Match($versionText, '(?m)^\s*#define\s+OCC_VERSION_MAJOR\s+(\d+)')
    $minorMatch = [regex]::Match($versionText, '(?m)^\s*#define\s+OCC_VERSION_MINOR\s+(\d+)')
    if (-not $majorMatch.Success -or -not $minorMatch.Success) {
        continue
    }
    $major = [int]$majorMatch.Groups[1].Value
    $minor = [int]$minorMatch.Groups[1].Value
    if ($major -lt 7 -or ($major -eq 7 -and $minor -lt 9)) {
        $occtMissing = $true
        Add-UniqueText $Missing "OCCT>=7.9 (found $major.$minor in Standard_Version.hxx)"
    }
    break
}
if ($occtMissing) {
    $fetch = Format-QuotedPath (Join-Path $RepoRoot "scripts\fetch-occt-prebuilt.ps1")
    Add-UniqueText $Fixes "powershell -ExecutionPolicy Bypass -File $fetch"
}

$crtDirs = New-Object System.Collections.Generic.List[string]
if (-not [string]::IsNullOrWhiteSpace($env:SystemRoot)) {
    Add-UniqueText $crtDirs (Join-Path $env:SystemRoot "System32")
} elseif (-not [string]::IsNullOrWhiteSpace($env:WINDIR)) {
    Add-UniqueText $crtDirs (Join-Path $env:WINDIR "System32")
}
if (-not [string]::IsNullOrWhiteSpace($env:PATH)) {
    foreach ($entry in ($env:PATH -split ';')) {
        if (-not [string]::IsNullOrWhiteSpace($entry)) {
            Add-UniqueText $crtDirs $entry.Trim()
        }
    }
}
foreach ($dir in $LibraryDirs) {
    Add-UniqueText $crtDirs $dir
}
$crtMissing = $false
foreach ($crt in $CrtLibraries) {
    $found = $false
    foreach ($dir in $crtDirs) {
        if (Test-NamedLibrary -Directory $dir -Canonical $crt -AllowVersioned $false) {
            $found = $true
            break
        }
    }
    if (-not $found) {
        $crtMissing = $true
        Add-UniqueText $Missing $crt
    }
}
if ($crtMissing) {
    Add-UniqueText $Fixes "winget install --id Microsoft.VCRedist.2015+.x64 -e"
}

$cadCli = $null
$cadCandidates = @(
    (Join-Path $RepoRoot "target\occt-ffi\release\cad-cli.exe"),
    (Join-Path $RepoRoot "target\occt-ffi\debug\cad-cli.exe")
)
if (-not $BootstrapOverride -and -not [string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $cadCandidates = @(
        (Join-Path $RepoRoot "target\occt-ffi\release\cad-cli.exe"),
        (Join-Path $env:CARGO_TARGET_DIR "occt-ffi\release\cad-cli.exe"),
        (Join-Path $RepoRoot "target\occt-ffi\debug\cad-cli.exe"),
        (Join-Path $env:CARGO_TARGET_DIR "occt-ffi\debug\cad-cli.exe")
    )
}
foreach ($candidate in $cadCandidates) {
    if (Test-Path -LiteralPath $candidate -PathType Leaf -ErrorAction SilentlyContinue) {
        $cadCli = [System.IO.Path]::GetFullPath($candidate)
        break
    }
}
if ($null -eq $cadCli) {
    Add-UniqueText $Missing "target/occt-ffi/release/cad-cli.exe"
    Add-UniqueText $Missing "target/occt-ffi/debug/cad-cli.exe"
    $targetDir = Format-QuotedPath (Join-Path $RepoRoot "target\occt-ffi")
    Add-UniqueText $Fixes "cargo build -p cad-cli --features occt-ffi --target-dir $targetDir --locked"
}

if ($Missing.Count -gt 0) {
    [Console]::Error.WriteLine("Error: OCCT runtime is incomplete. Refusing to launch a non-authoritative cad-cli.")
    foreach ($item in $Missing) {
        [Console]::Error.WriteLine("Missing: $item")
    }
    foreach ($fix in $Fixes) {
        [Console]::Error.WriteLine("Fix: $fix")
    }
    exit 1
}

$prefix = [string]::Join(";", $LibraryDirs)
if (-not [string]::IsNullOrWhiteSpace($prefix)) {
    if ([string]::IsNullOrWhiteSpace($env:PATH)) {
        $env:PATH = $prefix
    } else {
        $env:PATH = "$prefix;$($env:PATH)"
    }
}

if ($env:AICAD_OCCT_BOOTSTRAP_DRY_RUN -eq "1") {
    Write-Output "executable=$cadCli"
    Write-Output "argv_count=$($CadCliArgs.Count)"
    foreach ($arg in $CadCliArgs) {
        Write-Output "argv=$arg"
    }
    foreach ($dir in $LibraryDirs) {
        Write-Output "library_dir=$dir"
    }
    exit 0
}

& $cadCli @CadCliArgs
if ($null -eq $LASTEXITCODE) {
    exit 0
}
if ($LASTEXITCODE -eq -1073741515 -or $LASTEXITCODE -eq 0xC0000135) {
    [Console]::Error.WriteLine(
        "Error: cad-cli exited with STATUS_DLL_NOT_FOUND (-1073741515 / 0xC0000135). " +
        "A transitive OCCT or system DLL failed to load from: $prefix"
    )
    $fetch = Format-QuotedPath (Join-Path $RepoRoot "scripts\fetch-occt-prebuilt.ps1")
    [Console]::Error.WriteLine("Fix: powershell -ExecutionPolicy Bypass -File $fetch -Force")
}
exit $LASTEXITCODE

[CmdletBinding()]
param(
    [string]$OutDir = "dist/release",
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [switch]$SkipFrontend,
    [switch]$SkipBuild,
    [switch]$CliOnly,
    [switch]$WithOcct,
    [switch]$WithoutOcct,
    [switch]$NoZip,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
$Version = (Get-Content -Raw -LiteralPath (Join-Path $RepoRoot "package.json") | ConvertFrom-Json).version
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)*$') {
    throw "Invalid package version: $Version"
}
if ($WithOcct -and $WithoutOcct) {
    throw "Pass only one of -WithOcct or -WithoutOcct."
}
# Release bundles include the OCCT bridge and runtime DLLs unless -WithoutOcct is set.
if (-not $WithoutOcct) {
    $WithOcct = $true
}

function Invoke-BuildStep {
    param([string]$Tool, [string[]]$Arguments)

    Write-Host "[BUILD] $Tool $($Arguments -join ' ')"
    & $Tool @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Tool exited with code $LASTEXITCODE."
    }
}

# Windows PowerShell 5.1 can load the PowerShell 7 Utility module when PSModulePath
# lists it first (pnpm and Python do this). That module does not provide Get-FileHash.
function Get-PackageSha256 {
    param([string]$LiteralPath)

    $Algorithm = [System.Security.Cryptography.SHA256]::Create()
    $Stream = [System.IO.File]::Open(
        $LiteralPath,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        return ([System.BitConverter]::ToString($Algorithm.ComputeHash($Stream))).Replace("-", "").ToLowerInvariant()
    } finally {
        $Stream.Dispose()
        $Algorithm.Dispose()
    }
}

$OutputRoot = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
$BundleName = "ai-native-cad-v$Version-windows-x64"
$BundlePath = Join-Path $OutputRoot $BundleName
$ArchivePath = $null
if (-not $NoZip) {
    $ArchivePath = Join-Path $OutputRoot "$BundleName.zip"
}

$FrontendIndex = "apps/studio-ui/dist/index.html"
$FrontendArgs = @("--filter", "@ai-native-cad/studio-ui", "build")
$CliArgs = @("build", "-p", "cad-cli")
$DesktopArgs = @("build", "--manifest-path", "apps/desktop/src-tauri/Cargo.toml", "--features", "custom-protocol")
if ($Profile -eq "release") {
    $CliArgs += "--release"
    $DesktopArgs += "--release"
}
if ($WithOcct) {
    $CliArgs += @("--features", "occt-ffi")
    $DesktopArgs += @("--features", "occt-ffi")
}

$BinarySources = @("target/$Profile/cad-cli.exe")
if (-not $CliOnly) {
    $BinarySources += "apps/desktop/src-tauri/target/$Profile/ai-native-cad-desktop.exe"
}

if ($DryRun) {
    $Steps = @()
    if (-not $SkipBuild) {
        if (-not $CliOnly -and -not $SkipFrontend) {
            $Steps += "pnpm $($FrontendArgs -join ' ')"
            $Steps += "Verify $FrontendIndex"
        }
        $Steps += "cargo $($CliArgs -join ' ')"
        if (-not $CliOnly) {
            $Steps += "cargo $($DesktopArgs -join ' ')"
        }
    }
    $Steps += "Stage $($BinarySources -join ', ') in $BundlePath"
    if ($WithOcct) {
        $Steps += "Stage available occt_bridge.dll, cpp/occt-prebuilt/Library/bin/*.dll, and cpp/occt-prebuilt/*.dll"
    }
    $Steps += "Write start-web-workbench.cmd, README-PORTABLE.txt, BUILD-MANIFEST.json and SHA256SUMS.txt"
    if ($ArchivePath) {
        $Steps += "Compress-Archive -Force to $ArchivePath"
    }
    foreach ($Step in $Steps) {
        Write-Host "[DRY-RUN] $Step"
    }
    [ordered]@{
        version = $Version
        profile = $Profile
        dryRun = $true
        bundle = $BundlePath
        archive = $ArchivePath
        binaries = @($BinarySources | ForEach-Object { Split-Path -Leaf $_ })
        steps = $Steps
    } | ConvertTo-Json -Depth 4
    return
}

Push-Location -LiteralPath $RepoRoot
try {
    if ($WithOcct -and -not $SkipBuild -and -not $env:OCCT_BRIDGE_LIB_DIR) {
        $BridgeLibCandidates = @(
            "build/occt-bridge/Release",
            "build/occt-bridge/RelWithDebInfo",
            "build/occt-bridge/MinSizeRel",
            "build/occt-bridge/Debug",
            "build/occt-bridge/$Profile",
            "build/occt-bridge",
            "cpp/occt-bridge/build/Release",
            "cpp/occt-bridge/build/RelWithDebInfo",
            "cpp/occt-bridge/build/MinSizeRel",
            "cpp/occt-bridge/build/Debug",
            "cpp/occt-bridge/build/$Profile",
            "cpp/occt-bridge/build"
        )
        foreach ($Directory in $BridgeLibCandidates) {
            if (Test-Path -LiteralPath (Join-Path $Directory "occt_bridge.lib") -PathType Leaf) {
                $env:OCCT_BRIDGE_LIB_DIR = (Resolve-Path -LiteralPath $Directory).Path
                Write-Host "[OCCT] OCCT_BRIDGE_LIB_DIR=$env:OCCT_BRIDGE_LIB_DIR"
                break
            }
        }
        if (-not $env:OCCT_BRIDGE_LIB_DIR) {
            throw "OCCT bridge library not found. Build cpp/occt-bridge or set OCCT_BRIDGE_LIB_DIR."
        }
    }

    if (-not $SkipBuild) {
        if (-not $CliOnly -and -not $SkipFrontend) {
            Invoke-BuildStep -Tool pnpm -Arguments $FrontendArgs
            if (-not (Test-Path -LiteralPath $FrontendIndex -PathType Leaf)) {
                throw "Frontend build did not produce $FrontendIndex."
            }
        }
        Invoke-BuildStep -Tool cargo -Arguments $CliArgs
        if (-not $CliOnly) {
            Invoke-BuildStep -Tool cargo -Arguments $DesktopArgs
        }
    }

    foreach ($Source in $BinarySources) {
        if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
            throw "Missing binary: $Source. Build the selected profile before using -SkipBuild."
        }
    }

    if ($WithOcct) {
        $OcctPrefixDir = "cpp/occt-prebuilt"
        $OcctBinDir = "cpp/occt-prebuilt/Library/bin"
        $BridgeDirs = @(
            $env:OCCT_BRIDGE_LIB_DIR
            "build/occt-bridge/$Profile"
            "build/occt-bridge/Release"
            "build/occt-bridge/RelWithDebInfo"
            "build/occt-bridge/MinSizeRel"
            "build/occt-bridge/Debug"
            "build/occt-bridge"
            "target/$Profile"
            "apps/desktop/src-tauri/target/$Profile"
            "cpp/occt-bridge/build/$Profile"
            "cpp/occt-bridge/build/Release"
            "cpp/occt-bridge/build/RelWithDebInfo"
            "cpp/occt-bridge/build/MinSizeRel"
            "cpp/occt-bridge/build/Debug"
            "cpp/occt-bridge/build"
            $OcctBinDir
        )
        $StagedNames = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
        foreach ($ExistingSource in $BinarySources) {
            [void]$StagedNames.Add((Split-Path -Leaf $ExistingSource))
        }
        foreach ($Directory in $BridgeDirs) {
            if ($Directory) {
                $Bridge = Join-Path $Directory "occt_bridge.dll"
                if (Test-Path -LiteralPath $Bridge -PathType Leaf) {
                    $BinarySources += $Bridge
                    [void]$StagedNames.Add("occt_bridge.dll")
                    break
                }
            }
        }
        # Conda-forge packages on Windows place OCCT DLLs in Library/bin and transitive DLLs (e.g. zlib.dll) in the prefix root.
        foreach ($RuntimeDir in @($OcctBinDir, $OcctPrefixDir)) {
            if (Test-Path -LiteralPath $RuntimeDir -PathType Container) {
                $RuntimeDlls = @(Get-ChildItem -LiteralPath $RuntimeDir -Filter "*.dll" -File |
                    Sort-Object -Property Name)
                foreach ($Dll in $RuntimeDlls) {
                    if ($Dll.Name -ieq "occt_bridge.dll") {
                        continue
                    }
                    if ($StagedNames.Add($Dll.Name)) {
                        $BinarySources += $Dll.FullName
                    }
                }
            }
        }
        $BridgeStaged = $false
        foreach ($Source in $BinarySources) {
            if ((Split-Path -Leaf $Source) -eq "occt_bridge.dll") {
                $BridgeStaged = $true
                break
            }
        }
        if (-not $BridgeStaged) {
            throw "occt_bridge.dll was not found. Build the OCCT bridge or set OCCT_BRIDGE_LIB_DIR."
        }
        $RuntimeDllsStaged = @($BinarySources | Where-Object {
            $Name = Split-Path -Leaf $_
            $Name -like "*.dll" -and $Name -ne "occt_bridge.dll"
        })
        if ($RuntimeDllsStaged.Count -lt 1) {
            throw "No OCCT runtime DLLs found under cpp/occt-prebuilt."
        }
    }

    [System.IO.Directory]::CreateDirectory($OutputRoot) | Out-Null
    $WorkDir = Join-Path $OutputRoot ".package-$([guid]::NewGuid())"
    $StageDir = Join-Path $WorkDir $BundleName
    [System.IO.Directory]::CreateDirectory($StageDir) | Out-Null
    try {
        $Binaries = @(foreach ($Source in $BinarySources) {
            $Name = Split-Path -Leaf $Source
            $Destination = Join-Path $StageDir $Name
            Copy-Item -LiteralPath $Source -Destination $Destination
            [ordered]@{
                name = $Name
                sizeBytes = (Get-Item -LiteralPath $Destination).Length
                sha256 = Get-PackageSha256 -LiteralPath $Destination
            }
        })

        $PortableReadme = @("AI-Native CAD $Version portable bundle", "")
        if (-not $CliOnly) {
            $PortableReadme += "Open ai-native-cad-desktop.exe to start the desktop workbench."
        }
        $PortableReadme += @(
            "Run cad-cli.exe --help to list CLI commands."
            "Run cad-cli.exe daemon to start the CLI daemon."
            "For the web workbench, run start-web-workbench.cmd from a source checkout."
            "Use SHA256SUMS.txt to verify the packaged binaries."
        )
        $Launcher = @'
@echo off
if exist "apps\studio-ui\dev-server.mjs" (
    node apps/studio-ui/dev-server.mjs
    exit /b
)
echo From the AI-Native CAD source checkout, install dependencies with pnpm install.
echo Build the frontend with pnpm build, then run node apps/studio-ui/dev-server.mjs.
pause
'@
        $Manifest = [ordered]@{
            version = $Version
            profile = $Profile
            timestamp = [DateTime]::UtcNow.ToString("o")
            binaries = $Binaries
        } | ConvertTo-Json -Depth 4
        [System.IO.File]::WriteAllText((Join-Path $StageDir "start-web-workbench.cmd"), ($Launcher -replace '\r?\n', "`r`n") + "`r`n")
        [System.IO.File]::WriteAllLines((Join-Path $StageDir "README-PORTABLE.txt"), [string[]]$PortableReadme)
        [System.IO.File]::WriteAllText((Join-Path $StageDir "BUILD-MANIFEST.json"), $Manifest + "`n")
        $Checksums = @($Binaries | ForEach-Object { "$($_.sha256)  $($_.name)" })
        [System.IO.File]::WriteAllLines((Join-Path $StageDir "SHA256SUMS.txt"), [string[]]$Checksums)

        if ($ArchivePath) {
            if (Test-Path -LiteralPath $ArchivePath) {
                $ExistingArchive = Get-Item -LiteralPath $ArchivePath -Force
                if ($ExistingArchive.PSIsContainer -or
                    ($ExistingArchive.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
                    throw "Cannot replace archive at $ArchivePath."
                }
            }
            $StagedArchive = Join-Path $WorkDir "$BundleName.zip"
            Compress-Archive -LiteralPath $StageDir -DestinationPath $StagedArchive -Force
        }

        $PreviousBundle = Join-Path $WorkDir "previous-bundle"
        if (Test-Path -LiteralPath $BundlePath) {
            $Existing = Get-Item -LiteralPath $BundlePath -Force
            if ($Existing.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
                throw "Cannot replace a bundle at a reparse point: $BundlePath"
            }
            [System.IO.Directory]::Move($BundlePath, $PreviousBundle)
        }
        $PublishedBundle = $false
        try {
            [System.IO.Directory]::Move($StageDir, $BundlePath)
            $PublishedBundle = $true
            if ($ArchivePath) {
                Move-Item -LiteralPath $StagedArchive -Destination $ArchivePath -Force
            }
        } catch {
            if ($PublishedBundle) {
                [System.IO.Directory]::Move($BundlePath, $StageDir)
            }
            if (Test-Path -LiteralPath $PreviousBundle -PathType Container) {
                [System.IO.Directory]::Move($PreviousBundle, $BundlePath)
            }
            throw
        }
    } finally {
        # The cleanup target is the unique, direct child created under the selected output directory.
        if ([System.IO.Directory]::GetParent($WorkDir).FullName.TrimEnd('\', '/') -eq $OutputRoot.TrimEnd('\', '/') -and
            (Split-Path -Leaf $WorkDir) -match '^\.package-[0-9a-f-]{36}$') {
            Remove-Item -LiteralPath $WorkDir -Recurse -Force
        }
    }

    Write-Host "Bundle: $BundlePath"
    if ($ArchivePath) {
        Write-Host "Archive: $ArchivePath"
    }
} finally {
    Pop-Location
}

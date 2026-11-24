# Getting Started & Build Guide

**English** | [简体中文](../zh/getting-started.md) | [Docs Index](../README.md)

This guide covers cloning the repository, building `cad-cli`, optionally enabling the authoritative OpenCASCADE (OCCT) B-Rep kernel, launching the Web and Tauri desktop workbenches, and creating portable release bundles.

---

## 1. Prerequisites

| Tool | Minimum Version | Purpose |
| :--- | :--- | :--- |
| **Rust Toolchain** | Rust 2024 Edition (`cargo` / `rustc`) | Builds all 26 Rust crates, `cad-cli`, and the Tauri desktop host |
| **Node.js & pnpm** | Node.js `>= 22`, `pnpm >= 9` (do not use `npm`) | Builds and serves the React 19 Studio Workbench |
| **Python** | Python `>= 3.12` | Runs the `aicad` Python SDK, built-in `cad-agent`, and codegen helpers |
| **CMake + C++20 Compiler + OCCT 7.9+** (Optional) | CMake 3.24+, MSVC / Clang / GCC | Enables `--features occt-ffi` authoritative B-Rep, STEP/IGES/BREP exchange, and exact HLR |

---

## 2. Track A: Zero-C++ Pure Rust Quick Start

You can build and run the compiler, runtime, container CLI, and mesh importers without installing CMake or OCCT (`AnalyticKernel` + `cad-mesh-io`):

```sh
# 1. Build the entire Rust workspace
cargo build --workspace --locked

# 2. Check a golden mounting plate (syntax, units, watertightness, bounding box)
cargo run -p cad-cli -- check examples/mvp-tier1-plates/01_mounting_plate.kcl --json

# 3. Export STEP, SceneMesh JSON, 1:1 millimetre DXF, and 2D engineering drawing SVG
cargo run -p cad-cli -- build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --out-step tmp_out/bearing_asm.step \
  --out-mesh tmp_out/bearing_asm.mesh.json \
  --out-dxf tmp_out/bearing_asm.dxf \
  --out-drawing tmp_out/bearing_asm.drawing.svg \
  --json

# 4. Export a single component from an assembly
cargo run -p cad-cli -- build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --component comp_0_HousingBase \
  --out-step tmp_out/housing_base.step \
  --json

# 5. Inspect and compile external 3MF / STL mesh fixtures
cargo run -p cad-cli -- inspect examples/mvp-tier4-import/assets/custom_rel.3mf --json
cargo run -p cad-cli -- build examples/mvp-tier4-import/custom_rel.kcl \
  --workspace-root examples/mvp-tier4-import \
  --out-mesh tmp_out/custom_rel.mesh.json \
  --json

# 6. Inspect a V2 .aicad example project and project it to .kcl 2.0
cargo run -p cad-cli -- project inspect examples/aicad-projects/01_mounting_plate.aicad --json
cargo run -p cad-cli -- project emit-kcl examples/aicad-projects/01_mounting_plate.aicad
```

---

## 3. Track B: Authoritative OCCT B-Rep Kernel (`--features occt-ffi`)

Authoritative STEP AP242 / IGES / BREP import and export, exact fillet/chamfer/shell, exact B-Rep interference checks, and L4 STEP round-trip verification require building `cpp/occt-bridge` against OpenCASCADE 7.9+ shared libraries.

### Windows (PowerShell + Visual Studio Developer Environment + `micromamba`/`conda`)

```powershell
# 1. Fetch prebuilt OCCT shared libraries (preview first, then download)
./scripts/fetch-occt-prebuilt.ps1 -DryRun
./scripts/fetch-occt-prebuilt.ps1

# 2. Build the C++20 OCCT C-ABI bridge
cmake -S cpp/occt-bridge -B build/occt-bridge
cmake --build build/occt-bridge --config Release

# 3. Build native cad-cli into a dedicated target dir (keeps default pure-Rust target intact)
$env:OCCT_BRIDGE_LIB_DIR = (Resolve-Path build/occt-bridge/Release).Path
$env:PATH = "$env:OCCT_BRIDGE_LIB_DIR;$(Resolve-Path cpp/occt-prebuilt/Library/bin);$(Resolve-Path cpp/occt-prebuilt);$env:PATH"
cargo build -p cad-cli --features occt-ffi --target-dir target/occt-ffi --locked

# 4. Run via the bootstrap launcher (configures DLL search path & diagnoses missing deps)
./scripts/run-cad-cli-occt.ps1 -- doctor-agent --json
./scripts/run-cad-cli-occt.ps1 -- inspect examples/mvp-tier4-import/assets/nema17_mm.step --json
```

### Linux / macOS

```sh
./scripts/fetch-occt-prebuilt.sh --dry-run
./scripts/fetch-occt-prebuilt.sh
cmake -S cpp/occt-bridge -B build/occt-bridge -DCMAKE_BUILD_TYPE=Release
cmake --build build/occt-bridge --parallel

export OCCT_BRIDGE_LIB_DIR="$PWD/build/occt-bridge"
cargo build -p cad-cli --features occt-ffi --target-dir target/occt-ffi --locked

./scripts/run-cad-cli-occt.sh -- doctor-agent --json
./scripts/run-cad-cli-occt.sh -- inspect examples/mvp-tier4-import/assets/nema17_mm.step --json
```

> **Troubleshooting**: On Windows, exit code `-1073741515` (`0xC0000135`) means `occt_bridge.dll` or a transitive OCCT runtime DLL (including prefix-root DLLs such as `zlib.dll` under `cpp/occt-prebuilt/`) is missing from `PATH`. Always launch through `./scripts/run-cad-cli-occt.ps1` / `.sh`.

---

## 4. Launch the Studio Workbench (Web & Tauri Desktop)

### Web Workbench (Connected to Local `cad-cli`)

```sh
pnpm install
cargo build -p cad-cli --locked
pnpm build
pnpm dev:server
```

Open `http://127.0.0.1:5173`. To back the web server with the OCCT-enabled CLI, set `CAD_CLI_BIN` to `target/occt-ffi/debug/cad-cli(.exe)` with OCCT shared libraries on the loader path.

Hot-reload frontend development (two terminals):

```powershell
# PowerShell Terminal 1: API server
$env:PORT = '4173'; pnpm dev:server

# PowerShell Terminal 2: Vite frontend with /api proxy
$env:CAD_STUDIO_API_PROXY = 'http://127.0.0.1:4173'; pnpm dev
```

```sh
# macOS / Linux
PORT=4173 pnpm dev:server
CAD_STUDIO_API_PROXY=http://127.0.0.1:4173 pnpm dev
```

### Tauri v2 Desktop Workbench

The desktop app links `cad-runtime` in-process over native IPC (including STEP/3MF/DXF/SVG exports and the Live-Attach IPC host) without running the Node server:

```sh
# Build pure-Rust desktop binary
pnpm build:desktop

# Or create a portable release bundle (includes OCCT by default; pass -WithoutOcct / --without-occt for pure Rust)
./scripts/package-release.ps1 -DryRun
./scripts/package-release.ps1 -WithoutOcct
```

On Linux and macOS, use `./scripts/package-release.sh` (`--dry-run`, `--cli-only`, `--without-occt`).

### Automated GitHub Actions Release Packaging (Windows / Linux / macOS)

The repository includes `.github/workflows/release.yml` for automated cross-platform portable bundles (Tauri desktop app, `cad-cli`, optional authoritative OCCT 7.9+ shared libraries, `services/cad-agent`, and `SHA256SUMS.txt`):
- **Tag-driven Releases**: Pushing a `v*` tag (for example `git tag v0.1.0 && git push origin v0.1.0`) builds Windows x64 (`.zip`), Linux x64 (`.tar.gz`), and macOS arm64 (`.tar.gz`) bundles, smoke-verifies each packaged `cad-cli` in an isolated library path, and publishes a GitHub Release with combined `SHA256SUMS.txt`.
- **Manual Workflow Dispatch**: Run **Release Portable Bundles** from the GitHub Actions UI with configurable `with_occt`, `cli_only`, and `publish_release` (draft release) flags; artifacts are retained for 30 days.

---

## 5. Install the Python SDK & Run Examples

```sh
# Install the official Python SDK (aicad)
python -m pip install -e packages/python-sdk

# Run the three end-to-end SDK scripts (requires a built cad-cli)
python -X utf8 examples/developer-platform/01_inspect_and_measure.py
python -X utf8 examples/developer-platform/02_parametric_commit_and_verify.py
python -X utf8 examples/developer-platform/03_branch_preview_and_promote.py
```

Continue with the [Studio Workbench Guide](workbench-guide.md) and the [AI Ecosystem, MCP & Python SDK Guide](agent-and-mcp-guide.md).

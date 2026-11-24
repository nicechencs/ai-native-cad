# 快速开始与构建指南

[English](../en/getting-started.md) | **简体中文** | [文档索引](../README.md)

本指南覆盖从零克隆仓库到运行 `cad-cli`、启用原生 OpenCASCADE (OCCT) 精确内核、启动 Web / Tauri 桌面工作台，以及构建便携发布包的完整流程。

---

## 1. 环境准备

| 工具 | 最低版本 | 用途 |
| :--- | :--- | :--- |
| **Rust 工具链** | 支持 Rust 2024 Edition（`cargo` / `rustc`） | 编译 26 个 Rust 核心 crate、CLI 与 Tauri 桌面后端 |
| **Node.js & pnpm** | Node.js `>= 22`、`pnpm >= 9`（勿用 `npm`） | 编译和启动 React 19 Studio 工作台 |
| **Python** | Python `>= 3.12` | 运行 `aicad` Python SDK 示例、内置 `cad-agent` 与代码生成脚本 |
| **CMake + C++20 编译器 + OCCT 7.9+**（可选） | CMake 3.24+、MSVC / Clang / GCC | 启用 `--features occt-ffi` 权威 B-Rep、STEP/IGES/BREP 导入导出与精确 HLR |

---

## 2. 路径 A：纯 Rust 零 C++ 快速体验

无需安装 CMake 或 OCCT，直接在仓库根目录构建默认纯 Rust 工具链（`AnalyticKernel` + `cad-mesh-io`）：

```sh
# 1. 构建全工作区
cargo build --workspace --locked

# 2. 检查示例安装板（验证语法、量纲、水密性与包围盒）
cargo run -p cad-cli -- check examples/mvp-tier1-plates/01_mounting_plate.kcl --json

# 3. 导出 STEP、SceneMesh JSON、毫米坐标 DXF 与 2D 图纸 SVG
cargo run -p cad-cli -- build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --out-step tmp_out/bearing_asm.step \
  --out-mesh tmp_out/bearing_asm.mesh.json \
  --out-dxf tmp_out/bearing_asm.dxf \
  --out-drawing tmp_out/bearing_asm.drawing.svg \
  --json

# 4. 按子件拆件导出装配体
cargo run -p cad-cli -- build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --component comp_0_HousingBase \
  --out-step tmp_out/housing_base.step \
  --json

# 5. 检查外部 3MF / STL 离散网格导入
cargo run -p cad-cli -- inspect examples/mvp-tier4-import/assets/custom_rel.3mf --json
cargo run -p cad-cli -- build examples/mvp-tier4-import/custom_rel.kcl \
  --workspace-root examples/mvp-tier4-import \
  --out-mesh tmp_out/custom_rel.mesh.json \
  --json

# 6. 检查 V2 .aicad 示例工程并投影为 .kcl 2.0
cargo run -p cad-cli -- project inspect examples/aicad-projects/01_mounting_plate.aicad --json
cargo run -p cad-cli -- project emit-kcl examples/aicad-projects/01_mounting_plate.aicad
```

---

## 3. 路径 B：启用原生 OCCT 精确 B-Rep 内核（`--features occt-ffi`）

精确 STEP AP242 / IGES / BREP 导入导出、复杂圆角/倒角/抽壳、精确 B-Rep 布尔干涉与 L4 STEP 往返验证需要编译 `cpp/occt-bridge` 并链接 OpenCASCADE 7.9+ 动态库。

### Windows（PowerShell + Visual Studio 开发者环境 + `micromamba`/`conda`）

```powershell
# 1. 拉取预编译 OCCT 动态库（先预览再下载）
./scripts/fetch-occt-prebuilt.ps1 -DryRun
./scripts/fetch-occt-prebuilt.ps1

# 2. 编译 C++20 OCCT 桥接动态库
cmake -S cpp/occt-bridge -B build/occt-bridge
cmake --build build/occt-bridge --config Release

# 3. 使用独立 target 目录编译原生 cad-cli（不覆盖默认纯 Rust 构建）
$env:OCCT_BRIDGE_LIB_DIR = (Resolve-Path build/occt-bridge/Release).Path
$env:PATH = "$env:OCCT_BRIDGE_LIB_DIR;$(Resolve-Path cpp/occt-prebuilt/Library/bin);$(Resolve-Path cpp/occt-prebuilt);$env:PATH"
cargo build -p cad-cli --features occt-ffi --target-dir target/occt-ffi --locked

# 4. 通过自举脚本运行原生 CLI（自动注入动态库路径并诊断缺库）
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

> **常见排障**：如果在 Windows 上直接运行 `target/occt-ffi/debug/cad-cli.exe` 出现退出码 `-1073741515`（`0xC0000135`），说明 `occt_bridge.dll` 或 OCCT 运行时 DLL（含 `cpp/occt-prebuilt/` 下的传递依赖如 `zlib.dll`）不在 `PATH` 中。请优先使用 `./scripts/run-cad-cli-occt.ps1` / `.sh`。

---

## 4. 启动 Studio 工作台（Web 与 Tauri 桌面端）

### Web 工作台（连接真实本地编译器）

```sh
pnpm install
cargo build -p cad-cli --locked
pnpm build
pnpm dev:server
```

浏览器访问 `http://127.0.0.1:5173`。如需使用原生 OCCT CLI 驱动 Web 工作台，启动前将 `CAD_CLI_BIN` 指向 `target/occt-ffi/debug/cad-cli(.exe)` 并确保 OCCT 动态库已在搜索路径中。

前端热更新开发模式（双终端）：

```powershell
# PowerShell 终端 1：启动 API 服务
$env:PORT = '4173'; pnpm dev:server

# PowerShell 终端 2：启动 Vite 开发服务并转发 /api
$env:CAD_STUDIO_API_PROXY = 'http://127.0.0.1:4173'; pnpm dev
```

```sh
# macOS / Linux
PORT=4173 pnpm dev:server
CAD_STUDIO_API_PROXY=http://127.0.0.1:4173 pnpm dev
```

### Tauri v2 桌面工作台

Tauri 桌面端直接在进程内调用 `cad-runtime`（含 3MF/STEP/DXF/SVG 导出与 Live-Attach IPC 服务），无需启动 Node 服务：

```sh
# 纯 Rust 模式构建桌面端
pnpm build:desktop

# 或结合 scripts/package-release.* 一键打便携发布包（默认包含 OCCT，可加 -WithoutOcct / --without-occt）
./scripts/package-release.ps1 -DryRun
./scripts/package-release.ps1 -WithoutOcct
```

Linux / macOS 对应脚本为 `./scripts/package-release.sh`（支持 `--dry-run`、`--cli-only`、`--without-occt`）。

### GitHub Actions 自动打包、正式安装包与签名自动更新（Windows / Linux / macOS）

公开仓库内置 `.github/workflows/release.yml`（**Release Installers & Portable Bundles**），同时产出**正式自动更新安装包**与**便携压缩包**（默认包含权威 OCCT 7.9+ 动态库、Tauri 桌面端、`cad-cli`、`services/cad-agent` 与 `SHA256SUMS.txt`）：
- **正式安装包（支持 Tauri v2 签名自动更新）**：
  - Windows x64：NSIS 安装包（`*-setup.exe` + `.sig`）
  - macOS arm64：`.dmg` 安装镜像 + `.app.tar.gz` 更新归档（+ `.sig`）
  - Linux x64：`.AppImage`（+ `.sig`）
  - 桌面端内置自动更新器（位于 **文件 › 键鼠与工作台偏好 › 软件版本与自动更新**），启动时静默检查 GitHub Releases 的 `latest.json`，下载前进行 minisign 公钥验签，更新时连同桌面端、`cad-cli`、OCCT 动态库与 `services/cad-agent` 整体升级。
- **便携压缩包**：Windows x64（`.zip`）、Linux x64（`.tar.gz`）与 macOS arm64（`.tar.gz`），解压即用（不启用原位自动覆盖更新）。
- **打版本标签自动发布**：推送 `v*` 标签（如 `git tag v0.2.0 && git push origin v0.2.0`）自动并行构建三端产物，经隔离运行时自检后创建 GitHub Release 并自动生成 `latest.json` 与 `SHA256SUMS.txt`。
- **维护者启用签名自动更新（一次性配置）**：
  1. 本地生成 minisign 密钥对：`pnpm dlx @tauri-apps/cli@^2 signer generate -w ~/.tauri/ai-native-cad.key`
  2. 在公开仓库 **Settings › Secrets and variables › Actions** 新增以下 Repository Secrets：
     - `TAURI_SIGNING_PRIVATE_KEY`：`~/.tauri/ai-native-cad.key` 的完整文件内容
     - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：生成密钥时设置的口令（若为空密码可不填）
     - `TAURI_SIGNING_PUBLIC_KEY`：`~/.tauri/ai-native-cad.key.pub` 的完整文件内容
  3. 推送下一个 `v*` 标签即可自动产出已签名安装包、`.sig` 文件与 `latest.json`（未配置 Secrets 时仍可正常构建未签名安装包与便携包）。

---

## 5. 安装 Python SDK 与运行示例

```sh
# 安装官方 Python SDK（aicad）
python -m pip install -e packages/python-sdk

# 运行三个端到端 SDK 示例（需先执行 cargo build -p cad-cli --locked）
python -X utf8 examples/developer-platform/01_inspect_and_measure.py
python -X utf8 examples/developer-platform/02_parametric_commit_and_verify.py
python -X utf8 examples/developer-platform/03_branch_preview_and_promote.py
```

更多用法见 [Studio 工作台使用指南](workbench-guide.md) 与 [AI 生态、MCP 与 Python SDK 指南](agent-and-mcp-guide.md)。

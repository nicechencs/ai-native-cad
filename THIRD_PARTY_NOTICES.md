# Third-Party Notices & License Compliance / 第三方许可与合规说明

AI-Native CAD source code is licensed under the **Apache License, Version 2.0** ([LICENSE](LICENSE)).

AI-Native CAD 主仓库源码采用 **Apache-2.0** 开源许可证（见 [LICENSE](LICENSE)）。本项目严格执行依赖许可证边界隔离，以下说明核心第三方组件的许可关系。

---

## 1. OpenCASCADE Technology (OCCT) — LGPL-2.1 (Optional Dynamic Link)

- **Component**: Optional authoritative B-Rep geometry kernel accessed via `cpp/occt-bridge` (`--features occt-ffi`) and prebuilt runtime packages fetched by `scripts/fetch-occt-prebuilt.ps1` / `scripts/fetch-occt-prebuilt.sh`.
- **License**: GNU Lesser General Public License v2.1 (LGPL-2.1) with Open CASCADE Exception.
- **Compliance Boundary**:
  - Default pure-Rust builds (`cargo build --workspace --locked`) have **zero compile-time or runtime dependency on OCCT**.
  - When `--features occt-ffi` is enabled, OCCT is linked **strictly as a dynamic shared library** (`.dll` / `.so` / `.dylib`) behind the C ABI boundary (`cpp/occt-bridge/include/occt_bridge.h`), allowing end users to replace or relink the OCCT runtime libraries independently.
  - Prebuilt OCCT binaries are not committed to this repository; they are downloaded on demand from `conda-forge` (`occt`) or supplied by the user's environment.
- **中文说明**：默认纯 Rust 构建不依赖 OCCT；启用 `--features occt-ffi` 时，OCCT 严格通过 C ABI 动态链接库（`.dll` / `.so` / `.dylib`）隔离调用，遵循 LGPL-2.1 允许用户独立替换动态库的合规要求，预编译二进制不提交至源码仓库。

## 2. Core Rust, Frontend & Python Ecosystem

| Layer / 层级 | Key Dependencies / 核心依赖 | License / 许可证 |
| :--- | :--- | :--- |
| **Rust Core (`crates/`)** | `serde`, `clap`, `rusqlite` (SQLite bundled, public domain), `zstd`, `wgpu`, `bytemuck`, `nalgebra`, `tauri` | MIT / Apache-2.0 / BSD / Public Domain |
| **Frontend (`apps/studio-ui/`)** | `react`, `react-dom`, `monaco-editor`, `@monaco-editor/react`, `vite`, `typescript`, `occt-import-js` (LGPL-2.1 WASM module for optional browser STEP preview) | MIT / Apache-2.0 / LGPL-2.1 (`occt-import-js`) |
| **Python SDK & Agent (`packages/`, `services/`)** | Production runtime uses **Python 3.12+ standard library only** | PSF License |

## 3. Prohibited Copyleft & Proprietary SDK Boundary

- **GPLv3 / strong copyleft modules** (such as GPL-only geometry kernels) are not permitted in the core runtime.
- **Commercial format/kernel SDKs** (e.g. proprietary CAD exchange adapters) are never bundled into this open-source repository; any future enterprise adapter must load dynamically across an out-of-process or ABI plugin boundary.

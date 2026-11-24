# AI-Native CAD

[English](./README.en.md) | **简体中文**

AI-Native CAD 是一个面向参数化机械设计与人机协同的开源 CAD 工程平台。系统以 **`.aicad` 开放混合工程容器 + 强物理量纲 `Canonical Graph IR`** 为唯一真相源，提供 **Rust 编译器/运行时/CLI**、**OpenCASCADE (OCCT) 可插拔精确 B-Rep 桥接**、**NX/Creo 风格 React 19 / Tauri v2 混合工作台**、**官方 Python SDK (`aicad`)**，以及面向外部通用 Agent（Claude Code / Codex / Cursor / Grok）与内置多智能体的 **MCP / CLI 同源网关**。

> **能力与边界说明**：当前版本聚焦**参数化机械零件与刚体小装配**；支持纯 Rust 零 C++ 依赖快速构建（`AnalyticKernel` + `cad-mesh-io`）与 `--features occt-ffi` 原生精确 B-Rep 轨（动态链接 OpenCASCADE 7.9+）双模运行。所有工程验证遵循 `PASS / FAIL / UNKNOWN` 三值零信任原则，未启用权威 OCCT 内核时 L4 STEP 往返检查会诚实输出 `UNKNOWN`。详见 [导入导出与三值验证指南](docs/zh/import-export-and-verification.md)。

---

## 核心特性

- **单一真相源（`.aicad` + `Canonical Graph IR`）**：强物理量纲参数与实时 `Formula DAG`、结构化 2D 草图与约束求解（`cad-gcs`）、内核无关双轨持久化拓扑命名（`TopoRef`）、SQLite 单文件工作态（ACID 事务 / `oplog` Undo-Redo / 轻量分支 / 节点级 Merkle 缓存）与 Git 友好目录态双向互转。
- **三入口等价协同**：
  - **Studio 交互工作台（Web + Tauri v2 桌面端）**：NX/Creo 导航预设（支持切换 SolidWorks / Web 模式）、Feature Dashboard、3D 剖切与爆炸图、2D 关联工程图、内联 Point-and-Prompt 与基于候选分支的 `Change Preview` 审阅流。
  - **声明式 CAD-DSL（`.kcl`）**：强物理量纲静态检查（拦截 `Length + Angle` 等单位混用）、确定性拓扑消歧（`E201_AMBIGUOUS_TOPOLOGY`）、1:1 双向无损投影与 `cad-cli lsp`。
  - **官方 Python 科学生成 SDK（`aicad`）**：编写可复现的参数化建模、候选分支预览、验证与导出脚本（见 [packages/python-sdk](packages/python-sdk/README.md) 与 [examples/developer-platform](examples/developer-platform/README.md)）。
- **外部 Agent 原生互操作（BYOA）与内置 AI 编排**：
  - `cad-cli mcp` 提供 **8 个正交 MCP Tools + `aicad://` 渐进式资源**（支持桌面 Live-Attach、无头 Headless 与本地 Streamable HTTP 三态），开箱配置见 [skills/aicad-engineering/SKILL.md](skills/aicad-engineering/SKILL.md)。
  - `services/cad-agent` 提供基于 V2 `Semantic CAD API`、标准件/材料库摘要与候选分支探索的 Python 编排服务（见 [services/cad-agent/README.md](services/cad-agent/README.md)）。
- **多格式导入导出与零信任门禁**：支持 `STEP AP242`、`IGES`、`BREP`（OCCT）、`3MF`、`STL`、`OBJ`、`glTF/GLB`（纯 Rust）、`DXF` 与 `SVG` 工程图；支持 L0–L4 五层三值验收。

---

## 5 分钟快速开始

### 1. 编译 Rust 核心与 CLI（纯 Rust，零 C++ 依赖）

前置要求：支持 Rust 2024 Edition 的 Rust 工具链。

```sh
# 编译整个 Rust 工作区（默认纯 Rust 模式，无需安装 C++/OCCT）
cargo build --workspace --locked

# 查看 cad-cli 全部子命令
cargo run -p cad-cli -- --help

# 检查示例安装板（输出量纲、水密与体积指标）
cargo run -p cad-cli -- check examples/mvp-tier1-plates/01_mounting_plate.kcl --json

# 构建并导出 STEP、3D 网格、DXF 与 2D 工程图 SVG
cargo run -p cad-cli -- build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --out-step tmp_out/bearing_asm.step \
  --out-mesh tmp_out/bearing_asm.mesh.json \
  --out-dxf tmp_out/bearing_asm.dxf \
  --out-drawing tmp_out/bearing_asm.drawing.svg \
  --json

# 查看内置 V2 .aicad 示例工程并投影为 .kcl 2.0
cargo run -p cad-cli -- project inspect examples/aicad-projects/01_mounting_plate.aicad --json
cargo run -p cad-cli -- project emit-kcl examples/aicad-projects/01_mounting_plate.aicad
```

> 如需启用精确 B-Rep 造型、STEP/IGES/BREP 权威导入导出与精确 HLR，请按 [快速开始与 OCCT 构建指南](docs/zh/getting-started.md) 使用 `scripts/fetch-occt-prebuilt.*` 与 `scripts/run-cad-cli-occt.*` 启用 `--features occt-ffi`。

### 2. 启动 Studio 工作台（Web / Tauri 桌面端）

前置要求：Node.js 22+ 与 pnpm 9+。

```sh
pnpm install
cargo build -p cad-cli --locked

# 方式 A：启动本地编译服务 + Studio 工作台（默认 http://127.0.0.1:5173）
pnpm build
pnpm dev:server

# 方式 B：前端热更新开发（双终端）
# 终端 1（macOS/Linux: PORT=4173 pnpm dev:server | PowerShell: $env:PORT='4173'; pnpm dev:server）
# 终端 2（macOS/Linux: CAD_STUDIO_API_PROXY=http://127.0.0.1:4173 pnpm dev）
```

Tauri 桌面端直接通过原生 IPC 调用本地 Rust 运行时，无需启动 Node API 服务（详见 [快速开始与构建指南](docs/zh/getting-started.md)）。

### 3. 运行 Python SDK 示例与接入外部 Agent（MCP）

```sh
# 运行 Python SDK 端到端示例（需要 Python 3.12+ 与已构建的 cad-cli）
python -X utf8 examples/developer-platform/01_inspect_and_measure.py

# 外部 Agent（Claude Code / Codex / Cursor / Grok）环境自检
cargo run -p cad-cli -- doctor-agent --json
```

---

## 文档导航

完整双语文档索引见 [docs/README.md](docs/README.md)：

| 文档（简体中文） | English | 内容简介 |
| :--- | :--- | :--- |
| [快速开始与构建指南](docs/zh/getting-started.md) | [Getting Started](docs/en/getting-started.md) | 环境准备、纯 Rust / OCCT 双模构建、Web/Desktop 启动与便携包打包 |
| [系统架构与能力矩阵](docs/zh/architecture.md) | [Architecture](docs/en/architecture.md) | 五层架构、核心不变量、26 个 Crate 分层与真实实现状态矩阵 |
| [Studio 工作台使用指南](docs/zh/workbench-guide.md) | [Workbench Guide](docs/en/workbench-guide.md) | NX/Creo 导航预设、Feature Dashboard、Point-and-Prompt 与 Change Preview |
| [`.aicad` 混合容器与 Graph IR](docs/zh/aicad-container-and-ir.md) | [`.aicad` Container & IR](docs/en/aicad-container-and-ir.md) | SQLite / 目录 / `.cadpack` 存储规范、Formula DAG、双轨 `TopoRef` 与分支 |
| [CAD-DSL (`.kcl`) 语言参考](docs/zh/cad-dsl-reference.md) | [CAD-DSL Reference](docs/en/cad-dsl-reference.md) | 强量纲语法、草图约束、3D 特征算子、拓扑消歧与沙箱导入 |
| [`cad-cli` 命令行参考](docs/zh/cli-reference.md) | [CLI Reference](docs/en/cli-reference.md) | 14 个主子命令与 `project` 子命令速查、环境变量与退出码 |
| [AI 生态、MCP 与 Python SDK](docs/zh/agent-and-mcp-guide.md) | [Agent & MCP Guide](docs/en/agent-and-mcp-guide.md) | 8 大正交 MCP Tools、Live-Attach/Headless/HTTP 模式、内置 `cad-agent` 与 `aicad` SDK |
| [导入导出与五层三值验证](docs/zh/import-export-and-verification.md) | [Import/Export & Verification](docs/en/import-export-and-verification.md) | 格式支持矩阵、属性保留边界、OCCT 缝合修复与 L0–L4 `PASS/FAIL/UNKNOWN` |
| [学术文献与行业参考](docs/zh/references.md) | [References](docs/en/references.md) | B-Rep 深度学习、隐式场、双向程序变换与重拓扑参考索引 |

---

## 仓库目录速览

```text
ai-native-cad/
├── crates/                 # 26 个 Rust 核心 crate（IR / 拓扑 / 容器 / 求值器 / OCCT 桥 / 渲染 / MCP / CLI）
├── cpp/                    # C++20 OCCT 桥接动态库（occt-bridge）与服务化几何引擎骨架（cad-geometry-engine）
├── apps/
│   ├── studio-ui/          # React 19 + Monaco + WebGL2/WebGPU Studio 工作台
│   └── desktop/            # Tauri v2 桌面宿主（原生 IPC、Live-Attach 服务端与本地导出）
├── packages/python-sdk/    # 官方 Python SDK（aicad）
├── services/cad-agent/     # L4 Python 语义规划与分支探索服务
├── skills/aicad-engineering/ # 面向 Claude Code / Codex / Cursor / Grok 的即用 MCP Skill 与配置模板
├── examples/               # 示例模型库（.kcl 零件/装配、STEP/3MF/STL 导入样例、.aicad 工程、Python SDK 示例）
├── schemas/                # 跨语言 JSON Schema 契约与单一事实源（materials.json / contracts-ssot.json）
├── docs/                   # 中英双语完整文档（docs/zh/ 与 docs/en/）
└── scripts/                # OCCT 预编译拉取、自举运行、Schema 常量同步与便携包打包脚本
```

---

## 示例模型

所有示例均已分类整理在 [examples/README.md](examples/README.md)：
- **`examples/aicad-projects/`**：V2 原生 `.aicad` 示例工程（可直接用 `cad-cli project` 或 Studio 工作台打开）。
- **`examples/mvp-tier1-plates/`** ～ **`mvp-tier3-advanced/`**：13 个参数化 `.kcl` 零件与装配示例（安装板、法兰、支架、控制盒、扫掠管、两件轴承座）。
- **`examples/mvp-tier4-import/`**：STEP / 3MF / STL 外部资产导入样例与确定性生成器。
- **`examples/developer-platform/`**：3 个端到端 Python SDK (`aicad`) 实战脚本。

---

## 贡献与许可证

- 贡献与问题反馈方式见 [CONTRIBUTING.md](CONTRIBUTING.md)。
- 本项目主代码采用 [Apache-2.0](LICENSE) 许可证；第三方依赖与 OpenCASCADE（LGPL-2.1 动态链接隔离）合规说明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

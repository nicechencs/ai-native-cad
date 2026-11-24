# AI-Native CAD

**English** | [简体中文](./README.md)

AI-Native CAD is an open-source parametric CAD platform built for deterministic mechanical engineering and human-AI collaboration. Its single source of truth is the **`.aicad` open hybrid project container** backed by a strongly dimensional **`Canonical Graph IR`**, pairing a **Rust compiler/runtime/CLI**, a **pluggable OpenCASCADE (OCCT) exact B-Rep bridge**, an **NX/Creo-inspired React 19 + Tauri v2 hybrid workbench**, an **official Python SDK (`aicad`)**, and a unified **MCP / CLI gateway** for both external agents (Claude Code, Codex, Cursor, Grok) and the built-in multi-agent service.

> **Scope & Honesty Note**: The current release focuses on **parametric mechanical parts and rigid assemblies**. It supports both a zero-C++ pure Rust build (`AnalyticKernel` + `cad-mesh-io`) for fast local exploration and an authoritative native B-Rep track (`--features occt-ffi`, dynamically linking OpenCASCADE 7.9+). Verification strictly follows zero-trust tri-state semantics (`PASS / FAIL / UNKNOWN`); without the native OCCT backend, L4 STEP round-trip honestly reports `UNKNOWN`. See [Import/Export & Verification](docs/en/import-export-and-verification.md).

---

## Key Capabilities

- **Single Source of Truth (`.aicad` + `Canonical Graph IR`)**: Strongly typed physical units with live `Formula DAG`, structured 2D sketches with 2D geometric constraint solving (`cad-gcs`), kernel-agnostic dual-track persistent topological naming (`TopoRef`), single-file SQLite working containers (ACID transactions, `oplog` Undo/Redo, lightweight preview branches, node-level Merkle B-Rep cache), and Git-friendly unpacked directory containers.
- **Three Equivalent Workflows**:
  - **Studio Hybrid Workbench (Web + Tauri v2 Desktop)**: NX/Creo default navigation (switchable to SolidWorks or Web/Trackpad presets), Creo-style Feature Dashboard, live 3D section planes & exploded views, associative 2D drawings, inline Point-and-Prompt (`Alt+A`), and branch-isolated `Change Preview` approvals.
  - **Declarative CAD-DSL (`.kcl`)**: Compile-time dimensional type checking (rejecting invalid unit mixes such as `Length + Angle`), deterministic topology disambiguation (`E201_AMBIGUOUS_TOPOLOGY`), 1:1 lossless round-trip projection, and headless `cad-cli lsp`.
  - **Official Python SDK (`aicad`)**: Scriptable parametric modeling, candidate branch previews, verification, and manufacturing exports (see [packages/python-sdk](packages/python-sdk/README.md) and [examples/developer-platform](examples/developer-platform/README.md)).
- **Bring-Your-Own-Agent (MCP / CLI) & Built-in Orchestration**:
  - `cad-cli mcp` exposes **8 orthogonal MCP tools + progressive `aicad://` resources** across Desktop Live-Attach, Headless In-Process, and loopback Streamable HTTP modes. Ready-to-use client templates live in [skills/aicad-engineering/SKILL.md](skills/aicad-engineering/SKILL.md).
  - `services/cad-agent` provides the V2 semantic planner, standard-parts/materials digest injection, and Best-of-N candidate branch exploration (see [services/cad-agent/README.md](services/cad-agent/README.md)).
- **Multi-Format Exchange & Zero-Trust Verification**: `STEP AP242`, `IGES`, `BREP` (via OCCT), `3MF`, `STL`, `OBJ`, `glTF/GLB` (pure Rust), `DXF`, and `SVG` drawings, gated by a 5-layer L0–L4 tri-state verifier.

---

## 5-Minute Quick Start

### 1. Build the Rust Core & CLI (Zero-C++ Pure Rust)

Prerequisite: Rust toolchain supporting Rust 2024 Edition.

```sh
# Build the Rust workspace without requiring C++ or OCCT
cargo build --workspace --locked

# List all cad-cli subcommands
cargo run -p cad-cli -- --help

# Validate a golden mounting plate (dimensions, watertightness, volume)
cargo run -p cad-cli -- check examples/mvp-tier1-plates/01_mounting_plate.kcl --json

# Build and export STEP, viewport SceneMesh JSON, 1:1 DXF, and 2D drawing SVG
cargo run -p cad-cli -- build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --out-step tmp_out/bearing_asm.step \
  --out-mesh tmp_out/bearing_asm.mesh.json \
  --out-dxf tmp_out/bearing_asm.dxf \
  --out-drawing tmp_out/bearing_asm.drawing.svg \
  --json

# Inspect a V2 .aicad example project and emit its .kcl 2.0 projection
cargo run -p cad-cli -- project inspect examples/aicad-projects/01_mounting_plate.aicad --json
cargo run -p cad-cli -- project emit-kcl examples/aicad-projects/01_mounting_plate.aicad
```

> To enable authoritative B-Rep operations, STEP/IGES/BREP import/export, and exact HLR drawings, follow the [Getting Started & OCCT Build Guide](docs/en/getting-started.md) using `scripts/fetch-occt-prebuilt.*` and `scripts/run-cad-cli-occt.*` with `--features occt-ffi`.

### 2. Launch the Studio Workbench (Web or Tauri Desktop)

Prerequisites: Node.js 22+ and pnpm 9+.

```sh
pnpm install
cargo build -p cad-cli --locked

# Option A: Build frontend and serve with the local compiler server (http://127.0.0.1:5173)
pnpm build
pnpm dev:server

# Option B: Hot-reload frontend dev server (two terminals)
# Terminal 1 (macOS/Linux: PORT=4173 pnpm dev:server | PowerShell: $env:PORT='4173'; pnpm dev:server)
# Terminal 2 (macOS/Linux: CAD_STUDIO_API_PROXY=http://127.0.0.1:4173 pnpm dev)
```

The Tauri v2 desktop workbench links the Rust runtime in-process over native IPC and does not require the Node API server. See [Getting Started](docs/en/getting-started.md).

### 3. Run Python SDK Examples & External Agent Preflight

```sh
# Run an end-to-end Python SDK example (Python 3.12+ with built cad-cli)
python -X utf8 examples/developer-platform/01_inspect_and_measure.py

# Check binary, OCCT authority, MCP tool budget, and live-attach state
cargo run -p cad-cli -- doctor-agent --json
```

---

## Documentation

Browse the full bilingual index at [docs/README.md](docs/README.md):

| English | 简体中文 | Summary |
| :--- | :--- | :--- |
| [Getting Started & Build Guide](docs/en/getting-started.md) | [快速开始与构建指南](docs/zh/getting-started.md) | Prerequisites, Pure Rust vs OCCT builds, Web/Desktop launch, portable packaging |
| [Architecture & Capability Matrix](docs/en/architecture.md) | [系统架构与能力矩阵](docs/zh/architecture.md) | Five-layer stack, invariants, 26-crate layering, honest implementation status |
| [Studio Workbench Guide](docs/en/workbench-guide.md) | [Studio 工作台使用指南](docs/zh/workbench-guide.md) | NX/Creo navigation presets, Feature Dashboard, Point-and-Prompt, Change Preview |
| [`.aicad` Container & Graph IR](docs/en/aicad-container-and-ir.md) | [`.aicad` 混合容器与 Graph IR](docs/zh/aicad-container-and-ir.md) | SQLite / directory / `.cadpack` formats, Formula DAG, dual-track `TopoRef`, branches |
| [CAD-DSL (`.kcl`) Reference](docs/en/cad-dsl-reference.md) | [CAD-DSL (`.kcl`) 语言参考](docs/zh/cad-dsl-reference.md) | Dimensional grammar, 2D sketch constraints, 3D operators, topology selectors, imports |
| [`cad-cli` Reference](docs/en/cli-reference.md) | [`cad-cli` 命令行参考](docs/zh/cli-reference.md) | All 14 top-level subcommands, `project` workflows, environment variables, exit codes |
| [AI Ecosystem, MCP & Python SDK](docs/en/agent-and-mcp-guide.md) | [AI 生态、MCP 与 Python SDK](docs/zh/agent-and-mcp-guide.md) | 8 MCP tools, Live-Attach/Headless/HTTP modes, built-in `cad-agent`, `aicad` SDK |
| [Import/Export & Verification](docs/en/import-export-and-verification.md) | [导入导出与五层三值验证](docs/zh/import-export-and-verification.md) | Format support matrix, preservation boundaries, OCCT healing, L0–L4 tri-state gate |
| [Academic & Industry References](docs/en/references.md) | [学术文献与行业参考](docs/zh/references.md) | B-Rep deep learning, SDFs, bidirectional programming, and remeshing bibliography |

---

## Repository Layout

```text
ai-native-cad/
├── crates/                 # 26 Rust crates (IR, topo, container, evaluator, OCCT bridge, render, MCP, CLI)
├── cpp/                    # C++20 OCCT shared-library bridge (occt-bridge) & service engine skeleton (cad-geometry-engine)
├── apps/
│   ├── studio-ui/          # React 19 + Monaco + WebGL2/WebGPU hybrid CAD workbench
│   └── desktop/            # Tauri v2 desktop host (native IPC, Live-Attach host, local exports)
├── packages/python-sdk/    # Official Python generative SDK (aicad)
├── services/cad-agent/     # L4 Python semantic planning & branch exploration service
├── skills/aicad-engineering/ # Ready-to-use MCP skill & config templates for Claude Code, Codex, Cursor, Grok
├── examples/               # Sample models (.kcl parts/assemblies, STEP/3MF/STL imports, .aicad projects, Python scripts)
├── schemas/                # Cross-language JSON Schemas & SSOT catalogs (materials.json, contracts-ssot.json)
├── docs/                   # Full bilingual documentation (docs/en/ and docs/zh/)
└── scripts/                # OCCT prebuilt fetchers, OCCT launcher wrappers, schema codegen, release packaging
```

---

## Sample Models

Explore all curated examples in [examples/README.md](examples/README.md):
- **`examples/aicad-projects/`**: Native V2 `.aicad` starter projects ready for `cad-cli project` or the Studio Workbench.
- **`examples/mvp-tier1-plates/`** – **`mvp-tier3-advanced/`**: 13 parametric `.kcl` parts and assemblies (mounting plates, flanges, brackets, shelled control box, swept pipe, two-part bearing housing).
- **`examples/mvp-tier4-import/`**: Deterministic STEP, 3MF, and STL import examples with fixture verification script.
- **`examples/developer-platform/`**: 3 runnable Python SDK (`aicad`) scripts against `cad-cli`.

---

## Contributing & License

- Contribution guidelines and issue reporting: [CONTRIBUTING.md](CONTRIBUTING.md).
- Licensed under [Apache-2.0](LICENSE). Third-party dependency licenses and OpenCASCADE (LGPL-2.1 dynamic-linking isolation) compliance details are documented in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

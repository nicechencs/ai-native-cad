# Contributing to AI-Native CAD / 参与贡献指南

Thank you for your interest in AI-Native CAD! / 感谢你对 AI-Native CAD 的关注与支持！

---

## Snapshot Release Model / 快照发布说明

This public repository is published as a curated open-source snapshot from the main development repository. All production Rust crates, C++ OCCT bridge sources, React/Tauri workbench code, Python SDK, built-in agent runtime, MCP skill templates, JSON schemas, documentation, and curated sample models are included.

本公开仓库定期从主研发仓库同步发布源码快照，包含全部生产级 Rust crate、C++ OCCT 桥接源码、React/Tauri 工作台、Python SDK、内置 Agent 服务、MCP 技能模板、JSON Schema、双语文档与精选示例模型。

## Reporting Issues / 提交问题反馈

When opening a GitHub Issue, please include / 提交 Issue 时请尽量提供：

1. **Environment / 运行环境**: OS, Rust version (`rustc --version`), Node/pnpm version, Python version, and whether `--features occt-ffi` (OpenCASCADE) is enabled.
2. **Minimal Reproduction / 最小复现用例**: A small `.kcl` file, `.aicad` directory, or `SemanticCommand` JSON batch that triggers the issue.
3. **Command & JSON Output / 命令与输出**: The exact `cad-cli ... --json` command, exit code, and diagnostic codes (for example `E201_AMBIGUOUS_TOPOLOGY` or L0–L4 layer statuses).
4. **Expected vs. Actual Behavior / 预期与实际表现**: Especially for geometry precision, tolerances, STEP/DXF/3MF export, or UI interactions.

## Pull Requests & Code Conventions / 代码贡献规范

- **Rust**: Run `cargo check --workspace --locked` and keep compilation warnings at zero. Follow crate layering boundaries (leaf crates such as `cad-topo` / `cad-stdlib` must not depend on higher-layer crates).
- **Frontend (`apps/studio-ui`, `apps/desktop`)**: Use **pnpm 9+** with Node 22+ (do not generate `package-lock.json`). Verify with `pnpm typecheck && pnpm build`.
- **Single Source of Truth (SSOT)**: Material presets live in `schemas/materials.json` and shared contract constants live in `schemas/contracts-ssot.json` (synchronized via `python scripts/codegen/sync_schema_constants.py`).
- **Licensing Firewall**: Core Rust, TypeScript, and Python code must remain Apache-2.0 / MIT compatible. Do not introduce GPL-licensed dependencies into the core codebase, and keep OpenCASCADE (LGPL-2.1) isolated behind the dynamic C ABI bridge (`cpp/occt-bridge`). See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
- **Commit Messages**: Follow Conventional Commits (`feat:`, `fix:`, `docs:`, `chore:`, etc.).

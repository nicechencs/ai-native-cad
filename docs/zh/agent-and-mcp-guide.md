# AI 生态、MCP 网关与 Python SDK 指南

[English](../en/agent-and-mcp-guide.md) | **简体中文** | [文档索引](../README.md)

AI-Native CAD 让 **外部通用 Agent（Claude Code / OpenAI Codex / Cursor / xAI Grok / Antigravity）**、**内置 `services/cad-agent`** 与 **官方 Python SDK（`aicad`）** 100% 共享同一套 `DocumentRuntime` 与 `Semantic CAD API`，彻底消除内外两套胶水逻辑分叉。

---

## 1. 外部通用 Agent 接入（BYOA · MCP + CLI）

核心协议由 `crates/cad-mcp` 实现，通过 `cad-cli mcp` 暴露 **8 个正交高杠杆 MCP Tools**（常驻 Schema 严格控制 Token 开销）与按需读取的 **`aicad://` Resources**。

### 第一步：运行环境预检

```sh
cad-cli doctor-agent --json
```

关注两个关键字段：
- `kernel.authoritative`：`true` 表示已加载原生 OCCT 内核（L4 STEP 往返可达成 `PASS`）；`false` 表示当前为纯 Rust 回退核（L4 保持 `UNKNOWN`）。
- `live_attach.status`：`live` 表示 Tauri 桌面工作台正在运行并已发布 `.aicad-runtime.json`，外部 Agent 可直接热挂载协同；`absent` / `stale` 时使用无头模式指定 `--project <path.aicad>`。

### 三种运行模式

| 模式 | 启动命令 | 适用场景 |
| :--- | :--- | :--- |
| **Live-Attach 桌面协同** | `cad-cli mcp --workspace .` | 挂载正在运行的 Tauri 桌面会话，共享实时选中的面/边 `TopoRef`，修改默认进入预览分支并推送至桌面 `Change Preview` |
| **Headless 无头直连** | `cad-cli mcp --workspace . --project part.aicad` | 纯终端、Git Worktree 或 CI，随 `stdio` 关闭确定性退出，零孤儿守护进程 |
| **Streamable HTTP（仅本机回环）** | `cad-cli mcp --transport http --bind 127.0.0.1:0 --project part.aicad --token "<secret>" --allowed-origin http://127.0.0.1` | 远程/HTTP 工具调用（如 Grok）；强制校验 Bearer Token 与 `Origin` 白名单；可用 `--export-tool-schema openai` 导出函数定义 |

开箱即用的客户端模板位于 `skills/aicad-engineering/`：
- 完整操作规范：[skills/aicad-engineering/SKILL.md](../../skills/aicad-engineering/SKILL.md)
- Claude Code / Cursor 模板：`skills/aicad-engineering/templates/claude-code.mcp.json`（Live-Attach）与 `claude-code.headless.mcp.json`（无头）
- Codex 配置模板：`skills/aicad-engineering/templates/codex-config.toml`
- Grok / HTTP 接入细则：[skills/aicad-engineering/templates/grok-http-setup.md](../../skills/aicad-engineering/templates/grok-http-setup.md)

### 8 大正交 MCP Tools 与对等 CLI

| MCP Tool | 对等 CLI 命令 | 核心职责 |
| :--- | :--- | :--- |
| `cad_session_context` | `cad-cli session status` | 读取 `revision`、模式、活动分支、桌面选中 `TopoRef` 与 `DesignContract` |
| `cad_inspect_and_slice` | `cad-cli project inspect` / `slice-context` | 工程级摘要或焦点节点的最小闭合局部上下文切片 |
| `cad_query_geometry` | `cad-cli query` | 确定性筛选面/边 `TopoRef`、精确几何测量与参数/血缘 `explain` |
| `cad_apply_commands` | `cad-cli api apply` | 唯一结构化写入口，单次原子提交批量 `SemanticCommand`（携带 `expected_revision` 防覆盖） |
| `cad_sync_kcl_view` | `cad-cli project emit-kcl` / 同步 | 读取或编辑 `.kcl 2.0` 视图并差分提交至预览分支 |
| `cad_verify_and_render` | `cad-cli verify` | 输出五层三值验证报告（`PASS`/`FAIL`/`UNKNOWN`）与可选多视角 PNG |
| `cad_branch_control` | `cad-cli project branch` | `list`、`diff`、`request_review`、`commit_preview`、`discard_preview`、`undo`、`redo` |
| `cad_export_artifacts` | `cad-cli build` | 在工作区沙箱内导出 `STEP`、`3MF`、`DXF`、`SVG`，拦截路径逃逸（`E_PATH_ESCAPE`） |

按需查阅的 MCP Resources：`aicad://schema/semantic-commands`、`aicad://schema/feature/<kind>`、`aicad://stdlib/fasteners`、`aicad://stdlib/bearings`、`aicad://stdlib/iso286`、`aicad://stdlib/materials`、`aicad://diagnostics/catalog`。

---

## 2. 安全边界与防提示词注入

- **乐观并发锁**：写入携带 `expected_revision`，并发冲突返回 `E409_REVISION_CONFLICT`，绝不静默覆盖人类操作。
- **默认分支沙箱**：`live_attached` 模式下禁止直写 `main`，必须先在 `agent/<client>/<session_id>` 分支验证并通过人类审阅。
- **外部资产标签隔离**：第三方 STEP / IGES / 网格的零件名经控制字符消毒后包裹在 `untrusted_asset_labels` 字段中，Agent 必须将其仅视为数据而非指令。

---

## 3. 内置 Python Agent 服务（`services/cad-agent`）

`services/cad-agent` 基于 Python 3.12+ 标准库构建（核心运行时零第三方依赖），提供 `LLMSemanticPlanner`、`SemanticAgent`、`v2_context_adapter`（外部参考件配作）与 `BranchPatchExplorer`（Best-of-N 多分支验证择优）。

```sh
cargo build -p cad-cli --locked
python -m pip install -e services/cad-agent

# 需先配置 OPENAI_API_KEY 或 GEMINI_API_KEY
python -m cad_agent.cli \
  --semantic-v2 \
  --project part.aicad \
  --workspace-root . \
  --prompt "80 x 60 x 10 mm aluminum mounting plate with 4 M6 counterbored holes" \
  --json
```

完整说明见 [services/cad-agent/README.md](../../services/cad-agent/README.md)。

---

## 4. 官方 Python 科学生成 SDK（`packages/python-sdk`）

当需要用 `NumPy`/`SciPy` 生成参数化族表、批量提交 `SemanticCommand`、派生候选分支并自动化验证导出时，推荐使用 `aicad` Python SDK：

```sh
python -m pip install -e packages/python-sdk
python -X utf8 examples/developer-platform/01_inspect_and_measure.py
python -X utf8 examples/developer-platform/02_parametric_commit_and_verify.py
python -X utf8 examples/developer-platform/03_branch_preview_and_promote.py
```

API 详情见 [packages/python-sdk/README.md](../../packages/python-sdk/README.md) 与 [examples/developer-platform/README.md](../../examples/developer-platform/README.md)。

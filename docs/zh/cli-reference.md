# `cad-cli` 命令行参考

[English](../en/cli-reference.md) | **简体中文** | [文档索引](../README.md)

`cad-cli`（`crates/cad-cli`）是 AI-Native CAD 的统一无头命令行工具、文档运行时代理与 MCP 网关入口，全部核心子命令均支持 `--json` 机器可读契约。

---

## 1. 14 个顶层子命令总览

| 子命令 | 核心职责 | 典型用法 |
| :--- | :--- | :--- |
| `check` | 编译 `.kcl` 并校验语法、物理量纲、草图约束、水密性与可选干涉资产 | `cad-cli check model.kcl --json` |
| `build` | 编译 `.kcl` 并按需导出 STEP、3MF、SceneMesh、DXF、SVG 工程图与离屏 PNG | `cad-cli build model.kcl --out-step out/part.step --json` |
| `lsp` | 无需几何求值，查询指定行列的补全候选、量纲悬停与符号大纲 | `cad-cli lsp model.kcl --line 12 --col 10 --json` |
| `inspect` | 检查外部 3D 资产（STEP / IGES / BREP / 3MF / STL / OBJ / glTF）并输出接口摘要 | `cad-cli inspect asset.step --json` |
| `compare` | 对比编译实体与参考 STEP 文件的精确体积差 | `cad-cli compare model.kcl ref.step --json` |
| `decompile` | 对棱柱形 B-Rep 执行特征识别并反编译为参数化 `CanonicalGraph`（实验性） | `cad-cli decompile part.step --json` |
| `project` | `.aicad` 工程容器全生命周期管理（见第 3 节） | `cad-cli project inspect part.aicad --json` |
| `daemon` | 在 `stdin`/`stdout` 上运行持久化 JSON-RPC 编译与会话服务 | `cad-cli daemon` |
| `mcp` | 启动外部 Agent MCP 网关（stdio / 本地 Streamable HTTP）或导出 OpenAI 工具 Schema | `cad-cli mcp --workspace . --project part.aicad` |
| `session` | 查询当前文档运行时会话上下文（`revision`、`mode`、活动分支、选中态） | `cad-cli session status --project part.aicad --json` |
| `api` | 向文档运行时提交原子批量 `SemanticCommand` JSON | `cad-cli api apply --project part.aicad --commands cmds.json --json` |
| `query` | 读取文档摘要、查询拓扑面/边、精确测量或追溯节点/参数血缘（`explain`） | `cad-cli query summary --project part.aicad --json` |
| `verify` | 执行 L0–L4 五层三值验证（`fast` / `release`）并输出结构化报告 | `cad-cli verify --project part.aicad --json` |
| `doctor-agent` | 自检 CLI 可执行文件、OCCT 权威状态、MCP Token 预算、工作区权限与 Live-Attach 状态 | `cad-cli doctor-agent --json` |

---

## 2. `build` 常用导出选项

```sh
cad-cli build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --out-step tmp_out/asm.step \
  --out-3mf tmp_out/asm.3mf \
  --out-mesh tmp_out/asm.mesh.json \
  --out-dxf tmp_out/asm.dxf \
  --out-dxf-view tmp_out/asm_view.dxf \
  --out-drawing tmp_out/asm.drawing.svg \
  --out-views tmp_out/views \
  --json
```

- `--workspace-root <DIR>`：显式绑定导入资产的沙箱根目录（适用于脚本与资产位于不同目录时）。
- `--component <ID>`：从装配体中按局部坐标单独导出指定子件。
- `--out-render <PATH>`（需编译启用 `--features wgpu-renderer`）：配合 `--render-views iso,front,top,right`、`--render-size 1024x768` 与 `--emit-semantic-mask` 输出离屏多视角着色 PNG 与像素面 ID 语义掩码。

---

## 3. `project` 子命令速查

| `cad-cli project ...` | 说明 |
| :--- | :--- |
| `init <DIR> --name <NAME> [--format directory\|sqlite]` | 创建新的 `.aicad` 起步工程 |
| `inspect <PATH> [--metadata-only] [--json]` | 检查 SQLite / 目录 / 分片目录 `.aicad` 元数据与节点概览 |
| `emit-kcl <PATH> [--out <FILE>]` | 从 `CanonicalGraph` 1:1 投影输出 `.kcl 2.0` |
| `pack <SRC_DIR> --out <FILE>` / `unpack <FILE> --out <DIR> [--sharded]` | 目录态与 SQLite 单文件态无损互转 |
| `apply-patch <PATH> --patch <FILE> [--branch <NAME>]` | 以 ACID 事务方式应用结构化 `GraphPatch` |
| `branch` / `checkout` | 管理 SQLite 容器内的轻量分支与回滚 |
| `slice-context` | 围绕焦点节点/拓扑引用提取局部闭合上下文切片 |
| `pack-and-go` / `vacuum` | 内联外部引用自包含打包 / 清理孤儿 Blob 与紧缩页面 |
| `import-step-xcaf` / `export-step-xcaf` | 多组件 XCAF STEP 导入导出（需 `--features occt-ffi`） |
| `stdlib` | 查询内置公制紧固件、轴承、ISO 286 公差与材料标准库 |
| `dfm-check` / `render-drawing` / `slice` | 执行 DFM 检查、渲染关联 2D 工程图与增材直接切片 |
| `export-cadpack` / `cadpack-tree` / `cadpack-digest` | 生成与查询 `.cadpack` 大装配流式缓存包 |

---

## 4. 关键环境变量与退出码

- **`CAD_CLI_BIN`**：指定自定义 `cad-cli` 路径（供 Web `dev-server.mjs`、Python SDK 与 `cad-agent` 优先选用，例如指向 OCCT 原生构建 `target/occt-ffi/debug/cad-cli(.exe)`）。
- **`OCCT_BRIDGE_LIB_DIR`**：编译及运行 `--features occt-ffi` 时指向 `occt_bridge` 动态库目录；推荐使用 `./scripts/run-cad-cli-occt.ps1` / `.sh` 自动装配运行时路径。
- **`AICAD_MCP_TOKEN`**：`cad-cli mcp --transport http` 模式下的 Bearer 鉴权令牌。
- **退出码语义**：`0` 表示命令正常完成且所请求校验通过；非零退出码表示编译/几何/门禁错误或参数/路径沙箱拒绝（结构化错误详情总以 `--json` 输出）。在多格式导出场景下，单个格式失败会明确标注状态而不静默伪装成功。

# `.aicad` 混合工程容器与 Canonical Graph IR

[English](../en/aicad-container-and-ir.md) | **简体中文** | [文档索引](../README.md)

AI-Native CAD 以 **`.aicad` 开放混合工程容器** 与强物理量纲 **`Canonical Graph IR`** 作为唯一工程真相源，替代传统“仅靠纯文本脚本或专有二进制黑盒”的二元局限，同时服务于 3D GUI 直操、`.kcl 2.0` 代码视图、Python SDK 与 AI Agent。

---

## 1. 为什么采用混合容器 + Graph IR？

纯文本 `.kcl` 对常规机械板件和支架清晰且适合 Git 审查，但在复杂工程中会遇到三道天花板：
1. **高维二进制载荷不适合进入文本**：NURBS 控制网、SubD 控制笼、外部导入资产和物理场张量若写入源码会撑爆人类阅读与大模型上下文窗口；
2. **正交工程语义膨胀**：AP242 GD&T、材料/外观属性、设计契约（`DesignContract`）与关联式 2D 工程图更适合以结构化数据正交挂载在拓扑引用上；
3. **增量事务与多分支探索**：GUI 高频改参、持久化 Undo/Redo 与 AI Best-of-N 候选分支需要亚毫秒级事务写入与节点级精确几何缓存。

---

## 2. 三态物理存储与自动魔数嗅探

`crates/cad-container` 通过文件元数据与头部字节魔数自动识别容器形态，上层语义完全透明：

| 存储形态 | 物理特征 / 魔数 | 适用场景 |
| :--- | :--- | :--- |
| **SQLite 单文件工作态（默认）** | 前 16 字节 `SQLite format 3\0`，`application_id = 0x41494344` (`AICD`) | 桌面/本地日常设计、持久化 `oplog` Undo/Redo、秒级轻量分支（`branches`）、按需流式 Zstd `blobs`、节点级 `node_brep_cache` 与 `pack-and-go` 单文件交付；网络共享盘（SMB/NFS/UNC）自动从 `WAL` 回退至 `DELETE` Journal |
| **Git 友好目录 / 分片目录态** | 常规目录，含 `manifest.json` 与 `graph.ir.json`（或分片 `graphs/*.ir.json`） | Git 版本控制、PR 代码评审、团队模块化协同与仓库内示例工程（如 `examples/aicad-projects/`） |
| **流式渲染包（`.cadpack`）** | 前 4 字节 `CPK1`（64B 头部、64B 对齐分块目录与载荷） | 大装配 HLOD 0/1/2 流式网格、`ExactPickTable`、虚拟装配树与 `InterfaceDigest` |

### 目录态 `.aicad/` 结构示例

```text
01_mounting_plate.aicad/
├── manifest.json              # 格式版本、ir_schema_version、内核指纹、入口图与 KCL 投影策略
├── graph.ir.json              # 规范化排序的 CanonicalGraph（或分片模式下为 graphs/*.ir.json）
├── engineering.json           # 正交挂载在 TopoRef 上的设计契约、材料、PMI/GD&T 与 BOM 元数据
├── drawings/                  # 可选：绑定 3D TopoRef 的关联式 2D 工程图定义
├── blobs/                     # 可选：内容寻址（SHA-256）外部资产、样条与场二进制载荷
└── cache/                     # 可安全清理或 Vacuum 回收的加速缓存
```

---

## 3. `Canonical Graph IR` 核心组成

对应 JSON Schema 见 [graph-ir.schema.json](../../schemas/graph-ir.schema.json) 与 [graph-patch.schema.json](../../schemas/graph-patch.schema.json)：

1. **强量纲参数表与实时 `Formula DAG`**：支持 `length`（`mm`/`m`/`in`）、`angle`（`deg`/`rad`）、`force`（`N`/`kN`）、`pressure`（`MPa`）、`mass`（`kg`/`g`）与标量；公式依赖按拓扑序实时求值，编译期拦截循环依赖与量纲错配。
2. **结构化 2D 草图（`StructuredSketch2D`）**：一等公民存储草图基准面、2D 图元（点/线/圆弧/圆/矩形）与 `cad-gcs` 约束，求值后记录求解状态与剩余自由度（DOF）。
3. **强类型特征节点图**：涵盖体素、草图成形、标准孔/沉头孔/底孔、圆角/倒角/抽壳/拔模、阵列/镜像、布尔、外部导入、装配组件与配合约束，以及 `Suppressed`（节点抑制）和故障回退状态。
4. **内核无关双轨 `TopoRef`（`crates/cad-topo`）**：
   - **`LineageTopoRef`**：记录特征创建者、演化血缘（`Split` / `Merged` / `Trimmed`）与内蕴几何指纹；
   - **`QueryTopoRef`**：面向代码与 AI 的确定性几何谓词选择器（面类型、法向、包围盒、半径、标签），多解必须显式消歧，不依赖 OCCT 内部 `TNaming`。
5. **节点级确定性 Merkle 缓存**：每个节点按输入参数、上游 Merkle 哈希与内核指纹计算唯一键；修改下游圆角或切换候选分支时，未受影响的上游几何直接 $O(1)$ 复用。

---

## 4. 常用容器命令行工作流

```sh
# 1. 新建目录态或 SQLite 单文件工程
cad-cli project init work/plate.aicad --name "Mounting Plate" --format directory
cad-cli project init work/plate_sql.aicad --name "Mounting Plate" --format sqlite

# 2. 查看容器元数据与图结构（支持 --metadata-only 避免扫描大 Blob）
cad-cli project inspect examples/aicad-projects/01_mounting_plate.aicad --json

# 3. 1:1 投影生成 .kcl 2.0 代码视图
cad-cli project emit-kcl examples/aicad-projects/01_mounting_plate.aicad

# 4. 目录态 ↔ SQLite 单文件态无损互转
cad-cli project pack examples/aicad-projects/01_mounting_plate.aicad --out tmp_out/plate.aicad
cad-cli project unpack tmp_out/plate.aicad --out tmp_out/plate_unpacked.aicad

# 5. 自包含打包分发（内联外部引用）与垃圾回收紧缩
cad-cli project pack-and-go tmp_out/plate.aicad --out tmp_out/plate_bundle.aicad
cad-cli project vacuum tmp_out/plate.aicad
```

更多子命令详见 [`cad-cli` 命令行参考](cli-reference.md)。

## 5. 在 Web Studio 同步特征图补丁

在「模型树 → 特征图 Graph IR」中展开「工作区 .aicad 工程同步」并初始化工程。编辑特征后，点击「同步补丁」将最近一次特征图编辑写入工作区容器。如果开发服务暂时断开，编辑保留在本地，服务恢复后可重试。可以用 `cad-cli project emit-kcl <工程路径>` 重新读取容器并检查同步结果。

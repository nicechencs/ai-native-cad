# 导入导出能力与五层三值验证门禁

[English](../en/import-export-and-verification.md) | **简体中文** | [文档索引](../README.md)

工程 CAD 最怕“静默降级却伪装通过”。AI-Native CAD 对双内核能力边界、文件格式保留属性以及 L0–L4 验证结果全部执行**显式、诚实、可机读**的契约。

---

## 1. 双模几何内核对比

| 能力维度 | 默认纯 Rust 构建（`AnalyticKernel` + `cad-mesh-io`） | 原生 OCCT 构建（`--features occt-ffi` + `cpp/occt-bridge`） |
| :--- | :--- | :--- |
| **构建依赖** | 零 C++ 依赖，开箱即编译 | 需 CMake + C++20 + OpenCASCADE 7.9+ 动态库（LGPL-2.1 隔离） |
| **基础参数化板件/孔/阵列/简单布尔** | 支持（适合快速体验与基础建模） | **权威精确 B-Rep**（复杂布尔、局部圆角/倒角、抽壳与通孔直编） |
| **离散网格导入（3MF / STL / OBJ / glTF / GLB）** | 原生纯 Rust 支持（含 OPC 关系与流形校验） | 支持 |
| **精确 B-Rep 导入（STEP / IGES / BREP）** | 不支持（明确报错提示启用 `occt-ffi`） | **原生支持**（含源单位自动归一化、XCAF 多组件装配与可选缝合修复） |
| **2D 工程图消隐（HLR）与 STEP 导出** | 简化投影 / 基础写出（不作为第三方互操作认证） | **精确 OCCT B-Rep HLR 与权威 STEP AP242 写出** |
| **L4 STEP 往返验证结论** | 诚实输出 **`UNKNOWN`** | 真实重新导入比对体积与拓扑，达标输出 **`PASS`** |

---

## 2. 导入与导出格式矩阵

### 导入支持与默认单位

| 格式 | 所需后端 | 默认单位与说明 |
| :--- | :---: | :--- |
| **3MF** | 默认 Rust | 遵循 OPC `_rels/.rels` 寻址并按文件源单位归一化至 `mm` |
| **STL / OBJ** | 默认 Rust | 默认按 `mm` 读取；非水密开放壳默认报 `E505_IMPORT_NON_MANIFOLD_OR_OPEN`，设置 `allow_open_shell = true` 后仅限预览与测量 |
| **glTF（内嵌缓冲）/ GLB** | 默认 Rust | 默认按 `m` 读取并归一化至 `mm` |
| **STEP / IGES / BREP** | `occt-ffi` | 自动识别源单位（如英寸 `in` 转为 `25.4 mm`）并保留多组件世界位姿；默认不做自动修复，可显式开启 `heal = true` 与 `sewing_tol`（省略容差时默认 `0.001 mm`） |

> 离散网格资产（`import::mesh`）与开放壳均不可参与精确布尔运算；原生实体与导入 STEP 实体可在 `occt-ffi` 下执行双向精确布尔。

### 导出格式与属性边界

| 格式 | 几何类型 | 保留内容（`preserved`） | 未保证/不保留（`not_preserved`） |
| :--- | :---: | :--- | :--- |
| **STEP AP242** | `EXACT`（OCCT）/ 基础写出 | 精确实体边界与孔环、组件关系与基础描述性记录 | 不保证完整第三方商业软件语义 PMI、材质/外观颜色与装配运动副 |
| **3MF** | `MESH` | 三角网格与毫米坐标 | 非精确解析 B-Rep、不含参数历史与配合约束 |
| **DXF**（`--out-dxf` / `--out-dxf-view`） | `EXACT` / 投影 | 1:1 毫米投影轮廓（`--out-dxf-view` 含原生实/虚线工程视图） | 非任意 3D 钣金展开图或可直接下料刀路 |
| **SVG 工程图**（`--out-drawing` / `--out-views`） | `EXACT`（OCCT HLR） | 多视角投影轮廓与隐藏线消隐 | 非 ISO/国标法定出图认证 |
| **SceneMesh JSON / CMS2** | `MESH` | 视口三角化网格、组件元数据、拓扑槽位与编辑手柄 | 仅用于渲染与交互拾取，不替代 B-Rep 交付 |

---

## 3. 五层三值零信任验证门禁（L0–L4）

`cad-cli verify` 与 MCP 工具 `cad_verify_and_render` 输出结构化 [verification-report.schema.json](../../schemas/verification-report.schema.json)：

| 层级键名 | 验证内容 | `PASS` / `FAIL` / `UNKNOWN` 语义 |
| :--- | :--- | :--- |
| **`L0_structure_units`** | 图结构完整性、语法、强物理量纲与公式 DAG | 任何量纲混用或循环公式直接 `FAIL` |
| **`L1_geometry_topology`** | 求值实体水密性、零开放边、正有限体积、无故障回退节点 | 实验性轨（SubD / TPMS / 隐式场）保持 `UNKNOWN`（`L1_EXPERIMENTAL`） |
| **`L2_interfaces_constraints`** | 显式设计断言、2D 草图自由度（DOF）与装配配合约束 | 违例 `FAIL`；无法判定的外部自定义断言保持 `UNKNOWN` |
| **`L3_manufacturability_physics`** | DFM 壁厚/可达性检查与可选物理/体素 FEA 载荷校验 | 声明 FEA 载荷但无可用求解器时保持 `UNKNOWN` |
| **`L4_interop_roundtrip`** | STEP AP242 导出后重新导入比对体积与拓扑一致性 | 仅在权威 OCCT 内核启用时可达成 `PASS`；纯 Rust 回退恒为 `UNKNOWN` |

**总状态汇总铁律**：
- 任意一层为 `FAIL` → `overall_status = FAIL`；
- 无 `FAIL` 但任意一层为 `UNKNOWN`（或缺失）→ `overall_status = UNKNOWN`；
- **仅当 L0–L4 全部为 `PASS` 时**，`overall_status` 才允许为 `PASS`。
- 离屏多视角渲染的 `render.status` 始终为 `UNKNOWN`（像素图只能辅助发现明显语义异常，绝不能替代确定性尺寸检验）。

---

## 4. 常见结构化诊断码速查

| 诊断码 | 含义与处理方式 |
| :--- | :--- |
| `E102_UNIT_MISMATCH` | 物理量纲不匹配（例如长度与角度相加）；统一单位后重试 |
| `E201_AMBIGUOUS_TOPOLOGY` | 拓扑查询命中多个候选实体；查看 `recovery_hints` 并追加法向、包围盒、半径或 `.max_by()` / `.then_by()` 消歧 |
| `E204_OPEN_SKETCH_REGION` | 2D 草图未闭合或自交；补全闭合边环 |
| `E205_INVALID_FILLET_CHAMFER` / `E206_INVALID_SHELL_THICKNESS` | 圆角/倒角/抽壳参数越界或非正；按几何上下界提示缩小参数 |
| `E409_REVISION_CONFLICT` / `E409_PREVIEW_DIVERGED` | 会话版本号过期或主分支已前进；先重新获取 `session` 上下文再重放 |
| `E502_UNSAFE_ASSET_PATH` / `E_PATH_ESCAPE` | 路径试图逃逸 `--workspace-root` 或通过符号链接越界，被沙箱拦截 |
| `E505_IMPORT_NON_MANIFOLD_OR_OPEN` | 导入网格存在开放边或非流形边；修复源网格或显式加 `allow_open_shell = true` 仅作预览 |

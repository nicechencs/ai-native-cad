# Studio 工作台使用指南

[English](../en/workbench-guide.md) | **简体中文** | [文档索引](../README.md)

AI-Native CAD Studio（`apps/studio-ui` + `apps/desktop`）面向参数化机械工程设计，吸收 **Siemens NX** 与 **PTC Creo** 的工业界面范式，并内建 AI 副驾、局部 Point-and-Prompt 与候选分支 `Change Preview` 审阅流。

---

## 1. 界面分区总览

- **顶部文件后台（File Backstage）**：新建 / 打开 / 保存 `.aicad` 工程与 `.kcl` 源码、导入外部 3D 资产（STEP / 3MF / STL / OBJ / glTF）、导出交付件与工作台偏好设置。
- **Ribbon 功能区**：按任务划分为 **7 个常驻页签** 与 **2 个动态上下文页签（草图 / 2D 工程图）**，顶部边框栏（Top Border Bar）提供全局选择过滤与命令搜索。
- **左侧导航栏**：可在 **模型特征树（支持故障节点回退标红与抑制）**、**虚拟装配树（支持拆件选择与爆炸图联动）**、**参数与公式表**、**`.kcl` 代码编辑器（Monaco + LSP）** 之间切换；窄屏下与右侧面板自动互斥折叠以保障 3D 视口面积。
- **中央 3D / 2D 主视口**：
  - 支持 WebGL2 与零拷贝 `CMS2` WebGPU 渲染路径；
  - 集成 **ViewCube** 视角定向、**Mini Toolbar** 浮动快捷条、**QuickPick** 重叠几何消歧弹窗、**右键径向手势菜单（Radial Menu）**、实时剖切平面与装配体爆炸滑块；
  - 可切换 **2D 关联工程图** 视图。
- **右侧按需工具轨**：默认折叠为极窄垂直图标轨（优先保证 3D 画布占比），可按需展开 **AI 副驾面板**、**特征属性与 Explain 溯源面板**、**五层验证面板（Validation Panel）** 与 **导出面板**。

---

## 2. 鼠标导航预设与快捷键

工作台默认采用 **NX / Creo 工业标准鼠标导航**，并允许随时切换为 **SolidWorks** 或 **Web / 触控板兼容** 模式（偏好自动持久化）：

| 操作 | NX / Creo（默认） | SolidWorks 预设 | Web / 触控板预设 |
| :--- | :--- | :--- | :--- |
| **旋转视图** | 按住 **中键 (MMB)** 拖拽 | 按住 **中键 (MMB)** 拖拽 | 左键拖拽或双指滑动 |
| **平移视图** | `Shift + MMB` 或 `RMB + MMB` 拖拽 | `Ctrl + MMB` 拖拽 | 右键拖拽 / `Shift + 左键` |
| **缩放视图** | 滚轮滚动（光标中心缩放） | 滚轮滚动 | 滚轮 / 捏合缩放 |
| **适应窗口** | 双击 **中键 (MMB)** 或 `Ctrl + F` | `F` / 双击中键 | 双击空白 / `Ctrl + F` |
| **点选 / 框选** | 左键 (LMB) 点选与窗口/交叉框选 | 左键点选 | 左键点选 |
| **上下文 / 径向菜单** | 右键短按上下文菜单，长按滑动 8 向径向手势 | 右键上下文菜单 | 右键上下文菜单 |

**4 种切换导航预设的方式**：
1. 底部状态栏的导航预设下拉选择器；
2. `文件` Backstage 偏好设置页；
3. Ribbon `工具与代码` 页签；
4. 底部命令栏输入 `nav nx`、`nav sw` 或 `nav web`。

**常用快捷键**：
- `Ctrl + K`：打开命令面板（Command Palette）
- `Alt + A`：在视口选中面/边附近唤起内联 **Point-and-Prompt** 气泡
- `Ctrl + Shift + A`：展开 / 折叠右侧完整 AI 副驾面板
- `Ctrl + Z` / `Ctrl + Y`：事务级 Undo / Redo（接入 `.aicad` SQLite `oplog`）
- `Ctrl + S`：保存当前工程或导出源码

---

## 3. 特征创建与编辑（Feature Dashboard）

创建基础体素、孔、倒圆角、倒角、抽壳或阵列时，视口顶部会展开水平 **Feature Dashboard**：
- 顶部显示几何收集器（Collector，展示当前选中的面/边 `TopoRef`）与核心参数输入；
- 支持 **应用（Apply，连续创建不关闭）**、**确定（OK）** 与 **取消（Cancel）** 三段操作闭环；
- 所有提交均通过统一的 `Semantic CAD API` 写入文档运行时，并同步更新特征树、`.kcl` 视图与验证报告。

---

## 4. 在工作台中使用 AI 协同与 Change Preview

1. **配置模型提供商**：点击右侧 AI 面板的设置按钮配置 OpenAI 兼容或 Gemini API Key（桌面端与 Web 端均支持多配置 Profile 管理，凭证仅存本地）。
2. **局部 Point-and-Prompt**：在 3D 视口选中一个或多个面/边后按 `Alt + A`（或通过 Mini Toolbar / 径向手势触发），输入局部修改意图（例如“将选中孔径改为 M6 沉头孔”）。工作台会将真实结构化 `TopoRef` 与 `node_id` 传递给后端执行局部闭合切片修改。
3. **候选分支审阅（Change Preview）**：
   - AI 修改（含通过 Live-Attach 挂载的外部 Claude Code / Codex 修改）默认在隔离预览分支上求值，**不会直接覆盖 `main` 分支**；
   - 视口顶部弹出 `ViewportChangePreviewBar`，右侧呈现 `ChangePreviewCard`，对比参数/特征差异与 L0–L4 三值验证结果；
   - 确认无误后点击 **合入（Commit Preview）**，不满意则点击 **丢弃（Discard）**，主分支零污染。

---

## 5. 打开示例模型与导出交付件

- 通过 `文件 -> 打开` 可直接载入仓库内 `examples/aicad-projects/` 下的 `.aicad` 工程或 `examples/mvp-tier1..3/` 下的 `.kcl` 文件；
- 导入外部模型时，开放壳（如 `open_shell.stl`）仅用于预览与测量，不能参与精确布尔；
- 在右侧导出面板可按整件或装配体指定子件导出 `STEP AP242`、`3MF`、`DXF` 与 `SVG` 工程图，面板会清晰标注各格式的精确度与保留边界。

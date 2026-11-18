# 01. 系统总体技术架构（五层全栈设计）

整个平台采用**“前后端分离 + 云边协同 GPU 计算图”**架构，自下而上分为五层：

---

## L1：混合可微几何内核（Hybrid Differentiable Geometry Kernel）

抛弃单一内核限制，采用**双轨几何同步底座**（C++20 / Rust + CUDA 实现）：

### 1. 精确解析轨（Exact B-Rep Track）
* **职责**：承载配合面、螺纹孔、基准轴、密封槽等微米级公差特征。
* **落地选型**：初期封装开源 **OpenCASCADE (OCCT)** 或基于 Rust 生态改造 **Truck** 内核，聚焦解析几何构建、精确布尔与 STEP AP214/AP242 读写模块。

### 2. 连续隐式轨（Implicit SDF Track）
* **职责**：承载结构主体、平滑过渡圆角、TPMS 晶格、流道与拓扑生长区。
* **落地选型**：基于 **NVIDIA Warp** 或 **Taichi / OpenVDB** 构建稀疏体素八叉树（Sparse Voxel Octree）上的有向距离场计算图。
* **核心优势**：支持毫秒级平滑布尔（Smooth Min/Max）、等距抽壳（Offset $f(x) - t$，数学上永不自交崩溃）与空间域折叠阵列。

### 3. 双轨桥接算子（Hybrid Bridge）
* 将精确 B-Rep 特征实时转换为精确解析距离函数（Exact Analytic SDF）参与全局场运算。
* 在导出阶段，锁定配合区的原始 B-Rep 拓扑面不被网格化污染，仅对非配合自由区进行曲面重构并拓扑缝合。

---

## L2：声明式代码中间层与双向求解器（Code-as-IR & Constraint Solver）

连接“人类精确操作”与“AI 大模型推理”的中枢神经，彻底取代脆弱的传统二进制历史树：

### 1. 专属建模语言（CAD-DSL）
* 强类型、声明式领域特定语言（语法贴近 Rust / Python 子集）。
* **消除拓扑命名难题（Topological Naming Problem）**：原生支持拓扑语义查询器（例如 `body.faces().filter(normal=Z_AXIS).top()`），取代传统 CAD 极易断裂的内部面 ID 索引。

### 2. 可微几何约束求解器（Differentiable GCS）
* 解算 2D 草图与 3D 装配中的**相切、同心、平行、垂直、等距、对称**约束。
* 将几何约束转化为残差能量函数 $E(\theta) = \sum w_i r_i(\theta)^2$，基于 **Ceres Solver** 或 **LibTorch** 实现带正则化阻尼的 Levenberg-Marquardt 二阶优化器，支持过约束诊断高亮与防翻转能量锁定。

### 3. 双向 AST 同步引擎（Bi-directional AST Sync）
* **代码 -> 视口**：AI 修改代码 -> 增量重算抽象语法树（AST）节点 -> 50ms 内局部刷新 3D 视口。
* **视口 -> 代码**：人类在 3D 视口拖拽控制柄或点选两个面添加“相切” -> 反向解析拾取路径 -> 自动定位并改写 DSL 代码字面量或插入约束语句。

---

## L3：内嵌式实时物理与可制造性引擎（Real-time Physics & DFM）

将 CAE 和 CAM 规则写成可微算子内嵌进 L1 内核：

### 1. 双精度物理求解管线
* **交互探索态（毫秒级）**：集成基于 **FNO（傅里叶神经算子）** 或 **MeshGraphNets** 的代理模型，实时渲染应力云图、温度场与流体压降。
* **工程校核态（秒级定型）**：采用**浸入边界法（Immersed Boundary Method）或笛卡尔网格有限元（Voxel/CutCell FEA，结合 NVIDIA AmgX 求解器）**，跳过繁琐的四面体网格划分，直接在 SDF 场上完成线弹性静力学、模态与热传导复核。

### 2. 可制造性硬约束惩罚项（DFM Differentiable Losses）
* **增材制造（3D 打印）**：悬垂角算子 $\nabla f \cdot \vec{d}_{\text{build}} < \cos\theta_{\max}$，自动生长自支撑角度。
* **减材制造（CNC 机加工）**：基于光线投射（Ray-casting）与形态学运算的 3 轴 / 5 轴刀具可达性体积（Accessibility Volume）及最小内角刀具半径检查。
* **注塑与铸造**：拔模斜度场检查与基于中轴变换（Medial Axis）的壁厚均匀度分析。

---

## L4：多智能体协同层（Multi-Agent AI Orchestration）

构建闭环工程 Agent 工作流，杜绝端到端黑盒幻觉：

1. **Spec Parser（规格解析 Agent）**：将自然语言、手绘草图、2D 旧图纸转化为结构化 JSON《工程设计契约》（包络空间、接口坐标、材料牌号、载荷工况、制造工艺）。
2. **CAD Coder（几何架构师 Agent）**：经 CAD-DSL 微调（SFT + GRPO 强化学习）的代码大模型，模块化编写骨架与配合特征代码。
3. **Visual-Geometric Critic（视觉与几何自检闭环）**：后台无头渲染器自动拍摄正交多视图 + 剖视图 + 干涉高亮图，连同编译器断言、干涉体积与应力极值反馈给多模态大模型（VLM），自主迭代直至全部硬指标达标。

---

## L5：现代混合前端工作台（Modern Hybrid UI）

* **渲染引擎**：基于 **WebGPU（桌面端 wgpu/Dawn 原生封装 + 浏览器云端协同）**，混合 Raymarching（渲染隐式场）与硬件光栅化（渲染精确 B-Rep 边线与 PMI 标注）。
* **三栏工作台**：左侧“自然语言意图 + 工程约束面板 + 实时 DSL 代码树”，中间“交互式 3D 视口（保留传统点选、推拉、测量体验）”，右侧“实时质量/成本/安全系数仪表盘”。

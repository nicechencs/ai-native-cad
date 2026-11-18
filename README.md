# AI-Native 3D CAD Architecture & Blueprint

> **面向未来的工业级 AI 原生 3D 设计引擎技术白皮书与研发落地蓝图**  
> 打破传统参数化特征历史树（Feature-based History Tree）桎梏，融合**混合可微几何内核 + 神经物理求解器 + 声明式代码中间层（Code-as-IR）**，同时保持与现代制造业标准（STEP / 3MF / OpenUSD）的无损双向兼容。

---

## 核心设计哲学

PTC Creo、Siemens NX 与 SolidWorks 诞生于 1980 年代末的参数化特征历史树范式，本质是面向人类鼠标点击优化的串行绘图板。面向 AI 原生时代的工业 3D 软件应当遵循**“前端交互与中间表达彻底重构，底层工程几何确定性严格保留”**：

1. **交互范式重构**：从“步骤驱动（How）”转向“意图、接口约束与制造工艺驱动（What）”。
2. **双轨混合几何内核**：精确装配接口采用解析 B-Rep 锁定微米级公差；结构主体、流道与复杂过渡采用 GPU 连续有向距离场（SDF），实现 $O(1)$ 永不失败的布尔合并、圆角融合与域折叠阵列。
3. **代码作为中间表示（Code-as-IR）**：以强类型声明式建模语言（CAD-DSL）与双向几何约束求解器取代黑盒二进制特征树，天然支持大模型读写、视口双向同步以及 `git diff / merge` 协同版本控制。
4. **设计即仿真（CAD/CAE/CAM 融合）**：内嵌傅里叶神经算子（FNO）、GPU 无网格有限元（Voxel FEA）与 CNC/增材制造可微工艺惩罚项，实时闭环校核。
5. **工业生态双向兼容**：通过几何深度学习（UV-Net / BRepNet）逆向解析外部 STEP 哑几何；通过场对齐四边面重拓扑与 SubD-to-NURBS 精确缝合导出干净工业 STEP。

---

## 仓库目录结构

```text
ai-native-cad-blueprint/
├── README.md                          # 项目总览与快速导航
├── docs/
│   ├── 01-architecture.md             # 五层全栈系统架构设计详解
│   ├── 02-compatibility-pipeline.md   # STEP/3MF/OpenUSD 双向兼容与几何操作实现
│   ├── 03-roadmap-and-team.md         # 24个月研发里程碑、合成数据工程与团队配置
│   └── 04-references.md               # 核心学术文献与行业参考项目索引
├── dsl-spec/
│   └── bearing_mount_example.kcl      # 声明式 CAD-DSL 语法原型示例（带相切筋与阵列的轴承座）
└── .github/
    └── ISSUE_TEMPLATE/
        └── milestone_task.md          # 研发任务跟踪 Issue 模板
```

---

## 24 个月分阶段演进概览

| 阶段 | 周期 | 核心里程碑 | 目标切入场景 |
| :--- | :--- | :--- | :--- |
| **Phase 1: MVP 闭环** | M1 - M6 | CAD-DSL v0.1 + 精确 B-Rep 编译 + WebGPU 双向视口 + VLM 自检 Agent | 非标自动化夹具、标准支架、创客与机器人结构件 |
| **Phase 2: 混合场与物理** | M7 - M14 | GPU SDF 隐式内核 + Voxel FEA 实时力热校核 + Quad-NURBS 导出管线 | 无人机轻量化机架、人形机器人构件、液冷散热板 |
| **Phase 3: 系统协同** | M15 - M24 | 3D 装配体运动学求解 + 标准件库自动选型 + OpenUSD 仿真直通 | 精密机电组件、具身智能本体研发、企业级 Git-PLM |

---

## 快速上传至你的 GitHub 仓库

解压本仓库压缩包后，在终端执行以下命令即可一键推送到你的 GitHub：

```bash
cd ai-native-cad-blueprint
git init
git add .
git commit -m "feat: initial commit of AI-Native 3D CAD architecture blueprint"
git branch -M main

# 方式 A：如果已安装 GitHub CLI (gh)
gh repo create ai-native-cad-blueprint --private --source=. --remote=origin --push

# 方式 B：手动关联你在网页端新建的空仓库
# git remote add origin https://github.com/<你的用户名>/ai-native-cad-blueprint.git
# git push -u origin main
```

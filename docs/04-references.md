# 04. 核心学术文献与行业先驱参考索引

本架构蓝图建立在计算机图形学、几何深度学习、科学机器学习（SciML）与现代工业软件先驱的扎实成果之上：

## 1. B-Rep 几何深度学习与特征逆向识别
* **BRepNet (CVPR 2021)** — Lambourne et al. (Autodesk AI Lab), *"BRepNet: A topological message passing system for solid models"*. 直接在 B-Rep 面、边、半边拓扑结构上进行消息传递，精准识别加工特征。
* **UV-Net (CVPR 2021)** — Jayaraman et al. (Autodesk AI Lab), *"UV-Net: Learning from Boundary Representations"*. 融合曲面 UV 参数域网格与拓扑邻接图表征。
* **DeepCAD (ICCV 2021)** — Wu, Xiao, Zheng (Columbia University), *"A Deep Generative Network for Computer-Aided Design Models"*.

## 2. 隐式场（SDF）、混合内核与可微 CAD
* **DeepSDF (CVPR 2019)** — Park et al., *"Learning Continuous Signed Distance Functions for Shape Representation"*.
* **Cascaval et al. (MIT CSAIL)** — 可微参数化 CAD 与计算制造协同优化系列研究（Wojtek Matusik 团队）。
* **nTop (nTopology)** — 工业级 GPU 隐式场驱动设计（Field-driven Design）商业先驱，验证了精确 B-Rep 接口与复杂隐式晶格/流道的工程融合。

## 3. 代码作为中间表示（Code-as-IR / CAD-DSL）
* **CAD-Recode / Query2CAD / CADCodeVerify (2023–2025)** — 验证以结构化 CAD 脚本结合多视角视觉反馈自检（VLM Self-Refinement）生成精密实体模型的优越性。
* **Zoo.dev (KittyCAD) & KCL** — 现代 GPU 几何引擎与专为 AI/人类协同设计的声明式建模语言实践。
* **Build123d / CadQuery** — 基于 Python 与 OpenCASCADE 拓扑选择器的现代声明式代码建模开源框架。

## 4. 神经物理求解器与实时仿真（SciML）
* **FNO (ICLR 2021)** — Li et al. (Caltech), *"Fourier Neural Operator for Parametric Partial Differential Equations"*. 分辨率无关的超高速偏微分方程求解（NVIDIA PhysicsNeMo 核心基础）。
* **MeshGraphNets (ICLR 2021)** — Pfaff et al. (DeepMind), *"Learning Mesh-Based Simulation with Graph Networks"*.
* **PINNs (JCP 2019)** — Raissi, Perdikaris, Karniadakis, *"Physics-informed neural networks"*.

## 5. 场对齐四边面重拓扑与曲面重构
* **Instant Field-Aligned Meshes (SIGGRAPH Asia 2015)** — Jakob, Tarini, Panozzo, Sorkine-Hornung (ETH Zurich). 将离散网格转化为流线对齐纯四边面网格并进一步拟合为 G2 NURBS 曲面的核心算法基础。

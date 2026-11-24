# 学术文献与行业参考

[English](../en/references.md) | **简体中文** | [文档索引](../README.md)

AI-Native CAD 的架构设计建立在计算几何、几何深度学习、编程语言双向变换、科学机器学习（SciML）与现代开源/工业 CAD 工程实践的公开成果之上：

## 1. B-Rep 几何深度学习与特征识别
- **BRepNet (CVPR 2021)** — Lambourne et al. (Autodesk AI Lab), *"BRepNet: A topological message passing system for solid models"*. 在 B-Rep 面、边与共边（Coedge）拓扑结构上执行消息传递以识别加工特征。
- **UV-Net (CVPR 2021)** — Jayaraman et al. (Autodesk AI Lab), *"UV-Net: Learning from Boundary Representations"*. 融合曲面 UV 参数域采样网格与拓扑邻接图表征。
- **DeepCAD (ICCV 2021)** — Wu, Xiao, Zheng (Columbia University), *"A Deep Generative Network for Computer-Aided Design Models"*.

## 2. 隐式场（SDF）、可微着色器与混合几何
- **DeepSDF (CVPR 2019)** — Park et al., *"Learning Continuous Signed Distance Functions for Shape Representation"*.
- **Slang.D / Shader Slang (SIGGRAPH 2023/2024)** — Bangaru et al., *"SLANG.D: Fast, Modular and Differentiable Shader Programming"*.
- **Cascaval et al. (MIT CSAIL)** — 可微参数化 CAD 与计算制造协同优化系列研究。
- **nTop (nTopology)** — 工业级隐式场驱动设计（Field-driven Design）行业先驱，展示了精确 B-Rep 接口与隐式晶格/流道融合的工程价值。

## 3. 代码作为中间表示（Code-as-IR）与双向程序变换
- **Sketch-n-Sketch (POPL / UIST)** — Chugh, Hempel et al. (University of Chicago), *"Programmatic and Direct Manipulation, Together at Last"*. 奠定了直接图形交互与源代码 AST 受控双向同步的理论基础。
- **Zoo.dev (KittyCAD) & KCL** — 现代声明式建模语言与 Rust 工具链实践参考。
- **Build123d / CadQuery** — 基于拓扑语义选择器缓解传统拓扑命名难题（TNP）的开源参数化建模先驱。
- **CAD-Recode / Query2CAD / CADCodeVerify (2024–2025)** — 验证了结构化 CAD 代码结合多视角视觉自检与确定性几何门禁的有效性。

## 4. 神经物理求解器与无网格有限元（SciML & CAE）
- **FNO (ICLR 2021)** — Li et al. (Caltech), *"Fourier Neural Operator for Parametric Partial Differential Equations"*.
- **MeshGraphNets (ICLR 2021)** — Pfaff et al. (DeepMind), *"Learning Mesh-Based Simulation with Graph Networks"*.
- **CutFEM / Immersed Boundary FEA** — Burman et al., *"CutFEM: Discretizing geometry and partial differential equations"*.

## 5. 场对齐四边面重拓扑与曲面重构
- **Instant Field-Aligned Meshes (SIGGRAPH Asia 2015)** — Jakob, Tarini, Panozzo, Sorkine-Hornung (ETH Zurich).
- **QuadriFlow (Eurographics / CGF 2018)** — Huang et al. (Stanford), 引入全局最小费用流约束改善四边面流形质量与细分曲面重构稳定性。

# Academic & Industry References

**English** | [简体中文](../zh/references.md) | [Docs Index](../README.md)

The architecture of AI-Native CAD builds upon foundational research and open engineering work across computational geometry, geometric deep learning, bidirectional programming, scientific machine learning, and modern CAD systems:

## 1. B-Rep Geometric Deep Learning & Feature Recognition
- **BRepNet (CVPR 2021)** — Lambourne et al. (Autodesk AI Lab), *"BRepNet: A topological message passing system for solid models"*. Topological message passing over B-Rep faces, edges, and coedges for machining feature recognition.
- **UV-Net (CVPR 2021)** — Jayaraman et al. (Autodesk AI Lab), *"UV-Net: Learning from Boundary Representations"*. Combining UV-domain surface grids with B-Rep adjacency graphs.
- **DeepCAD (ICCV 2021)** — Wu, Xiao, Zheng (Columbia University), *"A Deep Generative Network for Computer-Aided Design Models"*.

## 2. Implicit Fields (SDFs), Differentiable Shaders & Hybrid Geometry
- **DeepSDF (CVPR 2019)** — Park et al., *"Learning Continuous Signed Distance Functions for Shape Representation"*.
- **Slang.D / Shader Slang (SIGGRAPH 2023/2024)** — Bangaru et al., *"SLANG.D: Fast, Modular and Differentiable Shader Programming"*.
- **Cascaval et al. (MIT CSAIL)** — Differentiable parametric CAD and computational fabrication research.
- **nTop (nTopology)** — Industrial field-driven implicit geometry combined with exact manufacturing interfaces.

## 3. Code-as-IR & Bidirectional Program Manipulation
- **Sketch-n-Sketch (POPL / UIST)** — Chugh, Hempel et al. (University of Chicago), *"Programmatic and Direct Manipulation, Together at Last"*. Theoretical foundation for output-directed bidirectional AST synchronization.
- **Zoo.dev (KittyCAD) & KCL** — Modern declarative CAD language and Rust tooling inspiration.
- **Build123d / CadQuery** — Open-source semantic topological selector pioneers addressing the Topological Naming Problem (TNP).
- **CAD-Recode / Query2CAD / CADCodeVerify (2024–2025)** — Programmatic CAD generation combined with deterministic geometric checks and multi-view visual feedback.

## 4. Neural Physics & Immersed-Boundary FEA (SciML & CAE)
- **FNO (ICLR 2021)** — Li et al. (Caltech), *"Fourier Neural Operator for Parametric Partial Differential Equations"*.
- **MeshGraphNets (ICLR 2021)** — Pfaff et al. (DeepMind), *"Learning Mesh-Based Simulation with Graph Networks"*.
- **CutFEM / Immersed Boundary FEA** — Burman et al., *"CutFEM: Discretizing geometry and partial differential equations"*.

## 5. Field-Aligned Quad Remeshing & Surface Reconstruction
- **Instant Field-Aligned Meshes (SIGGRAPH Asia 2015)** — Jakob, Tarini, Panozzo, Sorkine-Hornung (ETH Zurich).
- **QuadriFlow (Eurographics / CGF 2018)** — Huang et al. (Stanford), scalable manifold quad remeshing with minimum-cost flow singularity constraints.

# Sample Models & Examples / 示例模型库

**English** | **简体中文** | [Docs Index / 文档总览](../docs/README.md)

This directory contains curated, ready-to-run mechanical models, external import fixtures, V2 `.aicad` engineering containers, and Python SDK scripts. All included examples are verified to work out of the box with `cad-cli`.

本目录提供开箱即用的参数化机械模型、外部格式导入样例、V2 `.aicad` 示例工程与 Python SDK 脚本，均可直接使用 `cad-cli` 或 Studio 工作台运行与检查。

---

## Directory Guide / 示例分类

| Directory / 目录 | Format / 形态 | Count | Highlights / 涵盖内容 |
| :--- | :---: | :---: | :--- |
| [`aicad-projects/`](aicad-projects/README.md) | V2 `.aicad` 目录容器 | 2 | 参数化安装底板（含 M6 ISO 4762 沉头孔与 `DesignContract`）、阶梯法兰轴套；附确定性生成器 `generate_samples.py` |
| `mvp-tier1-plates/` | `.kcl` 零件 | 5 | `01_mounting_plate`、`02_circular_flange`、`03_sensor_spacer`、`04_slotted_rail_plate`、`05_bearing_end_cap`（基础板件/法兰/孔阵列/PMI/DFM 断言；其中 `05_bearing_end_cap` 的布尔并集倒角需 `--features occt-ffi`） |
| `mvp-tier2-brackets/` | `.kcl` 零件 | 5 | `06_l_sensor_bracket`、`07_u_clevis_mount`、`08_nema17_motor_mount`、`09_ribbed_shaft_support`、`10_gripper_link_arm`（草图约束/加强筋/多特征布尔） |
| `mvp-tier3-advanced/` | `.kcl` 零件与装配 | 3 | `11_shelled_control_box`（抽壳 + M4 底孔）、`12_swept_manifold_pipe`（扫掠/放样）、`13_bearing_housing_assembly`（两件轴承座装配与配合） |
| [`mvp-tier4-import/`](mvp-tier4-import/README.md) | `.kcl` + STEP/3MF/STL | 6 | 3MF OPC 寻址、开放 STL 拦截与预览分流、英寸制 STEP、装配偏移 STEP 与 NEMA17 电机 STEP；附 `generate_fixtures.py --check` |
| [`developer-platform/`](developer-platform/README.md) | Python (`aicad` SDK) | 3 | 查询测量、乐观锁改参提交、候选分支预览 diff 与 `commit_preview` 合入 |

---

## Quick Try Commands / 快速体验命令

First build `cad-cli` from the repository root (`cargo build -p cad-cli --locked`), then run:

在仓库根目录执行 `cargo build -p cad-cli --locked` 后，可直接运行以下命令：

```sh
# 1. Check a Tier 1 parametric mounting plate / 检查基础安装底板
cargo run -p cad-cli -- check examples/mvp-tier1-plates/01_mounting_plate.kcl --json

# 2. Export STEP, SceneMesh, 1:1 DXF, and 2D drawing SVG from a two-part assembly / 导出两件装配体
cargo run -p cad-cli -- build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --out-step tmp_out/bearing_asm.step \
  --out-mesh tmp_out/bearing_asm.mesh.json \
  --out-dxf tmp_out/bearing_asm.dxf \
  --out-drawing tmp_out/bearing_asm.drawing.svg \
  --json

# 3. Inspect a V2 .aicad project and emit its .kcl 2.0 view / 查看 V2 .aicad 工程并投影为 .kcl 2.0
cargo run -p cad-cli -- project inspect examples/aicad-projects/01_mounting_plate.aicad --json
cargo run -p cad-cli -- project emit-kcl examples/aicad-projects/01_mounting_plate.aicad

# 4. Pack a directory .aicad to single-file SQLite and verify L0–L4 / 打包为单文件 SQLite 并验证 L0–L4
cargo run -p cad-cli -- project pack examples/aicad-projects/01_mounting_plate.aicad --out tmp_out/01_mounting_plate.aicad
cargo run -p cad-cli -- verify --workspace tmp_out --project tmp_out/01_mounting_plate.aicad --json

# 5. Inspect a pure-Rust 3MF fixture / 检查纯 Rust 3MF 导入样例
cargo run -p cad-cli -- inspect examples/mvp-tier4-import/assets/custom_rel.3mf --json

# 6. Run an end-to-end Python SDK example / 运行 Python SDK 示例
python -X utf8 examples/developer-platform/01_inspect_and_measure.py
```

For native OCCT STEP import examples (`nema17_mm.kcl`, `collar_inch.kcl`, `two_part_offset_asm.kcl`), enable `--features occt-ffi` as described in [Getting Started](../docs/en/getting-started.md) / [快速开始与构建指南](../docs/zh/getting-started.md).

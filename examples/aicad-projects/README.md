# V2 `.aicad` Sample Projects / V2 `.aicad` 示例工程

**English** | **简体中文** | [All Examples / 示例总览](../README.md)

These unpacked Git-friendly `.aicad` directory containers demonstrate the V2 `Canonical Graph IR`, `DesignContract`, parametric dimensions, standard metric fastener holes (`iso4762_counterbore`), boolean unions, and `.kcl 2.0` projection.

本目录提供 Git 友好的目录态 `.aicad` 示例工程，展示 V2 `Canonical Graph IR`、`DesignContract`、参数驱动、ISO 4762 标准沉头孔、布尔并集与 `.kcl 2.0` 双向投影。

---

## Included Projects / 包含样例

| Project / 工程目录 | Description / 说明 |
| :--- | :--- |
| `01_mounting_plate.aicad/` | 120 × 80 × 12 mm Al-6061-T6 parametric mounting plate with a Ø28 mm center pilot bore and four M6 ISO 4762 counterbored corner holes. |
| `02_flanged_collar.aicad/` | Stepped flanged bearing collar combining a Ø72 mm flange disc and Ø44 mm hub via boolean union with a Ø20 mm through bore. |

## Usage / 使用方法

```sh
# Inspect metadata and graph node/parameter counts / 查看容器元数据与图结构
cargo run -p cad-cli -- project inspect examples/aicad-projects/01_mounting_plate.aicad --json

# Project to .kcl 2.0 / 投影为 .kcl 2.0 代码视图
cargo run -p cad-cli -- project emit-kcl examples/aicad-projects/01_mounting_plate.aicad

# Pack into a single-file SQLite .aicad (for Studio File->Open, branching, or cad-cli verify)
cargo run -p cad-cli -- project pack examples/aicad-projects/01_mounting_plate.aicad --out tmp_out/01_mounting_plate.aicad
cargo run -p cad-cli -- verify --workspace tmp_out --project tmp_out/01_mounting_plate.aicad --json

# Verify deterministic regeneration / 校验样本目录与生成器完全一致
python -X utf8 examples/aicad-projects/generate_samples.py --check
```

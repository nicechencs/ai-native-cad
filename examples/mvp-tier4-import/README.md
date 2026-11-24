# External 3D Model Import Examples / 外部模型导入示例

**English** | **简体中文** | [All Examples / 示例总览](../README.md)

These synthetic fixtures demonstrate controlled external CAD/mesh import, unit normalization, SHA-256 content pinning, open-shell handling, and multi-part STEP assemblies. Run `python -X utf8 examples/mvp-tier4-import/generate_fixtures.py --check` to verify that all assets match the pinned SHA-256 digests in the `.kcl` files.

本目录提供确定性合成的外部模型导入样例，用于演示沙箱路径约束、单位自动归一化、SHA-256 内容锁定、开放壳预览分流与多组件 STEP 装配导入。运行 `python -X utf8 examples/mvp-tier4-import/generate_fixtures.py --check` 可核对资产字节与 `.kcl` 中锁定的 SHA-256 摘要完全一致。

---

## Sample Matrix / 样例清单

| Example / 样本 | Focus / 演示重点 | Backend / 所需后端 |
| :--- | :--- | :---: |
| `custom_rel.kcl` | 3MF resolves `3D/parts/main.model` via OPC root relationships and normalizes source units to `mm`. | Default Pure Rust |
| `open_shell.kcl` | Open STL is rejected by default with `E505_IMPORT_NON_MANIFOLD_OR_OPEN`. | Default Pure Rust |
| `open_shell_preview.kcl` | Sets `allow_open_shell = true` to allow preview mesh export while reporting `watertight: false`. | Default Pure Rust |
| `collar_inch.kcl` | Inch-unit STEP normalized to `25.4 mm` outer diameter. | `occt-ffi` |
| `two_part_offset_asm.kcl` | Two-part STEP assembly with 90° Z rotation and offset placement (Part B world bbox `[5,20,5]`..`[10,22,8] mm`). | `occt-ffi` |
| `nema17_mm.kcl` | Millimetre STEP motor profile with four Ø3 mm mounting holes and a Ø22 mm pilot boss. | `occt-ffi` |

---

## Try It / 运行示例

### Pure Rust (3MF & Open-Shell Preview)

```sh
cargo run -p cad-cli -- inspect examples/mvp-tier4-import/assets/custom_rel.3mf --json
cargo run -p cad-cli -- build examples/mvp-tier4-import/custom_rel.kcl \
  --workspace-root examples/mvp-tier4-import \
  --out-mesh tmp_out/custom_rel.mesh.json \
  --json

# Expected: watertight = false (preview-only open shell)
cargo run -p cad-cli -- build examples/mvp-tier4-import/open_shell_preview.kcl \
  --workspace-root examples/mvp-tier4-import \
  --out-mesh tmp_out/open_shell.mesh.json \
  --json
```

### Native OCCT (`--features occt-ffi`)

```sh
./scripts/run-cad-cli-occt.sh -- inspect examples/mvp-tier4-import/assets/nema17_mm.step --json
./scripts/run-cad-cli-occt.sh -- build examples/mvp-tier4-import/two_part_offset_asm.kcl \
  --workspace-root examples/mvp-tier4-import \
  --out-mesh tmp_out/two_part_asm.mesh.json \
  --json
```

For Windows PowerShell, substitute `./scripts/run-cad-cli-occt.ps1`. Setup instructions: [Getting Started](../../docs/en/getting-started.md) / [快速开始与构建指南](../../docs/zh/getting-started.md).

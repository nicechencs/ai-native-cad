# CAD-DSL (`.kcl`) 语言与算子参考

[English](../en/cad-dsl-reference.md) | **简体中文** | [文档索引](../README.md)

CAD-DSL（文件后缀 `.kcl`）是面向参数化机械建模的受限声明式语言，既是独立可读的文本建模格式，也是 `Canonical Graph IR` 的 1:1 双向无损投影视图（由 `crates/cad-kcl-bridge` 与 `crates/cad-syntax` / `cad-hir` 承载）。

---

## 1. 语言设计原则

- **静态可分析与确定性终止**：无任意循环、无隐式副作用或外部网络 I/O，任意合法文件确定性求值为唯一几何 DAG。
- **编译期强物理量纲**：内置 `Length`（`mm`, `m`, `in`）、`Angle`（`deg`, `rad`）、`Force`（`N`, `kN`）、`Pressure`（`MPa`）等一等公民量纲类型；混用不兼容量纲（如 `10mm + 5deg`）直接报错 `E102_UNIT_MISMATCH`。
- **确定性拓扑消歧**：要求单面/单边时若匹配多候选实体，编译器直接报错 `E201_AMBIGUOUS_TOPOLOGY`，必须通过 `.max_by(...)` / `.min_by(...)` / `.then_by(...)` 显式消歧。

---

## 2. 顶层结构与示例

```kcl
@unit(length = "mm", angle = "deg", force = "N")
@material("Al-6061-T6")
@manufacturing(method = "CNC_3AXIS_MILLING", min_tool_radius = 2.0mm)

part Plate(
    width: Length = 80.0mm,
    depth: Length = 40.0mm,
    thickness: Length = 10.0mm,
    bore_d: Length = 16.0mm @pmi::tolerance("H7", roughness = "Ra1.6")
) -> Solid {
    let base = box(size = [width, depth, thickness], origin = [-width / 2.0, -depth / 2.0, 0.0mm])
        |> fillet(r = 4.0mm)
    let bore = cylinder(d = bore_d, h = thickness + 2.0mm, origin = [0.0mm, 0.0mm, -1.0mm], exact_brep = true)
        |> tag::cylindrical_faces("center_bore")
    let final_part = base |> boolean::subtract(tools = [bore], preserve_exact_interfaces = true)
    assert::bbox(target = final_part, max_size = [90.0mm, 50.0mm, 15.0mm])
    return final_part
}

assembly StackedPlates() -> Assembly {
    let base = component(part = Plate, origin = [0mm, 0mm, 0mm])
    let cap = component(part = Plate(width = 60mm), origin = [0mm, 0mm, 10mm], rotation = [0deg, 0deg, 0deg])
    mate::coaxial(a = base, b = cap, axis = Z_AXIS)
    mate::coincident(a = base, b = cap, offset = 0mm)
    return [base, cap]
}
```

### 坐标系与基准常量（全仓统一）
- 右手坐标系、Z 轴朝上；内部规范化长度单位为 `mm`，角度为 `deg`。
- 方向常量：`+X`, `-X`, `+Y`, `-Y`, `+Z`, `-Z`
- 坐标轴常量：`X_AXIS`, `Y_AXIS`, `Z_AXIS`
- 基准平面：`XY_PLANE`（`Z=0`）、`XZ_PLANE`（`Y=0`）、`YZ_PLANE`（`X=0`）
- 管道流算子 `|>` 将前序几何体作为第一隐式主语传给后续特征、打标或 PMI 算子。

---

## 3. 2D 闭合草图与几何约束（`cad-gcs`）

拉伸、旋转、扫掠截面必须显式闭合（未闭合或自交报错 `E204_OPEN_SKETCH_REGION`）；扫掠路径可使用 `region::path([...])`：

| 类别 | 算子 | 说明 |
| :--- | :--- | :--- |
| **草图块** | `sketch(plane = ...)` | 接受基准平面或唯一消歧的平面拓扑查询 |
| **图元** | `point`, `line`, `arc`, `circle`, `rectangle` | 2D 局部坐标图元 |
| **轮廓声明** | `region::closed([...])` / `region::path([...])` | 闭合成形截面 / 开放扫掠路径 |
| **硬约束** | `constraint::coincident`, `horizontal`, `vertical`, `parallel`, `perpendicular`, `tangent`, `distance`, `radius` | 由阻尼 LM + 同伦防翻转求解器解算并报告 DOF |

---

## 4. 3D 建模、孔特征、布尔与装配算子

| 类别 | 算子 | 关键说明 |
| :--- | :--- | :--- |
| **基础体素** | `box(size = [w, d, h], origin = [x, y, z])`<br>`cylinder(d = ..., h = ..., origin = [...], axis = Z_AXIS)` | `box` 的 `origin` 为最小角点；`cylinder` 的 `origin` 为底面圆心，默认沿 `+Z` 拉伸 |
| **草图成形** | `extrude(height = ..., direction = +Z)`<br>`extrude_symmetric(thickness = ...)`<br>`revolve(axis = ..., angle = 360deg)` | 由闭合草图生成 3D 实体 |
| **扫掠与放样** | `sweep(profile = ..., path = ...)`<br>`loft(sections = [...], height = ...)` | 沿路径或截面列构面成形 |
| **修饰特征** | `fillet(r = ...)`、`chamfer(d = ...)`<br>`shell(thickness = ..., open_faces = [...])`<br>`mirror(plane = YZ_PLANE, merge = false)` | 越界或非正半径/壁厚分别报 `E205_INVALID_FILLET_CHAMFER` / `E206_INVALID_SHELL_THICKNESS` |
| **孔特征** | `hole::simple(...)`、`hole::counterbore(...)`<br>`hole::tapped(spec = "M6x1.0-6H", depth = ..., ...)` | `hole::tapped` 查表生成底孔并附加螺纹标注，不建模实体螺旋牙形 |
| **阵列与布尔** | `array::linear(...)`、`array::polar(...)`<br>`boolean::union(...)`、`boolean::subtract(tools = [...], preserve_exact_interfaces = true)` | 支持管道流组合与端面内孔环标签传递 |
| **装配与配合** | `component(part = ..., origin = [...], rotation = [...])`<br>`mate::coaxial(...)`、`mate::coincident(...)` | 实例化子零件生成 BOM 并校核/求解装配配合 |

---

## 5. 拓扑查询、标签溯源、受控导入与断言

### 拓扑查询与打标
- 集合过滤：`.faces()`、`.edges()`、`.planar()`、`.cylindrical()`、`.filter(normal = +Z, angle_tol = 1deg)`、`.tagged("...")`
- 确定性消歧：`.max_by(coord = Z)`、`.min_by(coord = Z)`、`.max_by(radius)`、`.min_by(radius)`、`.then_by(coord = X)`
- 标签注入与 PMI：`tag::faces(...)`、`tag::cylindrical_faces(...)`、`pmi::datum("A", flatness = 0.02mm)`、`pmi::perpendicularity(...)`

### 受控外部资产导入（工作区沙箱）
```kcl
let motor = import::step(
    path = "assets/nema17_mm.step",
    sha256 = "3627a7a9af6fa13d5a20f9e69271c8843d260089052489eb3f20942dabc45e59",
    heal = false
)
let scan = import::mesh(path = "assets/open_shell.stl", allow_open_shell = true)
```
- 路径必须相对 `--workspace-root`，越界路径或符号链接逃逸报错 `E502_UNSAFE_ASSET_PATH`；
- 校验 `sha256` 不匹配报 `E503_ASSET_HASH_MISMATCH`；非水密开放壳默认报 `E505_IMPORT_NON_MANIFOLD_OR_OPEN`（设 `allow_open_shell = true` 仅限预览测量）；
- 无历史实体的通孔直编支持 `face::offset`、`face::move` 与 `face::delete`（每次编辑返回新实体，下一步须重新选面）。

### 工程断言
- `assert::bbox(target = ..., max_size = [x, y, z])`
- `assert::dfm_accessible(target = ..., axes = [+Z, -Z], min_wall_thickness = ...)`

完整可运行 `.kcl` 示例请浏览 [examples/README.md](../../examples/README.md)。

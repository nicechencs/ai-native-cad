# CAD-DSL (`.kcl`) Language & Operator Reference

**English** | [简体中文](../zh/cad-dsl-reference.md) | [Docs Index](../README.md)

CAD-DSL (`.kcl`) is a restricted, strongly dimensional declarative modeling language for parametric mechanical design. It works both as a standalone human-readable source format and as the 1:1 lossless projection view of `Canonical Graph IR` (`crates/cad-kcl-bridge`, `crates/cad-syntax`, `crates/cad-hir`).

---

## 1. Core Language Principles

- **Deterministic Termination**: No unbounded loops, reflection, or arbitrary I/O; every valid source deterministically evaluates to a unique geometry DAG.
- **Compile-Time Dimensional Safety**: First-class `Length` (`mm`, `m`, `in`), `Angle` (`deg`, `rad`), `Force` (`N`, `kN`), and `Pressure` (`MPa`) types. Mixing incompatible dimensions (e.g., `10mm + 5deg`) fails statically with `E102_UNIT_MISMATCH`.
- **Deterministic Topology Tie-Breaking**: Whenever an operator requires a single face or edge and multiple candidates match, compilation fails with `E201_AMBIGUOUS_TOPOLOGY` until disambiguated via `.max_by(...)`, `.min_by(...)`, or `.then_by(...)`.

---

## 2. Top-Level Structure & Example

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

### Coordinate Frame & Built-in Constants
- Right-handed, **Z-up** frame; canonical internal units are `mm` and `deg`.
- Directions: `+X`, `-X`, `+Y`, `-Y`, `+Z`, `-Z`
- Axes: `X_AXIS`, `Y_AXIS`, `Z_AXIS`
- Datum planes: `XY_PLANE` (`Z=0`), `XZ_PLANE` (`Y=0`), `YZ_PLANE` (`X=0`)
- Pipeline operator `|>` passes the left-hand geometry as the implicit first subject to the next feature, tagger, or PMI operator.

---

## 3. 2D Closed Sketches & Geometric Constraints (`cad-gcs`)

Profiles for extrude, revolve, and sweep must close explicitly (`E204_OPEN_SKETCH_REGION` rejects open or self-intersecting loops); sweep paths use `region::path([...])`:

| Category | Operators | Notes |
| :--- | :--- | :--- |
| **Sketch Block** | `sketch(plane = ...)` | Datum plane or uniquely disambiguated planar face query |
| **Entities** | `point`, `line`, `arc`, `circle`, `rectangle` | 2D local sketch geometry |
| **Regions** | `region::closed([...])`, `region::path([...])` | Closed profile loop / open sweep trajectory |
| **Constraints** | `constraint::coincident`, `horizontal`, `vertical`, `parallel`, `perpendicular`, `tangent`, `distance`, `radius` | Solved via damped LM + homotopy continuation with DOF reporting |

---

## 4. 3D Modeling, Holes, Booleans & Assemblies

| Category | Operators | Notes |
| :--- | :--- | :--- |
| **Primitives** | `box(size = [w, d, h], origin = [x, y, z])`<br>`cylinder(d = ..., h = ..., origin = [...], axis = Z_AXIS)` | `box` `origin` is min corner; `cylinder` `origin` is base center, extruded along `+Z` by default |
| **Sketch Form** | `extrude(height = ..., direction = +Z)`<br>`extrude_symmetric(thickness = ...)`<br>`revolve(axis = ..., angle = 360deg)` | Generates 3D solids from closed sketch regions |
| **Sweep & Loft** | `sweep(profile = ..., path = ...)`<br>`loft(sections = [...], height = ...)` | Swept trajectory or multi-section lofted solids |
| **Dress-Up** | `fillet(r = ...)`, `chamfer(d = ...)`<br>`shell(thickness = ..., open_faces = [...])`<br>`mirror(plane = YZ_PLANE, merge = false)` | Out-of-range parameters fail with `E205_INVALID_FILLET_CHAMFER` / `E206_INVALID_SHELL_THICKNESS` |
| **Holes** | `hole::simple(...)`, `hole::counterbore(...)`<br>`hole::tapped(spec = "M6x1.0-6H", depth = ..., ...)` | `hole::tapped` models the tap-drill bore with thread annotation (no physical helical thread geometry) |
| **Patterns & Booleans** | `array::linear(...)`, `array::polar(...)`<br>`boolean::union(...)`, `boolean::subtract(tools = [...], preserve_exact_interfaces = true)` | Preserves face provenance tags and inner loops across subtractions |
| **Assemblies** | `component(part = ..., origin = [...], rotation = [...])`<br>`mate::coaxial(...)`, `mate::coincident(...)` | Instantiates subparts with BOM metadata and rigid mate checks |

---

## 5. Topology Queries, Sandboxed Imports & Assertions

### Topology Selection & Tagging
- Filters: `.faces()`, `.edges()`, `.planar()`, `.cylindrical()`, `.filter(normal = +Z, angle_tol = 1deg)`, `.tagged("...")`
- Tie-breakers: `.max_by(coord = Z)`, `.min_by(coord = Z)`, `.max_by(radius)`, `.min_by(radius)`, `.then_by(coord = X)`
- Tagging & PMI: `tag::faces(...)`, `tag::cylindrical_faces(...)`, `pmi::datum("A", flatness = 0.02mm)`, `pmi::perpendicularity(...)`

### Sandboxed External Imports
```kcl
let motor = import::step(
    path = "assets/nema17_mm.step",
    sha256 = "3627a7a9af6fa13d5a20f9e69271c8843d260089052489eb3f20942dabc45e59",
    heal = false
)
let scan = import::mesh(path = "assets/open_shell.stl", allow_open_shell = true)
```
- Paths must stay inside `--workspace-root` (`E502_UNSAFE_ASSET_PATH` blocks escapes and symlinks).
- Optional `sha256` pins exact asset bytes (`E503_ASSET_HASH_MISMATCH`); non-watertight shells fail with `E505_IMPORT_NON_MANIFOLD_OR_OPEN` unless `allow_open_shell = true` (preview/measurement only).
- History-free direct hole editing supports `face::offset`, `face::move`, and `face::delete` (each edit produces a new solid that must be re-queried for subsequent edits).

### Engineering Assertions
- `assert::bbox(target = ..., max_size = [x, y, z])`
- `assert::dfm_accessible(target = ..., axes = [+Z, -Z], min_wall_thickness = ...)`

See [examples/README.md](../../examples/README.md) for runnable `.kcl` parts and assemblies.

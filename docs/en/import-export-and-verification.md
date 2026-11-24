# Import/Export Capabilities & 5-Tier Tri-State Verification

**English** | [简体中文](../zh/import-export-and-verification.md) | [Docs Index](../README.md)

Silent degradation is a cardinal sin in engineering CAD. AI-Native CAD enforces explicit, machine-readable contracts for dual-kernel capabilities, format preservation boundaries, and L0–L4 verification verdicts.

---

## 1. Dual-Mode Geometry Kernel Comparison

| Dimension | Default Pure Rust (`AnalyticKernel` + `cad-mesh-io`) | Native OCCT (`--features occt-ffi` + `cpp/occt-bridge`) |
| :--- | :--- | :--- |
| **Build Dependencies** | Zero C++ dependencies; compiles out of the box | Requires CMake + C++20 + OpenCASCADE 7.9+ shared libraries (LGPL-2.1 isolated) |
| **Parametric Plates, Holes, Patterns, Booleans** | Supported for rapid exploration & starter parts | **Authoritative exact B-Rep** (complex booleans, fillets/chamfers, shells, direct hole edits) |
| **Mesh Imports (3MF / STL / OBJ / glTF / GLB)** | Native pure-Rust support (with OPC rels & manifold checks) | Supported |
| **Exact B-Rep Imports (STEP / IGES / BREP)** | Unavailable (explicit diagnostic requesting `occt-ffi`) | **Native support** (unit normalization, XCAF assemblies, optional healing/sewing) |
| **2D HLR Drawings & STEP Export** | Simplified projections / basic export (not third-party certified) | **Exact OCCT B-Rep HLR & authoritative STEP AP242 export** |
| **L4 STEP Round-Trip Verdict** | Honestly reports **`UNKNOWN`** | Re-imports exported STEP and verifies volume/topology (`PASS` when valid) |

---

## 2. Import & Export Format Matrix

### Import Formats & Unit Defaults

| Format | Backend | Default Units & Rules |
| :--- | :---: | :--- |
| **3MF** | Default Rust | Resolves OPC `_rels/.rels` and normalizes source file units to `mm` |
| **STL / OBJ** | Default Rust | Defaults to `mm`; non-manifold/open meshes fail with `E505_IMPORT_NON_MANIFOLD_OR_OPEN` unless `allow_open_shell = true` (preview/measurement only) |
| **glTF (embedded) / GLB** | Default Rust | Defaults to `m` and normalizes to `mm` |
| **STEP / IGES / BREP** | `occt-ffi` | Detects source units (e.g., inches → `25.4 mm`) and preserves multi-component world poses; healing is off by default (`heal = true`, default `sewing_tol = 0.001 mm`) |

> Discrete meshes (`import::mesh`) and open shells cannot participate in exact booleans. Native solids and imported STEP solids support bidirectional exact booleans under `occt-ffi`.

### Export Formats & Preservation Boundaries

| Format | Geometry Type | Preserved (`preserved`) | Not Preserved (`not_preserved`) |
| :--- | :---: | :--- | :--- |
| **STEP AP242** | `EXACT` (OCCT) / basic writer | Solid faces, inner loops, component records, descriptive metadata | Full commercial semantic PMI, colors/materials, assembly mates |
| **3MF** | `MESH` | Triangle mesh in millimetre coordinates | Exact analytic B-Rep surfaces, parametric feature history, mates |
| **DXF** (`--out-dxf` / `--out-dxf-view`) | `EXACT` / projection | 1:1 millimetre cut contours (`--out-dxf-view` adds solid/dashed engineering view) | Sheet-metal unfold or CAM-ready toolpaths |
| **SVG Drawings** (`--out-drawing` / `--out-views`) | `EXACT` (OCCT HLR) | Orthographic/isometric projections with hidden-line removal | Formal ISO/GB drafting compliance certification |
| **SceneMesh JSON / CMS2** | `MESH` | Viewport triangles, component metadata, topology slots, edit handles | Viewport & picking transport only; not a B-Rep deliverable |

---

## 3. Zero-Trust 5-Layer Tri-State Verification (L0–L4)

`cad-cli verify` and MCP `cad_verify_and_render` emit structured reports conforming to [verification-report.schema.json](../../schemas/verification-report.schema.json):

| Layer Key | Scope | `PASS` / `FAIL` / `UNKNOWN` Semantics |
| :--- | :--- | :--- |
| **`L0_structure_units`** | Graph schema, syntax, dimensional typing, Formula DAG | Any unit mismatch or formula cycle is `FAIL` |
| **`L1_geometry_topology`** | Watertight solids, zero open edges, positive finite volume, zero rolled-back nodes | Experimental tracks (SubD, TPMS, implicit fields) stay `UNKNOWN` (`L1_EXPERIMENTAL`) |
| **`L2_interfaces_constraints`** | Explicit assertions, 2D sketch DOF, assembly mates | Violations are `FAIL`; unevaluable custom external assertions stay `UNKNOWN` |
| **`L3_manufacturability_physics`** | DFM wall/accessibility checks and optional FEA checks | Declared FEA loads without a solver stay `UNKNOWN` |
| **`L4_interop_roundtrip`** | STEP AP242 export + re-import volume and topology verification | `PASS` only under native OCCT; always `UNKNOWN` on the pure-Rust fallback |

**Overall Verdict Rules**:
- Any layer `FAIL` → `overall_status = FAIL`;
- No `FAIL`, but any layer `UNKNOWN` or missing → `overall_status = UNKNOWN`;
- **`overall_status = PASS` is permitted only when all five layers L0–L4 are `PASS`.**
- Offscreen renders always report `render.status = UNKNOWN` (*"Pixels are not a PASS verdict"*).

---

## 4. Common Structured Diagnostic Codes

| Code | Meaning & Recovery |
| :--- | :--- |
| `E102_UNIT_MISMATCH` | Incompatible physical dimensions (e.g., adding length and angle); normalize units |
| `E201_AMBIGUOUS_TOPOLOGY` | Multiple faces/edges matched a single-target selector; inspect `recovery_hints` and add normal/bbox/radius or `.max_by()`/`.then_by()` |
| `E204_OPEN_SKETCH_REGION` | 2D sketch profile is open or self-intersecting; close the loop |
| `E205_INVALID_FILLET_CHAMFER` / `E206_INVALID_SHELL_THICKNESS` | Out-of-bounds or non-positive radius/thickness; reduce within geometric bounds |
| `E409_REVISION_CONFLICT` / `E409_PREVIEW_DIVERGED` | Stale `expected_revision` or `main` moved ahead; refresh session context before retrying |
| `E502_UNSAFE_ASSET_PATH` / `E_PATH_ESCAPE` | Path escapes `--workspace-root` or traverses a symlink outside workspace |
| `E505_IMPORT_NON_MANIFOLD_OR_OPEN` | Imported mesh has open or non-manifold edges; repair source or pass `allow_open_shell = true` for preview only |

# `.aicad` Hybrid Container & Canonical Graph IR

**English** | [简体中文](../zh/aicad-container-and-ir.md) | [Docs Index](../README.md)

AI-Native CAD uses the **`.aicad` open hybrid engineering container** and strongly dimensional **`Canonical Graph IR`** as its single source of truth—bridging interactive 3D manipulation, 1:1 `.kcl 2.0` text views, the Python SDK, and AI agents without duplicating state.

---

## 1. Why a Hybrid Container + Graph IR?

Plain `.kcl` text excels for regular parametric plates and brackets, but encounters three structural walls in full engineering workflows:
1. **High-dimensional binary payloads do not belong in source text**: NURBS control nets, SubD cages, external imported assets, and field tensors overwhelm both human readers and LLM context windows.
2. **Orthogonal engineering semantics**: AP242 GD&T annotations, material/appearance metadata, `DesignContract` requirements, and associative 2D drawing definitions attach cleanly to persistent topology references outside feature syntax.
3. **Incremental ACID transactions & branching**: Interactive dragging, persistent Undo/Redo, and AI Best-of-N exploration require sub-millisecond incremental saves and node-level B-Rep caching.

---

## 2. Storage Modes & Magic-Byte Auto-Detection

`crates/cad-container` automatically detects the underlying container format by filesystem metadata and header magic bytes while exposing a single unified API:

| Storage Mode | Physical Signature / Magic | Primary Use Case |
| :--- | :--- | :--- |
| **Single-File SQLite (Default)** | First 16 bytes `SQLite format 3\0`, `application_id = 0x41494344` (`AICD`) | Local desktop/workbench editing, transactional `oplog` Undo/Redo, instant `branches`, streaming Zstd `blobs`, node-level `node_brep_cache`, and `pack-and-go` delivery; automatically falls back from `WAL` to `DELETE` journal on SMB/NFS/UNC shares |
| **Unpacked / Sharded Directory** | Regular directory with `manifest.json` and `graph.ir.json` (or `graphs/*.ir.json`) | Git version control, code review diffs, modular team collaboration, and repository examples (such as `examples/aicad-projects/`) |
| **Streaming Render Pack (`.cadpack`)** | First 4 bytes `CPK1` (64B header, 64B-aligned table and chunks) | Multi-level HLOD 0/1/2 mesh streaming, `ExactPickTable`, virtual assembly tree, and `InterfaceDigest` |

### Unpacked Directory `.aicad/` Layout

```text
01_mounting_plate.aicad/
├── manifest.json              # Format version, ir_schema_version, kernel fingerprint, KCL projection policy
├── graph.ir.json              # Deterministically ordered CanonicalGraph (or graphs/*.ir.json when sharded)
├── engineering.json           # Orthogonal DesignContract, materials, PMI/GD&T, and BOM metadata on TopoRefs
├── drawings/                  # Optional: associative 2D drawing sheets anchored to 3D TopoRefs
├── blobs/                     # Optional: content-addressed (SHA-256) external assets, splines, and fields
└── cache/                     # Safely purgeable / vacuumable acceleration caches
```

---

## 3. Inside `Canonical Graph IR`

Cross-language JSON Schemas live in [graph-ir.schema.json](../../schemas/graph-ir.schema.json) and [graph-patch.schema.json](../../schemas/graph-patch.schema.json):

1. **Dimensional Parameters & Live `Formula DAG`**: Supports `length` (`mm`/`m`/`in`), `angle` (`deg`/`rad`), `force` (`N`/`kN`), `pressure` (`MPa`), `mass` (`kg`/`g`), and scalars. Formulas evaluate in topological order with cycle and unit-mismatch rejection.
2. **First-Class 2D Sketches (`StructuredSketch2D`)**: Stores sketch planes, 2D entities (points, lines, arcs, circles, rectangles), and `cad-gcs` constraints, recording solver convergence and remaining DOF.
3. **Typed Feature Graph**: Primitives, sketch-based features, standard/counterbore/tapped holes, fillets/chamfers/shells/drafts, patterns/mirrors, booleans, external imports, assembly components/mates, plus `Suppressed` state and rollback metadata.
4. **Kernel-Agnostic Dual-Track `TopoRef` (`crates/cad-topo`)**:
   - **`LineageTopoRef`**: Tracks creator nodes, topological evolution (`Split` / `Merged` / `Trimmed`), and intrinsic geometric fingerprints.
   - **`QueryTopoRef`**: Deterministic semantic selectors (surface class, normal, bounding box, radius, tag) requiring explicit tie-breaking, independent of OCCT `TNaming`.
5. **Node-Level Merkle Cache**: Each node hashes its typed parameters, upstream Merkle keys, and kernel fingerprint so downstream fillet edits or parallel preview branches reuse unchanged upstream solids in $O(1)$ time.

---

## 4. Common Container CLI Workflows

```sh
# 1. Initialize a directory or single-file SQLite project
cad-cli project init work/plate.aicad --name "Mounting Plate" --format directory
cad-cli project init work/plate_sql.aicad --name "Mounting Plate" --format sqlite

# 2. Inspect container metadata & graph outline (use --metadata-only for large blob containers)
cad-cli project inspect examples/aicad-projects/01_mounting_plate.aicad --json

# 3. Emit the 1:1 .kcl 2.0 projection
cad-cli project emit-kcl examples/aicad-projects/01_mounting_plate.aicad

# 4. Convert losslessly between directory and single-file SQLite formats
cad-cli project pack examples/aicad-projects/01_mounting_plate.aicad --out tmp_out/plate.aicad
cad-cli project unpack tmp_out/plate.aicad --out tmp_out/plate_unpacked.aicad

# 5. Self-contained distribution bundle & garbage-collection vacuum
cad-cli project pack-and-go tmp_out/plate.aicad --out tmp_out/plate_bundle.aicad
cad-cli project vacuum tmp_out/plate.aicad
```

See the [`cad-cli` Reference](cli-reference.md) for the full subcommand catalog.

## 5. Sync a feature graph patch in Web Studio

In Model Tree → Graph IR, expand the workspace `.aicad` sync section and initialize a project. Edit a feature, then select Sync Patch to write the latest feature graph edit to the workspace container. If the development server disconnects, the edit stays local and you can retry when the server returns. Run `cad-cli project emit-kcl <project-path>` to reopen the container and check the synced result.

# `cad-cli` Command-Line Reference

**English** | [简体中文](../zh/cli-reference.md) | [Docs Index](../README.md)

`cad-cli` (`crates/cad-cli`) is the unified headless compiler, container tool, document runtime proxy, and MCP gateway binary. All core subcommands support deterministic `--json` machine-readable output.

---

## 1. All 14 Top-Level Subcommands

| Subcommand | Purpose | Example |
| :--- | :--- | :--- |
| `check` | Compile `.kcl` and verify syntax, physical units, sketch constraints, watertightness, and optional reference interference | `cad-cli check model.kcl --json` |
| `build` | Compile `.kcl` and export STEP, 3MF, SceneMesh, DXF, SVG drawings, and optional offscreen PNGs | `cad-cli build model.kcl --out-step out/part.step --json` |
| `lsp` | Query completions, dimensional hover info, and symbol outline without evaluating geometry | `cad-cli lsp model.kcl --line 12 --col 10 --json` |
| `inspect` | Summarize geometry and mating features of external 3D assets (STEP, IGES, BREP, 3MF, STL, OBJ, glTF) | `cad-cli inspect asset.step --json` |
| `compare` | Measure exact symmetric volume difference between a compiled solid and a reference STEP | `cad-cli compare model.kcl ref.step --json` |
| `decompile` | Recognize prismatic B-Rep features into a parametric `CanonicalGraph` (experimental) | `cad-cli decompile part.step --json` |
| `project` | Manage `.aicad` project containers end to end (see Section 3) | `cad-cli project inspect part.aicad --json` |
| `daemon` | Serve persistent JSON-RPC requests over `stdin` / `stdout` | `cad-cli daemon` |
| `mcp` | Serve the external-agent MCP gateway (stdio / loopback HTTP) or export OpenAI tool schemas | `cad-cli mcp --workspace . --project part.aicad` |
| `session` | Read active runtime session context (`revision`, `mode`, active branch, viewport selection) | `cad-cli session status --project part.aicad --json` |
| `api` | Apply an atomic batch of `SemanticCommand` JSON commands | `cad-cli api apply --project part.aicad --commands cmds.json --json` |
| `query` | Inspect document summaries, query topology faces/edges, measure geometry, or `explain` lineage | `cad-cli query summary --project part.aicad --json` |
| `verify` | Run the 5-layer L0–L4 tri-state verification report (`fast` or `release`) | `cad-cli verify --project part.aicad --json` |
| `doctor-agent` | Preflight binary health, OCCT authority, MCP token budget, workspace access, and Live-Attach status | `cad-cli doctor-agent --json` |

---

## 2. Common `build` Export Flags

```sh
cad-cli build examples/mvp-tier3-advanced/13_bearing_housing_assembly.kcl \
  --out-step tmp_out/asm.step \
  --out-3mf tmp_out/asm.3mf \
  --out-mesh tmp_out/asm.mesh.json \
  --out-dxf tmp_out/asm.dxf \
  --out-dxf-view tmp_out/asm_view.dxf \
  --out-drawing tmp_out/asm.drawing.svg \
  --out-views tmp_out/views \
  --json
```

- `--workspace-root <DIR>`: Explicitly pins the asset sandbox root when `.kcl` files and referenced assets reside in separate directories.
- `--component <ID>`: Exports a single assembly component in its local coordinates.
- `--out-render <PATH>` (requires `--features wgpu-renderer`): Combine with `--render-views iso,front,top,right`, `--render-size 1024x768`, and `--emit-semantic-mask` for shaded multi-view PNGs and semantic face-ID masks.

---

## 3. `cad-cli project` Subcommands

| `cad-cli project ...` | Description |
| :--- | :--- |
| `init <DIR> --name <NAME> [--format directory\|sqlite]` | Create a starter `.aicad` container |
| `inspect <PATH> [--metadata-only] [--json]` | Inspect SQLite, directory, or sharded `.aicad` metadata and node counts |
| `emit-kcl <PATH> [--out <FILE>]` | Project `CanonicalGraph` 1:1 to `.kcl 2.0` |
| `pack <SRC_DIR> --out <FILE>` / `unpack <FILE> --out <DIR> [--sharded]` | Convert losslessly between directory and SQLite containers |
| `apply-patch <PATH> --patch <FILE> [--branch <NAME>]` | Apply a structured `GraphPatch` atomically |
| `branch` / `checkout` | Create, switch, or roll back lightweight SQLite branches |
| `slice-context` | Extract a minimal closed context slice around focus nodes or `TopoRef`s |
| `pack-and-go` / `vacuum` | Inline external references into a portable bundle / garbage-collect orphan blobs |
| `import-step-xcaf` / `export-step-xcaf` | Multi-component XCAF STEP exchange (requires `--features occt-ffi`) |
| `stdlib` | Query built-in metric fasteners, bearings, ISO 286 tolerances, and materials |
| `dfm-check` / `render-drawing` / `slice` | Run DFM checks, render associative 2D SVG drawings, or generate additive slices |
| `export-cadpack` / `cadpack-tree` / `cadpack-digest` | Build and query `.cadpack` streaming assembly packs |

---

## 4. Environment Variables & Exit Codes

- **`CAD_CLI_BIN`**: Overrides `cad-cli` discovery for the web `dev-server.mjs`, Python SDK (`aicad`), and `cad-agent` (for example pointing to an OCCT build under `target/occt-ffi/debug/cad-cli`).
- **`OCCT_BRIDGE_LIB_DIR`**: Points Cargo and runtime loaders to the `occt_bridge` shared library directory; prefer `./scripts/run-cad-cli-occt.ps1` / `.sh`.
- **`AICAD_MCP_TOKEN`**: Bearer token used by `cad-cli mcp --transport http`.
- **Exit Codes**: `0` indicates success; non-zero indicates compile/geometry/gate errors, argument errors, or sandbox rejections (with structured details in `--json`). Individual export failures report explicit status rather than silently masking errors.

# `aicad` — Official Python Generative SDK for AI-Native CAD

`aicad` is the official Python 3.12+ SDK for building canonical AI-Native CAD graphs, reading/writing `.aicad` project containers (single-graph directory, Git-friendly sharded directory, ZIP archive, and SQLite), and driving headless `cad-cli` sessions with optimistic revision concurrency, preview branches, undo/redo, verification, and manufacturing artifact export.

## Installation

From the repository root:

```bash
python -m pip install -e "./packages/python-sdk[dev]"
```

To drive live `RuntimeSession` operations, build `cad-cli` or point `CAD_CLI_BIN` at a prebuilt binary:

```bash
cargo build -p cad-cli
export CAD_CLI_BIN="$PWD/target/debug/cad-cli"
```

## Unit Conventions

All physical quantities are strongly typed `Quantity` values that validate dimensional compatibility before serialization to `graph-ir.schema.json`:

| Dimension | Constructors | Canonical Internal Unit |
| :--- | :--- | :--- |
| `length` | `mm(v)`, `inch(v)`, `meter(v)` | `mm` |
| `angle` | `deg(v)`, `rad(v)` | `deg` |
| `force` | `newton(v)` | `N` |
| `pressure` | `mpa(v)` | `MPa` |
| `mass` | `kg(v)`, `g(v)` | `kg` |

Mixed compatible units (such as `mm(10) + inch(1)` or `kg(1) + g(250)`) convert automatically while preserving the left-hand display unit. Incompatible dimensions (such as `mm(10) + deg(90)`) raise `TypeError`.

## Minimal Runnable Example

### 1. Build and Save an `.aicad` Container

```python
from pathlib import Path
from aicad import AicadProject, GraphBuilder, inspect_aicad_container_metadata, mm

builder = GraphBuilder("mounting-plate")
builder.add_parameter("width", "length", mm(40.0))
builder.add_parameter("depth", "length", mm(30.0))
builder.add_parameter("thickness", "length", mm(10.0))
builder.add_node(
    "plate",
    "Mounting Plate",
    "primitive_box",
    {
        "width": mm(40.0),
        "depth": mm(30.0),
        "height": mm(10.0),
        "origin": {"type": "vec3", "value": [-20.0, -15.0, 0.0]},
    },
    "solid",
)
builder.set_outputs(["plate"])

project_dir = AicadProject("Mounting Plate", builder).save(Path("out/plate.aicad"))
meta = inspect_aicad_container_metadata(project_dir)
print(meta["project_name"], meta["node_count"])
```

### 2. Drive a Headless `RuntimeSession` via `cad-cli`

```python
from pathlib import Path
from aicad import CliError, RuntimeSession, SemanticModelBuilder, mm

workspace = Path(".").resolve()
model = SemanticModelBuilder()
model.upsert_parameter("width", mm(40.0))
model.create_box("BasePlate", mm(40.0), mm(30.0), mm(10.0))

with RuntimeSession("work/plate.aicad", workspace=workspace, timeout=120.0) as session:
    outcome = session.apply(model.commands)
    print("revision:", outcome["revision"])

    session.open_candidate()
    session.apply_candidate([
        {
            "command": "upsert_parameter",
            "args": {
                "name": "width",
                "ir_type": "length",
                "value": {"type": "quantity", "value": mm(50.0).to_json()},
            },
        }
    ])
    diff = session.diff_branch()
    session.commit_preview()
    report = session.verify()
    export = session.export_artifacts("step", "out/plate.step")
    print(report["report"]["overall_status"], export["path"])
```

Structured CLI failures raise `CliError` with `.code`, `.payload`, `.recovery_hints`, `.returncode`, `.stdout`, and `.stderr`.

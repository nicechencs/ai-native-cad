#!/usr/bin/env python3
"""Developer Platform Example 01: Inspect and measure a `.aicad` project.

Creates a mounting plate with a center boss using the official `aicad` Python SDK,
inspects its status and feature lineage via `RuntimeSession`, and runs real
`cad-cli verify` geometry & DFM checks.
"""

from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
SDK_SRC = REPO_ROOT / "packages" / "python-sdk" / "src"
if SDK_SRC.is_dir() and str(SDK_SRC) not in sys.path:
    sys.path.insert(0, str(SDK_SRC))

from aicad import EngineeringDocument, RuntimeSession, find_cad_cli, mm  # noqa: E402


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Inspect a `.aicad` project and measure its geometry via `cad-cli`.",
    )
    parser.add_argument(
        "--cad-cli",
        type=Path,
        default=None,
        help="Path to `cad-cli` binary (defaults to CAD_CLI_BIN or auto-discovery).",
    )
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=None,
        help="Optional working directory for generated project files (uses a temp dir if omitted).",
    )
    return parser.parse_args(argv)


def run_example(
    *,
    cad_cli_bin: Path | None = None,
    work_dir: Path | None = None,
) -> dict[str, Any]:
    cli_path = find_cad_cli(cad_cli_bin)
    if cli_path is None:
        raise FileNotFoundError(
            "cad-cli binary not found. Build it via `cargo build -p cad-cli`, "
            "pass --cad-cli, or set CAD_CLI_BIN."
        )

    if work_dir is not None:
        work_dir.mkdir(parents=True, exist_ok=True)
        return _execute_in_dir(work_dir, cli_path)

    with tempfile.TemporaryDirectory(prefix="aicad-ex01-") as tmp:
        return _execute_in_dir(Path(tmp), cli_path)


def _execute_in_dir(workspace: Path, cli_path: Path) -> dict[str, Any]:
    doc = EngineeringDocument("SensorMountPlate", graph_id="sensor-mount-plate")
    doc.set_design_contract(
        "SensorMountPlate",
        summary="Aluminum mounting plate with raised center boss",
        material="Al-6061-T6",
        manufacturing_method="CNC_3AXIS_MILLING",
    )
    doc.upsert_parameter("plate_width", mm(60.0))
    doc.upsert_parameter("plate_depth", mm(40.0))
    doc.upsert_parameter("plate_thickness", mm(8.0))
    doc.create_box("BasePlate", "plate_width", "plate_depth", "plate_thickness", node_id="base_plate")
    doc.create_hole(
        "CenterBore",
        "base_plate",
        12.0,
        node_id="center_bore",
        origin_mm=(30.0, 20.0, 8.0),
        axis=(0.0, 0.0, -1.0),
        depth="through_all",
    )

    project_path = workspace / "sensor_mount.aicad"
    with RuntimeSession(
        project_path,
        workspace=workspace,
        cad_cli_bin=cli_path,
        client="dev-example",
        session_id="inspect-measure",
    ) as session:
        apply_result = session.apply(doc.to_commands())
        status = session.status()
        summary = session.query_summary()
        plate_explain = session.explain("base_plate")
        verification = session.verify()

    outline = [item["node_id"] for item in summary["summary"]["feature_outline"]]
    solids = verification["solids"]
    report = verification["report"]

    return {
        "example": "01_inspect_and_measure",
        "cad_cli": str(cli_path),
        "project": str(project_path),
        "active_branch": status["active_branch"],
        "revision": apply_result["revision"],
        "feature_outline": outline,
        "explained_node": {
            "id": plate_explain["explanation"]["id"],
            "display_name": plate_explain["explanation"]["display_name"],
        },
        "solid_count": solids.get("solid_count"),
        "total_volume_mm3": solids["total_volume_mm3"],
        "overall_status": report["overall_status"],
        "layers": report["layers"],
    }


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    result = run_example(cad_cli_bin=args.cad_cli, work_dir=args.work_dir)
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

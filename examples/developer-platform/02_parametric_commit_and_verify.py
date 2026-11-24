#!/usr/bin/env python3
"""Developer Platform Example 02: Parametric commit with optimistic revision locking and verification.

Creates a parametric bracket block via the `aicad` Python SDK, verifies baseline
geometry, commits a parameter update guarded by `expected_revision`, and confirms
that the volume and revision advance deterministically.
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

from aicad import (  # noqa: E402
    EngineeringDocument,
    RuntimeSession,
    SemanticModelBuilder,
    find_cad_cli,
    mm,
)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Apply a parametric update with revision locking and verify geometry.",
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

    with tempfile.TemporaryDirectory(prefix="aicad-ex02-") as tmp:
        return _execute_in_dir(Path(tmp), cli_path)


def _execute_in_dir(workspace: Path, cli_path: Path) -> dict[str, Any]:
    # 1. Author initial parametric model
    doc = EngineeringDocument("ParametricSpacerBlock", graph_id="parametric-spacer-block")
    doc.set_design_contract(
        "ParametricSpacerBlock",
        summary="Parametric aluminum spacer block",
        material="Al-6061-T6",
        manufacturing_method="CNC_3AXIS_MILLING",
    )
    doc.upsert_parameter("block_w", mm(50.0))
    doc.upsert_parameter("block_d", mm(30.0))
    doc.upsert_parameter("block_h", mm(10.0))
    doc.create_box("SpacerBody", "block_w", "block_d", "block_h", node_id="spacer_body")

    project_path = workspace / "parametric_spacer.aicad"
    with RuntimeSession(
        project_path,
        workspace=workspace,
        cad_cli_bin=cli_path,
        client="dev-example",
        session_id="parametric-commit",
    ) as session:
        initial_apply = session.apply(doc.to_commands())
        initial_rev = int(initial_apply["revision"])
        initial_verify = session.verify()
        initial_volume = float(initial_verify["solids"]["total_volume_mm3"])

        # 2. Prepare a parametric update guarded by expected_revision
        patch = SemanticModelBuilder()
        patch.upsert_parameter("block_w", mm(80.0))
        patch.upsert_parameter("block_h", mm(15.0))

        updated_apply = session.apply(patch.commands, expected_revision=initial_rev)
        updated_rev = int(updated_apply["revision"])
        updated_verify = session.verify()
        updated_volume = float(updated_verify["solids"]["total_volume_mm3"])
        summary = session.query_summary()

    report = updated_verify["report"]
    return {
        "example": "02_parametric_commit_and_verify",
        "cad_cli": str(cli_path),
        "project": str(project_path),
        "initial_revision": initial_rev,
        "updated_revision": updated_rev,
        "initial_volume_mm3": initial_volume,
        "updated_volume_mm3": updated_volume,
        "volume_delta_mm3": round(updated_volume - initial_volume, 3),
        "feature_outline": [item["node_id"] for item in summary["summary"]["feature_outline"]],
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

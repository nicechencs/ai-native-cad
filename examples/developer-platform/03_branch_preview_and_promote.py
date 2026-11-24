#!/usr/bin/env python3
"""Developer Platform Example 03: Isolated candidate branch preview, review, and promotion.

Demonstrates the V2 branch workflow:
1. Commit a baseline model on `main`.
2. Open an isolated `ai_candidate` preview branch.
3. Apply a candidate feature on the preview branch, diff & verify it.
4. Promote the verified preview branch onto `main` via `commit_preview`.
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
    find_cad_cli,
    mm,
)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Preview a candidate CAD edit on an isolated branch and promote it to main.",
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

    with tempfile.TemporaryDirectory(prefix="aicad-ex03-") as tmp:
        return _execute_in_dir(Path(tmp), cli_path)


def _execute_in_dir(workspace: Path, cli_path: Path) -> dict[str, Any]:
    baseline = EngineeringDocument("BranchWorkflowPlate", graph_id="branch-workflow-plate")
    baseline.set_design_contract(
        "BranchWorkflowPlate",
        summary="Base plate before candidate reinforcement boss",
        material="Al-6061-T6",
        manufacturing_method="CNC_3AXIS_MILLING",
    )
    baseline.upsert_parameter("plate_w", mm(60.0))
    baseline.upsert_parameter("plate_d", mm(40.0))
    baseline.upsert_parameter("plate_h", mm(6.0))
    baseline.create_box("BasePlate", "plate_w", "plate_d", "plate_h", node_id="base_plate")

    project_path = workspace / "branch_workflow.aicad"
    with RuntimeSession(
        project_path,
        workspace=workspace,
        cad_cli_bin=cli_path,
        client="dev-example",
        session_id="branch-promote",
    ) as session:
        # 1. Baseline commit on main
        main_apply = session.apply(baseline.to_commands())
        baseline_rev = int(main_apply["revision"])

        # 2. Open an isolated candidate preview branch
        opened = session.open_candidate(from_branch="main")
        preview_branch = opened["branch"]["branch_name"]

        # 3. Apply candidate feature on the preview branch
        baseline.create_hole(
            "MountingHole",
            "base_plate",
            10.0,
            node_id="mounting_hole",
            origin_mm=(30.0, 20.0, 6.0),
            axis=(0.0, 0.0, -1.0),
            depth="through_all",
        )
        candidate_apply = session.apply(baseline.to_commands()[-1:])
        branches_during_preview = [
            item["branch_name"] for item in session.list_branches()["branches"]
        ]
        diff_report = session.diff_branch(preview_branch)
        review_report = session.request_review(preview_branch)
        preview_verify = session.verify()

        # 4. Promote verified candidate branch onto main
        promoted = session.commit_preview(preview_branch)
        final_status = session.status()
        final_summary = session.query_summary()
        final_verify = session.verify()

    final_outline = [item["node_id"] for item in final_summary["summary"]["feature_outline"]]
    final_report = final_verify["report"]
    return {
        "example": "03_branch_preview_and_promote",
        "cad_cli": str(cli_path),
        "project": str(project_path),
        "baseline_revision": baseline_rev,
        "preview_branch": preview_branch,
        "candidate_revision": int(candidate_apply["revision"]),
        "branches_during_preview": branches_during_preview,
        "diff_status": diff_report.get("status", "ok"),
        "review_status": review_report.get("status", "ok"),
        "preview_layers": preview_verify["report"]["layers"],
        "promoted_status": promoted.get("status", "ok"),
        "final_active_branch": final_status["active_branch"],
        "final_revision": int(final_status["revision"]),
        "final_feature_outline": final_outline,
        "final_total_volume_mm3": float(final_verify["solids"]["total_volume_mm3"]),
        "overall_status": final_report["overall_status"],
        "layers": final_report["layers"],
    }


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    result = run_example(cad_cli_bin=args.cad_cli, work_dir=args.work_dir)
    print(json.dumps(result, indent=2, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

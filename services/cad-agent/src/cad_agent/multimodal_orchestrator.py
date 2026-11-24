"""Build a routed `.aicad` project and check it with `cad-cli`.

`orchestrate_multitrack_design` classifies a prompt, writes a schema-valid
project with the Python SDK, runs `cad-cli project inspect`, and exports the
manufacturing or drawing file for that track. A `TrajectoryLogger` records
the attempt when the caller passes a JSONL path.
"""

from __future__ import annotations

import hashlib
import importlib
import json
import math
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .compiler_bridge import discover_cad_cli, find_workspace_root
from .graph_patch_agent import (
    TRACK_KCL_PRISMATIC,
    TRACK_PYTHON_SDK_CURVE,
    TRACK_SUBD_FREEFORM,
    TRACK_TPMS_IMPLICIT,
    TrackRoute,
    TrajectoryLogger,
    TrajectoryRecord,
    default_reward_score,
    route_design_task,
    utc_timestamp,
)

_CLI_TIMEOUT_SEC = 180
_SHEET_ID = "sheet_01"


@dataclass(frozen=True, slots=True)
class OrchestrationResult:
    """One multi-track generation, inspection, and export attempt."""

    route: str
    project_dir: Path
    inspect_ok: bool
    exported_artifacts: dict[str, str]
    trajectory_logged: bool
    error: str | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.route, str) or not self.route.strip():
            raise ValueError("route must contain a non-space character")
        if not isinstance(self.inspect_ok, bool) or not isinstance(self.trajectory_logged, bool):
            raise TypeError("inspect_ok and trajectory_logged must be bools")
        if not isinstance(self.exported_artifacts, dict):
            raise TypeError("exported_artifacts must be a dictionary")
        if self.error is not None and not isinstance(self.error, str):
            raise TypeError("error must be a string or None")
        object.__setattr__(self, "project_dir", Path(self.project_dir))
        object.__setattr__(
            self,
            "exported_artifacts",
            {str(key): str(value) for key, value in self.exported_artifacts.items()},
        )


def orchestrate_multitrack_design(
    prompt: str,
    output_dir: Path,
    trajectory_path: Path | None = None,
    cli_bin: Path | None = None,
) -> OrchestrationResult:
    """Route `prompt`, write an `.aicad` project, inspect it, and export artifacts.

    The router returns ``python_sdk_curve``, ``tpms_implicit``, ``subd_freeform``,
    or ``kcl_prismatic``. Curve prompts become a NACA 2412 or involute spline
    graph. TPMS prompts become a 20 x 20 x 10 mm gyroid lattice, then
    ``project slice`` and ``project export-3mf`` when `cad-cli` is available.
    SubD prompts become a 20 mm control cage and a 3MF mesh. Prismatic prompts
    become a box plus an associative drawing sheet rendered to ``drawing.svg``.
    Every track is checked with ``cad-cli project inspect --json``.
    """
    route = route_design_task(prompt)
    project_dir = Path(output_dir)
    inspect_ok = False
    artifacts: dict[str, str] = {}
    error: str | None = None
    try:
        _write_project(route, project_dir)
    except Exception as exc:
        error = f"failed to build .aicad project: {exc}"
    else:
        inspect_ok, artifacts, error = _verify_and_export(route.track, project_dir, cli_bin)

    logged = _log_attempt(
        trajectory_path,
        route=route.track,
        prompt=prompt,
        project_dir=project_dir,
        artifacts=artifacts,
        verified_ok=inspect_ok and error is None,
    )
    if logged is not None:
        error = logged if error is None else f"{error}; {logged}"
    return OrchestrationResult(
        route=route.track,
        project_dir=project_dir,
        inspect_ok=inspect_ok,
        exported_artifacts=artifacts,
        trajectory_logged=logged is None and trajectory_path is not None,
        error=error,
    )


def _write_project(route: TrackRoute, dest: Path) -> None:
    aicad = _load_aicad()
    if route.track == TRACK_PYTHON_SDK_CURVE:
        _write_curve_project(aicad, route.reason, dest)
    elif route.track == TRACK_TPMS_IMPLICIT:
        _write_tpms_project(aicad, dest)
    elif route.track == TRACK_SUBD_FREEFORM:
        _write_subd_project(aicad, dest)
    else:
        _write_prismatic_project(aicad, dest)


def _write_curve_project(aicad: Any, reason: str, dest: Path) -> None:
    project_name, handle, points, closed = _curve_spec(aicad, reason)
    builder = aicad.GraphBuilder("curve")
    builder.add_spline_blob(handle, points, degree=3, closed=closed)
    builder.add_node(
        "section",
        project_name,
        "sketch_2d",
        {"spline": aicad.ir_blob_ref(handle)},
        "sketch",
    )
    builder.set_outputs(["section"])
    aicad.save_aicad_directory(dest, project_name, builder)


def _curve_spec(aicad: Any, reason: str) -> tuple[str, str, list[Any], bool]:
    if reason in {"involute", "gear"}:
        return (
            "Involute Gear Tooth",
            "involute_tooth",
            aicad.generate_involute_gear_tooth(),
            True,
        )
    if reason == "helix":
        return (
            "Helix Curve",
            "helix",
            aicad.generate_helix_3d(10.0, 8.0, 2.0),
            False,
        )
    if reason == "cam":
        return ("Cam Profile", "cam_profile", _cam_profile(), True)
    return (
        "NACA 2412 Airfoil",
        "naca2412",
        aicad.generate_naca4_airfoil("2412", chord_mm=100.0, num_points=40),
        True,
    )


def _cam_profile() -> list[tuple[float, float]]:
    """Closed radial cam: a 20 mm base circle with a single 6 mm nose."""
    points: list[tuple[float, float]] = []
    for index in range(48):
        theta = 2.0 * math.pi * index / 48.0
        radius = 20.0 + 6.0 * math.cos(theta)
        points.append((radius * math.cos(theta), radius * math.sin(theta)))
    return points


def _write_tpms_project(aicad: Any, dest: Path) -> None:
    builder = aicad.GraphBuilder("cooling_plate")
    _add_box(aicad, builder, "box", "Cooling domain", 20.0, 20.0, 10.0)
    builder.add_tpms_lattice("lattice", "box", family="gyroid", period_mm=6.0, thickness_mm=1.2)
    # `add_tpms_lattice` stores thickness on the lattice wall. Field evaluation
    # also requires `thickness_mm` on the FieldTpms node, and a literal length
    # is accepted on that extra port.
    builder.nodes["lattice_field"]["inputs"]["thickness_mm"] = aicad.ir_quantity(aicad.mm(1.2))
    builder.set_outputs(["lattice"])
    aicad.save_aicad_directory(dest, "Gyroid Cooling Plate", builder)


def _write_subd_project(aicad: Any, dest: Path) -> None:
    builder = aicad.GraphBuilder("control_shell")
    cage = aicad.SubDCage.unit_cube(20.0)
    builder.add_subd_blob("cage", cage)
    builder.add_node(
        "shell",
        "Control shell",
        "subd_from_blob",
        {"blob": aicad.ir_blob_ref("cage")},
        "subd",
    )
    builder.set_outputs(["shell"])
    aicad.save_aicad_directory(dest, "Ergonomic Control Shell", builder)


def _write_prismatic_project(aicad: Any, dest: Path) -> None:
    builder = aicad.GraphBuilder("mounting_block")
    _add_box(aicad, builder, "block", "Mounting block", 100.0, 60.0, 40.0)
    builder.add_drawing_sheet(
        {
            "sheet_id": _SHEET_ID,
            "title": "CNC mounting block",
            "paper_size": "A3",
            "views": [
                {
                    "view_id": "front",
                    "kind": {"kind": "front"},
                    "placement_mm": [150.0, 100.0],
                    "scale": 1.0,
                }
            ],
            "dimensions": [
                {
                    "id": "width",
                    "label": "Width",
                    "anchors": [_axis_face("NegX"), _axis_face("PosX")],
                    "override_text": None,
                    "placement_mm": [150.0, 45.0],
                }
            ],
        }
    )
    builder.set_outputs(["block"])
    aicad.save_aicad_directory(dest, "CNC Mounting Block", builder)


def _add_box(
    aicad: Any,
    builder: Any,
    node_id: str,
    name: str,
    width_mm: float,
    depth_mm: float,
    height_mm: float,
) -> None:
    builder.add_node(
        node_id,
        name,
        "primitive_box",
        {
            "width": aicad.mm(width_mm),
            "depth": aicad.mm(depth_mm),
            "height": aicad.mm(height_mm),
            "origin": {"type": "vec3", "value": [0.0, 0.0, 0.0]},
        },
        "solid",
    )


def _axis_face(direction: str) -> dict[str, Any]:
    """Associative face query used by `cad-cli project render-drawing`."""
    return {
        "Query": {
            "entity_kind": "Face",
            "filters": [{"Normal": {"dir": direction, "angle_tol_deg": 0.0}}],
            "tie_breakers": [],
            "require_unique": True,
        }
    }


def _verify_and_export(
    track: str,
    project_dir: Path,
    cli_bin: Path | None,
) -> tuple[bool, dict[str, str], str | None]:
    cli = _resolve_cli(cli_bin)
    if cli is None:
        location = cli_bin if cli_bin is not None else "CAD_CLI_BIN, target/, or PATH"
        return False, {}, f"cad-cli was not found ({location})"
    inspect_ok, inspect_error = _inspect_project(cli, project_dir)
    if not inspect_ok:
        return False, {}, inspect_error
    artifacts, export_error = _export_track(track, cli, project_dir)
    return True, artifacts, export_error


def _export_track(track: str, cli: Path, project_dir: Path) -> tuple[dict[str, str], str | None]:
    artifacts: dict[str, str] = {}
    errors: list[str] = []
    if track == TRACK_TPMS_IMPLICIT:
        _export_one(
            artifacts,
            errors,
            cli,
            project_dir / "out.cli",
            [
                "slice",
                str(project_dir),
                "--out-cli",
                str(project_dir / "out.cli"),
                "--layer-height",
                "2",
                "--xy-step",
                "1",
            ],
        )
        _export_one(
            artifacts,
            errors,
            cli,
            project_dir / "out.3mf",
            [
                "export-3mf",
                str(project_dir),
                "--out",
                str(project_dir / "out.3mf"),
                "--resolution",
                "8",
            ],
        )
    elif track == TRACK_SUBD_FREEFORM:
        _export_one(
            artifacts,
            errors,
            cli,
            project_dir / "out.3mf",
            ["export-3mf", str(project_dir), "--out", str(project_dir / "out.3mf")],
        )
    elif track == TRACK_KCL_PRISMATIC:
        drawing = project_dir / "drawing.svg"
        _export_one(
            artifacts,
            errors,
            cli,
            drawing,
            [
                "render-drawing",
                str(project_dir),
                "--sheet",
                _SHEET_ID,
                "--out-svg",
                str(drawing),
            ],
        )
    if errors:
        return artifacts, "; ".join(errors)
    return artifacts, None


def _export_one(
    artifacts: dict[str, str],
    errors: list[str],
    cli: Path,
    output: Path,
    args: list[str],
) -> None:
    ok, message = _run_project(cli, args)
    if not ok:
        errors.append(message or f"cad-cli project {args[0]} failed")
        return
    if not output.is_file() or output.stat().st_size <= 0:
        errors.append(f"{output.name} was not written")
        return
    artifacts[output.name] = str(output)


def _inspect_project(cli: Path, project_dir: Path) -> tuple[bool, str | None]:
    ok, message = _run_project(cli, ["inspect", str(project_dir)])
    if ok:
        return True, None
    return False, message or "cad-cli project inspect failed"


def _run_project(cli: Path, args: list[str]) -> tuple[bool, str | None]:
    try:
        completed = subprocess.run(
            [str(cli), "project", *args, "--json"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=_CLI_TIMEOUT_SEC,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return False, f"cad-cli timed out after {_CLI_TIMEOUT_SEC}s"
    except OSError as exc:
        return False, f"failed to launch cad-cli: {exc}"
    payload = _parse_json_object(completed.stdout or "")
    if completed.returncode == 0 and payload is not None and payload.get("status") == "ok":
        return True, None
    return False, _cli_error(completed, payload)


def _resolve_cli(cli_bin: Path | None) -> Path | None:
    # An explicit path that is missing must not fall through to another build.
    if cli_bin is not None:
        candidate = Path(cli_bin)
        return candidate if candidate.is_file() else None
    return discover_cad_cli()


def _log_attempt(
    trajectory_path: Path | None,
    *,
    route: str,
    prompt: str,
    project_dir: Path,
    artifacts: dict[str, str],
    verified_ok: bool,
) -> str | None:
    """Append one flywheel row. Return an error string, or None when logging is done.

    ``trajectory_path is None`` is not an error: the caller asked not to log.
    """
    if trajectory_path is None:
        return None
    before = ""
    after = _graph_sha256(project_dir)
    patch = {
        "patch_id": f"orchestrate-{route}",
        "description": prompt,
        "ops": [
            {
                "op": "generate_project",
                "track": route,
                "project_dir": str(project_dir),
                "artifacts": sorted(artifacts),
            }
        ],
    }
    try:
        record = TrajectoryRecord(
            timestamp_utc=utc_timestamp(),
            task_route=route,
            prompt=prompt,
            patch_id=str(patch["patch_id"]),
            before_graph_sha256=before,
            after_graph_sha256=after,
            patch=patch,
            verified_ok=verified_ok,
            reward_score=default_reward_score(verified_ok, before, after),
        )
        TrajectoryLogger(trajectory_path).append(record)
    except Exception as exc:
        return f"trajectory log failed: {exc}"
    return None


def _graph_sha256(project_dir: Path) -> str:
    graph_path = project_dir / "graph.ir.json"
    if not graph_path.is_file():
        return ""
    return hashlib.sha256(graph_path.read_bytes()).hexdigest()


def _load_aicad() -> Any:
    """Import ``aicad``, adding ``packages/python-sdk/src`` when it is not installed."""
    try:
        return importlib.import_module("aicad")
    except ImportError:
        sdk_src = (
            find_workspace_root(Path(__file__)) / "packages" / "python-sdk" / "src"
        ).resolve()
        sdk_text = str(sdk_src)
        existing: set[str] = set()
        for entry in sys.path:
            if not entry:
                continue
            try:
                existing.add(str(Path(entry).resolve()))
            except OSError:
                continue
        if sdk_text not in existing:
            sys.path.insert(0, sdk_text)
        return importlib.import_module("aicad")


def _parse_json_object(stdout: str) -> dict[str, Any] | None:
    text = stdout.strip()
    if not text:
        return None
    try:
        payload = json.loads(text)
    except json.JSONDecodeError:
        return None
    if isinstance(payload, dict):
        return payload
    return None


def _cli_error(
    completed: subprocess.CompletedProcess[str],
    payload: dict[str, Any] | None,
) -> str:
    if payload is not None:
        message = payload.get("message")
        if isinstance(message, str) and message.strip():
            return message.strip()
    detail = (completed.stderr or "").strip() or (completed.stdout or "").strip()
    if len(detail) > 2000:
        detail = detail[:2000]
    if detail:
        return detail
    return f"cad-cli exited with code {completed.returncode}"


__all__ = [
    "OrchestrationResult",
    "orchestrate_multitrack_design",
]

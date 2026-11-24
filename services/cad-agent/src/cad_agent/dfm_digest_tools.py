"""Closed-loop DFM gate and bounded ROI digest for the repair agent.

The helpers shell out to ``cad-cli project``:

* ``export-cadpack --out`` writes a cadpack and returns its summary JSON.
* ``cadpack-digest --min X,Y,Z --max X,Y,Z`` returns a ``RoiInterfaceDigest``.
  Success JSON has no ``status`` field. Failures print ``{"status": "error"}``.
* ``dfm-check`` returns interference pairs and per-face ``thin_wall`` /
  ``draft_angle`` / ``tool_accessibility`` violations. ``passed`` is false when
  either check fails, and the process still exits 0.

``cadpack-digest`` pretty-prints JSON. The 12 KiB budget is defined on the
compact digest, so :func:`fetch_cadpack_roi_digest` stores that compact form
and rejects a summary above ``12 * 1024`` bytes.
"""

from __future__ import annotations

import copy
import json
import math
import subprocess
import tempfile
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .compiler_bridge import discover_cad_cli


# Matches ``cad_cadpack::digest::MAX_DIGEST_JSON_BYTES``.
_MAX_ROI_DIGEST_BYTES = 12 * 1024
_CLI_TIMEOUT_SEC = 180
_LIST_LIMIT = 32

_Vec3 = tuple[float, float, float]


class DfmDigestError(RuntimeError):
    """Raised when ``cad-cli`` cannot export, digest, or DFM-check a project."""


@dataclass(frozen=True, slots=True)
class CadpackExportResult:
    """A cadpack written by ``cad-cli project export-cadpack``."""

    cadpack_path: Path
    raw_json: dict[str, Any]

    def __post_init__(self) -> None:
        if not isinstance(self.raw_json, dict):
            raise TypeError("raw_json must be an object")
        object.__setattr__(self, "cadpack_path", Path(self.cadpack_path))
        object.__setattr__(self, "raw_json", copy.deepcopy(self.raw_json))

    def to_dict(self) -> dict[str, Any]:
        return {
            "cadpack_path": str(self.cadpack_path),
            "raw_json": copy.deepcopy(self.raw_json),
        }


@dataclass(frozen=True, slots=True)
class RoiDigestResult:
    """Compact ROI digest returned by ``cad-cli project cadpack-digest``."""

    roi_min_mm: _Vec3
    roi_max_mm: _Vec3
    component_count: int
    summary_json: str
    byte_length: int

    def __post_init__(self) -> None:
        object.__setattr__(self, "roi_min_mm", _require_vec3(self.roi_min_mm, "roi_min_mm"))
        object.__setattr__(self, "roi_max_mm", _require_vec3(self.roi_max_mm, "roi_max_mm"))
        object.__setattr__(
            self,
            "component_count",
            _require_count(self.component_count, "component_count"),
        )
        if not isinstance(self.summary_json, str):
            raise TypeError("summary_json must be a string")
        object.__setattr__(self, "byte_length", _require_count(self.byte_length, "byte_length"))

    def to_dict(self) -> dict[str, Any]:
        return {
            "roi_min_mm": [float(value) for value in self.roi_min_mm],
            "roi_max_mm": [float(value) for value in self.roi_max_mm],
            "component_count": self.component_count,
            "summary_json": self.summary_json,
            "byte_length": self.byte_length,
        }


@dataclass(frozen=True, slots=True)
class DfmCheckReport:
    """Manufacturing report returned by ``cad-cli project dfm-check``."""

    passed: bool
    interference_count: int
    violation_count: int
    min_thickness_mm: float | None
    raw_json: dict[str, Any]

    def __post_init__(self) -> None:
        if not isinstance(self.passed, bool):
            raise TypeError("passed must be a bool")
        object.__setattr__(
            self,
            "interference_count",
            _require_count(self.interference_count, "interference_count"),
        )
        object.__setattr__(
            self,
            "violation_count",
            _require_count(self.violation_count, "violation_count"),
        )
        object.__setattr__(self, "min_thickness_mm", _optional_mm(self.min_thickness_mm))
        if not isinstance(self.raw_json, dict):
            raise TypeError("raw_json must be an object")
        object.__setattr__(self, "raw_json", copy.deepcopy(self.raw_json))

    def to_dict(self) -> dict[str, Any]:
        return {
            "passed": self.passed,
            "interference_count": self.interference_count,
            "violation_count": self.violation_count,
            "min_thickness_mm": self.min_thickness_mm,
            "raw_json": copy.deepcopy(self.raw_json),
        }


@dataclass(frozen=True, slots=True)
class DfmRepairFeedback:
    """Repair-agent prompt plus the DFM report and optional ROI digest."""

    needs_repair: bool
    prompt_block: str
    roi_digest: RoiDigestResult | None
    dfm_report: DfmCheckReport

    def __post_init__(self) -> None:
        if not isinstance(self.needs_repair, bool):
            raise TypeError("needs_repair must be a bool")
        if not isinstance(self.prompt_block, str):
            raise TypeError("prompt_block must be a string")
        if self.roi_digest is not None and not isinstance(self.roi_digest, RoiDigestResult):
            raise TypeError("roi_digest must be a RoiDigestResult or None")
        if not isinstance(self.dfm_report, DfmCheckReport):
            raise TypeError("dfm_report must be a DfmCheckReport")

    def to_dict(self) -> dict[str, Any]:
        return {
            "needs_repair": self.needs_repair,
            "prompt_block": self.prompt_block,
            "roi_digest": None if self.roi_digest is None else self.roi_digest.to_dict(),
            "dfm_report": self.dfm_report.to_dict(),
        }


def export_project_cadpack(
    project_path: Path,
    out_cadpack: Path,
    *,
    cad_cli_bin: Path | None = None,
) -> CadpackExportResult:
    """Export ``project_path`` to ``out_cadpack`` with ``project export-cadpack``."""
    project_path = Path(project_path)
    out_cadpack = Path(out_cadpack)
    if _same_path(project_path, out_cadpack):
        raise DfmDigestError("cadpack output must differ from the input project")
    if not project_path.exists():
        raise DfmDigestError(f"project path does not exist: {project_path}")
    if out_cadpack.exists() and not out_cadpack.is_file():
        raise DfmDigestError(f"cadpack output is not a file: {out_cadpack}")
    out_cadpack.parent.mkdir(parents=True, exist_ok=True)
    # The CLI renames a temp file onto ``--out``. Windows rename will not
    # replace an existing file, so drop a previous cadpack first.
    if out_cadpack.is_file():
        out_cadpack.unlink()

    _completed, payload = _invoke(
        _resolve_cli(cad_cli_bin),
        ["export-cadpack", str(project_path), "--out", str(out_cadpack)],
    )
    written = out_cadpack.resolve()
    if not written.is_file() or written.stat().st_size <= 0:
        raise DfmDigestError(f"cad-cli did not write a cadpack at {written}")
    return CadpackExportResult(cadpack_path=written, raw_json=payload)


def fetch_cadpack_roi_digest(
    cadpack_path: Path,
    roi_min_mm: Sequence[float],
    roi_max_mm: Sequence[float],
    *,
    cad_cli_bin: Path | None = None,
) -> RoiDigestResult:
    """Return the compact ROI digest for ``cadpack_path``.

    ``byte_length`` is the UTF-8 size of ``summary_json`` and must stay
    ``<= 12 * 1024``.
    """
    roi_min = _require_vec3(roi_min_mm, "roi_min_mm")
    roi_max = _require_vec3(roi_max_mm, "roi_max_mm")
    _require_ordered_roi(roi_min, roi_max)
    cadpack_path = Path(cadpack_path)
    if not cadpack_path.is_file():
        raise DfmDigestError(f"cadpack does not exist: {cadpack_path}")

    _completed, payload = _invoke(
        _resolve_cli(cad_cli_bin),
        [
            "cadpack-digest",
            str(cadpack_path),
            "--min",
            _format_vec3(roi_min),
            "--max",
            _format_vec3(roi_max),
        ],
        require_status_ok=False,
    )
    if payload.get("status") == "error":
        raise DfmDigestError(f"cad-cli project cadpack-digest failed: {_message(payload)}")
    return _digest_from_payload(payload, roi_min, roi_max)


def run_project_dfm_check(
    project_path: Path,
    *,
    min_wall_mm: float = 1.5,
    min_draft_deg: float = 1.0,
    pull_dir: Sequence[float] = (0.0, 0.0, 1.0),
    cad_cli_bin: Path | None = None,
) -> DfmCheckReport:
    """Run ``project dfm-check`` and map the JSON report."""
    wall_mm = _require_finite(min_wall_mm, "min_wall_mm")
    if wall_mm < 0.0:
        raise ValueError("min_wall_mm must be nonnegative")
    draft_deg = _require_finite(min_draft_deg, "min_draft_deg")
    direction = _require_vec3(pull_dir, "pull_dir")
    project_path = Path(project_path)
    if not project_path.exists():
        raise DfmDigestError(f"project path does not exist: {project_path}")

    _completed, payload = _invoke(
        _resolve_cli(cad_cli_bin),
        [
            "dfm-check",
            str(project_path),
            "--min-wall-mm",
            _format_cli_number(wall_mm),
            "--min-draft-deg",
            _format_cli_number(draft_deg),
            "--pull-dir",
            _format_vec3(direction),
        ],
    )
    return _report_from_payload(payload)


def build_dfm_repair_feedback(
    dfm_report: DfmCheckReport,
    roi_digest: RoiDigestResult | None = None,
) -> DfmRepairFeedback:
    """Format an actionable repair prompt from a DFM report and optional ROI digest."""
    if not isinstance(dfm_report, DfmCheckReport):
        raise TypeError("dfm_report must be a DfmCheckReport")
    if roi_digest is not None and not isinstance(roi_digest, RoiDigestResult):
        raise TypeError("roi_digest must be a RoiDigestResult or None")

    needs_repair = not dfm_report.passed
    names = _instance_names(dfm_report.raw_json)
    lines: list[str] = []
    if needs_repair:
        lines.append(
            "DFM gate failed. Repair the project graph so interference, "
            "wall thickness, and draft all pass."
        )
    else:
        lines.append("DFM gate passed. No manufacturing repair is required.")

    config = dfm_report.raw_json.get("config")
    if isinstance(config, dict):
        lines.append(
            "Gates: min_wall_mm={wall}, min_draft_deg={draft}, pull_dir={pull}.".format(
                wall=_fmt_num(config.get("min_wall_mm")),
                draft=_fmt_num(config.get("min_draft_deg")),
                pull=_fmt_vec(config.get("pull_dir")),
            )
        )

    lines.append(f"Interference pairs ({dfm_report.interference_count}):")
    lines.extend(_interference_lines(dfm_report, names))
    lines.append(f"Wall and draft violations ({dfm_report.violation_count}):")
    lines.extend(_violation_lines(dfm_report, names))
    if dfm_report.min_thickness_mm is None:
        lines.append("Thinnest measured wall: not reported.")
    else:
        lines.append(f"Thinnest measured wall: {_fmt_num(dfm_report.min_thickness_mm)} mm.")

    if roi_digest is None:
        lines.append("ROI digest: not provided.")
    else:
        lines.append(
            "ROI digest ({size} bytes, {count} components, "
            "roi_min_mm={roi_min}, roi_max_mm={roi_max}):".format(
                size=roi_digest.byte_length,
                count=roi_digest.component_count,
                roi_min=_fmt_vec(roi_digest.roi_min_mm),
                roi_max=_fmt_vec(roi_digest.roi_max_mm),
            )
        )
        lines.append(roi_digest.summary_json)
        lines.append("Keep geometry edits inside this ROI.")

    if needs_repair:
        lines.append(
            "Separate penetrating instances, thicken every thin_wall face to the wall gate, "
            "and add draft so draft_angle faces clear the pull direction. "
            "Then run the DFM gate again."
        )
    return DfmRepairFeedback(
        needs_repair=needs_repair,
        prompt_block="\n".join(lines),
        roi_digest=roi_digest,
        dfm_report=dfm_report,
    )


def run_dfm_gate_and_digest(
    project_path: Path,
    *,
    roi_min_mm: Sequence[float] | None = None,
    roi_max_mm: Sequence[float] | None = None,
    min_wall_mm: float = 1.5,
    min_draft_deg: float = 1.0,
    cad_cli_bin: Path | None = None,
) -> DfmRepairFeedback:
    """Run the DFM gate and, when an ROI is given, attach a bounded cadpack digest."""
    roi_min, roi_max = _optional_roi(roi_min_mm, roi_max_mm)
    report = run_project_dfm_check(
        project_path,
        min_wall_mm=min_wall_mm,
        min_draft_deg=min_draft_deg,
        cad_cli_bin=cad_cli_bin,
    )
    digest: RoiDigestResult | None = None
    if roi_min is not None and roi_max is not None:
        with tempfile.TemporaryDirectory(prefix="cad-dfm-") as temp_dir:
            cadpack_path = Path(temp_dir) / "gate.cadpack"
            export_project_cadpack(project_path, cadpack_path, cad_cli_bin=cad_cli_bin)
            digest = fetch_cadpack_roi_digest(
                cadpack_path,
                roi_min,
                roi_max,
                cad_cli_bin=cad_cli_bin,
            )
    return build_dfm_repair_feedback(report, digest)


def _optional_roi(
    roi_min_mm: Sequence[float] | None,
    roi_max_mm: Sequence[float] | None,
) -> tuple[_Vec3 | None, _Vec3 | None]:
    if roi_min_mm is None and roi_max_mm is None:
        return None, None
    if roi_min_mm is None or roi_max_mm is None:
        raise ValueError("roi_min_mm and roi_max_mm must be provided together")
    roi_min = _require_vec3(roi_min_mm, "roi_min_mm")
    roi_max = _require_vec3(roi_max_mm, "roi_max_mm")
    _require_ordered_roi(roi_min, roi_max)
    return roi_min, roi_max


def _require_ordered_roi(roi_min: _Vec3, roi_max: _Vec3) -> None:
    if any(left > right for left, right in zip(roi_min, roi_max, strict=True)):
        raise ValueError("roi_min_mm must be less than or equal to roi_max_mm on every axis")


def _resolve_cli(cad_cli_bin: Path | None) -> Path:
    # An explicit path that is missing must not fall through to another build.
    if cad_cli_bin is not None:
        candidate = Path(cad_cli_bin)
        if not candidate.is_file():
            raise DfmDigestError(f"cad-cli was not found at {candidate}")
        return candidate
    found = discover_cad_cli()
    if found is None:
        raise DfmDigestError(
            "cad-cli was not found (pass cad_cli_bin, set CAD_CLI_BIN, or build target/debug)"
        )
    return found


def _invoke(
    cli: Path,
    args: list[str],
    *,
    require_status_ok: bool = True,
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
    command = args[0] if args else "project"
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
    except subprocess.TimeoutExpired as exc:
        raise DfmDigestError(f"cad-cli timed out after {_CLI_TIMEOUT_SEC}s") from exc
    except OSError as exc:
        raise DfmDigestError(f"failed to launch cad-cli: {exc}") from exc

    payload = _parse_json_object(completed.stdout or "")
    if payload is None:
        raise DfmDigestError(
            f"cad-cli project {command} did not return JSON: {_cli_error(completed, None)}"
        )
    if require_status_ok and (completed.returncode != 0 or payload.get("status") != "ok"):
        raise DfmDigestError(
            f"cad-cli project {command} failed: {_cli_error(completed, payload)}"
        )
    if not require_status_ok and completed.returncode != 0:
        raise DfmDigestError(
            f"cad-cli project {command} failed: {_cli_error(completed, payload)}"
        )
    return completed, payload


def _digest_from_payload(
    payload: dict[str, Any],
    roi_min: _Vec3,
    roi_max: _Vec3,
) -> RoiDigestResult:
    echoed_min = _require_vec3(payload.get("roi_min_mm"), "roi_min_mm")
    echoed_max = _require_vec3(payload.get("roi_max_mm"), "roi_max_mm")
    if not _same_vec(echoed_min, roi_min) or not _same_vec(echoed_max, roi_max):
        raise DfmDigestError(
            "cadpack-digest echoed ROI "
            f"{echoed_min}..{echoed_max}, expected {roi_min}..{roi_max}"
        )
    components = payload.get("components")
    if not isinstance(components, list):
        raise DfmDigestError("cadpack-digest JSON is missing components")
    total_matches = payload.get("total_matches")
    if isinstance(total_matches, bool) or not isinstance(total_matches, int):
        component_count = len(components)
    else:
        component_count = total_matches
    if component_count < 0:
        raise DfmDigestError("cadpack-digest total_matches must be nonnegative")
    summary_json = _compact_json(payload)
    byte_length = len(summary_json.encode("utf-8"))
    # Repair-agent context budget. Compact JSON matches the cadpack 12 KiB cap.
    if byte_length > _MAX_ROI_DIGEST_BYTES:
        raise DfmDigestError(
            f"ROI digest is {byte_length} bytes, which exceeds the {_MAX_ROI_DIGEST_BYTES} byte limit"
        )
    assert byte_length <= 12 * 1024
    return RoiDigestResult(
        roi_min_mm=echoed_min,
        roi_max_mm=echoed_max,
        component_count=component_count,
        summary_json=summary_json,
        byte_length=byte_length,
    )


def _report_from_payload(payload: dict[str, Any]) -> DfmCheckReport:
    passed = payload.get("passed")
    if not isinstance(passed, bool):
        raise DfmDigestError("dfm-check JSON is missing boolean `passed`")
    interference = payload.get("interference")
    dfm = payload.get("dfm")
    if not isinstance(interference, dict) or not isinstance(dfm, dict):
        raise DfmDigestError("dfm-check JSON is missing interference or dfm objects")
    pairs = interference.get("pairs")
    faces = dfm.get("faces")
    if not isinstance(pairs, list) or not isinstance(faces, list):
        raise DfmDigestError("dfm-check pairs and faces must be lists")
    return DfmCheckReport(
        passed=passed,
        interference_count=_json_count(interference.get("pair_count"), len(pairs)),
        violation_count=_json_count(dfm.get("violation_count"), _count_violating_faces(faces)),
        min_thickness_mm=_min_thickness(faces),
        raw_json=payload,
    )


def _json_count(value: Any, fallback: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        return fallback
    return value


def _count_violating_faces(faces: list[Any]) -> int:
    count = 0
    for face in faces:
        if isinstance(face, dict) and _violation_labels(face):
            count += 1
    return count


def _min_thickness(faces: list[Any]) -> float | None:
    thicknesses: list[float] = []
    for face in faces:
        if not isinstance(face, dict):
            continue
        value = face.get("min_thickness_mm")
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            continue
        number = float(value)
        if math.isfinite(number):
            thicknesses.append(number)
    if not thicknesses:
        return None
    return min(thicknesses)


def _instance_names(raw_json: dict[str, Any]) -> dict[int, str]:
    names: dict[int, str] = {}
    instances = raw_json.get("instances")
    if not isinstance(instances, list):
        return names
    for item in instances:
        if not isinstance(item, dict):
            continue
        instance_id = item.get("instance_id")
        name = item.get("name")
        if (
            isinstance(instance_id, int)
            and not isinstance(instance_id, bool)
            and isinstance(name, str)
            and name
        ):
            names[instance_id] = name
    return names


def _interference_lines(dfm_report: DfmCheckReport, names: dict[int, str]) -> list[str]:
    interference = dfm_report.raw_json.get("interference")
    pairs = interference.get("pairs") if isinstance(interference, dict) else None
    if not isinstance(pairs, list) or not pairs:
        if dfm_report.interference_count == 0:
            return ["- none"]
        return ["- pair details were not included in the report."]

    lines: list[str] = []
    for pair in pairs:
        if not isinstance(pair, dict):
            continue
        state = "penetrating" if pair.get("penetrating") is True else "clearance"
        lines.append(
            "- instance {left} face {face_a} intersects instance {right} face {face_b}; "
            "{state}; min_distance_mm={distance}.".format(
                left=_instance_label(names, pair.get("instance_a")),
                right=_instance_label(names, pair.get("instance_b")),
                face_a=pair.get("face_a"),
                face_b=pair.get("face_b"),
                state=state,
                distance=_fmt_num(pair.get("min_distance_mm")),
            )
        )
        if len(lines) >= _LIST_LIMIT:
            break
    if not lines:
        return ["- pair details were not included in the report."]
    omitted = dfm_report.interference_count - len(lines)
    if omitted > 0:
        lines.append(f"- {omitted} more interference pairs omitted.")
    return lines


def _violation_lines(dfm_report: DfmCheckReport, names: dict[int, str]) -> list[str]:
    dfm = dfm_report.raw_json.get("dfm")
    faces = dfm.get("faces") if isinstance(dfm, dict) else None
    if not isinstance(faces, list) or not faces:
        if dfm_report.violation_count == 0:
            return ["- none"]
        return ["- violation details were not included in the report."]

    lines: list[str] = []
    for face in faces:
        if not isinstance(face, dict):
            continue
        labels = _violation_labels(face)
        if not labels:
            continue
        lines.append(
            "- instance {instance} face {slot}: {kinds}; "
            "min_thickness_mm={thickness}; draft_angle_deg={draft}.".format(
                instance=_instance_label(names, face.get("instance_id")),
                slot=face.get("face_slot_id"),
                kinds=", ".join(labels),
                thickness=_fmt_num(face.get("min_thickness_mm")),
                draft=_fmt_num(face.get("draft_angle_deg")),
            )
        )
        if len(lines) >= _LIST_LIMIT:
            break
    if not lines:
        if dfm_report.violation_count == 0:
            return ["- none"]
        return ["- violation details were not included in the report."]
    omitted = dfm_report.violation_count - len(lines)
    if omitted > 0:
        lines.append(f"- {omitted} more violating faces omitted.")
    return lines


def _violation_labels(face: dict[str, Any]) -> list[str]:
    kinds = face.get("violations")
    if not isinstance(kinds, list):
        return []
    return [kind for kind in kinds if isinstance(kind, str) and kind]


def _instance_label(names: dict[int, str], instance_id: Any) -> str:
    if isinstance(instance_id, bool) or not isinstance(instance_id, int):
        return str(instance_id)
    name = names.get(instance_id)
    if name:
        return f"{instance_id} ({name})"
    return str(instance_id)


def _compact_json(payload: dict[str, Any]) -> str:
    try:
        return json.dumps(payload, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
    except (TypeError, ValueError) as exc:
        raise DfmDigestError(f"cadpack-digest JSON is not serializable: {exc}") from exc


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


def _message(payload: dict[str, Any]) -> str:
    message = payload.get("message")
    if isinstance(message, str) and message.strip():
        return message.strip()
    return "cad-cli returned an error"


def _cli_error(
    completed: subprocess.CompletedProcess[str],
    payload: dict[str, Any] | None,
) -> str:
    if payload is not None:
        message = payload.get("message")
        if isinstance(message, str) and message.strip():
            detail = message.strip()
        else:
            detail = ""
    else:
        detail = ""
    if not detail:
        detail = (completed.stderr or "").strip() or (completed.stdout or "").strip()
    if len(detail) > 2000:
        detail = detail[:2000]
    if detail:
        return detail
    return f"cad-cli exited with code {completed.returncode}"


def _require_vec3(values: Any, name: str) -> _Vec3:
    if isinstance(values, (str, bytes)) or not isinstance(values, Sequence):
        raise TypeError(f"{name} must be a sequence of three finite floats")
    if len(values) != 3:
        raise ValueError(f"{name} must contain three coordinates")
    coords: list[float] = []
    for value in values:
        coords.append(_require_finite(value, name))
    return (coords[0], coords[1], coords[2])


def _require_finite(value: Any, name: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"{name} must be a float")
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"{name} must be finite")
    return number


def _require_count(value: Any, name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{name} must be an int")
    if value < 0:
        raise ValueError(f"{name} must be nonnegative")
    return value


def _optional_mm(value: Any) -> float | None:
    if value is None:
        return None
    return _require_finite(value, "min_thickness_mm")


def _format_vec3(coords: _Vec3) -> str:
    return ",".join(_format_cli_number(value) for value in coords)


def _format_cli_number(value: float) -> str:
    return format(value, ".16g")


def _fmt_num(value: Any) -> str:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return "none"
    number = float(value)
    if not math.isfinite(number):
        return "none"
    text = f"{number:.4f}".rstrip("0").rstrip(".")
    return text or "0"


def _fmt_vec(value: Any) -> str:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
        return "none"
    return "[" + ", ".join(_fmt_num(item) for item in value) + "]"


def _same_vec(actual: _Vec3, expected: _Vec3) -> bool:
    return all(
        abs(left - right) <= 1e-6 * max(1.0, abs(left), abs(right))
        for left, right in zip(actual, expected, strict=True)
    )


def _same_path(left: Path, right: Path) -> bool:
    return left.resolve() == right.resolve()


__all__ = [
    "CadpackExportResult",
    "DfmCheckReport",
    "DfmDigestError",
    "DfmRepairFeedback",
    "RoiDigestResult",
    "build_dfm_repair_feedback",
    "export_project_cadpack",
    "fetch_cadpack_roi_digest",
    "run_dfm_gate_and_digest",
    "run_project_dfm_check",
]

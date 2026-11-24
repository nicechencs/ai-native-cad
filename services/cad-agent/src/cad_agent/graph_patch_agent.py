"""Route design prompts and record `.aicad` GraphPatch trajectories.

The router is a fixed rule table. It sends algorithmic curves to the Python
SDK, TPMS lattices to the implicit field track, freeform shells to SubD, and
ordinary prismatic parts to `.kcl`. `GraphPatchSession` applies a patch with
`cad-cli project apply-patch` and appends one JSONL row for the Phase G3
training flywheel, whether the patch commits or rolls back.
"""

from __future__ import annotations

import copy
import hashlib
import json
import math
import re
import subprocess
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal

from .compiler_bridge import discover_cad_cli


TrackName = Literal[
    "python_sdk_curve",
    "tpms_implicit",
    "subd_freeform",
    "kcl_prismatic",
]

TRACK_PYTHON_SDK_CURVE: TrackName = "python_sdk_curve"
TRACK_TPMS_IMPLICIT: TrackName = "tpms_implicit"
TRACK_SUBD_FREEFORM: TrackName = "subd_freeform"
TRACK_KCL_PRISMATIC: TrackName = "kcl_prismatic"

# Destinations named by the G2/G3 task split. The router only classifies;
# callers invoke the generator, slicer, or `.kcl` coder for the chosen track.
TRACK_TARGETS: dict[TrackName, str] = {
    TRACK_PYTHON_SDK_CURVE: "packages/python-sdk aicad generator",
    TRACK_TPMS_IMPLICIT: ".aicad FieldTpms + ImplicitLatticeInfill + cad-cli project slice",
    TRACK_SUBD_FREEFORM: "SubDCage + @subd blob",
    TRACK_KCL_PRISMATIC: ".kcl prismatic feature graph",
}

# First match wins. TPMS is ahead of "gear" or "shell" so a gyroid cold plate
# is not sent to the curve SDK or the SubD track.
_TPMS_RULES: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"\bgyroid\b", re.IGNORECASE), "gyroid"),
    (re.compile(r"\bschwarz(?:p|[\s_-]+p)\b", re.IGNORECASE), "schwarz p"),
    (re.compile(r"\blidinoids?\b", re.IGNORECASE), "lidinoid"),
    (re.compile(r"\btpms\b", re.IGNORECASE), "tpms"),
    (
        re.compile(
            r"\bdiamond\b(?:\s+\w+){0,4}\s+\b(?:tpms|lattice|infill)\b"
            r"|\b(?:tpms|lattice|infill)\b(?:\s+\w+){0,4}\s+\bdiamond\b",
            re.IGNORECASE,
        ),
        "diamond lattice",
    ),
    (re.compile(r"\blattice\s+infill\b", re.IGNORECASE), "lattice infill"),
    (re.compile(r"\bimplicit\s+lattice\b", re.IGNORECASE), "implicit lattice"),
    (re.compile(r"\bheat[\s-]*exchanger\s+lattice\b", re.IGNORECASE), "heat-exchanger lattice"),
)

_PYTHON_SDK_RULES: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"\bnaca\b", re.IGNORECASE), "naca airfoil"),
    (re.compile(r"\b(?:air|aero)foils?\b", re.IGNORECASE), "airfoil"),
    (re.compile(r"\binvolutes?\b", re.IGNORECASE), "involute"),
    (re.compile(r"\bgears?\b", re.IGNORECASE), "gear"),
    (re.compile(r"\b(?:helices|helixes|helical|helix)\b", re.IGNORECASE), "helix"),
    (re.compile(r"\bcamshafts?\b|\bcams?\b", re.IGNORECASE), "cam"),
)

_SUBD_RULES: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"\bsub-?d(?:_|\b)", re.IGNORECASE), "subd"),
    (re.compile(r"\bcontrol\s+cage\b", re.IGNORECASE), "control cage"),
    (re.compile(r"\borganic\s+shell\b", re.IGNORECASE), "organic shell"),
    (
        re.compile(
            r"\bergonomic\b(?:\s+\w+){0,4}\s+\b(?:shell|handle|grip|cage)\b",
            re.IGNORECASE,
        ),
        "ergonomic shell",
    ),
    (re.compile(r"\bfreeform\b", re.IGNORECASE), "freeform"),
    (re.compile(r"@subd\b", re.IGNORECASE), "@subd blob"),
    (re.compile(r"\bcatmull[\s-]*clark\b", re.IGNORECASE), "catmull-clark"),
)

_CLI_TIMEOUT_SEC = 120
_GRAPH_FILE_NAME = "graph.ir.json"

_TRAJECTORY_FIELDS = (
    "timestamp_utc",
    "task_route",
    "prompt",
    "patch_id",
    "before_graph_sha256",
    "after_graph_sha256",
    "patch",
    "verified_ok",
    "reward_score",
)


@dataclass(frozen=True, slots=True)
class TrackRoute:
    """One deterministic routing decision."""

    track: TrackName
    target: str
    reason: str


@dataclass(frozen=True, slots=True)
class PatchApplyResult:
    """What remained on disk after `cad-cli project apply-patch`."""

    ok: bool
    before_sha256: str
    after_sha256: str
    node_count: int
    error: str | None = None


@dataclass(frozen=True, slots=True)
class TrajectoryRecord:
    """One JSONL row in the GraphPatch training flywheel."""

    timestamp_utc: str
    task_route: str
    prompt: str
    patch_id: str
    before_graph_sha256: str
    after_graph_sha256: str
    patch: dict[str, Any]
    verified_ok: bool
    reward_score: float

    def __post_init__(self) -> None:
        if not isinstance(self.prompt, str):
            raise TypeError("prompt must be a string")
        if not isinstance(self.task_route, str):
            raise TypeError("task_route must be a string")
        if not isinstance(self.patch_id, str):
            raise TypeError("patch_id must be a string")
        if not isinstance(self.patch, dict):
            raise TypeError("patch must be an object")
        if not isinstance(self.verified_ok, bool):
            raise TypeError("verified_ok must be a bool")
        score = float(self.reward_score)
        if not math.isfinite(score):
            raise ValueError("reward_score must be finite")
        object.__setattr__(self, "patch", copy.deepcopy(self.patch))
        object.__setattr__(self, "reward_score", score)

    def to_dict(self) -> dict[str, Any]:
        """Return the flywheel fields in a stable order."""
        return {
            "timestamp_utc": self.timestamp_utc,
            "task_route": self.task_route,
            "prompt": self.prompt,
            "patch_id": self.patch_id,
            "before_graph_sha256": self.before_graph_sha256,
            "after_graph_sha256": self.after_graph_sha256,
            "patch": copy.deepcopy(self.patch),
            "verified_ok": self.verified_ok,
            "reward_score": self.reward_score,
        }


class TrajectoryLogger:
    """Append-only JSONL log of GUI and AI GraphPatch outcomes."""

    def __init__(self, jsonl_path: Path) -> None:
        self.jsonl_path = Path(jsonl_path)

    def append(self, record: TrajectoryRecord) -> str:
        """Write one deterministic JSON object and return that line."""
        if not isinstance(record, TrajectoryRecord):
            raise TypeError("record must be a TrajectoryRecord")
        line = trajectory_json(record)
        self.jsonl_path.parent.mkdir(parents=True, exist_ok=True)
        with self.jsonl_path.open("a", encoding="utf-8", newline="\n") as handle:
            handle.write(f"{line}\n")
        return line

    def read_records(self) -> list[dict[str, Any]]:
        """Parse every non-empty line. Raises if a line is not a JSON object."""
        if not self.jsonl_path.is_file():
            return []
        records: list[dict[str, Any]] = []
        text = self.jsonl_path.read_text(encoding="utf-8")
        for line_number, line in enumerate(text.splitlines(), start=1):
            if not line.strip():
                continue
            payload = json.loads(line)
            if not isinstance(payload, dict):
                raise ValueError(f"line {line_number} is not a JSON object")
            missing = [field for field in _TRAJECTORY_FIELDS if field not in payload]
            if missing:
                raise ValueError(f"line {line_number} is missing {', '.join(missing)}")
            records.append(payload)
        return records


class GraphPatchSession:
    """Apply a GraphPatch with cad-cli and keep the trajectory log."""

    def __init__(
        self,
        jsonl_path: Path,
        cli_bin: Path | None = None,
        *,
        clock: Callable[[], str] | None = None,
    ) -> None:
        self.logger = TrajectoryLogger(jsonl_path)
        self.cli_bin = None if cli_bin is None else Path(cli_bin)
        self._clock = clock or utc_timestamp

    def route(self, prompt: str) -> TrackRoute:
        """Classify `prompt` with the same rules as `route_design_task`."""
        return route_design_task(prompt)

    def apply(
        self,
        project_path: Path,
        patch: dict[str, Any],
        prompt: str,
        *,
        task_route: str | TrackRoute | None = None,
        reward_score: float | None = None,
    ) -> PatchApplyResult:
        """Apply `patch`, then append a flywheel row for the commit or rollback."""
        result = apply_graph_patch_cli(project_path, patch, cli_bin=self.cli_bin)
        self.record(
            prompt=prompt,
            patch=patch,
            before_graph_sha256=result.before_sha256,
            after_graph_sha256=result.after_sha256,
            verified_ok=result.ok,
            task_route=task_route,
            reward_score=reward_score,
            patch_id=_patch_id(patch),
        )
        return result

    def record(
        self,
        *,
        prompt: str,
        patch: dict[str, Any],
        before_graph_sha256: str,
        after_graph_sha256: str,
        verified_ok: bool,
        task_route: str | TrackRoute | None = None,
        reward_score: float | None = None,
        patch_id: str | None = None,
    ) -> TrajectoryRecord:
        """Append a GUI or AI outcome that was already applied."""
        route_name = _route_name(prompt, task_route)
        score = (
            default_reward_score(verified_ok, before_graph_sha256, after_graph_sha256)
            if reward_score is None
            else reward_score
        )
        record = TrajectoryRecord(
            timestamp_utc=self._clock(),
            task_route=route_name,
            prompt=prompt,
            patch_id=patch_id if patch_id is not None else _patch_id(patch),
            before_graph_sha256=before_graph_sha256,
            after_graph_sha256=after_graph_sha256,
            patch=patch,
            verified_ok=verified_ok,
            reward_score=score,
        )
        self.logger.append(record)
        return record

    def explore_best_of_n(
        self,
        project_path: Path,
        candidates: list[CandidatePatchSpec],
        *,
        base_branch: str = "main",
        branch_prefix: str = "ai-cand",
        prompt: str = "",
        prompt_id: str | None = None,
        task_route: str | TrackRoute | None = None,
        cli_runner: Callable[..., subprocess.CompletedProcess[str]] | None = None,
        gate_evaluator: Callable[..., Any] | None = None,
        grpo_jsonl_path: Path | None = None,
    ) -> BestOfNExplorationResult:
        """Explore multiple candidate GraphPatches on lightweight branches and log trajectories."""
        explorer = BranchPatchExplorer(
            cli_bin=self.cli_bin,
            cli_runner=cli_runner,
            gate_evaluator=gate_evaluator,
            trajectory_logger=self.logger,
            grpo_jsonl_path=grpo_jsonl_path,
            clock=self._clock,
        )
        return explorer.explore(
            project_path,
            candidates,
            base_branch=base_branch,
            branch_prefix=branch_prefix,
            prompt=prompt,
            prompt_id=prompt_id,
            task_route=task_route,
        )


def route_design_task(prompt: str) -> TrackRoute:
    """Classify a design prompt into one of the four geometry tracks.

    Matching is case-insensitive and does not call a model. TPMS language wins
    over curve and SubD language. Curve language wins over SubD. Anything else,
    including brackets, plates, enclosures, bores, and assemblies, stays on the
    `.kcl` prismatic track.
    """
    if not isinstance(prompt, str):
        raise TypeError("prompt must be a string")
    matched = _first_reason(_TPMS_RULES, prompt)
    if matched is not None:
        return _route(TRACK_TPMS_IMPLICIT, matched)
    matched = _first_reason(_PYTHON_SDK_RULES, prompt)
    if matched is not None:
        return _route(TRACK_PYTHON_SDK_CURVE, matched)
    matched = _first_reason(_SUBD_RULES, prompt)
    if matched is not None:
        return _route(TRACK_SUBD_FREEFORM, matched)
    return _route(TRACK_KCL_PRISMATIC, "default prismatic feature graph")


def apply_graph_patch_cli(
    project_path: Path,
    patch: dict[str, Any],
    cli_bin: Path | None = None,
) -> PatchApplyResult:
    """Write `patch` to a temp file and run `cad-cli project apply-patch --json`.

    `before_sha256` and `after_sha256` capture the deterministic graph/container
    fingerprint for both unpacked directory projects and single-file SQLite `.aicad`
    containers, while `node_count` is queried via `cad-cli project inspect --json`
    (with directory `graph.ir.json` fallback).
    """
    project_path = Path(project_path)
    before_sha256, node_count = _graph_snapshot(project_path, cli_bin=cli_bin)
    if not isinstance(patch, dict):
        return PatchApplyResult(
            ok=False,
            before_sha256=before_sha256,
            after_sha256=before_sha256,
            node_count=node_count,
            error="patch must be a JSON object",
        )
    try:
        patch_text = json.dumps(patch, ensure_ascii=False, allow_nan=False)
    except (TypeError, ValueError) as exc:
        return PatchApplyResult(
            ok=False,
            before_sha256=before_sha256,
            after_sha256=before_sha256,
            node_count=node_count,
            error=f"patch is not JSON-serializable: {exc}",
        )

    resolved = _resolve_cli(cli_bin)
    if resolved is None:
        location = cli_bin if cli_bin is not None else "CAD_CLI_BIN, target/, or PATH"
        return _unchanged(
            project_path,
            before_sha256,
            node_count,
            f"cad-cli was not found ({location})",
            cli_bin=cli_bin,
        )

    patch_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            suffix=".json",
            prefix="graph-patch-",
            delete=False,
        ) as handle:
            handle.write(patch_text)
            patch_path = Path(handle.name)
        completed = subprocess.run(
            [
                str(resolved),
                "project",
                "apply-patch",
                str(project_path),
                "--patch",
                str(patch_path),
                "--json",
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=_CLI_TIMEOUT_SEC,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return _unchanged(
            project_path,
            before_sha256,
            node_count,
            f"cad-cli timed out after {_CLI_TIMEOUT_SEC}s",
            cli_bin=resolved,
        )
    except OSError as exc:
        return _unchanged(
            project_path,
            before_sha256,
            node_count,
            f"failed to launch cad-cli: {exc}",
            cli_bin=resolved,
        )
    finally:
        if patch_path is not None:
            patch_path.unlink(missing_ok=True)

    after_sha256, after_count = _graph_snapshot(project_path, cli_bin=resolved)
    payload = _parse_cli_payload(completed.stdout or "")
    if completed.returncode == 0 and payload is not None and payload.get("status") == "ok":
        return PatchApplyResult(
            ok=True,
            before_sha256=before_sha256,
            after_sha256=after_sha256,
            node_count=after_count,
            error=None,
        )
    return PatchApplyResult(
        ok=False,
        before_sha256=before_sha256,
        after_sha256=after_sha256,
        node_count=after_count,
        error=_cli_error(completed, payload),
    )


def default_reward_score(verified_ok: bool, before_sha256: str, after_sha256: str) -> float:
    """Score a committed graph change as 1 and a rejection or no-op as 0.

    Later GRPO training can replace this with a DFM or FEA reward. The logger
    stores whichever score the caller passes.
    """
    if verified_ok and before_sha256 != after_sha256:
        return 1.0
    return 0.0


def utc_timestamp() -> str:
    """Return the current UTC time as `YYYY-MM-DDTHH:MM:SSZ`."""
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def trajectory_json(record: TrajectoryRecord) -> str:
    """Serialize one record with sorted keys and no extra whitespace."""
    payload = record.to_dict()
    unexpected = set(payload) - set(_TRAJECTORY_FIELDS)
    if unexpected:
        raise ValueError(f"unexpected trajectory fields: {sorted(unexpected)}")
    return json.dumps(
        payload,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        allow_nan=False,
    )


def _route(track: TrackName, reason: str) -> TrackRoute:
    return TrackRoute(track=track, target=TRACK_TARGETS[track], reason=reason)


def _first_reason(rules: tuple[tuple[re.Pattern[str], str], ...], prompt: str) -> str | None:
    for pattern, reason in rules:
        if pattern.search(prompt):
            return reason
    return None


def _route_name(prompt: str, task_route: str | TrackRoute | None) -> str:
    if isinstance(task_route, TrackRoute):
        return task_route.track
    if isinstance(task_route, str):
        return task_route
    if task_route is not None:
        raise TypeError("task_route must be a track name or TrackRoute")
    return route_design_task(prompt).track


def _patch_id(patch: dict[str, Any]) -> str:
    value = patch.get("patch_id", "")
    return value if isinstance(value, str) else str(value)


def _resolve_cli(cli_bin: Path | None) -> Path | None:
    # An explicit path that is missing must not fall through to another build.
    if cli_bin is not None:
        candidate = Path(cli_bin)
        return candidate if candidate.is_file() else None
    return discover_cad_cli()


def _inspect_project_cli(
    project_path: Path,
    cli_bin: Path | None = None,
) -> dict[str, Any] | None:
    """Query `cad-cli project inspect <path> --json` without touching SQLite directly."""
    resolved = _resolve_cli(cli_bin)
    if resolved is None or not project_path.exists():
        return None
    try:
        completed = subprocess.run(
            [str(resolved), "project", "inspect", str(project_path), "--json"],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=_CLI_TIMEOUT_SEC,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    payload = _parse_cli_payload(completed.stdout or "")
    if completed.returncode == 0 and payload is not None and payload.get("status") == "ok":
        return payload
    return None


def _graph_snapshot(
    project_path: Path,
    cli_bin: Path | None = None,
) -> tuple[str, int]:
    """Return `(sha256, node_count)` for an unpacked directory or SQLite `.aicad` container."""
    project_path = Path(project_path)
    if project_path.is_dir():
        graph_path = project_path / _GRAPH_FILE_NAME
        if graph_path.is_file():
            data = graph_path.read_bytes()
            digest = hashlib.sha256(data).hexdigest()
            inspect_payload = _inspect_project_cli(project_path, cli_bin)
            if inspect_payload is not None and isinstance(inspect_payload.get("node_count"), int):
                return digest, int(inspect_payload["node_count"])
            try:
                payload = json.loads(data)
            except json.JSONDecodeError:
                return digest, 0
            nodes = payload.get("nodes") if isinstance(payload, dict) else None
            if not isinstance(nodes, dict):
                return digest, 0
            return digest, len(nodes)

        graphs_dir = project_path / "graphs"
        if graphs_dir.is_dir():
            shard_files = sorted(graphs_dir.glob("*.ir.json"))
            if not shard_files:
                return "", 0
            hasher = hashlib.sha256()
            fallback_count = 0
            for shard in shard_files:
                shard_bytes = shard.read_bytes()
                hasher.update(shard.name.encode("utf-8"))
                hasher.update(b"\0")
                hasher.update(shard_bytes)
                hasher.update(b"\0")
                try:
                    shard_doc = json.loads(shard_bytes)
                    nodes = shard_doc.get("nodes") if isinstance(shard_doc, dict) else None
                    if isinstance(nodes, dict):
                        fallback_count += len(nodes)
                except json.JSONDecodeError:
                    pass
            digest = hasher.hexdigest()
            inspect_payload = _inspect_project_cli(project_path, cli_bin)
            if inspect_payload is not None and isinstance(inspect_payload.get("node_count"), int):
                return digest, int(inspect_payload["node_count"])
            return digest, fallback_count
        return "", 0

    if project_path.is_file():
        try:
            data = project_path.read_bytes()
        except OSError:
            return "", 0
        wal_path = Path(f"{project_path}-wal")
        wal_bytes = wal_path.read_bytes() if wal_path.is_file() else b""
        inspect_payload = _inspect_project_cli(project_path, cli_bin)
        node_count = (
            int(inspect_payload["node_count"])
            if inspect_payload is not None and isinstance(inspect_payload.get("node_count"), int)
            else 0
        )
        hasher = hashlib.sha256(data)
        if wal_bytes:
            hasher.update(b"\0")
            hasher.update(wal_bytes)
        if inspect_payload is not None and not data.startswith(b"SQLite format 3\x00"):
            canonical_summary = json.dumps(
                inspect_payload,
                sort_keys=True,
                separators=(",", ":"),
                ensure_ascii=False,
            ).encode("utf-8")
            hasher.update(b"\0")
            hasher.update(canonical_summary)
        return hasher.hexdigest(), node_count

    return "", 0


def _unchanged(
    project_path: Path,
    before_sha256: str,
    before_count: int,
    error: str,
    *,
    cli_bin: Path | None = None,
) -> PatchApplyResult:
    after_sha256, after_count = _graph_snapshot(project_path, cli_bin=cli_bin)
    return PatchApplyResult(
        ok=False,
        before_sha256=before_sha256,
        after_sha256=after_sha256,
        node_count=after_count if after_sha256 else before_count,
        error=error,
    )


def _parse_cli_payload(stdout: str) -> dict[str, Any] | None:
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
    stderr = (completed.stderr or "").strip()
    stdout = (completed.stdout or "").strip()
    detail = stderr or stdout
    if detail:
        return detail
    return f"cad-cli exited with code {completed.returncode}"


from .branch_explorer import (  # noqa: E402
    BestOfNExplorationResult,
    BranchExplorationError,
    BranchPatchExplorer,
    CandidateEvalOutcome,
    CandidateGateVerdict,
    CandidatePatchSpec,
)

__all__ = [
    "TRACK_KCL_PRISMATIC",
    "TRACK_PYTHON_SDK_CURVE",
    "TRACK_SUBD_FREEFORM",
    "TRACK_TARGETS",
    "TRACK_TPMS_IMPLICIT",
    "BestOfNExplorationResult",
    "BranchExplorationError",
    "BranchPatchExplorer",
    "CandidateEvalOutcome",
    "CandidateGateVerdict",
    "CandidatePatchSpec",
    "GraphPatchSession",
    "PatchApplyResult",
    "TrackRoute",
    "TrajectoryLogger",
    "TrajectoryRecord",
    "apply_graph_patch_cli",
    "default_reward_score",
    "route_design_task",
    "trajectory_json",
    "utc_timestamp",
]

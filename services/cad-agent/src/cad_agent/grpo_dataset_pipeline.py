"""Score verified GraphPatch rollouts and export prompt groups for GRPO."""

from __future__ import annotations

import json
import math
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from statistics import mean, pstdev
from tempfile import NamedTemporaryFile
from typing import Any

_DFM_VIOLATION_MSG_RE = re.compile(r"(\d+)\s+(?:face\s+)?violation", re.IGNORECASE)
_INTERFERENCE_MSG_RE = re.compile(r"(\d+)\s+(?:interference|overlap)", re.IGNORECASE)


@dataclass(frozen=True, slots=True)
class RewardWeights:
    """Nonnegative coefficients for the four reward components."""

    watertight: float = 0.35
    topo_stability: float = 0.25
    dfm_compliance: float = 0.25
    fea_margin: float = 0.15

    def __post_init__(self) -> None:
        values = (self.watertight, self.topo_stability, self.dfm_compliance, self.fea_margin)
        if any(not math.isfinite(value) or value < 0.0 for value in values):
            raise ValueError("reward weights must be finite and nonnegative")


@dataclass(frozen=True, slots=True)
class RolloutCandidateMetrics:
    """GraphPatch completion and measurements from its verification checks."""

    candidate_id: str
    track: str
    patch_json: str
    is_watertight: bool
    failed_topo_ref_count: int
    total_topo_ref_count: int
    dfm_interference_count: int
    dfm_violation_count: int
    fea_safety_factor: float | None = None


@dataclass(frozen=True, slots=True)
class ScoredRolloutCandidate:
    """A completion with raw component scores and its group-relative advantage."""

    candidate_id: str
    track: str
    patch_json: str
    reward_breakdown: dict[str, float]
    total_reward: float
    group_advantage: float


@dataclass(frozen=True, slots=True)
class GrpoPromptGroup:
    """All scored candidates for one prompt, in rollout order."""

    prompt_id: str
    prompt_text: str
    candidates: list[ScoredRolloutCandidate]
    mean_reward: float
    std_reward: float


def compute_zero_trust_reward(
    metrics: RolloutCandidateMetrics, weights: RewardWeights | None = None
) -> tuple[float, dict[str, float]]:
    """Return the weighted reward and the four normalized component scores.

    Topology credit is the fraction of references that resolved, with full
    credit when there were no references to resolve. DFM credit is the inverse
    of one plus the combined interference and violation count. FEA credit is
    zero at a safety factor of 1 or below and full at 2 or above, with linear
    credit between them. Missing and non-finite FEA measurements earn zero.
    """
    counts = (
        metrics.failed_topo_ref_count,
        metrics.total_topo_ref_count,
        metrics.dfm_interference_count,
        metrics.dfm_violation_count,
    )
    if min(counts) < 0:
        raise ValueError("topology and DFM counts must be nonnegative")

    reference_count = max(1, metrics.total_topo_ref_count)
    topo_stability = 1.0 - min(metrics.failed_topo_ref_count, reference_count) / reference_count
    dfm_issue_count = metrics.dfm_interference_count + metrics.dfm_violation_count
    dfm_compliance = 1 / (1 + dfm_issue_count)
    fea_margin = 0.0
    safety_factor = metrics.fea_safety_factor
    if safety_factor is not None and math.isfinite(safety_factor):
        fea_margin = min(1.0, max(0.0, safety_factor - 1.0))

    breakdown = {
        "watertight": float(metrics.is_watertight),
        "topo_stability": topo_stability,
        "dfm_compliance": dfm_compliance,
        "fea_margin": fea_margin,
    }
    if not metrics.is_watertight:
        return 0.0, breakdown

    if weights is None:
        weights = RewardWeights()
    reward = math.fsum(
        (
            weights.watertight,
            weights.topo_stability * topo_stability,
            weights.dfm_compliance * dfm_compliance,
            weights.fea_margin * fea_margin,
        )
    )
    return reward, breakdown


def compute_grpo_group_advantages(
    prompt_id: str,
    prompt_text: str,
    candidates: Sequence[RolloutCandidateMetrics],
    weights: RewardWeights | None = None,
    eps: float = 1e-6,
) -> GrpoPromptGroup:
    """Normalize rewards against the prompt's population mean and deviation."""
    candidates = tuple(candidates)
    if not candidates:
        raise ValueError("a prompt group must contain at least one candidate")
    if not math.isfinite(eps) or eps < 0.0:
        raise ValueError("eps must be finite and nonnegative")
    if weights is None:
        weights = RewardWeights()

    results = [compute_zero_trust_reward(candidate, weights) for candidate in candidates]
    rewards = [reward for reward, _ in results]
    mean_reward = mean(rewards)
    std_reward = pstdev(rewards)
    scale = std_reward + eps

    scored = [
        ScoredRolloutCandidate(
            candidate_id=candidate.candidate_id,
            track=candidate.track,
            patch_json=candidate.patch_json,
            reward_breakdown=breakdown,
            total_reward=reward,
            group_advantage=(reward - mean_reward) / scale if std_reward else 0.0,
        )
        for candidate, (reward, breakdown) in zip(candidates, results, strict=True)
    ]
    return GrpoPromptGroup(prompt_id, prompt_text, scored, mean_reward, std_reward)


def _nonneg_int_from(value: Any) -> int | None:
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value if value >= 0 else None


def _finite_float_from(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    num = float(value)
    return num if math.isfinite(num) else None


def rollout_metrics_from_verification_report(
    *,
    candidate_id: str,
    track: str,
    patch_json: str,
    report: Mapping[str, Any] | Any,
    applied_ok: bool = True,
) -> RolloutCandidateMetrics:
    """Map a V2 L0-L4 tri-state ``VerificationReport`` (or ``cad-cli verify`` JSON) to GRPO metrics.

    Zero-trust invariants:
    - ``is_watertight`` is ``True`` ONLY when ``applied_ok`` is ``True`` and L1 geometry/topology
      is deterministically ``PASS`` (never ``FAIL`` or ``UNKNOWN``).
    - ``fea_safety_factor`` is extracted ONLY from deterministic ``PASS`` / ``FAIL`` FEA checks
      with a numeric ``safety_factor`` measurement (never from ``UNKNOWN`` checks or thresholds).
    """
    raw: Mapping[str, Any]
    if hasattr(report, "to_dict") and callable(report.to_dict):
        raw = report.to_dict()
    elif isinstance(report, Mapping):
        raw = report
    else:
        raw = {}

    if isinstance(raw.get("report"), Mapping):
        raw = raw["report"]
    elif isinstance(raw.get("verification"), Mapping):
        raw = raw["verification"]

    layers = raw.get("layers") if isinstance(raw.get("layers"), Mapping) else {}
    checks = raw.get("checks") if isinstance(raw.get("checks"), Sequence) else ()

    l1_layer = str(layers.get("L1_geometry_topology", "")).upper()
    l3_layer = str(layers.get("L3_manufacturability_physics", "")).upper()

    is_watertight = bool(applied_ok and l1_layer == "PASS")
    total_topo_refs = 0
    failed_topo_refs = 0
    dfm_interferences = 0
    dfm_violations = 0
    fea_safety_factor: float | None = None
    saw_unknown_fea = l3_layer == "UNKNOWN"

    for item in checks:
        if not isinstance(item, Mapping):
            continue
        code = str(item.get("code", "")).upper()
        layer = str(item.get("layer", ""))
        status = str(item.get("status", "")).upper()
        message = str(item.get("message", ""))
        evidence = item.get("evidence") if isinstance(item.get("evidence"), Mapping) else {}

        if layer == "L1_geometry_topology" or code.startswith("L1_"):
            if status != "PASS":
                is_watertight = False
            if evidence.get("watertight") is False:
                is_watertight = False
            open_edges = _nonneg_int_from(evidence.get("open_edges"))
            if open_edges is None:
                open_edges = _nonneg_int_from(evidence.get("open_edge_count"))
            if open_edges is not None and open_edges > 0:
                is_watertight = False

        if (
            layer == "L2_interfaces_constraints"
            or code.startswith("L2_")
            or "TOPO" in code
        ):
            ev_total = _nonneg_int_from(evidence.get("total_topo_ref_count"))
            ev_failed = _nonneg_int_from(evidence.get("failed_topo_ref_count"))
            if ev_total is not None:
                total_topo_refs += ev_total
                failed_topo_refs += ev_failed if ev_failed is not None else (0 if status == "PASS" else ev_total)
            elif any(tok in code for tok in ("TOPO", "SKETCH", "MATE", "INTERFACE")):
                total_topo_refs += 1
                if status != "PASS":
                    failed_topo_refs += 1

        if "INTERFERENCE" in code or code == "L2_CLEAR":
            ev_inter = _nonneg_int_from(evidence.get("interference_count"))
            if ev_inter is None:
                ev_inter = _nonneg_int_from(evidence.get("interference_pair_count"))
            if ev_inter is not None:
                dfm_interferences += ev_inter
            elif status == "FAIL":
                m = _INTERFERENCE_MSG_RE.search(message)
                dfm_interferences += int(m.group(1)) if m else 1

        if code == "L3_DFM" or ("DFM" in code and layer == "L3_manufacturability_physics"):
            ev_viol = _nonneg_int_from(evidence.get("violation_count"))
            if ev_viol is None:
                ev_viol = _nonneg_int_from(evidence.get("dfm_violation_count"))
            if ev_viol is not None:
                dfm_violations += ev_viol
            elif status == "FAIL":
                m = _DFM_VIOLATION_MSG_RE.search(message)
                dfm_violations += int(m.group(1)) if m else 1

        if code in {"L3_FEA", "L3_LOAD_SAFETY_FACTOR", "L3_LOADS_NO_FEA_SOLVER"} or "FEA" in code:
            if status == "UNKNOWN" or code == "L3_LOADS_NO_FEA_SOLVER":
                saw_unknown_fea = True
            elif status in {"PASS", "FAIL"}:
                sf = _finite_float_from(evidence.get("safety_factor"))
                if sf is None:
                    sf = _finite_float_from(evidence.get("fea_safety_factor"))
                if sf is not None:
                    fea_safety_factor = sf if fea_safety_factor is None else min(fea_safety_factor, sf)

    if saw_unknown_fea:
        fea_safety_factor = None

    return RolloutCandidateMetrics(
        candidate_id=candidate_id,
        track=track,
        patch_json=patch_json,
        is_watertight=is_watertight,
        failed_topo_ref_count=failed_topo_refs,
        total_topo_ref_count=total_topo_refs,
        dfm_interference_count=dfm_interferences,
        dfm_violation_count=dfm_violations,
        fea_safety_factor=fea_safety_factor,
    )


def export_grpo_training_jsonl(
    groups: Sequence[GrpoPromptGroup],
    out_path: Path,
    *,
    append: bool = False,
) -> int:
    """Atomically write (or append) one UTF-8 record per prompt, preserving rollout order."""
    out_path = Path(out_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    # Pre-validate & serialize all new lines before touching disk.
    serialized_lines: list[str] = []
    for group in groups:
        candidates = tuple(group.candidates)
        record = {
            "prompt_id": group.prompt_id,
            "prompt": group.prompt_text,
            "completions": [candidate.patch_json for candidate in candidates],
            "rewards": [candidate.total_reward for candidate in candidates],
            "advantages": [candidate.group_advantage for candidate in candidates],
            "reward_breakdowns": [candidate.reward_breakdown for candidate in candidates],
        }
        line = json.dumps(
            record, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
        )
        serialized_lines.append(line + "\n")

    existing_prefix = ""
    if append and out_path.is_file():
        existing_prefix = out_path.read_text(encoding="utf-8")
        if existing_prefix and not existing_prefix.endswith("\n"):
            existing_prefix += "\n"

    handle = NamedTemporaryFile(
        mode="w", encoding="utf-8", newline="\n", dir=out_path.parent, delete=False
    )
    temporary_path = Path(handle.name)
    try:
        with handle:
            if existing_prefix:
                handle.write(existing_prefix)
            for line in serialized_lines:
                handle.write(line)
        temporary_path.replace(out_path)
    finally:
        temporary_path.unlink(missing_ok=True)
    return len(serialized_lines)


def load_grpo_training_jsonl(in_path: Path) -> list[dict[str, Any]]:
    """Read JSONL objects in file order, skipping blank lines."""
    records = []
    with Path(in_path).open(encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            record = json.loads(line)
            if not isinstance(record, dict):
                raise ValueError(f"line {line_number} is not a JSON object")
            records.append(record)
    return records


__all__ = [
    "GrpoPromptGroup",
    "RewardWeights",
    "RolloutCandidateMetrics",
    "ScoredRolloutCandidate",
    "compute_grpo_group_advantages",
    "compute_zero_trust_reward",
    "export_grpo_training_jsonl",
    "load_grpo_training_jsonl",
    "rollout_metrics_from_verification_report",
]

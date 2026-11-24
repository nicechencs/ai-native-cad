"""AI Best-of-N candidate patch exploration on lightweight container branches.

Architecture boundary: this module NEVER touches relational database drivers or raw SQL.
All branch creation, checkout, atomic ``GraphPatch`` application, and DFM
verification flow strictly through ``cad-cli project`` contracts.
"""

from __future__ import annotations

import copy
import json
import math
import subprocess
import tempfile
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from statistics import mean, pstdev
from typing import TYPE_CHECKING, Any

from .compiler_bridge import discover_cad_cli
from .grpo_dataset_pipeline import (
    GrpoPromptGroup,
    RewardWeights,
    RolloutCandidateMetrics,
    compute_grpo_group_advantages,
    export_grpo_training_jsonl,
    rollout_metrics_from_verification_report,
)
from .llm import LLMConfigError

if TYPE_CHECKING:
    from .contracts import DesignContract
    from .graph_patch_agent import TrackRoute, TrajectoryLogger
    from .semantic_agent import EditContext, SemanticPlanner

_CLI_TIMEOUT_SEC = 120.0


class BranchExplorationError(RuntimeError):
    """Raised when ``BranchPatchExplorer`` cannot execute CLI branch operations."""


@dataclass(frozen=True, slots=True)
class CandidatePatchSpec:
    """One candidate ``GraphPatch`` proposed during Best-of-N rollout sampling."""

    candidate_id: str
    description: str
    patch: dict[str, Any]

    def __post_init__(self) -> None:
        cid = _validate_branch_segment(self.candidate_id, "candidate_id")
        if not isinstance(self.description, str):
            raise TypeError("description must be a string")
        if not isinstance(self.patch, dict):
            raise TypeError("patch must be a dict")
        object.__setattr__(self, "candidate_id", cid)
        object.__setattr__(self, "patch", copy.deepcopy(self.patch))

    def to_dict(self) -> dict[str, Any]:
        return {
            "candidate_id": self.candidate_id,
            "description": self.description,
            "patch": copy.deepcopy(self.patch),
        }


@dataclass(frozen=True, slots=True)
class CandidateGateVerdict:
    """Verification outcome returned by a pluggable gate/reward callback."""

    gate_passed: bool
    score: float
    diagnostics: list[str]
    kept_node_cache_hits: int = 0

    def __post_init__(self) -> None:
        if not isinstance(self.gate_passed, bool):
            raise TypeError("gate_passed must be a bool")
        score = _require_finite_float(self.score, "score")
        hits = _require_nonneg_int(self.kept_node_cache_hits, "kept_node_cache_hits")
        diags = _normalize_diagnostics(self.diagnostics)
        object.__setattr__(self, "score", score)
        object.__setattr__(self, "diagnostics", diags)
        object.__setattr__(self, "kept_node_cache_hits", hits)


@dataclass(frozen=True, slots=True)
class CandidateEvalOutcome:
    """Outcome of evaluating a single candidate branch in Best-of-N exploration."""

    candidate_id: str
    branch_name: str
    applied_ok: bool
    gate_passed: bool
    score: float
    diagnostics: list[str]
    kept_node_cache_hits: int = 0

    def __post_init__(self) -> None:
        if not isinstance(self.candidate_id, str) or not self.candidate_id.strip():
            raise ValueError("candidate_id must be a non-empty string")
        if not isinstance(self.branch_name, str) or not self.branch_name.strip():
            raise ValueError("branch_name must be a non-empty string")
        if not isinstance(self.applied_ok, bool):
            raise TypeError("applied_ok must be a bool")
        if not isinstance(self.gate_passed, bool):
            raise TypeError("gate_passed must be a bool")
        score = _require_finite_float(self.score, "score")
        hits = _require_nonneg_int(self.kept_node_cache_hits, "kept_node_cache_hits")
        diags = _normalize_diagnostics(self.diagnostics)
        object.__setattr__(self, "score", score)
        object.__setattr__(self, "diagnostics", diags)
        object.__setattr__(self, "kept_node_cache_hits", hits)

    def to_dict(self) -> dict[str, Any]:
        return {
            "candidate_id": self.candidate_id,
            "branch_name": self.branch_name,
            "applied_ok": self.applied_ok,
            "gate_passed": self.gate_passed,
            "score": self.score,
            "diagnostics": list(self.diagnostics),
            "kept_node_cache_hits": self.kept_node_cache_hits,
        }


@dataclass(frozen=True, slots=True)
class BestOfNExplorationResult:
    """Summary of a Best-of-N multi-branch candidate patch exploration run."""

    base_branch: str
    active_branch: str
    winner_candidate_id: str | None
    winner_branch: str | None
    winner_score: float | None
    rolled_back_to_base: bool
    outcomes: list[CandidateEvalOutcome]
    grpo_record: dict[str, Any] | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.base_branch, str) or not self.base_branch.strip():
            raise ValueError("base_branch must be a non-empty string")
        if not isinstance(self.active_branch, str) or not self.active_branch.strip():
            raise ValueError("active_branch must be a non-empty string")
        if not isinstance(self.rolled_back_to_base, bool):
            raise TypeError("rolled_back_to_base must be a bool")
        if self.winner_score is not None:
            object.__setattr__(
                self,
                "winner_score",
                _require_finite_float(self.winner_score, "winner_score"),
            )
        object.__setattr__(self, "outcomes", list(self.outcomes))
        if self.grpo_record is not None:
            object.__setattr__(self, "grpo_record", copy.deepcopy(self.grpo_record))

    def to_dict(self) -> dict[str, Any]:
        return {
            "base_branch": self.base_branch,
            "active_branch": self.active_branch,
            "winner_candidate_id": self.winner_candidate_id,
            "winner_branch": self.winner_branch,
            "winner_score": self.winner_score,
            "rolled_back_to_base": self.rolled_back_to_base,
            "outcomes": [item.to_dict() for item in self.outcomes],
            "grpo_record": copy.deepcopy(self.grpo_record),
        }


@dataclass(frozen=True, slots=True)
class SemanticRolloutConfig:
    """Configuration for V2 semantic Best-of-N candidate rollout over SQLite branches."""

    num_candidates: int = 3
    base_branch: str = "main"
    branch_prefix: str = "ai/cand"
    track: str = "parametric_brep"
    require_full_pass: bool = False
    reward_weights: RewardWeights | None = None
    grpo_out_path: Path | None = None
    append_grpo: bool = True
    prompt_id: str | None = None

    def __post_init__(self) -> None:
        if isinstance(self.num_candidates, bool) or not isinstance(self.num_candidates, int) or self.num_candidates < 1:
            raise ValueError("num_candidates must be a positive integer")
        base = _validate_branch_name(self.base_branch, "base_branch")
        prefix = _validate_branch_name(self.branch_prefix, "branch_prefix")
        object.__setattr__(self, "base_branch", base)
        object.__setattr__(self, "branch_prefix", prefix)
        if self.grpo_out_path is not None:
            object.__setattr__(self, "grpo_out_path", Path(self.grpo_out_path))


@dataclass(frozen=True, slots=True)
class SemanticCandidateOutcome:
    """Verification & GRPO scoring outcome for one V2 semantic candidate branch."""

    candidate_id: str
    branch_name: str
    commands: list[dict[str, Any]]
    commands_json: str
    applied_ok: bool
    overall_status: str
    gate_passed: bool
    reward: float
    group_advantage: float
    reward_breakdown: dict[str, float]
    diagnostics: list[str]
    verification: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "candidate_id": self.candidate_id,
            "branch_name": self.branch_name,
            "commands": copy.deepcopy(self.commands),
            "commands_json": self.commands_json,
            "applied_ok": self.applied_ok,
            "overall_status": self.overall_status,
            "gate_passed": self.gate_passed,
            "reward": self.reward,
            "group_advantage": self.group_advantage,
            "reward_breakdown": dict(self.reward_breakdown),
            "diagnostics": list(self.diagnostics),
            "verification": copy.deepcopy(self.verification),
        }


@dataclass(frozen=True, slots=True)
class SemanticBestOfNResult:
    """Summary of a V2 semantic Best-of-N rollout with GRPO group advantages."""

    base_branch: str
    active_branch: str
    winner_candidate_id: str | None
    winner_branch: str | None
    winner_score: float | None
    rolled_back_to_base: bool
    outcomes: list[SemanticCandidateOutcome]
    grpo_group: GrpoPromptGroup
    grpo_out_path: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "base_branch": self.base_branch,
            "active_branch": self.active_branch,
            "winner_candidate_id": self.winner_candidate_id,
            "winner_branch": self.winner_branch,
            "winner_score": self.winner_score,
            "rolled_back_to_base": self.rolled_back_to_base,
            "outcomes": [item.to_dict() for item in self.outcomes],
            "grpo_group": {
                "prompt_id": self.grpo_group.prompt_id,
                "prompt_text": self.grpo_group.prompt_text,
                "mean_reward": self.grpo_group.mean_reward,
                "std_reward": self.grpo_group.std_reward,
                "candidates": [
                    {
                        "candidate_id": c.candidate_id,
                        "track": c.track,
                        "patch_json": c.patch_json,
                        "reward_breakdown": dict(c.reward_breakdown),
                        "total_reward": c.total_reward,
                        "group_advantage": c.group_advantage,
                    }
                    for c in self.grpo_group.candidates
                ],
            },
            "grpo_out_path": self.grpo_out_path,
        }


CliRunner = Callable[..., subprocess.CompletedProcess[str]]
GateEvaluator = Callable[
    [Path, str, CandidatePatchSpec],
    CandidateGateVerdict | Mapping[str, Any] | tuple[Any, ...],
]


class BranchPatchExplorer:
    """Explore ``N`` candidate ``GraphPatch`` specs across lightweight container branches.

    Workflow per candidate ``i``:
    1. ``cad-cli project branch <path> create <branch_name> --from <base_branch> --purpose ai_candidate --json``
    2. ``cad-cli project branch <path> checkout <branch_name> --json`` +
       ``cad-cli project apply-patch <path> --patch <tmp.json> --branch <branch_name> --json``
    3. Evaluate candidate via ``gate_evaluator`` or ``cad-cli project dfm-check``
    4. Activate the highest-scoring gate-passing branch, or losslessly checkout
       ``base_branch`` when every candidate fails so the trunk stays 100% clean.
    """

    def __init__(
        self,
        *,
        cli_bin: str | Path | None = None,
        cli_runner: CliRunner | None = None,
        gate_evaluator: GateEvaluator | None = None,
        min_wall_mm: float = 1.5,
        min_draft_deg: float = 1.0,
        pull_dir: Sequence[float] = (0.0, 0.0, 1.0),
        trajectory_logger: TrajectoryLogger | None = None,
        grpo_jsonl_path: str | Path | None = None,
        clock: Callable[[], str] | None = None,
        timeout_sec: float = _CLI_TIMEOUT_SEC,
    ) -> None:
        self._cli_bin = Path(cli_bin) if cli_bin is not None else None
        self._cli_runner = cli_runner
        self._gate_evaluator = gate_evaluator
        self._min_wall_mm = _require_finite_float(min_wall_mm, "min_wall_mm")
        if self._min_wall_mm < 0.0:
            raise ValueError("min_wall_mm must be nonnegative")
        self._min_draft_deg = _require_finite_float(min_draft_deg, "min_draft_deg")
        if isinstance(pull_dir, (str, bytes)) or len(pull_dir) != 3:
            raise ValueError("pull_dir must contain three coordinates")
        self._pull_dir = tuple(_require_finite_float(v, "pull_dir") for v in pull_dir)
        self._trajectory_logger = trajectory_logger
        self._grpo_jsonl_path = Path(grpo_jsonl_path) if grpo_jsonl_path is not None else None
        self._clock = clock or _utc_timestamp
        self._timeout_sec = _require_finite_float(timeout_sec, "timeout_sec")
        if self._timeout_sec <= 0.0:
            raise ValueError("timeout_sec must be positive")

    def explore(
        self,
        project_path: str | Path,
        candidates: Sequence[CandidatePatchSpec],
        *,
        base_branch: str = "main",
        branch_prefix: str = "ai-cand",
        prompt: str = "",
        prompt_id: str | None = None,
        task_route: str | TrackRoute | None = None,
    ) -> BestOfNExplorationResult:
        """Run Best-of-N candidate evaluation and checkout the winning branch or ``base_branch``."""
        target = Path(project_path)
        base = _validate_branch_name(base_branch, "base_branch")
        prefix = _validate_branch_name(branch_prefix, "branch_prefix") if branch_prefix else ""

        candidate_list = list(candidates)
        if not candidate_list:
            raise ValueError("candidates must contain at least one CandidatePatchSpec")
        seen_ids: set[str] = set()
        for spec in candidate_list:
            if not isinstance(spec, CandidatePatchSpec):
                raise TypeError("every item in candidates must be a CandidatePatchSpec")
            if spec.candidate_id in seen_ids:
                raise ValueError(f"duplicate candidate_id {spec.candidate_id!r}")
            seen_ids.add(spec.candidate_id)

        cli_exec = self._resolve_cli_executable()
        outcomes: list[CandidateEvalOutcome] = []

        for spec in candidate_list:
            branch_name = f"{prefix}/{spec.candidate_id}" if prefix else spec.candidate_id
            outcome = self._evaluate_one_candidate(
                cli_exec=cli_exec,
                project_path=target,
                base_branch=base,
                branch_name=branch_name,
                spec=spec,
            )
            outcomes.append(outcome)

        passing = [item for item in outcomes if item.applied_ok and item.gate_passed]
        if passing:
            winner = max(passing, key=lambda item: item.score)
            checkout_ok, _, checkout_err = self._run_cli_json(
                cli_exec,
                ["project", "branch", str(target), "checkout", winner.branch_name, "--json"],
            )
            if not checkout_ok:
                raise BranchExplorationError(
                    f"failed to checkout winning branch {winner.branch_name!r}: {checkout_err}"
                )
            active_branch = winner.branch_name
            winner_id: str | None = winner.candidate_id
            winner_branch: str | None = winner.branch_name
            winner_score: float | None = winner.score
            rolled_back = False
        else:
            rollback_ok, _, rollback_err = self._run_cli_json(
                cli_exec,
                ["project", "branch", str(target), "checkout", base, "--json"],
            )
            if not rollback_ok:
                raise BranchExplorationError(
                    f"failed to restore base branch {base!r} after candidate failures: {rollback_err}"
                )
            active_branch = base
            winner_id = None
            winner_branch = None
            winner_score = None
            rolled_back = True

        route_str = _resolve_route(prompt, task_route)
        grpo_row = _build_grpo_record(
            prompt_id=prompt_id or f"best-of-n-{self._clock()}",
            prompt=prompt,
            task_route=route_str,
            base_branch=base,
            winner_candidate_id=winner_id,
            specs=candidate_list,
            outcomes=outcomes,
        )
        if self._grpo_jsonl_path is not None:
            _append_jsonl_row(self._grpo_jsonl_path, grpo_row)

        if self._trajectory_logger is not None:
            from .graph_patch_agent import TrajectoryRecord

            base_tag = _digest_label(f"{target}:{base}")
            for spec, outcome in zip(candidate_list, outcomes, strict=True):
                safe_patch = _sanitize_for_json(spec.patch)
                after_tag = (
                    _digest_label(f"{target}:{outcome.branch_name}:{canonical_patch_json(safe_patch)}")
                    if outcome.applied_ok
                    else base_tag
                )
                patch_id = str(spec.patch.get("patch_id") or spec.candidate_id)
                self._trajectory_logger.append(
                    TrajectoryRecord(
                        timestamp_utc=self._clock(),
                        task_route=route_str,
                        prompt=prompt,
                        patch_id=patch_id,
                        before_graph_sha256=base_tag,
                        after_graph_sha256=after_tag,
                        patch=safe_patch,
                        verified_ok=outcome.applied_ok and outcome.gate_passed,
                        reward_score=outcome.score,
                    )
                )

        return BestOfNExplorationResult(
            base_branch=base,
            active_branch=active_branch,
            winner_candidate_id=winner_id,
            winner_branch=winner_branch,
            winner_score=winner_score,
            rolled_back_to_base=rolled_back,
            outcomes=outcomes,
            grpo_record=grpo_row,
        )

    def _evaluate_one_candidate(
        self,
        *,
        cli_exec: str,
        project_path: Path,
        base_branch: str,
        branch_name: str,
        spec: CandidatePatchSpec,
    ) -> CandidateEvalOutcome:
        patch_error = _validate_patch_object(spec.patch)
        if patch_error is not None:
            return CandidateEvalOutcome(
                candidate_id=spec.candidate_id,
                branch_name=branch_name,
                applied_ok=False,
                gate_passed=False,
                score=0.0,
                diagnostics=[patch_error],
                kept_node_cache_hits=0,
            )

        create_ok, _, create_err = self._run_cli_json(
            cli_exec,
            [
                "project",
                "branch",
                str(project_path),
                "create",
                branch_name,
                "--from",
                base_branch,
                "--purpose",
                "ai_candidate",
                "--json",
            ],
        )
        if not create_ok:
            return CandidateEvalOutcome(
                candidate_id=spec.candidate_id,
                branch_name=branch_name,
                applied_ok=False,
                gate_passed=False,
                score=0.0,
                diagnostics=[f"branch create failed: {create_err}"],
                kept_node_cache_hits=0,
            )

        checkout_ok, _, checkout_err = self._run_cli_json(
            cli_exec,
            ["project", "branch", str(project_path), "checkout", branch_name, "--json"],
        )
        if not checkout_ok:
            return CandidateEvalOutcome(
                candidate_id=spec.candidate_id,
                branch_name=branch_name,
                applied_ok=False,
                gate_passed=False,
                score=0.0,
                diagnostics=[f"branch checkout failed: {checkout_err}"],
                kept_node_cache_hits=0,
            )

        patch_text = canonical_patch_json(spec.patch)
        temp_patch_path: Path | None = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w",
                encoding="utf-8",
                prefix=f"cand-{spec.candidate_id}-",
                suffix=".json",
                delete=False,
            ) as handle:
                handle.write(patch_text)
                temp_patch_path = Path(handle.name)

            apply_ok, apply_payload, apply_err = self._run_cli_json(
                cli_exec,
                [
                    "project",
                    "apply-patch",
                    str(project_path),
                    "--patch",
                    str(temp_patch_path),
                    "--branch",
                    branch_name,
                    "--json",
                ],
            )
        finally:
            if temp_patch_path is not None:
                temp_patch_path.unlink(missing_ok=True)

        if not apply_ok:
            return CandidateEvalOutcome(
                candidate_id=spec.candidate_id,
                branch_name=branch_name,
                applied_ok=False,
                gate_passed=False,
                score=0.0,
                diagnostics=[f"apply-patch rejected: {apply_err}"],
                kept_node_cache_hits=0,
            )

        apply_cache_hits = _extract_cache_hits(apply_payload)

        if self._gate_evaluator is not None:
            verdict = _coerce_gate_verdict(
                self._gate_evaluator(project_path, branch_name, spec),
                default_cache_hits=apply_cache_hits,
            )
        else:
            verdict = self._run_default_dfm_gate(
                cli_exec=cli_exec,
                project_path=project_path,
                default_cache_hits=apply_cache_hits,
            )

        return CandidateEvalOutcome(
            candidate_id=spec.candidate_id,
            branch_name=branch_name,
            applied_ok=True,
            gate_passed=verdict.gate_passed,
            score=verdict.score,
            diagnostics=verdict.diagnostics,
            kept_node_cache_hits=verdict.kept_node_cache_hits,
        )

    def _run_default_dfm_gate(
        self,
        *,
        cli_exec: str,
        project_path: Path,
        default_cache_hits: int,
    ) -> CandidateGateVerdict:
        pull_str = ",".join(format(v, ".16g") for v in self._pull_dir)
        ok, payload, err = self._run_cli_json(
            cli_exec,
            [
                "project",
                "dfm-check",
                str(project_path),
                "--min-wall-mm",
                format(self._min_wall_mm, ".16g"),
                "--min-draft-deg",
                format(self._min_draft_deg, ".16g"),
                "--pull-dir",
                pull_str,
                "--json",
            ],
        )
        if not ok or payload is None:
            return CandidateGateVerdict(
                gate_passed=False,
                score=0.0,
                diagnostics=[f"dfm-check failed: {err}"],
                kept_node_cache_hits=default_cache_hits,
            )

        passed = bool(payload.get("passed", False))
        interference = payload.get("interference")
        dfm = payload.get("dfm")
        pair_count = (
            int(interference.get("pair_count", 0))
            if isinstance(interference, Mapping)
            and isinstance(interference.get("pair_count"), int)
            and not isinstance(interference.get("pair_count"), bool)
            else 0
        )
        violation_count = (
            int(dfm.get("violation_count", 0))
            if isinstance(dfm, Mapping)
            and isinstance(dfm.get("violation_count"), int)
            and not isinstance(dfm.get("violation_count"), bool)
            else 0
        )
        issue_count = max(0, pair_count) + max(0, violation_count)
        cache_hits = max(default_cache_hits, _extract_cache_hits(payload))

        explicit_score = payload.get("reward_score")
        if isinstance(explicit_score, (int, float)) and not isinstance(explicit_score, bool):
            score = float(explicit_score) if math.isfinite(float(explicit_score)) else 0.0
        elif passed:
            score = 1.0 / (1.0 + issue_count)
        else:
            score = 0.25 / (1.0 + issue_count)

        diagnostics: list[str] = []
        if passed:
            diagnostics.append("DFM gate passed")
        else:
            diagnostics.append(
                f"DFM gate failed: {pair_count} interference pair(s), {violation_count} violation(s)"
            )
        return CandidateGateVerdict(
            gate_passed=passed,
            score=score,
            diagnostics=diagnostics,
            kept_node_cache_hits=cache_hits,
        )

    def explore_semantic_candidates(
        self,
        project_path: str | Path,
        planner: SemanticPlanner,
        *,
        workspace_root: str | Path | None = None,
        contract: DesignContract | None = None,
        intent: str = "",
        target: str | None = None,
        instruction: str = "",
        edit_context: EditContext | None = None,
        config: SemanticRolloutConfig | None = None,
    ) -> SemanticBestOfNResult:
        """Run V2 Best-of-N semantic candidate rollouts over SQLite preview branches + L0-L4 verification.

        Creates isolated branches ``<branch_prefix>-1..N`` (default ``ai/cand-1..N``), applies each
        candidate ``SemanticCommand`` batch atomically via ``cad-cli api apply``, runs zero-trust
        L0-L4 verification via ``cad-cli verify``, computes GRPO group-relative advantages via
        ``grpo_dataset_pipeline``, selects the highest-scoring passing branch (or rolls back cleanly
        to ``base_branch``), and optionally exports/appends a GRPO JSONL record.
        """
        cfg = config or SemanticRolloutConfig()
        editing = target is not None and bool(str(target).strip())
        if editing:
            if not instruction or not instruction.strip():
                raise ValueError("instruction must be non-empty when target is provided")
            prompt_text = instruction.strip()
        else:
            if contract is None:
                raise ValueError("contract is required for 0->1 semantic creation rollouts")
            prompt_text = (intent or contract.summary or contract.part_name).strip()

        proj = Path(project_path)
        ws_root = Path(workspace_root) if workspace_root is not None else proj.parent
        cli_exec = self._resolve_cli_executable()

        from .semantic_agent import (
            SemanticCommandError,
            _assert_local_batch,
            enforce_honest_verdict,
            validate_semantic_batch,
        )

        try:
            from .llm_semantic_planner import LLMPlanError
        except Exception:  # pragma: no cover - defensive import guard
            LLMPlanError = ValueError  # type: ignore[misc, assignment]

        resolved_edit_context = edit_context
        if editing and resolved_edit_context is None:
            resolved_edit_context = self._fetch_edit_context(
                cli_exec=cli_exec,
                project_path=proj,
                workspace_root=ws_root,
                target=str(target).strip(),
            )

        prior_diagnostics: list[dict[str, Any]] = []
        raw_candidates: list[dict[str, Any]] = []
        metrics_list: list[RolloutCandidateMetrics] = []

        for idx in range(1, cfg.num_candidates + 1):
            candidate_id = f"cand-{idx}"
            branch_name = f"{cfg.branch_prefix}-{idx}"
            commands: list[dict[str, Any]] = []
            commands_json = "[]"
            applied_ok = False
            overall_status = "FAIL"
            gate_passed = False
            verification_payload: dict[str, Any] | None = None
            diagnostics: list[str] = []

            try:
                if editing:
                    assert resolved_edit_context is not None
                    proposed = planner.plan_local_edit(
                        target=str(target).strip(),
                        instruction=instruction.strip(),
                        context=resolved_edit_context,
                        diagnostics=list(prior_diagnostics),
                        attempt=idx - 1,
                    )
                    commands = validate_semantic_batch(proposed)
                    _assert_local_batch(commands, resolved_edit_context)
                else:
                    assert contract is not None
                    proposed = planner.plan_creation(
                        contract,
                        intent,
                        diagnostics=list(prior_diagnostics),
                        attempt=idx - 1,
                    )
                    commands = validate_semantic_batch(proposed)
                commands_json = canonical_commands_json(commands)
            except LLMConfigError:
                raise
            except (SemanticCommandError, LLMPlanError, ValueError, TypeError) as exc:
                msg = f"planner rejected candidate {candidate_id}: {exc}"
                diagnostics.append(msg)
                prior_diagnostics.append({"code": "E_PLANNER", "message": msg})
                metrics = rollout_metrics_from_verification_report(
                    candidate_id=candidate_id,
                    track=cfg.track,
                    patch_json=commands_json,
                    report={},
                    applied_ok=False,
                )
                metrics_list.append(metrics)
                raw_candidates.append(
                    {
                        "candidate_id": candidate_id,
                        "branch_name": branch_name,
                        "commands": commands,
                        "commands_json": commands_json,
                        "applied_ok": False,
                        "overall_status": "FAIL",
                        "gate_passed": False,
                        "diagnostics": diagnostics,
                        "verification": None,
                    }
                )
                continue

            # Create isolated SQLite candidate branch from base_branch.
            create_ok, _, create_err = self._run_cli_json(
                cli_exec,
                [
                    "project",
                    "branch",
                    str(proj),
                    "create",
                    branch_name,
                    "--from",
                    cfg.base_branch,
                    "--purpose",
                    "ai_candidate",
                    "--json",
                ],
            )
            if not create_ok:
                msg = f"branch create failed for {branch_name}: {create_err}"
                diagnostics.append(msg)
                prior_diagnostics.append({"code": "E_BRANCH_CREATE", "message": msg})
                metrics_list.append(
                    rollout_metrics_from_verification_report(
                        candidate_id=candidate_id,
                        track=cfg.track,
                        patch_json=commands_json,
                        report={},
                        applied_ok=False,
                    )
                )
                raw_candidates.append(
                    {
                        "candidate_id": candidate_id,
                        "branch_name": branch_name,
                        "commands": commands,
                        "commands_json": commands_json,
                        "applied_ok": False,
                        "overall_status": "FAIL",
                        "gate_passed": False,
                        "diagnostics": diagnostics,
                        "verification": None,
                    }
                )
                continue

            # Apply semantic commands atomically to candidate branch via @tmpfile spill.
            tmp_cmd_file: Path | None = None
            try:
                with tempfile.NamedTemporaryFile(
                    mode="w",
                    encoding="utf-8",
                    suffix=".json",
                    prefix=f"aicad_cand_{idx}_",
                    delete=False,
                ) as fh:
                    fh.write(commands_json)
                    tmp_cmd_file = Path(fh.name)
                apply_ok, apply_payload, apply_err = self._run_cli_json(
                    cli_exec,
                    [
                        "api",
                        "apply",
                        "--project",
                        str(proj),
                        "--workspace",
                        str(ws_root),
                        "--branch",
                        branch_name,
                        "--commands",
                        f"@{tmp_cmd_file}",
                        "--json",
                    ],
                )
            finally:
                if tmp_cmd_file is not None:
                    tmp_cmd_file.unlink(missing_ok=True)

            if not apply_ok:
                msg = f"api apply failed on {branch_name}: {apply_err}"
                diagnostics.append(msg)
                prior_diagnostics.append(
                    {
                        "code": (apply_payload or {}).get("code", "E_APPLY_FAILED"),
                        "message": msg,
                    }
                )
                metrics_list.append(
                    rollout_metrics_from_verification_report(
                        candidate_id=candidate_id,
                        track=cfg.track,
                        patch_json=commands_json,
                        report={},
                        applied_ok=False,
                    )
                )
                raw_candidates.append(
                    {
                        "candidate_id": candidate_id,
                        "branch_name": branch_name,
                        "commands": commands,
                        "commands_json": commands_json,
                        "applied_ok": False,
                        "overall_status": "FAIL",
                        "gate_passed": False,
                        "diagnostics": diagnostics,
                        "verification": None,
                    }
                )
                continue

            applied_ok = True
            # Checkout candidate branch for L0-L4 verify, always restoring base_branch in finally.
            pending_exc: BaseException | None = None
            try:
                co_ok, _, co_err = self._run_cli_json(
                    cli_exec,
                    ["project", "branch", str(proj), "checkout", branch_name, "--json"],
                )
                if not co_ok:
                    diagnostics.append(f"checkout failed for {branch_name}: {co_err}")
                else:
                    ver_ok, ver_payload, ver_err = self._run_cli_json(
                        cli_exec,
                        [
                            "verify",
                            "--project",
                            str(proj),
                            "--workspace",
                            str(ws_root),
                            "--json",
                        ],
                    )
                    if ver_payload is not None:
                        from .contracts import VerificationReport

                        report_dict = (
                            ver_payload.get("report")
                            if isinstance(ver_payload.get("report"), dict)
                            else (
                                ver_payload.get("verification")
                                if isinstance(ver_payload.get("verification"), dict)
                                else ver_payload
                            )
                        )
                        try:
                            report_obj = VerificationReport.from_dict(copy.deepcopy(report_dict))
                            verification_payload = enforce_honest_verdict(
                                report_obj,
                                loads_declared=bool(contract is not None and contract.loads),
                            ).to_dict()
                        except Exception:
                            verification_payload = copy.deepcopy(report_dict)
                        overall_status = str(verification_payload.get("overall_status", "FAIL")).upper()
                        layers = verification_payload.get("layers") or {}
                        l0 = str(layers.get("L0_structure_units", "FAIL")).upper()
                        l1 = str(layers.get("L1_geometry_topology", "FAIL")).upper()
                        l2 = str(layers.get("L2_interfaces_constraints", "FAIL")).upper()
                        l3 = str(layers.get("L3_manufacturability_physics", "UNKNOWN")).upper()
                        l4 = str(layers.get("L4_interop_roundtrip", "UNKNOWN")).upper()
                        if cfg.require_full_pass:
                            gate_passed = overall_status == "PASS"
                        else:
                            gate_passed = (
                                l0 == "PASS"
                                and l1 == "PASS"
                                and l2 == "PASS"
                                and l3 != "FAIL"
                                and l4 != "FAIL"
                                and overall_status != "FAIL"
                            )
                        for chk in verification_payload.get("checks") or []:
                            if isinstance(chk, Mapping) and str(chk.get("status", "")).upper() == "FAIL":
                                c_code = str(chk.get("code", "CHECK_FAIL"))
                                c_msg = str(chk.get("message", "verification check failed"))
                                diagnostics.append(f"{c_code}: {c_msg}")
                                prior_diagnostics.append({"code": c_code, "message": c_msg})
                    elif not ver_ok:
                        msg = f"verify failed on {branch_name}: {ver_err}"
                        diagnostics.append(msg)
                        prior_diagnostics.append({"code": "E_VERIFY_FAILED", "message": msg})
            except BaseException as exc:
                pending_exc = exc
                raise
            finally:
                restore_ok, _, restore_err = self._run_cli_json(
                    cli_exec,
                    ["project", "branch", str(proj), "checkout", cfg.base_branch, "--json"],
                )
                if not restore_ok:
                    restore_msg = f"failed to restore base branch {cfg.base_branch!r}: {restore_err}"
                    if pending_exc is not None:
                        pending_exc.add_note(restore_msg)
                    else:
                        raise BranchExplorationError(restore_msg)

            metrics = rollout_metrics_from_verification_report(
                candidate_id=candidate_id,
                track=cfg.track,
                patch_json=commands_json,
                report=verification_payload or {},
                applied_ok=applied_ok,
            )
            metrics_list.append(metrics)
            raw_candidates.append(
                {
                    "candidate_id": candidate_id,
                    "branch_name": branch_name,
                    "commands": commands,
                    "commands_json": commands_json,
                    "applied_ok": applied_ok,
                    "overall_status": overall_status,
                    "gate_passed": gate_passed,
                    "diagnostics": diagnostics,
                    "verification": verification_payload,
                }
            )

        effective_prompt_id = cfg.prompt_id or f"prompt-{_digest_label(prompt_text)[:12]}"
        grpo_group = compute_grpo_group_advantages(
            effective_prompt_id,
            prompt_text,
            metrics_list,
            weights=cfg.reward_weights,
        )

        outcomes: list[SemanticCandidateOutcome] = []
        for raw, scored in zip(raw_candidates, grpo_group.candidates, strict=True):
            outcomes.append(
                SemanticCandidateOutcome(
                    candidate_id=raw["candidate_id"],
                    branch_name=raw["branch_name"],
                    commands=raw["commands"],
                    commands_json=raw["commands_json"],
                    applied_ok=raw["applied_ok"],
                    overall_status=raw["overall_status"],
                    gate_passed=raw["gate_passed"],
                    reward=scored.total_reward,
                    group_advantage=scored.group_advantage,
                    reward_breakdown=dict(scored.reward_breakdown),
                    diagnostics=raw["diagnostics"],
                    verification=raw["verification"],
                )
            )

        passing_indexed = [
            (index, item)
            for index, item in enumerate(outcomes)
            if item.applied_ok and item.gate_passed
        ]
        if passing_indexed:
            _, winner = max(
                passing_indexed,
                key=lambda pair: (
                    pair[1].reward,
                    1 if pair[1].overall_status == "PASS" else 0,
                    -pair[0],
                ),
            )
            co_ok, _, co_err = self._run_cli_json(
                cli_exec,
                ["project", "branch", str(proj), "checkout", winner.branch_name, "--json"],
            )
            if not co_ok:
                raise BranchExplorationError(
                    f"failed to checkout winning branch {winner.branch_name!r}: {co_err}"
                )
            active_branch = winner.branch_name
            winner_id: str | None = winner.candidate_id
            winner_branch: str | None = winner.branch_name
            winner_score: float | None = winner.reward
            rolled_back = False
        else:
            co_ok, _, co_err = self._run_cli_json(
                cli_exec,
                ["project", "branch", str(proj), "checkout", cfg.base_branch, "--json"],
            )
            if not co_ok:
                raise BranchExplorationError(
                    f"failed to rollback to base branch {cfg.base_branch!r}: {co_err}"
                )
            active_branch = cfg.base_branch
            winner_id = None
            winner_branch = None
            winner_score = None
            rolled_back = True

        out_path = cfg.grpo_out_path or self._grpo_jsonl_path
        written_path: str | None = None
        if out_path is not None:
            export_grpo_training_jsonl([grpo_group], out_path, append=cfg.append_grpo)
            written_path = str(out_path)

        return SemanticBestOfNResult(
            base_branch=cfg.base_branch,
            active_branch=active_branch,
            winner_candidate_id=winner_id,
            winner_branch=winner_branch,
            winner_score=winner_score,
            rolled_back_to_base=rolled_back,
            outcomes=outcomes,
            grpo_group=grpo_group,
            grpo_out_path=written_path,
        )

    def _fetch_edit_context(
        self,
        *,
        cli_exec: str,
        project_path: Path,
        workspace_root: Path,
        target: str,
    ) -> EditContext:
        from .semantic_agent import EditContext

        ok, payload, err = self._run_cli_json(
            cli_exec,
            [
                "query",
                "--project",
                str(project_path),
                "--workspace",
                str(workspace_root),
                "--json",
                "explain",
                "--target",
                target,
            ],
        )
        if not ok or payload is None:
            raise BranchExplorationError(f"query explain failed for target {target!r}: {err}")
        explanation = payload.get("explanation") if isinstance(payload.get("explanation"), dict) else {}
        node_ids: list[str] = []
        for candidate_key in ("node_id", "target"):
            val = explanation.get(candidate_key)
            if isinstance(val, str) and val.strip() and val.strip() not in node_ids:
                node_ids.append(val.strip())
        if not node_ids and target:
            node_ids.append(target)
        params: list[str] = []
        raw_params = explanation.get("parameters") or explanation.get("bound_parameters") or []
        if isinstance(raw_params, Sequence) and not isinstance(raw_params, (str, bytes)):
            for item in raw_params:
                if isinstance(item, str) and item.strip():
                    params.append(item.strip())
                elif isinstance(item, Mapping) and isinstance(item.get("name"), str):
                    params.append(item["name"].strip())
        return EditContext(
            target=target,
            explanation=explanation,
            slice_context=payload,
            editable_node_ids=tuple(node_ids),
            allowed_parameters=tuple(dict.fromkeys(params)),
        )

    def _resolve_cli_executable(self) -> str:
        if self._cli_bin is not None:
            if self._cli_runner is not None:
                return str(self._cli_bin)
            if not self._cli_bin.is_file():
                raise BranchExplorationError(f"cad-cli was not found at {self._cli_bin}")
            return str(self._cli_bin.resolve())

        discovered = discover_cad_cli()
        if discovered is not None:
            return str(discovered)
        if self._cli_runner is not None:
            return "cad-cli"
        raise BranchExplorationError(
            "cad-cli was not found (pass cli_bin, set CAD_CLI_BIN, or build target/debug/cad-cli)"
        )

    def _run_cli_json(
        self,
        cli_exec: str,
        sub_args: list[str],
    ) -> tuple[bool, dict[str, Any] | None, str]:
        cmd = [cli_exec, *sub_args]
        try:
            if self._cli_runner is not None:
                completed = self._cli_runner(cmd)
            else:
                completed = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                    timeout=self._timeout_sec,
                    check=False,
                )
        except subprocess.TimeoutExpired:
            return False, None, f"cad-cli timed out after {self._timeout_sec}s"
        except OSError as exc:
            return False, None, f"failed to launch cad-cli: {exc}"

        payload = _parse_json_dict(completed.stdout or "")
        if completed.returncode != 0:
            return False, payload, _extract_cli_error(completed, payload)
        if payload is None:
            return False, None, "cad-cli did not return a JSON object"
        status = payload.get("status")
        if status is not None and status != "ok":
            return False, payload, _extract_cli_error(completed, payload)
        return True, payload, ""


def canonical_patch_json(patch: Mapping[str, Any]) -> str:
    """Serialize a ``GraphPatch`` deterministically without NaN/Inf."""
    return json.dumps(
        dict(patch),
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    )


def canonical_commands_json(commands: Sequence[Mapping[str, Any]]) -> str:
    """Serialize a V2 ``SemanticCommand`` batch deterministically without NaN/Inf."""
    return json.dumps(
        [dict(cmd) for cmd in commands],
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    )


def _validate_patch_object(patch: Any) -> str | None:
    if not isinstance(patch, dict):
        return "patch must be a JSON object"
    patch_id = patch.get("patch_id")
    if not isinstance(patch_id, str) or not patch_id.strip():
        return "patch is missing a non-empty patch_id"
    ops = patch.get("ops")
    if not isinstance(ops, list) or not all(isinstance(op, Mapping) for op in ops):
        return "patch ops must be a list of operation objects"
    try:
        canonical_patch_json(patch)
    except (TypeError, ValueError) as exc:
        return f"patch is not valid finite JSON: {exc}"
    return None


def _coerce_gate_verdict(
    raw: CandidateGateVerdict | Mapping[str, Any] | tuple[Any, ...],
    *,
    default_cache_hits: int,
) -> CandidateGateVerdict:
    if isinstance(raw, CandidateGateVerdict):
        if raw.kept_node_cache_hits == 0 and default_cache_hits > 0:
            return CandidateGateVerdict(
                gate_passed=raw.gate_passed,
                score=raw.score,
                diagnostics=raw.diagnostics,
                kept_node_cache_hits=default_cache_hits,
            )
        return raw
    if isinstance(raw, Mapping):
        passed = bool(raw.get("gate_passed", raw.get("passed", False)))
        score = float(raw.get("score", raw.get("reward_score", 0.0)))
        diags = raw.get("diagnostics", [])
        hits = int(raw.get("kept_node_cache_hits", default_cache_hits))
        return CandidateGateVerdict(
            gate_passed=passed,
            score=score,
            diagnostics=_normalize_diagnostics(diags),
            kept_node_cache_hits=hits,
        )
    if isinstance(raw, tuple) and 2 <= len(raw) <= 4:
        passed = bool(raw[0])
        score = float(raw[1])
        diags = _normalize_diagnostics(raw[2]) if len(raw) >= 3 else []
        hits = int(raw[3]) if len(raw) == 4 else default_cache_hits
        return CandidateGateVerdict(
            gate_passed=passed,
            score=score,
            diagnostics=diags,
            kept_node_cache_hits=hits,
        )
    raise TypeError(
        "gate_evaluator must return CandidateGateVerdict, Mapping, or (gate_passed, score[, diagnostics[, cache_hits]])"
    )


def _sanitize_for_json(value: Any) -> Any:
    if isinstance(value, bool) or value is None or isinstance(value, (int, str)):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else f"<non-finite:{value}>"
    if isinstance(value, Mapping):
        return {str(k): _sanitize_for_json(v) for k, v in value.items()}
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
        return [_sanitize_for_json(item) for item in value]
    return repr(value)


def _build_grpo_record(
    *,
    prompt_id: str,
    prompt: str,
    task_route: str,
    base_branch: str,
    winner_candidate_id: str | None,
    specs: Sequence[CandidatePatchSpec],
    outcomes: Sequence[CandidateEvalOutcome],
) -> dict[str, Any]:
    rewards = [item.score for item in outcomes]
    avg = mean(rewards)
    std = pstdev(rewards)
    scale = std + 1e-6
    advantages = [(score - avg) / scale if std > 0.0 else 0.0 for score in rewards]
    return {
        "prompt_id": prompt_id,
        "prompt": prompt,
        "task_route": task_route,
        "base_branch": base_branch,
        "winner_candidate_id": winner_candidate_id,
        "candidate_ids": [spec.candidate_id for spec in specs],
        "branch_names": [item.branch_name for item in outcomes],
        "completions": [canonical_patch_json(_sanitize_for_json(spec.patch)) for spec in specs],
        "applied_ok": [item.applied_ok for item in outcomes],
        "gate_passed": [item.gate_passed for item in outcomes],
        "rewards": rewards,
        "advantages": advantages,
        "kept_node_cache_hits": [item.kept_node_cache_hits for item in outcomes],
    }


def _append_jsonl_row(path: Path, row: Mapping[str, Any]) -> None:
    line = json.dumps(
        dict(row),
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    )
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8", newline="\n") as handle:
        handle.write(line + "\n")


def _extract_cache_hits(payload: Mapping[str, Any] | None) -> int:
    if not isinstance(payload, Mapping):
        return 0
    for key in ("kept_node_cache_hits", "cache_hits", "node_brep_cache_hits", "node_brep_cache_count"):
        value = payload.get(key)
        if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
            return value
    return 0


def _parse_json_dict(stdout: str) -> dict[str, Any] | None:
    text = stdout.strip()
    if not text:
        return None
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _extract_cli_error(
    completed: subprocess.CompletedProcess[str],
    payload: Mapping[str, Any] | None,
) -> str:
    if isinstance(payload, Mapping):
        msg = payload.get("message")
        if isinstance(msg, str) and msg.strip():
            return msg.strip()
    detail = (completed.stderr or "").strip() or (completed.stdout or "").strip()
    return detail or f"cad-cli exited with code {completed.returncode}"


def _utc_timestamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _resolve_route(prompt: str, task_route: str | TrackRoute | None) -> str:
    from .graph_patch_agent import TrackRoute, route_design_task

    if isinstance(task_route, TrackRoute):
        return task_route.track
    if isinstance(task_route, str) and task_route.strip():
        return task_route.strip()
    return route_design_task(prompt).track


def _digest_label(seed: str) -> str:
    import hashlib

    return hashlib.sha256(seed.encode("utf-8")).hexdigest()


def _validate_branch_segment(value: object, label: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{label} must be a string")
    stripped = value.strip()
    if (
        not stripped
        or stripped in {".", ".."}
        or "/" in stripped
        or "\\" in stripped
        or any(ch.isspace() or ord(ch) < 32 for ch in stripped)
    ):
        raise ValueError(f"unsafe {label}: {value!r}")
    return stripped


def _validate_branch_name(value: object, label: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{label} must be a string")
    stripped = value.strip()
    if (
        not stripped
        or stripped.startswith("/")
        or stripped.endswith("/")
        or "\\" in stripped
        or ".." in stripped
        or any(ch.isspace() or ord(ch) < 32 for ch in stripped)
    ):
        raise ValueError(f"unsafe {label}: {value!r}")
    parts = stripped.split("/")
    if any(not part or part in {".", ".."} for part in parts):
        raise ValueError(f"unsafe {label}: {value!r}")
    return stripped


def _normalize_diagnostics(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [value]
    if isinstance(value, Sequence):
        return [str(item) for item in value]
    raise TypeError("diagnostics must be a sequence of strings")


def _require_finite_float(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"{label} must be a real number")
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"{label} must be finite")
    return number


def _require_nonneg_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{label} must be an int")
    if value < 0:
        raise ValueError(f"{label} must be nonnegative")
    return value


__all__ = [
    "BestOfNExplorationResult",
    "BranchExplorationError",
    "BranchPatchExplorer",
    "CandidateEvalOutcome",
    "CandidateGateVerdict",
    "CandidatePatchSpec",
    "SemanticBestOfNResult",
    "SemanticCandidateOutcome",
    "SemanticRolloutConfig",
    "canonical_commands_json",
    "canonical_patch_json",
]

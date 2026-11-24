"""Engineering Agent 2.0: 0→1 creation and 1→N point-and-prompt edits.

The model plans ``SemanticCommand`` batches. ``cad-cli`` evaluates them.
A layer that did not run stays ``UNKNOWN``. ``UNKNOWN`` is never reported as ``PASS``.
"""

from __future__ import annotations

import json
import re
import sys
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field, replace
from typing import Any, Protocol

from .context_slicer import slice_canonical_graph_dict
from .contracts import (
    VERIFICATION_LAYERS,
    DesignContract,
    KernelIdentity,
    TriVerdict,
    VerificationCheck,
    VerificationLayer,
    VerificationReport,
    aggregate_tri_verdicts,
)
from .runtime_client import ApplyOutcome, RuntimeToolClient, RuntimeToolError
from .v2_context_adapter import ContextAssetRecord, ExternalInterferenceVerdict
from .vision import VisualReviewResult, attach_visual_assist

SEMANTIC_COMMAND_NAMES: frozenset[str] = frozenset(
    {
        "upsert_parameter",
        "set_feature_input",
        "remove_parameter",
        "set_design_contract",
        "create_sketch",
        "upsert_sketch_entities",
        "upsert_sketch_constraints",
        "create_primitive_box",
        "create_primitive_cylinder",
        "extrude_sketch",
        "revolve_sketch",
        "sweep_profile",
        "loft_sections",
        "create_hole",
        "fillet_edges",
        "chamfer_edges",
        "shell_solid",
        "draft_faces",
        "pattern_feature",
        "mirror_feature",
        "boolean_solids",
        "direct_face_edit",
        "insert_component",
        "add_mate",
        "rename_node",
        "set_node_suppressed",
        "delete_node",
        "reorder_feature",
        "tag_topology",
        "add_design_assertion",
    }
)

_REQUIRED_ARGS: dict[str, tuple[str, ...]] = {
    "upsert_parameter": ("name", "ir_type", "value"),
    "set_feature_input": ("node_id", "input"),
    "remove_parameter": ("name",),
    "set_design_contract": ("contract",),
    "create_sketch": ("name", "plane"),
    "upsert_sketch_entities": ("node_id", "entities"),
    "upsert_sketch_constraints": ("node_id", "constraints"),
    "create_primitive_box": ("name", "width", "depth", "height"),
    "create_primitive_cylinder": ("name", "radius", "height"),
    "extrude_sketch": ("name", "sketch", "distance"),
    "revolve_sketch": ("name", "sketch", "angle"),
    "sweep_profile": ("name", "profile", "path"),
    "loft_sections": ("name", "sections"),
    "create_hole": ("name", "solid", "definition"),
    "fillet_edges": ("name", "solid", "radius"),
    "chamfer_edges": ("name", "solid", "distance"),
    "shell_solid": ("name", "solid", "thickness"),
    "draft_faces": ("name", "solid", "faces", "pull_direction", "neutral_plane", "angle"),
    "pattern_feature": ("name", "solid", "definition"),
    "mirror_feature": ("name", "solid", "plane"),
    "boolean_solids": ("name", "op", "target", "tools"),
    "direct_face_edit": ("name", "solid", "face", "edit"),
    "insert_component": ("name", "source"),
    "add_mate": ("name", "left", "right", "left_ref", "right_ref", "mate_kind"),
    "rename_node": ("node_id", "name"),
    "set_node_suppressed": ("node_id", "suppressed"),
    "delete_node": ("node_id",),
    "reorder_feature": ("node_id",),
    "tag_topology": ("name", "solid", "tag"),
    "add_design_assertion": ("name", "assertion_id", "kind"),
}

_NODE_KEYS = {"node_id", "sketch", "solid", "profile", "target", "left", "right", "before", "condition"}
_NODE_LIST_KEYS = {"sections", "guides", "tools"}
_TOPO_REF_KEYS = {"face", "target_face", "left_ref", "right_ref"}
_TOPO_REF_LIST_KEYS = {"edges", "open_faces", "faces", "targets"}
_TOPO_KINDS = {"face", "edge", "vertex", "solid", "body"}
_FORBIDDEN_LOCAL_COMMANDS: frozenset[str] = frozenset({"set_design_contract"})
_CREATING_COMMANDS: frozenset[str] = frozenset(
    {
        "create_sketch",
        "create_primitive_box",
        "create_primitive_cylinder",
        "extrude_sketch",
        "revolve_sketch",
        "sweep_profile",
        "loft_sections",
        "create_hole",
        "fillet_edges",
        "chamfer_edges",
        "shell_solid",
        "draft_faces",
        "pattern_feature",
        "mirror_feature",
        "boolean_solids",
        "direct_face_edit",
        "insert_component",
        "add_mate",
        "tag_topology",
        "add_design_assertion",
    }
)
_HOLE_FEATURES = {"hole", "std_fastener_hole"}
_BODY_FEATURES = {
    "primitive_box",
    "primitive_cylinder",
    "extrude",
    "revolve",
    "sweep",
    "loft",
    "boolean",
    "shell",
}
_SOLVER_KEYS = ("fea_solver", "solver", "solver_id")
_ABSENT_SOLVER = {"", "UNKNOWN", "NONE", "UNMOUNTED", "MISSING"}
_THICKEN_RE = re.compile(
    r"(?:加厚|thicken|thickness)[^\d]{0,40}(\d+(?:\.\d+)?)\s*mm",
    re.IGNORECASE,
)
_HOLE_CHANGE_RE = re.compile(
    r"M\s*(\d+(?:\.\d+)?)(?:[^\nM]{0,48})M\s*(\d+(?:\.\d+)?)\s*"
    r"(countersink|counterbore|沉头|锪平|csink|cbore)",
    re.IGNORECASE,
)
_HOLE_INTENT_RE = re.compile(r"孔|hole|countersink|counterbore|沉头|锪平", re.IGNORECASE)


class SemanticCommandError(ValueError):
    """A planned batch is not a valid local ``SemanticCommand`` list."""

    def __init__(
        self,
        message: str,
        *,
        code: str = "E400_INVALID_COMMAND",
        recovery_hints: Mapping[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.recovery_hints: dict[str, Any] = dict(recovery_hints) if recovery_hints else {}


class SemanticPlanner(Protocol):
    """Turns a contract or a local edit into one ``SemanticCommand`` batch."""

    def plan_creation(
        self,
        contract: DesignContract,
        intent: str,
        *,
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        """Return the next 0→1 batch. ``attempt`` is 0 on the first try."""

    def plan_local_edit(
        self,
        *,
        target: str,
        instruction: str,
        context: Mapping[str, Any],
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        """Return a batch that touches only the local edit set."""


@dataclass(slots=True)
class CreationResult:
    """Mode A result. ``passed`` is true only when every layer is ``PASS``."""

    commands: list[dict[str, Any]]
    branch: str
    revision: int | None
    report: VerificationReport | None
    attempts: int
    repairs: int
    overall_status: TriVerdict
    diagnostics: list[dict[str, Any]] = field(default_factory=list)
    error_code: str | None = None
    error_message: str | None = None
    visual_review: dict[str, Any] | None = None
    interference_status: str | None = None
    untrusted_asset_labels: list[dict[str, str]] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        return (
            self.error_code is None
            and self.report is not None
            and self.report.passed
            and self.overall_status == "PASS"
        )

    def to_dict(self) -> dict[str, Any]:
        payload = {
            "mode": "create",
            "passed": self.passed,
            "overall_status": self.overall_status,
            "branch": self.branch,
            "revision": self.revision,
            "attempts": self.attempts,
            "repairs": self.repairs,
            "commands": self.commands,
            "diagnostics": self.diagnostics,
            "error_code": self.error_code,
            "error_message": self.error_message,
            "verification": None if self.report is None else self.report.to_dict(),
        }
        return _with_assist_fields(
            payload,
            visual_review=self.visual_review,
            interference_status=self.interference_status,
            untrusted_asset_labels=self.untrusted_asset_labels,
        )


@dataclass(slots=True)
class ChangePreviewPayload:
    """Mode B review packet. The caller confirms before anything lands on ``main``."""

    plan_summary: str
    diff: dict[str, Any]
    verdict: TriVerdict
    report: VerificationReport | None
    branch: str
    revision: int | None
    expected_revision: int | None
    commands: list[dict[str, Any]]
    untouched_preserved: bool
    preserved_node_ids: list[str] = field(default_factory=list)
    drifted_node_ids: list[str] = field(default_factory=list)
    approval_required: bool = True
    repairs: int = 0
    error_code: str | None = None
    error_message: str | None = None
    visual_review: dict[str, Any] | None = None
    interference_status: str | None = None
    untrusted_asset_labels: list[dict[str, str]] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        return (
            self.error_code is None
            and self.untouched_preserved
            and self.report is not None
            and self.report.passed
            and self.verdict == "PASS"
        )

    def to_dict(self) -> dict[str, Any]:
        payload = {
            "mode": "local_edit",
            "plan_summary": self.plan_summary,
            "diff": self.diff,
            "verdict": self.verdict,
            "passed": self.passed,
            "approval_required": self.approval_required,
            "branch": self.branch,
            "revision": self.revision,
            "expected_revision": self.expected_revision,
            "commands": self.commands,
            "untouched_preserved": self.untouched_preserved,
            "preserved_node_ids": list(self.preserved_node_ids),
            "drifted_node_ids": list(self.drifted_node_ids),
            "repairs": self.repairs,
            "error_code": self.error_code,
            "error_message": self.error_message,
            "verification": None if self.report is None else self.report.to_dict(),
        }
        return _with_assist_fields(
            payload,
            visual_review=self.visual_review,
            interference_status=self.interference_status,
            untrusted_asset_labels=self.untrusted_asset_labels,
        )


def length_mm_value(millimeters: float) -> dict[str, Any]:
    """``IrValue::Quantity`` JSON for a millimetre length."""
    return {
        "type": "quantity",
        "value": {"dimension": "length", "value": float(millimeters), "unit": "mm"},
    }


def dim_param(name: str) -> dict[str, Any]:
    """``DimInput::Param`` JSON."""
    return {"source": "param", "param_id": name}


def validate_semantic_batch(commands: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    """Return a copy of ``commands`` or raise ``SemanticCommandError``.

    The wire shape matches ``crates/cad-api/src/command.rs``: each item is
    ``{"command": "<snake_name>", "args": { ... }}`` with the required args.
    """
    if not isinstance(commands, Sequence) or isinstance(commands, (str, bytes)):
        raise SemanticCommandError("commands must be a list of SemanticCommand objects")
    if len(commands) == 0:
        raise SemanticCommandError("commands must contain at least one SemanticCommand")
    validated: list[dict[str, Any]] = []
    for index, raw in enumerate(commands):
        if not isinstance(raw, Mapping):
            raise SemanticCommandError(f"commands[{index}] must be an object")
        extra = set(raw) - {"command", "args"}
        if extra:
            raise SemanticCommandError(
                f"commands[{index}] has unknown fields {sorted(extra)}; "
                "only 'command' and 'args' are accepted"
            )
        name = raw.get("command")
        if not isinstance(name, str) or name not in SEMANTIC_COMMAND_NAMES:
            raise SemanticCommandError(
                f"commands[{index}].command must be one of the Semantic CAD API commands, "
                f"got {name!r}"
            )
        args = raw.get("args")
        if not isinstance(args, Mapping):
            raise SemanticCommandError(f"commands[{index}].args must be an object")
        missing = [key for key in _REQUIRED_ARGS.get(name, ()) if key not in args]
        if missing:
            raise SemanticCommandError(
                f"commands[{index}] ({name}) is missing args: {', '.join(missing)}"
            )
        validated.append({"command": name, "args": _json_copy(dict(args))})
    return validated


def enforce_honest_verdict(
    report: VerificationReport,
    *,
    loads_declared: bool,
) -> VerificationReport:
    """Recompute the tri-state verdict. Never promote ``UNKNOWN`` or a missing solver to ``PASS``."""
    checks = [
        VerificationCheck(
            code=check.code,
            layer=check.layer,
            status=check.status,
            message=check.message,
            evidence=dict(check.evidence),
        )
        for check in report.checks
    ]
    layers: dict[VerificationLayer, TriVerdict] = {
        layer: report.layers.get(layer, "UNKNOWN") for layer in VERIFICATION_LAYERS
    }
    if loads_declared and not _has_fea_solver(checks) and layers["L3_manufacturability_physics"] != "FAIL":
        layers["L3_manufacturability_physics"] = "UNKNOWN"
        if not any(check.code == "L3_FEA_UNAVAILABLE" for check in checks):
            checks.append(
                VerificationCheck(
                    code="L3_FEA_UNAVAILABLE",
                    layer="L3_manufacturability_physics",
                    status="UNKNOWN",
                    message="Structural loads are declared but no FEA solver is mounted",
                    evidence={"loads_declared": True, "solver": None},
                )
            )
    for layer in VERIFICATION_LAYERS:
        layer_statuses = [check.status for check in checks if check.layer == layer]
        if "UNKNOWN" in layer_statuses and layers[layer] == "PASS":
            layers[layer] = "UNKNOWN"
        if layer_statuses and "FAIL" in layer_statuses:
            layers[layer] = "FAIL"
    rebuilt = VerificationReport(
        kernel_identity=report.kernel_identity,
        layers=layers,
        checks=checks,
        overall_status=None,
        schema_version=report.schema_version,
        summary=report.summary,
    )
    expected = aggregate_tri_verdicts(rebuilt.layers[layer] for layer in VERIFICATION_LAYERS)
    if rebuilt.overall_status != expected or (
        rebuilt.overall_status == "PASS" and any(rebuilt.layers[layer] != "PASS" for layer in VERIFICATION_LAYERS)
    ):
        raise SemanticCommandError(
            f"honest verdict drifted: overall={rebuilt.overall_status!r} aggregate={expected!r}"
        )
    return rebuilt


def merge_mating_contract(base: DesignContract, record: ContextAssetRecord) -> DesignContract:
    """Add trusted mating geometry. Asset labels stay on the record, not the contract."""
    taken_interfaces = {item.interface_id for item in base.interfaces}
    taken_zones = {item.zone_id for item in base.keep_out_zones}
    interfaces = [
        item for item in record.contract.interfaces if item.interface_id not in taken_interfaces
    ]
    zones = [item for item in record.contract.keep_out_zones if item.zone_id not in taken_zones]
    return replace(
        base,
        interfaces=[*base.interfaces, *interfaces],
        keep_out_zones=[*base.keep_out_zones, *zones],
        external_context=record.summary,
    )


def unknown_external_interference(message: str) -> ExternalInterferenceVerdict:
    """An exact recheck that did not run, or cannot be trusted, stays ``UNKNOWN``."""
    check = VerificationCheck(
        code="L2_EXTERNAL_BREP_INTERFERENCE",
        layer="L2_interfaces_constraints",
        status="UNKNOWN",
        message=message,
        evidence={
            "exact_brep_check": False,
            "used_preview_mesh": False,
            "authoritative": False,
            "kernel_backend": "unknown",
            "diagnostic_codes": [],
            "asset_format": "UNKNOWN",
        },
    )
    return ExternalInterferenceVerdict(
        status="UNKNOWN",
        message=message,
        kernel_identity=KernelIdentity(provider="unknown", version="unknown", authoritative=False),
        check=check,
    )


def merge_external_interference(
    report: VerificationReport,
    verdict: ExternalInterferenceVerdict,
) -> VerificationReport:
    """Fold an exact-interference check into the report.

    A non-authoritative kernel is forced to ``UNKNOWN``. A ``PASS`` check cannot
    promote a geometric ``FAIL`` or ``UNKNOWN``.
    """
    check = verdict.check
    if not verdict.kernel_identity.authoritative and check.status != "FAIL":
        check = VerificationCheck(
            code=check.code,
            layer=check.layer,
            status="UNKNOWN",
            message=verdict.message or "Non-authoritative kernel cannot prove exact external clearance.",
            evidence={
                **dict(check.evidence),
                "authoritative": False,
                "exact_brep_check": False,
            },
        )
    before_layers = {layer: report.layers[layer] for layer in VERIFICATION_LAYERS}
    before_overall = report.overall_status or "UNKNOWN"
    rebuilt = VerificationReport(
        kernel_identity=report.kernel_identity,
        layers=dict(before_layers),
        checks=[*report.checks, check],
        overall_status=None,
        schema_version=report.schema_version,
        summary=report.summary,
    )
    if before_overall in {"FAIL", "UNKNOWN"} and rebuilt.overall_status == "PASS":
        return report
    for layer, status in before_layers.items():
        if status in {"FAIL", "UNKNOWN"} and rebuilt.layers.get(layer) == "PASS":
            return report
    return rebuilt


def _with_assist_fields(
    payload: dict[str, Any],
    *,
    visual_review: dict[str, Any] | None,
    interference_status: str | None,
    untrusted_asset_labels: list[dict[str, str]],
) -> dict[str, Any]:
    if visual_review is not None:
        payload["visual_review"] = visual_review
    if interference_status is not None:
        payload["interference_status"] = interference_status
    if untrusted_asset_labels:
        payload["untrusted_asset_labels"] = list(untrusted_asset_labels)
    return payload


def report_from_verify_payload(payload: Mapping[str, Any]) -> VerificationReport:
    """Read the ``report`` object from ``cad-cli verify --json``."""
    raw_report = payload.get("report", payload)
    if not isinstance(raw_report, Mapping):
        raise SemanticCommandError("verify payload is missing a report object", code="E_IO")
    return VerificationReport.from_dict(dict(raw_report))


def diagnostics_from_report(report: VerificationReport) -> list[dict[str, Any]]:
    """FAIL checks plus machine-readable recovery hints. ``UNKNOWN`` is not a repair trigger."""
    found: list[dict[str, Any]] = []
    for check in report.checks:
        if check.status != "FAIL":
            continue
        hints = dict(check.evidence)
        embedded = _hints_in_message(check.message)
        hints.update(embedded)
        node_id = hints.get("node_id")
        found.append(
            {
                "code": check.code,
                "layer": check.layer,
                "message": check.message,
                "node_id": node_id if isinstance(node_id, str) else None,
                "recovery_hints": hints,
            }
        )
    return found


def focus_node_from_target(target: str) -> str:
    """Use the creator node of a ``TopoRef`` (``node::face``) or the node id itself."""
    text = target.strip()
    if "::" in text:
        head, _tail = text.split("::", 1)
        head = head.strip()
        if head and ":" not in head:
            return head
    return text


def preview_branch_name(session_id: str, *, client: str = "cad-agent") -> str:
    """Candidate branch ``agent/<client>/<session>``."""
    return f"agent/{_branch_component(client)}/{_branch_component(session_id)}"


def slice_context(
    project_or_graph: Mapping[str, Any] | Any,
    focus_nodes: Sequence[str],
    *,
    client: RuntimeToolClient | None = None,
    upstream_depth: int = 1,
    downstream_depth: int = 1,
) -> dict[str, Any]:
    """Local neighborhood. In-memory graphs stay in-process. Files go through ``cad-cli``."""
    seeds = [focus_node_from_target(str(node)) for node in focus_nodes if str(node).strip()]
    if not seeds:
        raise SemanticCommandError("slice_context requires a focus node or TopoRef")
    if isinstance(project_or_graph, Mapping):
        sliced = slice_canonical_graph_dict(
            project_or_graph,
            seeds,
            upstream_depth=upstream_depth,
            downstream_depth=downstream_depth,
        )
        kept = [str(node_id) for node_id in sliced.get("nodes", {})]
        return {
            "source": "memory",
            "focus_nodes": seeds,
            "graph": sliced,
            "neighborhood_node_ids": kept,
            "kcl_view": "",
        }
    if client is None:
        raise SemanticCommandError(
            "slice_context needs a document graph or a RuntimeToolClient",
            code="E400_INVALID_ARGUMENT",
        )
    payload = client.slice_context(
        seeds,
        upstream_depth=upstream_depth,
        downstream_depth=downstream_depth,
    )
    graph = payload.get("graph") if isinstance(payload.get("graph"), Mapping) else {}
    kept_raw = payload.get("kept_nodes")
    if isinstance(kept_raw, list) and kept_raw:
        kept = [str(node_id) for node_id in kept_raw]
    else:
        kept = [str(node_id) for node_id in graph.get("nodes", {})] if isinstance(graph, Mapping) else []
    return {
        "source": "cad-cli",
        "focus_nodes": seeds,
        "graph": dict(graph) if isinstance(graph, Mapping) else {},
        "neighborhood_node_ids": kept,
        "kcl_view": payload.get("kcl") if isinstance(payload.get("kcl"), str) else "",
        "raw": payload,
    }


class HeuristicSemanticPlanner:
    """Deterministic planner used when no model client is injected.

    It emits parametric boxes and standard holes from the contract, and it
    applies ``recovery_hints`` that name a parameter and a ``suggested_mm``.
    Local edits only retarget hole features and one thickness parameter.
    """

    def plan_creation(
        self,
        contract: DesignContract,
        intent: str,
        *,
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        del intent, attempt
        params = _numeric_params(contract.feature_params)
        for diagnostic in diagnostics:
            hints = diagnostic.get("recovery_hints")
            if not isinstance(hints, Mapping):
                continue
            name = hints.get("parameter")
            suggested = hints.get("suggested_mm")
            if isinstance(name, str) and _is_number(suggested):
                params[name] = float(suggested)
        commands: list[dict[str, Any]] = [_design_contract_command(contract)]
        for name in sorted(params):
            commands.append(_upsert_length(name, params[name]))
        if {"width", "depth", "height"} <= set(params):
            commands.append(
                {
                    "command": "create_primitive_box",
                    "args": {
                        "node_id": "body",
                        "name": contract.part_name,
                        "width": dim_param("width"),
                        "depth": dim_param("depth"),
                        "height": dim_param("height"),
                    },
                }
            )
        return commands

    def plan_local_edit(
        self,
        *,
        target: str,
        instruction: str,
        context: Mapping[str, Any],
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        del attempt
        commands: list[dict[str, Any]] = []
        allowed = set(context.get("editable_node_ids") or [])
        thicken = _THICKEN_RE.search(instruction)
        if thicken is not None:
            delta = float(thicken.group(1))
            param_name, current = _thickness_parameter(context)
            suggested = _suggested_override(diagnostics, param_name)
            if param_name is not None and current is not None:
                updated = suggested if suggested is not None else current + delta
                commands.append(_upsert_length(param_name, updated))
        hole_change = _HOLE_CHANGE_RE.search(instruction)
        if hole_change is not None:
            designation = f"M{hole_change.group(2)}"
            kind_word = hole_change.group(3).lower()
            standard = (
                "iso4762_counterbore"
                if kind_word in {"counterbore", "cbore", "锪平"}
                else "iso10642_countersink"
            )
            solid = _body_solid(context, target, allowed)
            for hole_id in sorted(node for node in allowed if _feature_of(node, context) in _HOLE_FEATURES):
                commands.append({"command": "delete_node", "args": {"node_id": hole_id}})
                commands.append(
                    {
                        "command": "create_hole",
                        "args": {
                            "node_id": f"{hole_id}_edited",
                            "name": f"{designation} hole",
                            "solid": solid or focus_node_from_target(target),
                            "definition": {
                                "hole": "standard",
                                "standard": standard,
                                "designation": designation,
                                "depth": {"kind": "through_all"},
                            },
                        },
                    }
                )
        if not commands:
            raise SemanticCommandError(
                "the local edit instruction did not resolve to a parameter or hole change "
                "inside the sliced neighborhood"
            )
        return commands


class EngineeringSemanticAgent:
    """Runs Mode A and Mode B against one ``RuntimeToolClient``."""

    def __init__(
        self,
        client: RuntimeToolClient,
        *,
        planner: SemanticPlanner | None = None,
        max_repairs: int = 3,
        session_id: str = "session",
        preview_branch: str | None = None,
        progress_callback: Callable[[dict[str, Any]], None] | None = None,
        context_record: ContextAssetRecord | None = None,
        interference_checker: Callable[[], ExternalInterferenceVerdict | None] | None = None,
        visual_reviewer: Callable[[VerificationReport], VisualReviewResult | None] | None = None,
    ) -> None:
        if isinstance(max_repairs, bool) or not isinstance(max_repairs, int) or max_repairs < 0:
            raise ValueError("max_repairs must be an integer >= 0")
        self.client = client
        self.planner: SemanticPlanner = planner if planner is not None else HeuristicSemanticPlanner()
        self.max_repairs = max_repairs
        self.session_id = session_id
        self.preview_branch = preview_branch or preview_branch_name(session_id)
        self.progress_callback = progress_callback
        self.context_record = context_record
        self.interference_checker = interference_checker
        self.visual_reviewer = visual_reviewer
        self._last_visual: VisualReviewResult | None = None
        self._last_interference_status: str | None = None

    def create_part(self, contract: DesignContract, intent: str = "") -> CreationResult:
        """Mode A: validate a command batch, apply it on a preview branch, verify, repair on FAIL."""
        self._last_visual = None
        self._last_interference_status = None
        contract = self._prepare_contract(contract)
        contract.assert_valid()
        intent = self._planner_intent(intent)
        branch = self.preview_branch
        revision = self._initial_revision()
        diagnostics: list[dict[str, Any]] = []
        commands: list[dict[str, Any]] = []
        report: VerificationReport | None = None
        repairs = 0
        attempts = 0
        try:
            for attempt in range(self.max_repairs + 1):
                attempts = attempt + 1
                self._emit(
                    "semantic_plan",
                    f"Planning creation batch attempt {attempts}",
                    attempt=attempts,
                    mode="create",
                )
                commands = validate_semantic_batch(
                    self.planner.plan_creation(
                        contract,
                        intent,
                        diagnostics=diagnostics,
                        attempt=attempt,
                    )
                )
                try:
                    outcome, revision = self._apply_with_revision_retry(commands, revision, branch)
                except RuntimeToolError as exc:
                    if not _is_recoverable_apply_error(exc):
                        raise
                    diag = _diagnostic_from_runtime_error(exc)
                    diagnostics = [diag]
                    if attempt < self.max_repairs:
                        repairs += 1
                        continue
                    return self._annotate(
                        CreationResult(
                            commands=commands,
                            branch=branch,
                            revision=revision,
                            report=report,
                            attempts=attempts,
                            repairs=repairs,
                            overall_status="FAIL",
                            diagnostics=diagnostics,
                            error_code=exc.code,
                            error_message=str(exc),
                        )
                    )
                self.client.checkout_branch(branch)
                report = self._post_verify(
                    self._verify(contract),
                    loads_declared=bool(contract.loads),
                )
                self._emit(
                    "semantic_verified",
                    f"Creation verification {report.overall_status}",
                    overall_status=report.overall_status,
                    mode="create",
                )
                diagnostics = diagnostics_from_report(report)
                if diagnostics and attempt < self.max_repairs:
                    repairs += 1
                    revision = outcome.revision
                    continue
                revision = outcome.revision
                break
        except RuntimeToolError as exc:
            return self._annotate(
                CreationResult(
                    commands=commands,
                    branch=branch,
                    revision=revision,
                    report=report,
                    attempts=attempts,
                    repairs=repairs,
                    overall_status="UNKNOWN",
                    diagnostics=diagnostics,
                    error_code=exc.code,
                    error_message=str(exc),
                )
            )
        except SemanticCommandError as exc:
            return self._annotate(
                CreationResult(
                    commands=commands,
                    branch=branch,
                    revision=revision,
                    report=report,
                    attempts=attempts,
                    repairs=repairs,
                    overall_status="FAIL",
                    diagnostics=diagnostics,
                    error_code=exc.code,
                    error_message=str(exc),
                )
            )
        status: TriVerdict = report.overall_status if report is not None and report.overall_status else "UNKNOWN"
        return self._annotate(
            CreationResult(
                commands=commands,
                branch=branch,
                revision=revision,
                report=report,
                attempts=attempts,
                repairs=repairs,
                overall_status=status,
                diagnostics=diagnostics,
            )
        )

    def edit_local(
        self,
        target: str,
        instruction: str,
        *,
        document_graph: Mapping[str, Any] | None = None,
        upstream_depth: int = 1,
        downstream_depth: int = 1,
        loads_declared: bool = False,
    ) -> ChangePreviewPayload:
        """Mode B: slice the target, edit only that neighborhood, and return a preview."""
        self._last_visual = None
        self._last_interference_status = None
        branch = self.preview_branch
        commands: list[dict[str, Any]] = []
        report: VerificationReport | None = None
        revision: int | None = None
        expected: int | None = None
        repairs = 0
        context: dict[str, Any] = {}
        original_branch: str | None = None
        checked_out_preview = False
        payload: ChangePreviewPayload | None = None
        restore_error: RuntimeToolError | None = None
        try:
            if not target.strip():
                raise SemanticCommandError("a TopoRef or feature node id is required")
            if not instruction.strip():
                raise SemanticCommandError("a local edit instruction is required")
            status = self.client.session_status()
            original_branch = status.active_branch
            summary_payload = self.client.query_summary()
            focus = focus_node_from_target(target)
            try:
                explanation = self.client.query_explain(focus)
            except RuntimeToolError as exc:
                if exc.code != "E404_NODE_NOT_FOUND":
                    raise
                explanation = {"query": "explain", "target": focus, "explanation": None}
            sliced = slice_context(
                document_graph if document_graph is not None else self.client.project_path,
                [focus],
                client=None if document_graph is not None else self.client,
                upstream_depth=upstream_depth,
                downstream_depth=downstream_depth,
            )
            context = _assemble_context(
                target=target,
                focus=focus,
                summary_payload=summary_payload,
                explanation=explanation,
                sliced=sliced,
                instruction=instruction,
            )
            before_ids = _outline_ids(summary_payload)
            before_merkle = _merkle_map(summary_payload)
            context = self._prepare_edit_context(context)
            allowed = set(context["editable_node_ids"])
            allowed_params = set(context["allowed_parameters"])
            full_params = _parameter_names(summary_payload)
            expected = status.revision
            revision = expected
            diagnostics: list[dict[str, Any]] = []
            outcome: ApplyOutcome | None = None
            planner_instruction = instruction
            for attempt in range(self.max_repairs + 1):
                self._emit(
                    "semantic_plan",
                    f"Planning local edit attempt {attempt + 1}",
                    attempt=attempt + 1,
                    mode="local_edit",
                )
                commands = validate_semantic_batch(
                    self.planner.plan_local_edit(
                        target=target,
                        instruction=planner_instruction,
                        context=context,
                        diagnostics=diagnostics,
                        attempt=attempt,
                    )
                )
                _assert_local_batch(
                    commands,
                    document_node_ids=before_ids,
                    allowed_node_ids=allowed,
                    document_parameters=full_params,
                    allowed_parameters=allowed_params,
                )
                try:
                    outcome, revision = self._apply_with_revision_retry(commands, revision, branch)
                except RuntimeToolError as exc:
                    if not _is_recoverable_apply_error(exc):
                        raise
                    diag = _diagnostic_from_runtime_error(exc)
                    diagnostics = [diag]
                    if attempt < self.max_repairs:
                        repairs += 1
                        continue
                    payload = self._annotate(
                        _failed_preview(
                            instruction=instruction,
                            commands=commands,
                            branch=branch,
                            revision=revision,
                            expected_revision=expected,
                            report=report,
                            repairs=repairs,
                            code=exc.code,
                            message=str(exc),
                            verdict="FAIL",
                        )
                    )
                    break
                self.client.checkout_branch(branch)
                checked_out_preview = True
                report = self._post_verify(
                    self._verify_loads(
                        summary_payload,
                        loads_declared=loads_declared,
                    ),
                    loads_declared=loads_declared or _summary_declares_loads(summary_payload),
                )
                self._emit(
                    "semantic_verified",
                    f"Local edit verification {report.overall_status}",
                    overall_status=report.overall_status,
                    mode="local_edit",
                )
                diagnostics = diagnostics_from_report(report)
                if diagnostics and attempt < self.max_repairs:
                    repairs += 1
                    revision = outcome.revision
                    continue
                revision = outcome.revision
                break
            if payload is None:
                after_summary = self.client.query_summary()
                touched = _referenced_existing(commands, before_ids)
                if any(_parameter_refs(cmd) for cmd in commands):
                    touched |= allowed & before_ids
                payload = self._annotate(
                    _preview_from_apply(
                        instruction=instruction,
                        commands=commands,
                        outcome=outcome,
                        report=report,
                        branch=branch,
                        expected_revision=expected,
                        before_ids=before_ids,
                        before_merkle=before_merkle,
                        after_summary=after_summary,
                        referenced=touched,
                        repairs=repairs,
                    )
                )
        except RuntimeToolError as exc:
            payload = self._annotate(
                _failed_preview(
                    instruction=instruction,
                    commands=commands,
                    branch=branch,
                    revision=revision,
                    expected_revision=expected,
                    report=report,
                    repairs=repairs,
                    code=exc.code,
                    message=str(exc),
                    verdict="UNKNOWN",
                )
            )
        except SemanticCommandError as exc:
            payload = self._annotate(
                _failed_preview(
                    instruction=instruction,
                    commands=commands,
                    branch=branch,
                    revision=revision,
                    expected_revision=expected,
                    report=report,
                    repairs=repairs,
                    code=exc.code,
                    message=str(exc),
                    verdict="FAIL",
                )
            )
        finally:
            pending_error = sys.exc_info()[1]
            if checked_out_preview and original_branch and original_branch != branch:
                try:
                    self.client.checkout_branch(original_branch)
                except RuntimeToolError as exc:
                    message = f"failed to restore active branch {original_branch}: {exc}"
                    if pending_error is not None:
                        pending_error.add_note(message)
                    else:
                        restore_error = RuntimeToolError(message, code=exc.code or "E_IO")
        if payload is None:
            raise RuntimeToolError("local edit produced no preview", code="E_IO")
        if restore_error is not None:
            message = str(restore_error)
            if payload.error_message:
                message = f"{payload.error_message}; {message}"
            return self._annotate(
                _failed_preview(
                    instruction=instruction,
                    commands=payload.commands,
                    branch=branch,
                    revision=payload.revision,
                    expected_revision=payload.expected_revision,
                    report=payload.report,
                    repairs=payload.repairs,
                    code=restore_error.code or "E_IO",
                    message=message,
                    verdict="FAIL",
                )
            )
        return payload

    def _prepare_contract(self, contract: DesignContract) -> DesignContract:
        record = self.context_record
        if record is None:
            return contract
        if contract.external_context is None:
            return merge_mating_contract(contract, record)
        return contract

    def _planner_intent(self, intent: str) -> str:
        if self.context_record is None:
            return intent
        hints = self.context_record.planner_alignment_hints
        return (
            f"{intent}\n\nTrusted mating hints (asset labels omitted):\n"
            f"{json.dumps(hints, ensure_ascii=False)}"
        )

    def _prepare_edit_context(self, context: dict[str, Any]) -> dict[str, Any]:
        prepared = dict(context)
        if self.context_record is None:
            return prepared
        explanation = prepared.get("explanation")
        if not isinstance(explanation, dict):
            explanation = {"text": explanation} if isinstance(explanation, str) else {}
        else:
            explanation = dict(explanation)
        explanation["planner_alignment_hints"] = list(self.context_record.planner_alignment_hints)
        prepared["explanation"] = explanation
        return prepared

    def _post_verify(
        self,
        report: VerificationReport,
        *,
        loads_declared: bool,
    ) -> VerificationReport:
        if self.interference_checker is not None:
            self._emit("interference_check", "Rechecking exact external interference")
            verdict = self.interference_checker()
            if verdict is not None:
                report = enforce_honest_verdict(
                    merge_external_interference(report, verdict),
                    loads_declared=loads_declared,
                )
                if verdict.kernel_identity.authoritative or verdict.status == "FAIL":
                    self._last_interference_status = verdict.status
                else:
                    self._last_interference_status = "UNKNOWN"
        if self.visual_reviewer is not None:
            self._emit("visual_review", "Reviewing offscreen semantic masks")
            visual = self.visual_reviewer(report)
            if visual is not None:
                self._last_visual = visual
                report = attach_visual_assist(report, visual)
        return report

    def _emit(self, stage: str, message: str, **extra: Any) -> None:
        if self.progress_callback is None:
            return
        event: dict[str, Any] = {"event": "progress", "stage": stage, "message": message}
        event.update(extra)
        self.progress_callback(event)

    def _annotate(self, result: CreationResult | ChangePreviewPayload) -> CreationResult | ChangePreviewPayload:
        if self._last_visual is not None:
            visual = self._last_visual.to_dict()
            visual["advisory_only"] = True
            visual["geometric_override"] = False
            result.visual_review = visual
        result.interference_status = self._last_interference_status
        if self.context_record is not None:
            result.untrusted_asset_labels = [
                label.to_dict() for label in self.context_record.untrusted_asset_labels
            ]
        return result

    def _verify(self, contract: DesignContract) -> VerificationReport:
        payload = self.client.verify()
        return enforce_honest_verdict(
            report_from_verify_payload(payload),
            loads_declared=bool(contract.loads),
        )

    def _verify_loads(
        self,
        summary_payload: Mapping[str, Any],
        *,
        loads_declared: bool = False,
    ) -> VerificationReport:
        payload = self.client.verify()
        report = report_from_verify_payload(payload)
        declared = (
            loads_declared
            or _summary_declares_loads(summary_payload)
            or _report_declares_loads(report)
        )
        return enforce_honest_verdict(report, loads_declared=declared)

    def _initial_revision(self) -> int:
        try:
            return self.client.session_status().revision
        except RuntimeToolError as exc:
            if exc.code == "E_IO":
                return 0
            raise

    def _apply_with_revision_retry(
        self,
        commands: Sequence[Mapping[str, Any]],
        revision: int | None,
        branch: str,
    ) -> tuple[ApplyOutcome, int | None]:
        try:
            outcome = self.client.apply_commands(
                commands,
                expected_revision=revision,
                branch=branch,
            )
            return outcome, outcome.revision
        except RuntimeToolError as exc:
            if exc.code != "E409_REVISION_CONFLICT":
                raise
            refreshed = exc.actual_revision
            if refreshed is None or refreshed == revision:
                raise
            outcome = self.client.apply_commands(
                commands,
                expected_revision=refreshed,
                branch=branch,
            )
            return outcome, outcome.revision


def _design_contract_command(contract: DesignContract) -> dict[str, Any]:
    body: dict[str, Any] = {"part_name": contract.part_name}
    summary = contract.summary or contract.notes
    if summary:
        body["summary"] = summary
    material = contract.material_name.strip()
    if material:
        body["material"] = material
    if contract.manufacturing_method:
        body["manufacturing_method"] = contract.manufacturing_method
    return {"command": "set_design_contract", "args": {"contract": body}}


def _upsert_length(name: str, millimeters: float) -> dict[str, Any]:
    return {
        "command": "upsert_parameter",
        "args": {
            "name": name,
            "ir_type": "length",
            "value": length_mm_value(millimeters),
        },
    }


def _numeric_params(feature_params: Mapping[str, Any]) -> dict[str, float]:
    found: dict[str, float] = {}
    for key, value in feature_params.items():
        if _is_number(value):
            found[str(key)] = float(value)
    return found


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _json_copy(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _json_copy(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_json_copy(item) for item in value]
    return value


def _is_recoverable_apply_error(exc: RuntimeToolError) -> bool:
    code = (exc.code or "").upper()
    if code in {"E409_REVISION_CONFLICT", "E_IO", "E_TIMEOUT", "E_PATH_ESCAPE", "E_RUNTIME"}:
        return False
    return code.startswith(("E1", "E2", "E3", "E400", "E404", "E422"))


def _diagnostic_from_runtime_error(exc: RuntimeToolError) -> dict[str, Any]:
    hints: dict[str, Any] = {}
    if isinstance(exc.payload, Mapping):
        raw_hints = exc.payload.get("recovery_hints") or exc.payload.get("recovery_hint")
        if isinstance(raw_hints, Mapping):
            hints.update(raw_hints)
    hints.update(_hints_in_message(str(exc)))
    node_id = hints.get("node_id")
    if not isinstance(node_id, str) and isinstance(exc.payload, Mapping):
        raw_node = exc.payload.get("node_id")
        if isinstance(raw_node, str):
            node_id = raw_node
    return {
        "code": exc.code,
        "layer": "L1_geometry_topology",
        "message": str(exc),
        "node_id": node_id if isinstance(node_id, str) else None,
        "recovery_hints": hints,
    }


def _hints_in_message(message: str) -> dict[str, Any]:
    marker = "recovery_hints="
    index = message.find(marker)
    if index < 0:
        return {}
    raw = message[index + len(marker):].strip()
    if not raw.startswith("{") and not raw.startswith("["):
        return {}
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {}
    return dict(parsed) if isinstance(parsed, dict) else {}


def _has_fea_solver(checks: Sequence[VerificationCheck]) -> bool:
    for check in checks:
        if check.layer != "L3_manufacturability_physics" or check.status != "PASS":
            continue
        for key in _SOLVER_KEYS:
            value = check.evidence.get(key)
            if isinstance(value, str) and value.strip().upper() not in _ABSENT_SOLVER:
                return True
    return False


def _branch_component(raw: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9._-]+", "_", raw.strip()).strip("._")
    return cleaned or "session"


def _outline(summary_payload: Mapping[str, Any]) -> list[dict[str, Any]]:
    summary = summary_payload.get("summary")
    source = summary if isinstance(summary, Mapping) else summary_payload
    outline = source.get("feature_outline") if isinstance(source, Mapping) else None
    if not isinstance(outline, list):
        return []
    return [dict(entry) for entry in outline if isinstance(entry, Mapping)]


def _outline_ids(summary_payload: Mapping[str, Any]) -> set[str]:
    return {str(entry["node_id"]) for entry in _outline(summary_payload) if entry.get("node_id")}


def _parameter_entries(summary_payload: Mapping[str, Any]) -> list[dict[str, Any]]:
    summary = summary_payload.get("summary")
    source = summary if isinstance(summary, Mapping) else summary_payload
    raw = source.get("parameters") if isinstance(source, Mapping) else None
    if not isinstance(raw, list):
        return []
    return [dict(entry) for entry in raw if isinstance(entry, Mapping)]


def _parameter_names(summary_payload: Mapping[str, Any]) -> set[str]:
    names: set[str] = set()
    for entry in _parameter_entries(summary_payload):
        name = entry.get("name")
        if isinstance(name, str) and name:
            names.add(name)
    return names


def _merkle_map(summary_payload: Mapping[str, Any]) -> dict[str, str]:
    summary = summary_payload.get("summary")
    source = summary if isinstance(summary, Mapping) else summary_payload
    found: dict[str, str] = {}
    if isinstance(source, Mapping):
        explicit = source.get("node_merkle")
        if isinstance(explicit, Mapping):
            for key, value in explicit.items():
                if isinstance(value, str) and value:
                    found[str(key)] = value
    for entry in _outline(summary_payload):
        digest = entry.get("merkle_hash")
        node_id = entry.get("node_id")
        if isinstance(node_id, str) and isinstance(digest, str) and digest:
            found[node_id] = digest
    return found


def _quantity_mm(value: Any) -> float | None:
    if not isinstance(value, Mapping):
        return None
    if value.get("type") == "quantity" and isinstance(value.get("value"), Mapping):
        inner = value["value"]
        if inner.get("dimension") == "length" and _is_number(inner.get("value")):
            return float(inner["value"])
    if value.get("dimension") == "length" and _is_number(value.get("value")):
        return float(value["value"])
    return None


def _feature_of(node_id: str, context: Mapping[str, Any]) -> str:
    for entry in context.get("feature_outline") or []:
        if isinstance(entry, Mapping) and str(entry.get("node_id")) == node_id:
            feature = entry.get("feature")
            if isinstance(feature, str):
                return feature
    graph = context.get("graph")
    if isinstance(graph, Mapping):
        nodes = graph.get("nodes")
        if isinstance(nodes, Mapping) and isinstance(nodes.get(node_id), Mapping):
            kind = nodes[node_id].get("kind")
            if isinstance(kind, Mapping) and isinstance(kind.get("kind"), str):
                return kind["kind"]
    return ""


def _graph_node_ids(graph: Mapping[str, Any] | None) -> list[str]:
    if not isinstance(graph, Mapping):
        return []
    nodes = graph.get("nodes")
    if not isinstance(nodes, Mapping):
        return []
    return [str(node_id) for node_id in nodes]


def _input_node_refs(graph: Mapping[str, Any] | None, node_id: str) -> set[str]:
    if not isinstance(graph, Mapping):
        return set()
    nodes = graph.get("nodes")
    if not isinstance(nodes, Mapping) or not isinstance(nodes.get(node_id), Mapping):
        return set()
    found: set[str] = set()

    def walk(value: Any) -> None:
        if isinstance(value, Mapping):
            if value.get("type") == "node_ref" and isinstance(value.get("value"), Mapping):
                ref = value["value"].get("node_id")
                if isinstance(ref, str):
                    found.add(ref)
            for item in value.values():
                walk(item)
        elif isinstance(value, list):
            for item in value:
                walk(item)

    walk(nodes[node_id].get("inputs"))
    return found


def _editable_ids(
    *,
    focus: str,
    instruction: str,
    neighborhood: set[str],
    context: Mapping[str, Any],
) -> set[str]:
    allowed: set[str] = set()
    if focus in neighborhood or not neighborhood:
        allowed.add(focus)
    focus_feature = _feature_of(focus, context)
    wants_holes = _HOLE_INTENT_RE.search(instruction) is not None
    if focus_feature in _HOLE_FEATURES | {"fillet", "chamfer", "shell", "draft"}:
        for solid in _input_node_refs(context.get("graph"), focus):
            if solid in neighborhood and _feature_of(solid, context) in _BODY_FEATURES | {""}:
                allowed.add(solid)
    if wants_holes:
        hole_ids = [
            node_id
            for node_id in neighborhood
            if _feature_of(node_id, context) in _HOLE_FEATURES
        ]
        if focus_feature in _HOLE_FEATURES:
            allowed.add(focus)
        else:
            allowed.update(hole_ids)
        for hole_id in list(allowed):
            if _feature_of(hole_id, context) in _HOLE_FEATURES:
                for solid in _input_node_refs(context.get("graph"), hole_id):
                    if solid in neighborhood and _feature_of(solid, context) in _BODY_FEATURES | {""}:
                        allowed.add(solid)
    if focus_feature in _BODY_FEATURES or focus_feature == "":
        allowed.add(focus)
    return {node_id for node_id in allowed if node_id in neighborhood or node_id == focus}


def _allowed_parameters(context_parts: Mapping[str, Any], explanation: Mapping[str, Any]) -> set[str]:
    names: set[str] = set()
    graph = context_parts.get("graph")
    if isinstance(graph, Mapping) and isinstance(graph.get("parameters"), Mapping):
        names.update(str(name) for name in graph["parameters"])
    raw_explanation = explanation.get("explanation")
    if isinstance(raw_explanation, Mapping):
        chain = raw_explanation.get("parameter_chain")
        if isinstance(chain, list):
            for link in chain:
                if isinstance(link, Mapping) and isinstance(link.get("name"), str):
                    names.add(link["name"])
    return names


def _assemble_context(
    *,
    target: str,
    focus: str,
    summary_payload: Mapping[str, Any],
    explanation: Mapping[str, Any],
    sliced: Mapping[str, Any],
    instruction: str,
) -> dict[str, Any]:
    graph = sliced.get("graph") if isinstance(sliced.get("graph"), Mapping) else {}
    neighborhood = set(sliced.get("neighborhood_node_ids") or _graph_node_ids(graph))
    if focus:
        neighborhood.add(focus)
    outline = [
        entry
        for entry in _outline(summary_payload)
        if str(entry.get("node_id")) in neighborhood
    ]
    # Keep feature labels for nodes the summary knows, even when the slice is the source of ids.
    known = {str(entry.get("node_id")): entry for entry in _outline(summary_payload)}
    for node_id in sorted(neighborhood):
        if node_id not in {str(entry.get("node_id")) for entry in outline} and node_id in known:
            outline.append(known[node_id])
    context: dict[str, Any] = {
        "target": target,
        "focus_node": focus,
        "graph": graph,
        "neighborhood_node_ids": sorted(neighborhood),
        "feature_outline": outline,
        "parameters": _parameter_entries(summary_payload),
        "explanation": explanation.get("explanation"),
        "kcl_view": sliced.get("kcl_view") or "",
        "summary": summary_payload.get("summary", summary_payload),
    }
    context["allowed_parameters"] = sorted(
        _allowed_parameters(context, explanation) & _parameter_names(summary_payload)
        or _allowed_parameters(context, explanation)
    )
    context["editable_node_ids"] = sorted(
        _editable_ids(
            focus=focus,
            instruction=instruction,
            neighborhood=neighborhood,
            context=context,
        )
    )
    return context


def _thickness_parameter(context: Mapping[str, Any]) -> tuple[str | None, float | None]:
    allowed = set(context.get("allowed_parameters") or [])
    ranked: list[tuple[int, str, float]] = []
    for entry in context.get("parameters") or []:
        if not isinstance(entry, Mapping):
            continue
        name = entry.get("name")
        if not isinstance(name, str) or name not in allowed:
            continue
        millimeters = _quantity_mm(entry.get("value"))
        if millimeters is None:
            continue
        score = 0
        lowered = name.lower()
        if any(token in lowered for token in ("thick", "plate", "base", "height")):
            score += 2
        ranked.append((score, name, millimeters))
    chain = []
    explanation = context.get("explanation")
    if isinstance(explanation, Mapping) and isinstance(explanation.get("parameter_chain"), list):
        chain = explanation["parameter_chain"]
    for link in chain:
        if not isinstance(link, Mapping):
            continue
        name = link.get("name")
        if not isinstance(name, str) or name not in allowed:
            continue
        millimeters = _quantity_mm(link.get("value"))
        if millimeters is None:
            continue
        lowered = name.lower()
        score = 1
        if any(token in lowered for token in ("thick", "plate", "base", "height")):
            score += 2
        ranked.append((score, name, millimeters))
    if not ranked:
        return None, None
    ranked.sort(key=lambda item: (-item[0], item[1]))
    _score, name, millimeters = ranked[0]
    return name, millimeters


def _suggested_override(diagnostics: Sequence[Mapping[str, Any]], param_name: str | None) -> float | None:
    if param_name is None:
        return None
    for diagnostic in diagnostics:
        hints = diagnostic.get("recovery_hints")
        if not isinstance(hints, Mapping):
            continue
        if hints.get("parameter") == param_name and _is_number(hints.get("suggested_mm")):
            return float(hints["suggested_mm"])
    return None


def _body_solid(context: Mapping[str, Any], target: str, allowed: set[str]) -> str | None:
    focus = focus_node_from_target(target)
    if _feature_of(focus, context) in _BODY_FEATURES or (
        focus in allowed and _feature_of(focus, context) not in _HOLE_FEATURES
    ):
        return focus
    for node_id in sorted(allowed):
        if _feature_of(node_id, context) in _BODY_FEATURES:
            return node_id
    return focus if focus in allowed else None


_TOPO_SELECTOR_PREFIXES = frozenset({
    "tag",
    "normal",
    "bbox",
    "radius",
    "area",
    "axis",
    "centroid",
    "plane",
    "cylinder",
    "kind",
    "surface",
})


def _extract_topo_ref_node(value: str, *, allow_bare: bool = False) -> str | None:
    text = value.strip()
    if not text:
        return None
    if "::" in text:
        head = text.split("::", 1)[0].strip()
        if not head or head.lower() in _TOPO_SELECTOR_PREFIXES:
            return None
        return head
    if ":" in text:
        parts = [part.strip() for part in text.split(":")]
        head_lower = parts[0].lower()
        if head_lower in _TOPO_SELECTOR_PREFIXES:
            return None
        if len(parts) >= 3 and head_lower in _TOPO_KINDS:
            return parts[1] or None
        if parts[0] and head_lower not in _TOPO_KINDS:
            return parts[0]
        return None
    if allow_bare:
        return text
    return None


def _iter_node_refs(value: Any, *, skip_top_node_id: bool = False) -> list[str]:
    found: list[str] = []

    def walk(item: Any, key: str | None = None, depth: int = 0) -> None:
        if isinstance(item, Mapping):
            is_face_ref = item.get("kind") == "face_ref"
            for child_key, child in item.items():
                if skip_top_node_id and depth == 0 and child_key == "node_id":
                    continue
                if is_face_ref and child_key == "face" and isinstance(child, str):
                    node_id = _extract_topo_ref_node(child, allow_bare=True)
                    if node_id:
                        found.append(node_id)
                elif child_key in _NODE_KEYS and isinstance(child, str):
                    stripped = child.strip()
                    if stripped:
                        found.append(stripped)
                elif child_key in _NODE_LIST_KEYS and isinstance(child, list):
                    for entry in child:
                        if isinstance(entry, str):
                            stripped = entry.strip()
                            if stripped:
                                found.append(stripped)
                        else:
                            walk(entry, child_key, depth + 1)
                elif child_key in _TOPO_REF_KEYS and isinstance(child, str):
                    node_id = _extract_topo_ref_node(child, allow_bare=False)
                    if node_id:
                        found.append(node_id)
                elif child_key in _TOPO_REF_LIST_KEYS and isinstance(child, list):
                    for entry in child:
                        if isinstance(entry, str):
                            node_id = _extract_topo_ref_node(entry, allow_bare=False)
                            if node_id:
                                found.append(node_id)
                        else:
                            walk(entry, child_key, depth + 1)
                else:
                    walk(child, child_key, depth + 1)
        elif isinstance(item, list):
            for entry in item:
                walk(entry, key, depth + 1)

    walk(value)
    return found


def _parameter_refs(command: Mapping[str, Any]) -> list[str]:
    if command.get("command") not in {"upsert_parameter", "remove_parameter"}:
        return []
    args = command.get("args")
    if not isinstance(args, Mapping):
        return []
    name = args.get("name")
    return [name] if isinstance(name, str) else []


def _referenced_existing(commands: Sequence[Mapping[str, Any]], document_ids: set[str]) -> set[str]:
    found: set[str] = set()
    for command in commands:
        for node_id in _iter_node_refs(command.get("args")):
            if node_id in document_ids:
                found.add(node_id)
    return found


def _context_string_set(value: Any) -> set[str]:
    if not isinstance(value, (list, tuple, set, frozenset)):
        return set()
    return {str(item).strip() for item in value if str(item).strip()}


def _context_known_nodes(context: Mapping[str, Any]) -> set[str]:
    found = _context_string_set(context.get("neighborhood_node_ids"))
    found.update(_context_string_set(context.get("editable_node_ids")))
    found.update(_context_string_set(context.get("read_only_node_ids")))
    focus = context.get("focus_node")
    if isinstance(focus, str) and focus.strip():
        found.add(focus.strip())
    graph = context.get("graph")
    if isinstance(graph, Mapping) and isinstance(graph.get("nodes"), Mapping):
        found.update(str(node_id).strip() for node_id in graph["nodes"] if str(node_id).strip())
    for source in (context, context.get("summary") if isinstance(context.get("summary"), Mapping) else {}):
        if not isinstance(source, Mapping):
            continue
        outline = source.get("feature_outline")
        if isinstance(outline, list):
            for entry in outline:
                if isinstance(entry, Mapping) and entry.get("node_id"):
                    found.add(str(entry["node_id"]).strip())
        inner = source.get("summary")
        if isinstance(inner, Mapping) and isinstance(inner.get("feature_outline"), list):
            for entry in inner["feature_outline"]:
                if isinstance(entry, Mapping) and entry.get("node_id"):
                    found.add(str(entry["node_id"]).strip())
    return {node_id for node_id in found if node_id}


def _validate_local_edit_bounds(
    commands: Sequence[Mapping[str, Any]],
    *,
    allowed_node_ids: set[str],
    allowed_parameters: set[str],
    known_node_ids: set[str] | None = None,
) -> None:
    """Fail-closed local edit guard shared by both the planner and the agent."""
    editable = {node_id for node_id in allowed_node_ids if node_id}
    params = {name for name in allowed_parameters if name}
    known = {node_id for node_id in (known_node_ids or set()) if node_id}
    created_in_batch: set[str] = set()

    for index, command in enumerate(commands):
        cmd_name = command.get("command") if isinstance(command, Mapping) else None
        args = command.get("args") if isinstance(command, Mapping) else None

        if cmd_name in _FORBIDDEN_LOCAL_COMMANDS:
            raise SemanticCommandError(
                f"commands[{index}] uses {cmd_name!r}, which modifies the global design contract "
                "and is forbidden during local edits.",
                code="E400_INVALID_COMMAND",
                recovery_hints={
                    "editable_node_ids": sorted(editable),
                    "allowed_parameters": sorted(params),
                    "rejected_nodes": [],
                    "rejected_parameters": [],
                    "repair": (
                        "remove global contract commands; local edits may touch only "
                        "editable_node_ids and allowed_parameters"
                    ),
                },
            )

        for name in _parameter_refs(command):
            if name not in params:
                raise SemanticCommandError(
                    f"commands[{index}] rewrites parameter {name!r}, which is outside the sliced "
                    f"neighborhood and allowed_parameters {sorted(params)}",
                    code="E400_INVALID_COMMAND",
                    recovery_hints={
                        "editable_node_ids": sorted(editable),
                        "allowed_parameters": sorted(params),
                        "rejected_nodes": [],
                        "rejected_parameters": [name],
                        "repair": "rewrite the batch so parameters stay inside allowed_parameters",
                    },
                )

        allowed_targets = editable | created_in_batch

        if cmd_name in _CREATING_COMMANDS:
            created_id = args.get("node_id") if isinstance(args, Mapping) else None
            created_clean = created_id.strip() if isinstance(created_id, str) and created_id.strip() else None
            if created_clean is not None and created_clean in known and created_clean not in allowed_targets:
                raise SemanticCommandError(
                    f"commands[{index}] assigns node_id {created_clean!r}, which collides with a "
                    f"read-only node outside editable_node_ids {sorted(editable)}",
                    code="E400_INVALID_COMMAND",
                    recovery_hints={
                        "editable_node_ids": sorted(editable),
                        "allowed_parameters": sorted(params),
                        "rejected_nodes": [created_clean],
                        "rejected_parameters": [],
                        "repair": (
                            "use a fresh node_id for new features or edit only nodes inside "
                            "editable_node_ids"
                        ),
                    },
                )
            for node_id in _iter_node_refs(args, skip_top_node_id=True):
                if node_id not in allowed_targets:
                    raise SemanticCommandError(
                        f"commands[{index}] references {node_id!r}, which is outside "
                        f"editable_node_ids {sorted(editable)} and the point-and-prompt edit set. "
                        "Untouched features must stay unchanged.",
                        code="E400_INVALID_COMMAND",
                        recovery_hints={
                            "editable_node_ids": sorted(editable),
                            "allowed_parameters": sorted(params),
                            "rejected_nodes": [node_id],
                            "rejected_parameters": [],
                            "repair": (
                                "rewrite the batch so referenced nodes stay inside "
                                "editable_node_ids or nodes created earlier in this batch"
                            ),
                        },
                    )
            if created_clean is not None:
                created_in_batch.add(created_clean)
        else:
            for node_id in _iter_node_refs(args, skip_top_node_id=False):
                if node_id not in allowed_targets:
                    raise SemanticCommandError(
                        f"commands[{index}] references {node_id!r}, which is outside "
                        f"editable_node_ids {sorted(editable)} and the point-and-prompt edit set. "
                        "Untouched features must stay unchanged.",
                        code="E400_INVALID_COMMAND",
                        recovery_hints={
                            "editable_node_ids": sorted(editable),
                            "allowed_parameters": sorted(params),
                            "rejected_nodes": [node_id],
                            "rejected_parameters": [],
                            "repair": (
                                "rewrite the batch so modified or referenced nodes stay inside "
                                "editable_node_ids or nodes created earlier in this batch"
                            ),
                        },
                    )


def _assert_local_batch(
    commands: Sequence[Mapping[str, Any]],
    context: Mapping[str, Any] | None = None,
    *,
    document_node_ids: set[str] | None = None,
    allowed_node_ids: set[str] | None = None,
    document_parameters: set[str] | None = None,
    allowed_parameters: set[str] | None = None,
) -> None:
    del document_parameters
    if context is not None:
        if allowed_node_ids is None:
            allowed_node_ids = _context_string_set(context.get("editable_node_ids"))
        if allowed_parameters is None:
            allowed_parameters = _context_string_set(context.get("allowed_parameters"))
        if document_node_ids is None:
            document_node_ids = _context_known_nodes(context)
    _validate_local_edit_bounds(
        commands,
        allowed_node_ids=set(allowed_node_ids or ()),
        allowed_parameters=set(allowed_parameters or ()),
        known_node_ids=set(document_node_ids or ()),
    )


def _summary_declares_loads(summary_payload: Mapping[str, Any]) -> bool:
    summary = summary_payload.get("summary")
    source = summary if isinstance(summary, Mapping) else summary_payload
    contract = source.get("design_contract") if isinstance(source, Mapping) else None
    if isinstance(contract, Mapping):
        loads = contract.get("loads")
        if isinstance(loads, list) and loads:
            return True
        attributes = contract.get("attributes")
        if isinstance(attributes, Mapping) and attributes.get("loads"):
            return True
    return False


def _report_declares_loads(report: VerificationReport) -> bool:
    for check in report.checks:
        if check.evidence.get("loads_declared") is True and not _has_fea_solver([check]):
            return True
    return False


def _preview_from_apply(
    *,
    instruction: str,
    commands: list[dict[str, Any]],
    outcome: ApplyOutcome | None,
    report: VerificationReport | None,
    branch: str,
    expected_revision: int | None,
    before_ids: set[str],
    before_merkle: dict[str, str],
    after_summary: Mapping[str, Any],
    referenced: set[str],
    repairs: int,
) -> ChangePreviewPayload:
    after_ids = _outline_ids(after_summary)
    after_merkle = _merkle_map(after_summary)
    modified = set(outcome.modified_nodes) if outcome is not None else set()
    deleted = set(outcome.deleted_nodes) if outcome is not None else set()
    untouched = before_ids - referenced
    drifted: list[str] = []
    for node_id in sorted(untouched):
        if node_id in modified or node_id in deleted:
            drifted.append(node_id)
            continue
        if after_ids and node_id not in after_ids:
            drifted.append(node_id)
            continue
        if before_merkle and after_merkle and node_id in before_merkle:
            if after_merkle.get(node_id) != before_merkle[node_id]:
                drifted.append(node_id)
    preserved = sorted(untouched - set(drifted))
    verdict: TriVerdict
    if report is None or report.overall_status is None:
        verdict = "UNKNOWN"
    else:
        verdict = report.overall_status
    if drifted:
        verdict = "FAIL"
    diff = {
        "commands": commands,
        "created_nodes": [] if outcome is None else list(outcome.created_nodes),
        "modified_nodes": [] if outcome is None else list(outcome.modified_nodes),
        "deleted_nodes": [] if outcome is None else list(outcome.deleted_nodes),
        "summary": {} if outcome is None else outcome.summary,
        "volume_mm3": None if outcome is None else outcome.solids.get("total_volume_mm3"),
        "branch": branch,
    }
    titles = [command["command"] for command in commands]
    plan_summary = f"{instruction.strip()} -> {', '.join(titles)}" if titles else instruction.strip()
    return ChangePreviewPayload(
        plan_summary=plan_summary,
        diff=diff,
        verdict=verdict,
        report=report,
        branch=branch,
        revision=None if outcome is None else outcome.revision,
        expected_revision=expected_revision,
        commands=commands,
        untouched_preserved=not drifted,
        preserved_node_ids=preserved,
        drifted_node_ids=drifted,
        repairs=repairs,
    )


def _failed_preview(
    *,
    instruction: str,
    commands: list[dict[str, Any]],
    branch: str,
    revision: int | None,
    expected_revision: int | None,
    report: VerificationReport | None,
    repairs: int,
    code: str,
    message: str,
    verdict: TriVerdict,
) -> ChangePreviewPayload:
    if code == "E409_REVISION_CONFLICT":
        verdict = "UNKNOWN"
    elif report is not None and report.overall_status == "FAIL":
        verdict = "FAIL"
    if verdict == "PASS":
        verdict = "UNKNOWN"
    return ChangePreviewPayload(
        plan_summary=instruction.strip(),
        diff={"commands": commands, "branch": branch},
        verdict=verdict,
        report=report,
        branch=branch,
        revision=revision,
        expected_revision=expected_revision,
        commands=commands,
        untouched_preserved=False,
        repairs=repairs,
        error_code=code,
        error_message=message,
    )

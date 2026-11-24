"""LLM-backed ``SemanticPlanner``.

The model proposes a ``SemanticCommand`` batch. This module validates that
batch and, for a local edit, rejects nodes or parameters outside the slice.
A rejected completion is sent back once with the failure diagnostic. It is
never replaced by a heuristic template.
"""

from __future__ import annotations

import json
import re
from collections.abc import Mapping, Sequence
from typing import Any

from .llm import LLMClient
from .semantic_agent import (
    SEMANTIC_COMMAND_NAMES,
    SemanticCommandError,
    _REQUIRED_ARGS,
    _validate_local_edit_bounds,
    validate_semantic_batch,
)
from .stdlib_digest import build_stdlib_digest

_FENCE_RE = re.compile(r"```(?:json)?\s*(.*?)```", re.DOTALL | re.IGNORECASE)
_GRAPH_LIMIT = 6000
_KCL_LIMIT = 1500

_SYSTEM_PROMPT = (
    "You are the semantic planner for an AI-native CAD kernel. "
    "Return ONLY a JSON array of SemanticCommand objects "
    '{"command": "<name>", "args": {...}}. '
    "No markdown and no prose. "
    "Copy fastener, counterbore, countersink, bearing, ISO 286, and material "
    "numbers from the stdlib digest verbatim. Never calculate a missing size. "
    "If the digest marks a size absent, do not guess its dimensions. "
    "When diagnostics include recovery_hints, apply them and return a corrected batch."
)

_LOCAL_RULES = (
    "Local edit rules: modify only editable_node_ids and allowed_parameters. "
    "Do not reference read_only_node_ids or any other existing node. "
    "New node ids are allowed only for features created in this batch. "
    "E201_AMBIGUOUS_TOPOLOGY: follow suggested_disambiguator "
    "(normal, bbox, radius, or tag). Do not take the first candidate. "
    "GeometryBoundExceeded: set the named parameter to limit_mm when it is present. "
    "A recovery hint suggested_mm replaces that parameter value."
)


class LLMPlanError(SemanticCommandError):
    """The model batch was rejected. No heuristic template was substituted."""

    def __init__(
        self,
        message: str,
        *,
        code: str = "E400_INVALID_COMMAND",
        diagnostics: Sequence[Mapping[str, Any]] | None = None,
        recovery_hints: Mapping[str, Any] | None = None,
    ) -> None:
        super().__init__(message, code=code)
        self.diagnostics = [dict(item) for item in diagnostics] if diagnostics else []
        if recovery_hints is not None:
            self.recovery_hints: dict[str, Any] = dict(recovery_hints)
        elif self.diagnostics and isinstance(self.diagnostics[-1].get("recovery_hints"), Mapping):
            self.recovery_hints = dict(self.diagnostics[-1]["recovery_hints"])
        else:
            self.recovery_hints = {}


class LLMSemanticPlanner:
    """``SemanticPlanner`` that calls ``LLMClient.complete`` and validates the batch."""

    def __init__(self, client: LLMClient, *, max_model_tries: int = 2) -> None:
        complete = getattr(client, "complete", None)
        if not callable(complete):
            raise TypeError("LLMSemanticPlanner requires an LLMClient with complete()")
        if isinstance(max_model_tries, bool) or not isinstance(max_model_tries, int) or max_model_tries < 1:
            raise ValueError("max_model_tries must be an integer >= 1")
        self.client = client
        self.max_model_tries = max_model_tries

    def plan_creation(
        self,
        contract: Any,
        intent: str,
        *,
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        """Plan a 0→1 batch from the contract, the stdlib digest, and prior failures."""
        digest = build_stdlib_digest(intent or "", contract)
        return self._plan(
            mode="create",
            attempt=attempt,
            diagnostics=diagnostics,
            digest=digest,
            body={
                "intent": intent or "",
                "design_contract": _contract_payload(contract),
            },
        )

    def plan_local_edit(
        self,
        *,
        target: str,
        instruction: str,
        context: Mapping[str, Any],
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        """Plan a 1→N batch that may touch only the sliced edit set."""
        if not isinstance(context, Mapping):
            raise SemanticCommandError("plan_local_edit context must be a slice_context mapping")
        digest = build_stdlib_digest(_local_digest_text(target, instruction, context))
        return self._plan(
            mode="local_edit",
            attempt=attempt,
            diagnostics=diagnostics,
            digest=digest,
            body=_local_prompt_body(target, instruction, context),
            check_bounds=True,
            bounds_context=context,
        )

    def _plan(
        self,
        *,
        mode: str,
        attempt: int,
        diagnostics: Sequence[Mapping[str, Any]] | None,
        digest: str,
        body: Mapping[str, Any],
        check_bounds: bool = False,
        bounds_context: Mapping[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        pending = _diagnostics_payload(diagnostics)
        caller_count = len(pending)
        last_error: SemanticCommandError | None = None
        for model_try in range(self.max_model_tries):
            user_prompt = _user_prompt(
                mode=mode,
                attempt=attempt,
                model_try=model_try,
                body=body,
                digest=digest,
                diagnostics=pending,
            )
            raw = self.client.complete(_SYSTEM_PROMPT, user_prompt)
            if not isinstance(raw, str):
                last_error = LLMPlanError("LLMClient.complete must return text")
                pending.append(_failure_diagnostic(last_error))
                continue
            try:
                commands = validate_semantic_batch(_parse_command_list(raw))
                if check_bounds:
                    _assert_edit_bounds(commands, bounds_context or {})
            except SemanticCommandError as exc:
                last_error = exc
                pending.append(_failure_diagnostic(exc))
                continue
            return commands
        assert last_error is not None
        raise LLMPlanError(
            "LLMSemanticPlanner rejected the model output after "
            f"{self.max_model_tries} attempts: {last_error}. "
            "A heuristic template was not substituted.",
            code=getattr(last_error, "code", "E400_INVALID_COMMAND"),
            diagnostics=pending[caller_count:],
            recovery_hints=getattr(last_error, "recovery_hints", None),
        )


def semantic_command_contract() -> str:
    """Compact required-arg contract taken from the semantic command schema."""
    lines = [
        'Each item is {"command": <name>, "args": {...}}. Return a JSON array.',
        "Required args:",
    ]
    for name in sorted(SEMANTIC_COMMAND_NAMES):
        required = _REQUIRED_ARGS.get(name, ())
        lines.append(f"{name}: {', '.join(required) if required else '(none)'}")
    lines.append(
        'upsert_parameter args.value uses IrValue::Quantity: '
        '{"type":"quantity","value":{"dimension":"length","value":80,"unit":"mm"}} '
        '(or "dimension":"angle","unit":"deg").'
    )
    lines.append(
        'Feature dimension fields (width, depth, height, radius, distance, etc.) use DimInput: '
        '{"source":"param","param_id":"name"} or '
        '{"source":"literal","dimension":"length","value":80,"unit":"mm"}.'
    )
    lines.append(
        "Change an existing literal box or hole with set_feature_input "
        '{node_id, input: "width"|"depth"|"height"|"diameter"|"origin_mm"|"axis", '
        "length_mm or vector_mm}. Do not delete and recreate the node. "
        "If the input is bound to a parameter, upsert_parameter that parameter instead."
    )
    lines.append(
        "Standard holes use create_hole definition "
        '{"hole":"standard","standard":"iso4762_counterbore|iso10642_countersink|'
        'metric_tap_drill|metric_clearance","designation":"M6",'
        '"depth":{"kind":"through_all"}}.'
    )
    return "\n".join(lines)


def _user_prompt(
    *,
    mode: str,
    attempt: int,
    model_try: int,
    body: Mapping[str, Any],
    digest: str,
    diagnostics: Sequence[Mapping[str, Any]],
) -> str:
    sections = [
        f"mode: {mode}",
        f"attempt: {attempt}",
        f"model_try: {model_try}",
        "request:",
        json.dumps(body, ensure_ascii=False, default=str),
        "stdlib_digest:",
        digest,
        "semantic_command_contract:",
        semantic_command_contract(),
        "diagnostics:",
        json.dumps(list(diagnostics), ensure_ascii=False, default=str),
    ]
    if mode == "local_edit":
        sections.append(_LOCAL_RULES)
    sections.append("Return the JSON array only.")
    return "\n".join(sections)


def _local_prompt_body(target: str, instruction: str, context: Mapping[str, Any]) -> dict[str, Any]:
    editable = _string_set(context.get("editable_node_ids"))
    known = _known_node_ids(context)
    graph = context.get("graph") if isinstance(context.get("graph"), Mapping) else {}
    nodes = graph.get("nodes") if isinstance(graph, Mapping) and isinstance(graph.get("nodes"), Mapping) else {}
    return {
        "target": str(target or ""),
        "instruction": str(instruction or ""),
        "editable_node_ids": sorted(editable),
        "allowed_parameters": sorted(_string_set(context.get("allowed_parameters"))),
        "read_only_node_ids": sorted(known - editable),
        "neighborhood_node_ids": sorted(_string_set(context.get("neighborhood_node_ids"))),
        "feature_outline": list(context.get("feature_outline") or []),
        "parameters": list(context.get("parameters") or []),
        "explanation": context.get("explanation"),
        "kcl_view": str(context.get("kcl_view") or "")[:_KCL_LIMIT],
        "graph_nodes": _cap_json(dict(nodes)),
    }


def _local_digest_text(target: str, instruction: str, context: Mapping[str, Any]) -> str:
    parts = [str(target or ""), str(instruction or "")]
    for entry in context.get("feature_outline") or []:
        if isinstance(entry, Mapping):
            parts.append(str(entry.get("name") or ""))
            parts.append(str(entry.get("feature") or ""))
    explanation = context.get("explanation")
    if isinstance(explanation, str):
        parts.append(explanation)
    elif isinstance(explanation, Mapping):
        for key in ("summary", "text", "message"):
            value = explanation.get(key)
            if isinstance(value, str):
                parts.append(value)
    return "\n".join(part for part in parts if part)


def _contract_payload(contract: Any) -> dict[str, Any]:
    to_dict = getattr(contract, "to_dict", None)
    if callable(to_dict):
        payload = to_dict()
        if isinstance(payload, Mapping):
            return dict(payload)
    if isinstance(contract, Mapping):
        return dict(contract)
    raise SemanticCommandError("plan_creation requires a DesignContract")


def _parse_command_list(raw_text: str) -> list[Any]:
    text = raw_text.strip()
    if not text:
        raise LLMPlanError("LLM returned an empty semantic plan")
    fence = _FENCE_RE.search(text)
    if fence:
        text = fence.group(1).strip()
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError as exc:
        raise LLMPlanError(
            f"LLM semantic plan is not valid JSON ({exc.msg} at line {exc.lineno})"
        ) from exc
    if isinstance(parsed, Mapping) and isinstance(parsed.get("commands"), list):
        parsed = parsed["commands"]
    elif isinstance(parsed, Mapping) and isinstance(parsed.get("command"), str):
        parsed = [dict(parsed)]
    if isinstance(parsed, list):
        return parsed
    raise LLMPlanError("LLM semantic plan must be a JSON array of SemanticCommand objects")


def _assert_edit_bounds(commands: Sequence[Mapping[str, Any]], context: Mapping[str, Any]) -> None:
    editable = _string_set(context.get("editable_node_ids"))
    allowed_parameters = _string_set(context.get("allowed_parameters"))
    known_nodes = _known_node_ids(context)
    try:
        _validate_local_edit_bounds(
            commands,
            allowed_node_ids=editable,
            allowed_parameters=allowed_parameters,
            known_node_ids=known_nodes,
        )
    except SemanticCommandError as exc:
        raise LLMPlanError(
            str(exc),
            code=exc.code,
            recovery_hints=exc.recovery_hints,
        ) from exc


def _known_node_ids(context: Mapping[str, Any]) -> set[str]:
    found = _string_set(context.get("neighborhood_node_ids"))
    found.update(_string_set(context.get("editable_node_ids")))
    found.update(_string_set(context.get("read_only_node_ids")))
    focus = context.get("focus_node")
    if isinstance(focus, str) and focus.strip():
        found.add(focus.strip())
    graph = context.get("graph")
    if isinstance(graph, Mapping) and isinstance(graph.get("nodes"), Mapping):
        found.update(str(node_id) for node_id in graph["nodes"] if str(node_id).strip())
    for source in (context, context.get("summary") if isinstance(context.get("summary"), Mapping) else {}):
        if not isinstance(source, Mapping):
            continue
        outline = source.get("feature_outline")
        if isinstance(outline, list):
            for entry in outline:
                if isinstance(entry, Mapping) and entry.get("node_id"):
                    found.add(str(entry["node_id"]))
        inner = source.get("summary")
        if isinstance(inner, Mapping) and isinstance(inner.get("feature_outline"), list):
            for entry in inner["feature_outline"]:
                if isinstance(entry, Mapping) and entry.get("node_id"):
                    found.add(str(entry["node_id"]))
    return found


def _failure_diagnostic(exc: SemanticCommandError) -> dict[str, Any]:
    hints = getattr(exc, "recovery_hints", None)
    if not isinstance(hints, Mapping) or not hints:
        hints = {
            "repair": (
                "Return only a JSON array of {command, args} objects that satisfy "
                "the SemanticCommand schema."
            )
        }
    return {
        "code": getattr(exc, "code", "E400_INVALID_COMMAND"),
        "message": str(exc),
        "recovery_hints": dict(hints),
    }


def _diagnostics_payload(diagnostics: Sequence[Mapping[str, Any]] | None) -> list[dict[str, Any]]:
    if not diagnostics:
        return []
    payload: list[dict[str, Any]] = []
    for item in diagnostics:
        if isinstance(item, Mapping):
            payload.append(json.loads(json.dumps(dict(item), default=str)))
        else:
            payload.append({"message": str(item)})
    return payload


def _string_set(value: Any) -> set[str]:
    if not isinstance(value, (list, tuple, set)):
        return set()
    return {str(item) for item in value if str(item).strip()}


def _cap_json(value: Any) -> Any:
    text = json.dumps(value, ensure_ascii=False, default=str)
    if len(text) <= _GRAPH_LIMIT:
        return value
    return {"truncated": True, "preview": text[:_GRAPH_LIMIT]}

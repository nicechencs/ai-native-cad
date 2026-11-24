"""Fault-tolerant structured `DesignContract` extraction, normalization, and LLM repair."""

from __future__ import annotations

import copy
import json
import re
from dataclasses import dataclass, field
from typing import Any

from .contracts import (
    VALID_ANGLE_UNITS,
    VALID_FORCE_UNITS,
    VALID_INTERFACE_KINDS,
    VALID_KEEP_OUT_KINDS,
    VALID_LENGTH_UNITS,
    VALID_LOAD_KINDS,
    VALID_MANUFACTURING_METHODS,
    ContractValidationError,
    DesignContract,
)
from .llm import LLMClient

_FENCE_RE = re.compile(r"```(?P<lang>[a-zA-Z0-9_-]*)[ \t]*\r?\n(?P<body>.*?)```", re.DOTALL)
_NUMERIC_STR_RE = re.compile(r"^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$")
_INT_STR_RE = re.compile(r"^[+-]?\d+$")

_VALID_METHOD_LOOKUP: dict[str, str] = {
    m.lower(): m for m in VALID_MANUFACTURING_METHODS
} | {m.lower().replace("_", "-"): m for m in VALID_MANUFACTURING_METHODS}

REPAIR_SYSTEM_PROMPT = (
    "You are a strict JSON repair assistant for an AI-Native CAD DesignContract. "
    "Fix ONLY the reported schema/validation errors while preserving all valid engineering "
    "intent from the original request. Return ONLY a single valid JSON object with no markdown "
    "prose or explanation."
)

DESIGN_CONTRACT_SCHEMA_SUMMARY = (
    "Expected DesignContract JSON schema:\n"
    "{\n"
    '  "part_name": "<PascalCase or snake_case matching ^[A-Za-z_][A-Za-z0-9_]*$>",\n'
    '  "units": {"length": "mm|m|in", "angle": "deg|rad", "force": "N|kN"},\n'
    '  "material": "<non-empty material string, e.g. Al-6061-T6>",\n'
    f'  "manufacturing_method": "one of {list(VALID_MANUFACTURING_METHODS)}",\n'
    '  "keep_in_bbox_mm": {"min": [x_min, y_min, z_min], "max": [x_max, y_max, z_max]},  // min[i] <= max[i]\n'
    '  "keep_out_zones": [{"zone_id": "...", "kind": '
    f'"{ "|".join(VALID_KEEP_OUT_KINDS) }", "origin_mm": [x,y,z], "size_mm": [dx,dy,dz]}}],\n'
    '  "interfaces": [{"interface_id": "...", "kind": '
    f'"{ "|".join(VALID_INTERFACE_KINDS) }", "diameter_mm": 10.0, "count": 4, "preserve_exact_brep": true}}],\n'
    '  "loads": [{"load_id": "...", "kind": '
    f'"{ "|".join(VALID_LOAD_KINDS) }", "target_interface": "...", "vector": [fx,fy,fz], "min_safety_factor": 2.0}}]\n'
    "}"
)


@dataclass(slots=True)
class StructuredParseResult:
    """Validated `DesignContract` plus repair count and normalization warnings."""

    contract: DesignContract
    repair_attempts: int = 0
    warnings: list[str] = field(default_factory=list)


def _try_parse_dict(candidate: str) -> dict[str, Any] | None:
    text = candidate.strip()
    if not text:
        return None
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _iter_balanced_brace_slices(text: str) -> list[str]:
    """Find balanced `{ ... }` substrings while respecting JSON string literals and escapes."""
    slices: list[str] = []
    n = len(text)
    idx = 0

    while idx < n:
        start = text.find("{", idx)
        if start == -1:
            break

        depth = 0
        in_string = False
        escaped = False
        end = -1

        for pos in range(start, n):
            ch = text[pos]
            if in_string:
                if escaped:
                    escaped = False
                elif ch == "\\":
                    escaped = True
                elif ch == '"':
                    in_string = False
                continue

            if ch == '"':
                in_string = True
            elif ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    end = pos
                    break

        if end != -1:
            slices.append(text[start : end + 1])
            idx = end + 1
        else:
            # Unclosed outer brace; advance past this `{` to try inner objects
            idx = start + 1

    return slices


def extract_json_object(raw_text: str) -> dict[str, Any]:
    """Extract a JSON object from raw LLM output across fences, prose, and nested braces."""
    if not isinstance(raw_text, str) or not raw_text.strip():
        raise ContractValidationError(["LLM response is empty; expected a JSON object"])

    # Strategy 1: Direct JSON parse
    direct = _try_parse_dict(raw_text)
    if direct is not None:
        return direct

    # Strategy 2: Markdown code fences (prefer ```json blocks first)
    json_fences: list[str] = []
    other_fences: list[str] = []
    for match in _FENCE_RE.finditer(raw_text):
        lang = match.group("lang").strip().lower()
        body = match.group("body")
        if lang == "json":
            json_fences.append(body)
        else:
            other_fences.append(body)

    for block in (*json_fences, *other_fences):
        parsed = _try_parse_dict(block)
        if parsed is not None:
            return parsed
        for candidate in _iter_balanced_brace_slices(block):
            parsed = _try_parse_dict(candidate)
            if parsed is not None:
                return parsed

    # Strategy 3: Balanced curly-brace scan across full raw text
    for candidate in _iter_balanced_brace_slices(raw_text):
        parsed = _try_parse_dict(candidate)
        if parsed is not None:
            return parsed

    # Strategy 4: Fallback JSONDecoder.raw_decode starting from every `{`
    decoder = json.JSONDecoder()
    for pos, ch in enumerate(raw_text):
        if ch != "{":
            continue
        try:
            parsed, _ = decoder.raw_decode(raw_text, pos)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            return parsed

    raise ContractValidationError(
        ["Failed to extract a valid JSON object for DesignContract from LLM response"]
    )


def _coerce_float(value: Any) -> Any:
    """Convert numeric strings / ints to float; leave non-numeric or bool values untouched."""
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        stripped = value.strip()
        if _NUMERIC_STR_RE.match(stripped):
            return float(stripped)
    return value


def _coerce_int(value: Any) -> Any:
    """Convert integer strings / whole floats to int; leave invalid values untouched."""
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str):
        stripped = value.strip()
        if _INT_STR_RE.match(stripped):
            return int(stripped)
    return value


def _coerce_vec(value: Any) -> Any:
    if isinstance(value, (list, tuple)):
        return [_coerce_float(v) for v in value]
    return value


def _normalize_bbox(raw_bbox: Any, warnings: list[str]) -> Any:
    """Normalize dict or list bbox formats into `{min: [x,y,z], max: [x,y,z]}` without hiding invalid data."""
    if isinstance(raw_bbox, dict):
        bbox = dict(raw_bbox)
        if "min" not in bbox and "min_mm" in bbox:
            bbox["min"] = bbox.pop("min_mm")
            warnings.append("Normalized keep_in_bbox_mm.min_mm -> min")
        if "max" not in bbox and "max_mm" in bbox:
            bbox["max"] = bbox.pop("max_mm")
            warnings.append("Normalized keep_in_bbox_mm.max_mm -> max")
        if "min" in bbox:
            bbox["min"] = _coerce_vec(bbox["min"])
        if "max" in bbox:
            bbox["max"] = _coerce_vec(bbox["max"])
        return bbox

    if isinstance(raw_bbox, (list, tuple)):
        # Format A: [[x0, y0, z0], [x1, y1, z1]]
        if (
            len(raw_bbox) == 2
            and isinstance(raw_bbox[0], (list, tuple))
            and isinstance(raw_bbox[1], (list, tuple))
        ):
            warnings.append("Normalized keep_in_bbox_mm from [[min], [max]] list to {min, max} dict")
            return {
                "min": _coerce_vec(raw_bbox[0]),
                "max": _coerce_vec(raw_bbox[1]),
            }
        # Format B: [x0, y0, z0, x1, y1, z1]
        if len(raw_bbox) == 6 and not any(isinstance(v, (list, tuple, dict)) for v in raw_bbox):
            coerced = [_coerce_float(v) for v in raw_bbox]
            warnings.append("Normalized keep_in_bbox_mm from 6-element list to {min, max} dict")
            return {"min": coerced[:3], "max": coerced[3:]}
        # Format C: [dx, dy, dz] positive envelope dimensions centered in X/Y with Z in [0, dz]
        if len(raw_bbox) == 3 and not any(isinstance(v, (list, tuple, dict)) for v in raw_bbox):
            coerced = [_coerce_float(v) for v in raw_bbox]
            if all(isinstance(v, float) and v > 0 for v in coerced):
                dx, dy, dz = coerced
                warnings.append(
                    "Normalized keep_in_bbox_mm from [dx, dy, dz] dimensions to {min, max} dict"
                )
                return {
                    "min": [-dx / 2.0, -dy / 2.0, 0.0],
                    "max": [dx / 2.0, dy / 2.0, dz],
                }

    return raw_bbox


def _ensure_list_field(
    out: dict[str, Any],
    canonical_key: str,
    aliases: tuple[str, ...],
    warnings: list[str],
) -> None:
    if canonical_key not in out:
        for alias in aliases:
            if alias in out:
                out[canonical_key] = out.pop(alias)
                warnings.append(f"Normalized key {alias!r} -> {canonical_key!r}")
                break

    if canonical_key not in out or out[canonical_key] is None:
        out[canonical_key] = []
    elif isinstance(out[canonical_key], dict):
        out[canonical_key] = [out[canonical_key]]
        warnings.append(f"Wrapped single object in {canonical_key!r} into a list")


def normalize_contract_dict(
    data: dict[str, Any],
    *,
    warnings: list[str] | None = None,
) -> dict[str, Any]:
    """Normalize common LLM JSON deviations without masking truly invalid contract values."""
    if not isinstance(data, dict):
        raise ContractValidationError(["DesignContract payload must be a JSON object"])

    notes = warnings if warnings is not None else []
    out: dict[str, Any] = copy.deepcopy(data)

    # 1. Top-level key aliases
    for canonical, aliases in (
        ("part_name", ("partName", "name")),
        ("units", ("unit",)),
        ("manufacturing_method", ("manufacturingMethod", "manufacturing", "process")),
        (
            "keep_in_bbox_mm",
            ("keep_in_bbox", "keepInBboxMm", "keep_in", "bbox_mm", "bounding_box_mm"),
        ),
    ):
        if canonical not in out:
            for alias in aliases:
                if alias in out:
                    out[canonical] = out.pop(alias)
                    notes.append(f"Normalized key {alias!r} -> {canonical!r}")
                    break

    # 2. Safe defaults ONLY when missing or None (do NOT overwrite explicitly invalid values)
    if "part_name" not in out or out["part_name"] is None:
        out["part_name"] = "GeneratedPart"
        notes.append("Defaulted missing 'part_name' to 'GeneratedPart'")
    elif isinstance(out["part_name"], str):
        out["part_name"] = out["part_name"].strip()

    if "units" not in out or out["units"] is None:
        out["units"] = {"length": "mm", "angle": "deg", "force": "N"}
        notes.append("Defaulted missing 'units' to mm/deg/N")

    if "material" not in out or out["material"] is None:
        out["material"] = "Al-6061-T6"
        notes.append("Defaulted missing 'material' to 'Al-6061-T6'")

    if "manufacturing_method" not in out or out["manufacturing_method"] is None:
        out["manufacturing_method"] = "CNC_3AXIS_MILLING"
        notes.append("Defaulted missing 'manufacturing_method' to 'CNC_3AXIS_MILLING'")
    elif isinstance(out["manufacturing_method"], str):
        raw_method = out["manufacturing_method"].strip()
        matched_method = _VALID_METHOD_LOOKUP.get(raw_method.lower())
        if matched_method is not None and matched_method != raw_method:
            out["manufacturing_method"] = matched_method
            notes.append(
                f"Normalized manufacturing_method {raw_method!r} -> {matched_method!r}"
            )
        else:
            out["manufacturing_method"] = raw_method

    # 3. keep_in_bbox_mm normalization (keep missing/invalid intact so validation catches it)
    if "keep_in_bbox_mm" in out and out["keep_in_bbox_mm"] is not None:
        out["keep_in_bbox_mm"] = _normalize_bbox(out["keep_in_bbox_mm"], notes)

    # 4. List collections & singular/plural normalization
    _ensure_list_field(out, "keep_out_zones", ("keep_out_zone", "keep_outs", "keepOutZones"), notes)
    _ensure_list_field(out, "interfaces", ("interface", "mechanical_interfaces"), notes)
    _ensure_list_field(out, "loads", ("load", "load_cases"), notes)

    # 5. Coerce numeric string fields inside nested items
    if isinstance(out.get("keep_out_zones"), list):
        normalized_zones: list[Any] = []
        for zone in out["keep_out_zones"]:
            if not isinstance(zone, dict):
                normalized_zones.append(zone)
                continue
            z = dict(zone)
            if "zone_id" not in z and "id" in z:
                z["zone_id"] = z.pop("id")
            if "bbox_mm" not in z and "bbox" in z:
                z["bbox_mm"] = z.pop("bbox")
            if "bbox_mm" in z and z["bbox_mm"] is not None:
                z["bbox_mm"] = _normalize_bbox(z["bbox_mm"], notes)
            for vec_key in ("origin_mm", "size_mm"):
                if vec_key in z and z[vec_key] is not None:
                    z[vec_key] = _coerce_vec(z[vec_key])
            for num_key in ("diameter_mm", "height_mm"):
                if num_key in z and z[num_key] is not None:
                    z[num_key] = _coerce_float(z[num_key])
            normalized_zones.append(z)
        out["keep_out_zones"] = normalized_zones

    if isinstance(out.get("interfaces"), list):
        normalized_ifaces: list[Any] = []
        for iface in out["interfaces"]:
            if not isinstance(iface, dict):
                normalized_ifaces.append(iface)
                continue
            i = dict(iface)
            if "interface_id" not in i and "id" in i:
                i["interface_id"] = i.pop("id")
            for num_key in ("nominal_mm", "diameter_mm", "pitch_circle_diameter_mm"):
                if num_key in i and i[num_key] is not None:
                    i[num_key] = _coerce_float(i[num_key])
            if "count" in i and i["count"] is not None:
                i["count"] = _coerce_int(i["count"])
            normalized_ifaces.append(i)
        out["interfaces"] = normalized_ifaces

    if isinstance(out.get("loads"), list):
        normalized_loads: list[Any] = []
        for load in out["loads"]:
            if not isinstance(load, dict):
                normalized_loads.append(load)
                continue
            ld = dict(load)
            if "load_id" not in ld and "id" in ld:
                ld["load_id"] = ld.pop("id")
            for num_key in ("magnitude", "min_safety_factor"):
                if num_key in ld and ld[num_key] is not None:
                    ld[num_key] = _coerce_float(ld[num_key])
            if "vector" in ld and ld["vector"] is not None:
                ld["vector"] = _coerce_vec(ld["vector"])
            normalized_loads.append(ld)
        out["loads"] = normalized_loads

    if isinstance(out.get("dfm_constraints"), dict):
        dfm = dict(out["dfm_constraints"])
        for num_key in ("min_tool_radius_mm", "min_wall_thickness_mm"):
            if num_key in dfm and dfm[num_key] is not None:
                dfm[num_key] = _coerce_float(dfm[num_key])
        out["dfm_constraints"] = dfm

    return out


def _validate_and_build_contract(
    raw_text: str,
) -> tuple[DesignContract, list[str]]:
    """Extract, normalize, instantiate, and validate a `DesignContract`."""
    extracted = extract_json_object(raw_text)
    warnings: list[str] = []
    normalized = normalize_contract_dict(extracted, warnings=warnings)

    # Guard required structural fields before `DesignContract.from_dict` raises opaque KeyError/TypeError
    pre_errors: list[str] = []
    bbox = normalized.get("keep_in_bbox_mm")
    if not isinstance(bbox, dict) or "min" not in bbox or "max" not in bbox:
        pre_errors.append(
            "keep_in_bbox_mm is required and must be an object with 3-element 'min' and 'max' arrays"
        )
    else:
        for side in ("min", "max"):
            vec = bbox.get(side)
            if not isinstance(vec, list) or len(vec) != 3:
                pre_errors.append(f"keep_in_bbox_mm.{side} must be a 3-element numeric array")
            elif any(isinstance(v, bool) or not isinstance(v, (int, float)) for v in vec):
                pre_errors.append(f"keep_in_bbox_mm.{side} must contain only numbers")

    for list_field in ("keep_out_zones", "interfaces", "loads"):
        val = normalized.get(list_field)
        if not isinstance(val, list):
            pre_errors.append(f"{list_field} must be a list")
        elif any(not isinstance(item, dict) for item in val):
            pre_errors.append(f"{list_field} entries must be JSON objects")

    if pre_errors:
        raise ContractValidationError(pre_errors)

    try:
        contract = DesignContract.from_dict(normalized)
    except (KeyError, TypeError, ValueError) as exc:
        raise ContractValidationError([f"Failed to construct DesignContract: {exc}"]) from exc

    contract.assert_valid()
    return contract, warnings


def _build_repair_prompt(
    *,
    original_prompt: str,
    previous_output: str,
    errors: list[str],
    attempt: int,
    max_attempts: int,
) -> str:
    clipped_prev = previous_output.strip()
    if len(clipped_prev) > 3000:
        clipped_prev = clipped_prev[:3000] + "\n... [truncated]"

    sections: list[str] = [
        f"Repair attempt {attempt}/{max_attempts}.",
    ]
    if original_prompt.strip():
        sections.append(f"Original engineering requirement:\n{original_prompt.strip()}")
    sections.append(
        "Validation errors to fix:\n" + "\n".join(f"- {err}" for err in errors)
    )
    sections.append(DESIGN_CONTRACT_SCHEMA_SUMMARY)
    sections.append(f"Previous invalid output:\n{clipped_prev}")
    sections.append("Return ONLY the corrected JSON object.")
    return "\n\n".join(sections)


def parse_contract_with_repair(
    raw_text: str,
    *,
    llm_client: LLMClient | None = None,
    original_prompt: str = "",
    max_repair_attempts: int = 2,
) -> StructuredParseResult:
    """Parse and validate a `DesignContract`, optionally repairing via `llm_client` on failure."""
    max_repairs = max(0, int(max_repair_attempts))
    current_text = raw_text
    last_errors: list[str] = []

    for attempt in range(max_repairs + 1):
        try:
            contract, warnings = _validate_and_build_contract(current_text)
            return StructuredParseResult(
                contract=contract,
                repair_attempts=attempt,
                warnings=warnings,
            )
        except ContractValidationError as exc:
            last_errors = list(exc.errors)
            if llm_client is None or attempt >= max_repairs:
                if attempt > 0:
                    raise ContractValidationError(
                        [
                            f"DesignContract validation failed after {attempt} repair attempt(s)",
                            *last_errors,
                        ]
                    ) from exc
                raise

            repair_user_prompt = _build_repair_prompt(
                original_prompt=original_prompt,
                previous_output=current_text,
                errors=last_errors,
                attempt=attempt + 1,
                max_attempts=max_repairs,
            )
            current_text = llm_client.complete(REPAIR_SYSTEM_PROMPT, repair_user_prompt)

    raise ContractValidationError(last_errors or ["Unknown DesignContract validation error"])

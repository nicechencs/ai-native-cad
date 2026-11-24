"""Low-token local subgraph context slicer for 10,000-part large assembly AI modeling.

Architecture boundary: this module NEVER touches physical container internals directly.
It consumes ``cad-cli project slice-context`` JSON/KCL outputs, in-memory
``CanonicalGraph`` dictionaries, and bounded ``RoiDigestResult`` spatial digests.
"""

from __future__ import annotations

import copy
import json
import math
import subprocess
from collections import deque
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .compiler_bridge import discover_cad_cli
from .dfm_digest_tools import RoiDigestResult, fetch_cadpack_roi_digest

_CLI_TIMEOUT_SEC = 120.0
_MAX_ROI_DIGEST_BYTES = 12 * 1024

_IR_TYPE_NAMES: dict[str, str] = {
    "scalar": "Scalar",
    "int": "Int",
    "bool": "Bool",
    "string": "String",
    "length": "Length",
    "angle": "Angle",
    "force": "Force",
    "pressure": "Pressure",
    "mass": "Mass",
    "density": "Density",
    "vec2_length": "Vec2Length",
    "vec3_length": "Vec3Length",
    "vec3_dir": "Vec3Dir",
    "solid": "Solid",
    "surface": "Surface",
    "subd": "SubD",
    "scalar_field": "ScalarField",
    "vector_field": "VectorField",
    "sketch": "Sketch",
    "assembly": "Assembly",
    "component": "Component",
    "topo_ref": "TopoRef",
    "blob_ref": "BlobRef",
    "external_ref": "ExternalRef",
}

_NODE_OPERATOR_NAMES: dict[str, str] = {
    "parameter_ref": "parameter_ref",
    "sketch_2d": "sketch_2d",
    "primitive_box": "box",
    "primitive_cylinder": "cylinder",
    "extrude": "extrude",
    "revolve": "revolve",
    "sweep": "sweep",
    "loft_guided": "loft_guided",
    "shell": "shell",
    "fillet": "fillet",
    "chamfer": "chamfer",
    "draft": "draft",
    "mirror": "mirror",
    "pattern_linear": "pattern_linear",
    "pattern_polar": "pattern_polar",
    "boolean_union": "boolean_union",
    "boolean_subtract": "boolean_subtract",
    "boolean_intersect": "boolean_intersect",
    "hole": "hole",
    "direct_face_edit": "direct_face_edit",
    "surface_boundary": "surface_boundary",
    "surface_trim": "surface_trim",
    "surface_knit_to_solid": "surface_knit_to_solid",
    "subd_from_blob": "subd_from_blob",
    "subd_to_nurbs": "subd_to_nurbs",
    "field_sdf": "field::sdf",
    "field_ramp": "field::ramp",
    "field_from_fea_blob": "field::from_fea",
    "implicit_smooth_blend": "implicit::smooth_blend",
    "implicit_lattice_infill": "implicit::lattice_infill",
    "std_lib_call": "std::call",
    "import_asset": "import::asset",
    "assembly_component": "assembly::component",
    "assembly_mate": "assembly::mate",
    "tag_faces": "tag::faces",
    "assert_gate": "assert::gate",
}

_KCL_KEYWORDS = {"part", "assembly", "let", "return", "sketch", "const", "true", "false"}
_KCL_VALUE_KEYWORDS = {"vec3", "node"}


class ContextSliceError(ValueError):
    """Raised when subgraph context slicing fails validation or CLI invocation."""


@dataclass(frozen=True, slots=True)
class FocusedModelingContext:
    """Compact two-layer modeling context package ready for LLM prompt injection."""

    focus_nodes: list[str]
    upstream_depth: int
    downstream_depth: int
    total_node_count: int
    kept_node_count: int
    node_reduction_ratio: float
    sliced_graph: dict[str, Any]
    kcl_view: str
    parameter_summary: list[dict[str, Any]]
    node_summary: list[dict[str, Any]]
    external_ref_interfaces: list[dict[str, Any]]
    roi_digest: RoiDigestResult | None
    estimated_full_tokens: int
    estimated_slice_tokens: int
    token_reduction_ratio: float
    prompt_context: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "focus_nodes", [str(item) for item in self.focus_nodes])
        object.__setattr__(self, "sliced_graph", copy.deepcopy(self.sliced_graph))
        object.__setattr__(self, "parameter_summary", copy.deepcopy(self.parameter_summary))
        object.__setattr__(self, "node_summary", copy.deepcopy(self.node_summary))
        object.__setattr__(
            self,
            "external_ref_interfaces",
            copy.deepcopy(self.external_ref_interfaces),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "focus_nodes": list(self.focus_nodes),
            "upstream_depth": self.upstream_depth,
            "downstream_depth": self.downstream_depth,
            "total_node_count": self.total_node_count,
            "kept_node_count": self.kept_node_count,
            "node_reduction_ratio": self.node_reduction_ratio,
            "sliced_graph": copy.deepcopy(self.sliced_graph),
            "kcl_view": self.kcl_view,
            "parameter_summary": copy.deepcopy(self.parameter_summary),
            "node_summary": copy.deepcopy(self.node_summary),
            "external_ref_interfaces": copy.deepcopy(self.external_ref_interfaces),
            "roi_digest": None if self.roi_digest is None else self.roi_digest.to_dict(),
            "estimated_full_tokens": self.estimated_full_tokens,
            "estimated_slice_tokens": self.estimated_slice_tokens,
            "token_reduction_ratio": self.token_reduction_ratio,
            "prompt_context": self.prompt_context,
        }


def slice_canonical_graph_dict(
    graph_dict: Mapping[str, Any],
    focus_nodes: Sequence[str],
    *,
    upstream_depth: int = 1,
    downstream_depth: int = 1,
    ensure_valid_closure: bool = True,
) -> dict[str, Any]:
    """Pure-Python deterministic equivalent of ``cad_ir::extract_context_slice``.

    Extracts the bounded upstream/downstream neighborhood around ``focus_nodes``,
    closes over required transitive upstream ``NodeRef`` ancestors when
    ``ensure_valid_closure=True``, and prunes ``parameters``, ``blobs``, and
    ``external_refs`` to only those referenced by the kept nodes.
    """
    if not isinstance(graph_dict, Mapping):
        raise TypeError("graph_dict must be a CanonicalGraph mapping")
    up_steps = _require_nonneg_int(upstream_depth, "upstream_depth")
    down_steps = _require_nonneg_int(downstream_depth, "downstream_depth")
    if not isinstance(ensure_valid_closure, bool):
        raise TypeError("ensure_valid_closure must be a bool")

    nodes_raw = graph_dict.get("nodes")
    if not isinstance(nodes_raw, Mapping):
        raise ContextSliceError("graph_dict is missing a 'nodes' mapping")
    parameters_raw = _optional_mapping(graph_dict.get("parameters", {}), "parameters")
    blobs_raw = _optional_mapping(graph_dict.get("blobs", {}), "blobs")
    ext_refs_raw = _optional_mapping(graph_dict.get("external_refs", {}), "external_refs")
    outputs_raw = graph_dict.get("outputs", [])
    if isinstance(outputs_raw, (str, bytes)) or not isinstance(outputs_raw, Sequence):
        raise ContextSliceError("graph_dict 'outputs' must be a list of node IDs")

    seeds = _normalize_focus_nodes(focus_nodes)
    for seed in seeds:
        if seed not in nodes_raw:
            raise ContextSliceError(f"dangling focus node {seed!r}: not found in graph nodes")

    upstream_of: dict[str, set[str]] = {}
    downstream_of: dict[str, set[str]] = {str(nid): set() for nid in nodes_raw}

    for raw_id, node in nodes_raw.items():
        node_id = str(raw_id)
        if not isinstance(node, Mapping):
            raise ContextSliceError(f"node {node_id!r} must be an object")
        inputs = node.get("inputs", {})
        if not isinstance(inputs, Mapping):
            raise ContextSliceError(f"node {node_id!r} inputs must be an object")
        deps: set[str] = set()
        visited_ext: set[str] = set()
        for val in inputs.values():
            _collect_node_deps(ext_refs_raw, val, visited_ext, deps)
        for dep in deps:
            if dep not in nodes_raw:
                raise ContextSliceError(f"node {node_id!r} references missing upstream node {dep!r}")
            downstream_of.setdefault(dep, set()).add(node_id)
        upstream_of[node_id] = deps

    up_candidates = _bfs_steps(seeds, up_steps, upstream_of)
    down_candidates = _bfs_steps(seeds, down_steps, downstream_of)
    kept_nodes: set[str] = up_candidates | down_candidates

    if ensure_valid_closure:
        queue: deque[str] = deque(sorted(kept_nodes))
        while queue:
            curr = queue.popleft()
            for parent in sorted(upstream_of.get(curr, ())):
                if parent not in kept_nodes:
                    kept_nodes.add(parent)
                    queue.append(parent)

    kept_params: set[str] = set()
    kept_blobs: set[str] = set()
    kept_ext_refs: set[str] = set()

    for node_id in sorted(kept_nodes):
        node = nodes_raw[node_id]
        kind = node.get("kind")
        if isinstance(kind, Mapping) and kind.get("kind") == "parameter_ref":
            param_id = kind.get("param_id")
            if isinstance(param_id, str):
                if ensure_valid_closure and param_id not in parameters_raw:
                    raise ContextSliceError(
                        f"node {node_id!r} references missing parameter {param_id!r}"
                    )
                if param_id in parameters_raw:
                    kept_params.add(param_id)
                    param_def = parameters_raw[param_id]
                    if isinstance(param_def, Mapping) and "value" in param_def:
                        _collect_resource_refs(
                            ext_refs_raw,
                            param_def["value"],
                            kept_blobs,
                            kept_ext_refs,
                        )
        for val in node.get("inputs", {}).values():
            _collect_resource_refs(ext_refs_raw, val, kept_blobs, kept_ext_refs)

    sorted_kept_nodes = sorted(kept_nodes)
    outputs = [str(nid) for nid in outputs_raw if str(nid) in kept_nodes]
    if not outputs and sorted_kept_nodes:
        for nid in sorted_kept_nodes:
            consumers = downstream_of.get(nid, set())
            has_kept_consumer = any(c != nid and c in kept_nodes for c in consumers)
            if not has_kept_consumer:
                outputs.append(nid)

    sliced: dict[str, Any] = {
        "ir_schema_version": int(graph_dict.get("ir_schema_version", 1)),
        "graph_id": str(graph_dict.get("graph_id", "sliced")),
        "units": copy.deepcopy(
            dict(
                graph_dict.get(
                    "units",
                    {
                        "length": "mm",
                        "angle": "deg",
                        "force": "N",
                        "pressure": "MPa",
                        "mass": "kg",
                    },
                )
            )
        ),
        "parameters": {
            pid: copy.deepcopy(dict(parameters_raw[pid]))
            for pid in sorted(kept_params)
            if pid in parameters_raw
        },
        "blobs": {
            handle: copy.deepcopy(dict(blobs_raw[handle]))
            for handle in sorted(kept_blobs)
            if handle in blobs_raw
        },
    }
    pruned_ext_refs = {
        ref_id: copy.deepcopy(dict(ext_refs_raw[ref_id]))
        for ref_id in sorted(kept_ext_refs)
        if ref_id in ext_refs_raw
    }
    if pruned_ext_refs:
        sliced["external_refs"] = pruned_ext_refs
    sliced["nodes"] = {
        nid: copy.deepcopy(dict(nodes_raw[nid]))
        for nid in sorted_kept_nodes
    }
    sliced["outputs"] = outputs

    if ensure_valid_closure:
        # Verify topological acyclicity of the sliced subgraph.
        _topological_node_order(sliced)

    return sliced


def emit_kcl_from_graph_dict(graph_dict: Mapping[str, Any]) -> str:
    """Project a validated ``CanonicalGraph`` dict into a deterministic ``.kcl 2.0`` view."""
    order = _topological_node_order(graph_dict)
    lines: list[str] = []
    schema_ver = int(graph_dict.get("ir_schema_version", 1))
    graph_id = str(graph_dict.get("graph_id", "main"))
    units = graph_dict.get("units", {})

    lines.append(f"@ir_schema({schema_ver})")
    lines.append(f"@graph_id({_fmt_str(graph_id)})")
    lines.append(
        "@unit(length = {length}, angle = {angle}, force = {force}, pressure = {pressure}, mass = {mass})".format(
            length=units.get("length", "mm"),
            angle=units.get("angle", "deg"),
            force=units.get("force", "N"),
            pressure=units.get("pressure", "MPa"),
            mass=units.get("mass", "kg"),
        )
    )
    lines.append("")

    blobs = graph_dict.get("blobs", {})
    if isinstance(blobs, Mapping) and blobs:
        for handle in sorted(blobs):
            blob = blobs[handle]
            lines.append(
                "@blob({handle}, kind = {kind}, sha256 = {sha}, path = {path}, bytes = {size})".format(
                    handle=_fmt_str(str(blob.get("handle", handle))),
                    kind=_fmt_str(str(blob.get("kind", "import"))),
                    sha=_fmt_str(str(blob.get("sha256", ""))),
                    path=_fmt_str(str(blob.get("relative_path", ""))),
                    size=int(blob.get("byte_len", 0)),
                )
            )
        lines.append("")

    ext_refs = graph_dict.get("external_refs", {})
    if isinstance(ext_refs, Mapping) and ext_refs:
        for ref_id in sorted(ext_refs):
            lines.append(_fmt_extref_decl(ext_refs[ref_id]))
        lines.append("")

    parameters = graph_dict.get("parameters", {})
    if isinstance(parameters, Mapping) and parameters:
        for param_id in sorted(parameters):
            param = parameters[param_id]
            ty = _IR_TYPE_NAMES.get(str(param.get("ir_type", "scalar")), "Scalar")
            val = _fmt_ir_value(param.get("value", {"type": "scalar", "value": 0.0}))
            stmt = f"param {_fmt_binding(param_id)}: {ty} = {val}"
            formula = param.get("formula")
            if isinstance(formula, str):
                stmt += f" @formula({_fmt_str(formula)})"
            lines.append(stmt)
        lines.append("")

    nodes = graph_dict.get("nodes", {})
    for idx, node_id in enumerate(order):
        if idx > 0:
            lines.append("")
        lines.extend(_emit_kcl_node(nodes[node_id]))
    if order:
        lines.append("")

    outputs = [
        _fmt_node_ref(str(nid))
        for nid in graph_dict.get("outputs", [])
    ]
    lines.append(f"return [{', '.join(outputs)}]")
    return "\n".join(lines) + "\n"


def build_focused_modeling_context(
    project_path: str | Path | Mapping[str, Any],
    focus_nodes: Sequence[str],
    *,
    upstream_depth: int = 1,
    downstream_depth: int = 1,
    roi_bounds: tuple[Sequence[float], Sequence[float]] | None = None,
    cli_runner: Callable[..., subprocess.CompletedProcess[str]] | None = None,
    cadpack_path: str | Path | None = None,
    cad_cli_bin: str | Path | None = None,
    ensure_valid_closure: bool = True,
) -> FocusedModelingContext:
    """Build a focused, low-token LLM modeling context from a container path or in-memory graph.

    1. When ``project_path`` is a ``Mapping``, slices in pure Python without spawning a subprocess;
       when ``project_path`` is a filesystem path, calls
       ``cad-cli project slice-context <path> --focus <ids> --up <u_depth> --down <d_depth> --json``.
    2. When ``roi_bounds=(min_xyz, max_xyz)`` is supplied and a ``.cadpack`` file is available,
       attaches a bounded spatial ``RoiDigestResult`` (``<= 12 KiB``).
    3. Formats a compact prompt envelope combining local ``.kcl``, parameter/node summaries,
       external reference interfaces, ROI spatial digest, and compression statistics.
    """
    seeds = _normalize_focus_nodes(focus_nodes)
    up_steps = _require_nonneg_int(upstream_depth, "upstream_depth")
    down_steps = _require_nonneg_int(downstream_depth, "downstream_depth")
    parsed_roi = _validate_roi_bounds(roi_bounds)

    if isinstance(project_path, Mapping):
        total_node_count = len(project_path.get("nodes", {}))
        sliced_graph = slice_canonical_graph_dict(
            project_path,
            seeds,
            upstream_depth=up_steps,
            downstream_depth=down_steps,
            ensure_valid_closure=ensure_valid_closure,
        )
        kept_node_count = len(sliced_graph.get("nodes", {}))
        kcl_view = emit_kcl_from_graph_dict(sliced_graph)
        full_chars = len(
            json.dumps(dict(project_path), ensure_ascii=False, separators=(",", ":"))
        )
        resolved_cadpack = _discover_cadpack_path(None, cadpack_path)
    elif isinstance(project_path, (str, Path)):
        target_path = Path(project_path)
        sliced_graph, kcl_view, total_node_count, kept_node_count = _slice_via_cli(
            project_path=target_path,
            focus_nodes=seeds,
            upstream_depth=up_steps,
            downstream_depth=down_steps,
            ensure_valid_closure=ensure_valid_closure,
            cli_runner=cli_runner,
            cad_cli_bin=cad_cli_bin,
        )
        slice_chars = max(
            1, len(json.dumps(sliced_graph, ensure_ascii=False, separators=(",", ":")))
        )
        if kept_node_count > 0 and total_node_count >= kept_node_count:
            full_chars = int(round(slice_chars * (total_node_count / kept_node_count)))
        else:
            full_chars = slice_chars
        resolved_cadpack = _discover_cadpack_path(target_path, cadpack_path)
    else:
        raise TypeError("project_path must be a Path, str, or in-memory CanonicalGraph mapping")

    roi_digest: RoiDigestResult | None = None
    if parsed_roi is not None and resolved_cadpack is not None:
        roi_min, roi_max = parsed_roi
        roi_digest = _load_roi_digest(
            cadpack_path=resolved_cadpack,
            roi_min=roi_min,
            roi_max=roi_max,
            cli_runner=cli_runner,
            cad_cli_bin=cad_cli_bin,
        )

    node_reduction_ratio = (
        max(0.0, 1.0 - (kept_node_count / total_node_count))
        if total_node_count > 0
        else 0.0
    )

    param_summary = _summarize_parameters(sliced_graph)
    node_summary = _summarize_nodes(sliced_graph, set(seeds))
    extref_summary = _summarize_external_refs(sliced_graph)

    prompt_context = _render_prompt_envelope(
        focus_nodes=seeds,
        upstream_depth=up_steps,
        downstream_depth=down_steps,
        total_node_count=total_node_count,
        kept_node_count=kept_node_count,
        node_reduction_ratio=node_reduction_ratio,
        kcl_view=kcl_view,
        parameter_summary=param_summary,
        node_summary=node_summary,
        external_ref_interfaces=extref_summary,
        roi_digest=roi_digest,
    )

    estimated_full_tokens = max(1, math.ceil(full_chars / 4))
    slice_payload_chars = len(kcl_view) + (
        roi_digest.byte_length if roi_digest is not None else 0
    )
    estimated_slice_tokens = max(1, math.ceil(slice_payload_chars / 4))
    token_reduction_ratio = (
        max(0.0, 1.0 - min(1.0, estimated_slice_tokens / estimated_full_tokens))
        if total_node_count > kept_node_count
        else 0.0
    )

    return FocusedModelingContext(
        focus_nodes=seeds,
        upstream_depth=up_steps,
        downstream_depth=down_steps,
        total_node_count=total_node_count,
        kept_node_count=kept_node_count,
        node_reduction_ratio=node_reduction_ratio,
        sliced_graph=sliced_graph,
        kcl_view=kcl_view,
        parameter_summary=param_summary,
        node_summary=node_summary,
        external_ref_interfaces=extref_summary,
        roi_digest=roi_digest,
        estimated_full_tokens=estimated_full_tokens,
        estimated_slice_tokens=estimated_slice_tokens,
        token_reduction_ratio=token_reduction_ratio,
        prompt_context=prompt_context,
    )


def _slice_via_cli(
    *,
    project_path: Path,
    focus_nodes: list[str],
    upstream_depth: int,
    downstream_depth: int,
    ensure_valid_closure: bool,
    cli_runner: Callable[..., subprocess.CompletedProcess[str]] | None,
    cad_cli_bin: str | Path | None,
) -> tuple[dict[str, Any], str, int, int]:
    cli_exec = _resolve_cli_exec(cad_cli_bin, allow_virtual=cli_runner is not None)
    cmd = [
        cli_exec,
        "project",
        "slice-context",
        str(project_path),
        "--focus",
        ",".join(focus_nodes),
        "--up",
        str(upstream_depth),
        "--down",
        str(downstream_depth),
    ]
    if not ensure_valid_closure:
        cmd.append("--no-closure")
    cmd.append("--json")

    completed = _invoke_runner(cmd, cli_runner=cli_runner)
    payload = _parse_json_object(completed.stdout or "")
    if completed.returncode != 0 or payload is None or payload.get("status") not in (None, "ok"):
        detail = _cli_error_text(completed, payload)
        raise ContextSliceError(f"cad-cli project slice-context failed: {detail}")

    sliced_graph = payload.get("graph")
    if not isinstance(sliced_graph, dict):
        raise ContextSliceError("slice-context JSON response is missing a 'graph' object")

    kept_count = int(payload.get("kept_node_count", len(sliced_graph.get("nodes", {}))))
    total_count = int(payload.get("total_node_count", kept_count))
    kcl_text = payload.get("kcl")
    if not isinstance(kcl_text, str) or not kcl_text.strip():
        kcl_text = emit_kcl_from_graph_dict(sliced_graph)

    return sliced_graph, kcl_text, max(kept_count, total_count), kept_count


def _load_roi_digest(
    *,
    cadpack_path: Path,
    roi_min: tuple[float, float, float],
    roi_max: tuple[float, float, float],
    cli_runner: Callable[..., subprocess.CompletedProcess[str]] | None,
    cad_cli_bin: str | Path | None,
) -> RoiDigestResult:
    if cli_runner is None:
        explicit = Path(cad_cli_bin) if cad_cli_bin is not None else None
        return fetch_cadpack_roi_digest(
            cadpack_path,
            roi_min,
            roi_max,
            cad_cli_bin=explicit,
        )

    cli_exec = _resolve_cli_exec(cad_cli_bin, allow_virtual=True)
    cmd = [
        cli_exec,
        "project",
        "cadpack-digest",
        str(cadpack_path),
        "--min",
        _fmt_vec3_cli(roi_min),
        "--max",
        _fmt_vec3_cli(roi_max),
        "--json",
    ]
    completed = _invoke_runner(cmd, cli_runner=cli_runner)
    payload = _parse_json_object(completed.stdout or "")
    if completed.returncode != 0 or payload is None or payload.get("status") == "error":
        raise ContextSliceError(
            f"cad-cli project cadpack-digest failed: {_cli_error_text(completed, payload)}"
        )
    components = payload.get("components", [])
    if not isinstance(components, list):
        raise ContextSliceError("cadpack-digest JSON is missing components list")
    total_matches = payload.get("total_matches")
    comp_count = (
        total_matches
        if isinstance(total_matches, int) and not isinstance(total_matches, bool)
        else len(components)
    )
    compact = json.dumps(payload, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
    byte_len = len(compact.encode("utf-8"))
    if byte_len > _MAX_ROI_DIGEST_BYTES:
        raise ContextSliceError(
            f"ROI digest ({byte_len} bytes) exceeds 12 KiB budget ({_MAX_ROI_DIGEST_BYTES} bytes)"
        )
    return RoiDigestResult(
        roi_min_mm=roi_min,
        roi_max_mm=roi_max,
        component_count=comp_count,
        summary_json=compact,
        byte_length=byte_len,
    )


def _discover_cadpack_path(
    project_path: Path | None,
    explicit_cadpack: str | Path | None,
) -> Path | None:
    if explicit_cadpack is not None:
        candidate = Path(explicit_cadpack)
        return candidate if candidate.is_file() else None
    if project_path is None:
        return None
    candidates: list[Path] = []
    if project_path.is_dir():
        candidates.append(project_path / "cache" / "scene.cadpack")
        candidates.append(project_path.with_suffix(".cadpack"))
    else:
        candidates.append(project_path.with_suffix(".cadpack"))
        candidates.append(project_path.parent / "scene.cadpack")
    for item in candidates:
        if item.is_file():
            return item
    return None


def _summarize_parameters(sliced_graph: Mapping[str, Any]) -> list[dict[str, Any]]:
    params = sliced_graph.get("parameters", {})
    if not isinstance(params, Mapping):
        return []
    items: list[dict[str, Any]] = []
    for key in sorted(params):
        entry = params[key]
        if not isinstance(entry, Mapping):
            continue
        items.append(
            {
                "name": str(entry.get("name", key)),
                "ir_type": str(entry.get("ir_type", "")),
                "value": _fmt_ir_value(entry.get("value", {})),
                "formula": entry.get("formula"),
            }
        )
    return items


def _summarize_nodes(
    sliced_graph: Mapping[str, Any],
    focus_set: set[str],
) -> list[dict[str, Any]]:
    nodes = sliced_graph.get("nodes", {})
    if not isinstance(nodes, Mapping):
        return []
    items: list[dict[str, Any]] = []
    for key in _topological_node_order(sliced_graph):
        node = nodes[key]
        kind_obj = node.get("kind", {})
        kind_name = kind_obj.get("kind", "unknown") if isinstance(kind_obj, Mapping) else str(kind_obj)
        items.append(
            {
                "id": key,
                "name": str(node.get("name", key)),
                "kind": str(kind_name),
                "output_type": str(node.get("output_type", "")),
                "is_focus": key in focus_set,
            }
        )
    return items


def _summarize_external_refs(sliced_graph: Mapping[str, Any]) -> list[dict[str, Any]]:
    ext_refs = sliced_graph.get("external_refs", {})
    if not isinstance(ext_refs, Mapping):
        return []
    items: list[dict[str, Any]] = []
    for ref_id in sorted(ext_refs):
        entry = ext_refs[ref_id]
        if not isinstance(entry, Mapping):
            continue
        overrides = entry.get("overrides", {})
        override_summary = (
            {str(k): _fmt_ir_value(v) for k, v in sorted(overrides.items())}
            if isinstance(overrides, Mapping)
            else {}
        )
        items.append(
            {
                "ref_id": str(entry.get("ref_id", ref_id)),
                "container_path": str(entry.get("container_path", "")),
                "payload_policy": str(entry.get("payload_policy", "eager")),
                "target_graph_id": entry.get("target_graph_id"),
                "overrides": override_summary,
            }
        )
    return items


def _render_prompt_envelope(
    *,
    focus_nodes: list[str],
    upstream_depth: int,
    downstream_depth: int,
    total_node_count: int,
    kept_node_count: int,
    node_reduction_ratio: float,
    kcl_view: str,
    parameter_summary: list[dict[str, Any]],
    node_summary: list[dict[str, Any]],
    external_ref_interfaces: list[dict[str, Any]],
    roi_digest: RoiDigestResult | None,
) -> str:
    pct = node_reduction_ratio * 100.0
    lines: list[str] = [
        "=== FOCUSED ASSEMBLY MODELING CONTEXT ===",
        (
            f"Slice Window: focus={focus_nodes}, upstream_depth={upstream_depth}, "
            f"downstream_depth={downstream_depth}"
        ),
        (
            f"Subgraph Compression: kept {kept_node_count}/{total_node_count} nodes "
            f"(node_reduction_ratio={node_reduction_ratio:.4f}, {pct:.1f}% pruned)"
        ),
    ]

    if parameter_summary:
        lines.append("Active Parameters:")
        for param in parameter_summary:
            formula_note = f" [formula: {param['formula']}]" if param.get("formula") else ""
            lines.append(f"  - {param['name']}: {param['ir_type']} = {param['value']}{formula_note}")

    if external_ref_interfaces:
        lines.append("External Composition Interfaces (OpenUSD Refs):")
        for ext in external_ref_interfaces:
            ov = f", overrides={ext['overrides']}" if ext["overrides"] else ""
            lines.append(
                f"  - @{ext['ref_id']} -> {ext['container_path']} "
                f"(policy={ext['payload_policy']}{ov})"
            )

    lines.append("Closed Subgraph Nodes:")
    for item in node_summary:
        marker = "[FOCUS] " if item["is_focus"] else ""
        lines.append(f"  - {marker}{item['id']} ({item['kind']} -> {item['output_type']})")

    lines.append("Local .kcl 2.0 Projection:")
    lines.append("```kcl")
    lines.append(kcl_view.rstrip("\n"))
    lines.append("```")

    if roi_digest is not None:
        lines.append(
            f"Spatial ROI InterfaceDigest ({roi_digest.byte_length} bytes, "
            f"{roi_digest.component_count} neighboring components):"
        )
        lines.append(roi_digest.summary_json)

    lines.append(
        "Contract: propose an atomic GraphPatch JSON targeting only the sliced subgraph "
        "while respecting surrounding ROI envelopes and external reference overrides."
    )
    return "\n".join(lines)


def _bfs_steps(
    seeds: Sequence[str],
    max_depth: int,
    adjacency: Mapping[str, set[str]],
) -> set[str]:
    visited: dict[str, int] = {}
    queue: deque[tuple[str, int]] = deque()
    for seed in seeds:
        if seed not in visited:
            visited[seed] = 0
            queue.append((seed, 0))

    while queue:
        curr, depth = queue.popleft()
        if depth >= max_depth:
            continue
        for nxt in sorted(adjacency.get(curr, ())):
            next_depth = depth + 1
            prev = visited.get(nxt)
            if prev is None or next_depth < prev:
                visited[nxt] = next_depth
                queue.append((nxt, next_depth))
    return set(visited)


def _collect_node_deps(
    ext_refs: Mapping[str, Any],
    value: Any,
    visited_ext: set[str],
    deps: set[str],
) -> None:
    if not isinstance(value, Mapping):
        return
    vtype = value.get("type")
    payload = value.get("value")
    if vtype == "node_ref" and isinstance(payload, Mapping):
        node_id = payload.get("node_id")
        if isinstance(node_id, str):
            deps.add(node_id)
    elif vtype == "external_ref" and isinstance(payload, str):
        if payload not in visited_ext:
            visited_ext.add(payload)
            ext_def = ext_refs.get(payload)
            if isinstance(ext_def, Mapping):
                overrides = ext_def.get("overrides", {})
                if isinstance(overrides, Mapping):
                    for ov in overrides.values():
                        _collect_node_deps(ext_refs, ov, visited_ext, deps)
    elif vtype == "list" and isinstance(payload, Sequence) and not isinstance(payload, (str, bytes)):
        for item in payload:
            _collect_node_deps(ext_refs, item, visited_ext, deps)
    elif vtype == "map" and isinstance(payload, Mapping):
        for item in payload.values():
            _collect_node_deps(ext_refs, item, visited_ext, deps)


def _collect_resource_refs(
    ext_refs: Mapping[str, Any],
    value: Any,
    blobs: set[str],
    kept_ext_refs: set[str],
) -> None:
    if not isinstance(value, Mapping):
        return
    vtype = value.get("type")
    payload = value.get("value")
    if vtype == "blob_ref" and isinstance(payload, str):
        blobs.add(payload)
    elif vtype == "external_ref" and isinstance(payload, str):
        if payload not in kept_ext_refs:
            kept_ext_refs.add(payload)
            ext_def = ext_refs.get(payload)
            if isinstance(ext_def, Mapping):
                overrides = ext_def.get("overrides", {})
                if isinstance(overrides, Mapping):
                    for ov in overrides.values():
                        _collect_resource_refs(ext_refs, ov, blobs, kept_ext_refs)
    elif vtype == "list" and isinstance(payload, Sequence) and not isinstance(payload, (str, bytes)):
        for item in payload:
            _collect_resource_refs(ext_refs, item, blobs, kept_ext_refs)
    elif vtype == "map" and isinstance(payload, Mapping):
        for item in payload.values():
            _collect_resource_refs(ext_refs, item, blobs, kept_ext_refs)


def _topological_node_order(graph_dict: Mapping[str, Any]) -> list[str]:
    nodes = graph_dict.get("nodes", {})
    ext_refs = graph_dict.get("external_refs", {})
    if not isinstance(nodes, Mapping):
        return []
    ext_map = ext_refs if isinstance(ext_refs, Mapping) else {}
    in_degree: dict[str, int] = {}
    dependents: dict[str, list[str]] = {str(k): [] for k in nodes}

    for raw_id in sorted(nodes, key=str):
        nid = str(raw_id)
        node = nodes[raw_id]
        deps: set[str] = set()
        visited_ext: set[str] = set()
        if isinstance(node, Mapping):
            for val in node.get("inputs", {}).values():
                _collect_node_deps(ext_map, val, visited_ext, deps)
        for dep in deps:
            if dep not in nodes:
                raise ContextSliceError(f"sliced node {nid!r} has dangling dependency {dep!r}")
            dependents[dep].append(nid)
        in_degree[nid] = len(deps)

    ready = sorted(nid for nid, deg in in_degree.items() if deg == 0)
    order: list[str] = []
    while ready:
        curr = ready.pop(0)
        order.append(curr)
        for child in sorted(dependents.get(curr, ())):
            in_degree[child] -= 1
            if in_degree[child] == 0:
                ready.append(child)
                ready.sort()

    if len(order) != len(nodes):
        raise ContextSliceError("cycle detected in CanonicalGraph slice")
    return order


def _emit_kcl_node(node: Mapping[str, Any]) -> list[str]:
    lines: list[str] = []
    node_id = str(node.get("id", ""))
    name = str(node.get("name", node_id))
    state = node.get("state", {})
    if isinstance(state, Mapping):
        status = state.get("status", "valid")
        if status == "suppressed":
            lines.append("@suppressed")
        elif status == "stale":
            lines.append("@stale")
        elif status == "failed_with_rollback":
            reason = _fmt_str(str(state.get("reason", "")))
            refs = state.get("failed_topo_refs", [])
            if refs:
                refs_str = ", ".join(_fmt_str(str(r)) for r in refs)
                lines.append(f"@failed({reason}, refs = [{refs_str}])")
            else:
                lines.append(f"@failed({reason})")
    if name != node_id:
        lines.append(f"@name({_fmt_str(name)})")
    kernel_ver = node.get("kernel_op_version")
    if isinstance(kernel_ver, str):
        lines.append(f"@kernel({_fmt_str(kernel_ver)})")
    metadata = node.get("metadata", {})
    if isinstance(metadata, Mapping) and metadata:
        pairs = ", ".join(
            f"{_fmt_key(str(k))} = {_fmt_str(str(metadata[k]))}"
            for k in sorted(metadata, key=str)
        )
        lines.append(f"@meta({pairs})")

    kind_obj = node.get("kind", {})
    positional: str | None = None
    if isinstance(kind_obj, Mapping):
        kname = str(kind_obj.get("kind", ""))
        if kname == "field_tpms":
            op_name = f"field::{kind_obj.get('family', 'gyroid')}"
        elif kname == "parameter_ref":
            op_name = "parameter_ref"
            positional = _fmt_str(str(kind_obj.get("param_id", "")))
        else:
            op_name = _NODE_OPERATOR_NAMES.get(kname, kname)
    else:
        op_name = str(kind_obj)

    out_ty = _IR_TYPE_NAMES.get(str(node.get("output_type", "solid")), "Solid")
    head = f"let {_fmt_binding(node_id)} = {op_name}"
    inputs = node.get("inputs", {})
    sorted_inputs = sorted(inputs.items(), key=lambda kv: str(kv[0])) if isinstance(inputs, Mapping) else []

    if positional is None and not sorted_inputs:
        lines.append(f"{head}() -> {out_ty}")
    elif positional is not None and not sorted_inputs:
        lines.append(f"{head}({positional}) -> {out_ty}")
    else:
        lines.append(f"{head}(")
        if positional is not None:
            lines.append(f"    {positional},")
        for key, val in sorted_inputs:
            lines.append(f"    {_fmt_key(str(key))} = {_fmt_ir_value(val)},")
        lines.append(f") -> {out_ty}")
    return lines


def _fmt_extref_decl(ext_ref: Mapping[str, Any]) -> str:
    parts = [
        _fmt_str(str(ext_ref.get("ref_id", ""))),
        f"path = {_fmt_str(str(ext_ref.get('container_path', '')))}",
        f"policy = {_fmt_str(str(ext_ref.get('payload_policy', 'eager')))}",
    ]
    sha256 = ext_ref.get("expected_sha256")
    if isinstance(sha256, str):
        parts.append(f"sha256 = {_fmt_str(sha256)}")
    target_graph = ext_ref.get("target_graph_id")
    if isinstance(target_graph, str):
        parts.append(f"target_graph = {_fmt_str(target_graph)}")
    overrides = ext_ref.get("overrides", {})
    if isinstance(overrides, Mapping) and overrides:
        inner = ", ".join(
            f"{_fmt_key(str(k))} = {_fmt_ir_value(overrides[k])}"
            for k in sorted(overrides, key=str)
        )
        parts.append(f"overrides = {{ {inner} }}")
    return f"@extref({', '.join(parts)})"


def _fmt_ir_value(value: Any) -> str:
    if not isinstance(value, Mapping):
        return _fmt_str(str(value))
    vtype = value.get("type")
    payload = value.get("value")
    if vtype == "quantity" and isinstance(payload, Mapping):
        num = _fmt_num(float(payload.get("value", 0.0)))
        dim = payload.get("dimension")
        if dim == "density":
            return f"{num}kg_m3"
        return f"{num}{payload.get('unit', 'mm')}"
    if vtype == "scalar":
        text = _fmt_num(float(payload or 0.0))
        if "." not in text and "e" not in text and "E" not in text:
            text += ".0"
        return text
    if vtype == "int":
        return str(int(payload or 0))
    if vtype == "bool":
        return "true" if bool(payload) else "false"
    if vtype == "string":
        return _fmt_str(str(payload or ""))
    if vtype == "vec3" and isinstance(payload, Sequence) and len(payload) == 3:
        return f"vec3({_fmt_num(float(payload[0]))}, {_fmt_num(float(payload[1]))}, {_fmt_num(float(payload[2]))})"
    if vtype == "node_ref" and isinstance(payload, Mapping):
        return _fmt_node_ref(str(payload.get("node_id", "")))
    if vtype == "blob_ref":
        handle = str(payload or "")
        for prefix, kw in (("@spline/", "spline"), ("@subd/", "subd"), ("@field/", "field"), ("@import/", "import")):
            if handle.startswith(prefix):
                return f"@{kw}({_fmt_str(handle[len(prefix):])})"
        return f"@blob_ref({_fmt_str(handle)})"
    if vtype == "external_ref":
        return f"@extref({_fmt_str(str(payload or ''))})"
    if vtype == "topo_ref_expr":
        return f"@topo({_fmt_str(str(payload or ''))})"
    if vtype == "list" and isinstance(payload, Sequence) and not isinstance(payload, (str, bytes)):
        return "[" + ", ".join(_fmt_ir_value(item) for item in payload) + "]"
    if vtype == "map" and isinstance(payload, Mapping):
        if not payload:
            return "{}"
        inner = ", ".join(
            f"{_fmt_key(str(k))} = {_fmt_ir_value(payload[k])}"
            for k in sorted(payload, key=str)
        )
        return f"{{ {inner} }}"
    return _fmt_str(json.dumps(dict(value), sort_keys=True))


def _is_plain_ident(text: str) -> bool:
    return bool(text) and (text[0].isalpha() or text[0] == "_") and text[0].isascii() and all(
        (ch.isalnum() and ch.isascii()) or ch == "_" for ch in text
    )


def _fmt_str(text: str) -> str:
    escaped = (
        text.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")
    )
    return f'"{escaped}"'


def _fmt_key(key: str) -> str:
    return key if _is_plain_ident(key) and key not in _KCL_KEYWORDS else _fmt_str(key)


def _fmt_binding(ident: str) -> str:
    return ident if _is_plain_ident(ident) and ident not in _KCL_KEYWORDS else _fmt_str(ident)


def _fmt_node_ref(ident: str) -> str:
    if _is_plain_ident(ident) and ident not in _KCL_KEYWORDS and ident not in _KCL_VALUE_KEYWORDS:
        return ident
    return f"node({_fmt_str(ident)})"


def _fmt_num(value: float) -> str:
    if value == 0.0:
        return "0"
    text = format(value, ".15g")
    return "0" if text == "-0" else text


def _fmt_vec3_cli(coords: tuple[float, float, float]) -> str:
    return ",".join(format(c, ".16g") for c in coords)


def _normalize_focus_nodes(focus_nodes: Sequence[str]) -> list[str]:
    if isinstance(focus_nodes, (str, bytes)) or not isinstance(focus_nodes, Sequence):
        raise TypeError("focus_nodes must be a sequence of node ID strings")
    normalized: list[str] = []
    seen: set[str] = set()
    for item in focus_nodes:
        if not isinstance(item, str) or not item.strip():
            raise ContextSliceError("each focus node ID must be a non-empty string")
        cleaned = item.strip()
        if cleaned not in seen:
            seen.add(cleaned)
            normalized.append(cleaned)
    if not normalized:
        raise ContextSliceError("focus_nodes must contain at least one node ID")
    return normalized


def _validate_roi_bounds(
    roi_bounds: tuple[Sequence[float], Sequence[float]] | None,
) -> tuple[tuple[float, float, float], tuple[float, float, float]] | None:
    if roi_bounds is None:
        return None
    if isinstance(roi_bounds, (str, bytes)) or len(roi_bounds) != 2:
        raise ValueError("roi_bounds must be a (min_xyz, max_xyz) pair")
    min_xyz = _vec3(roi_bounds[0], "roi_bounds[0]")
    max_xyz = _vec3(roi_bounds[1], "roi_bounds[1]")
    if any(lo > hi for lo, hi in zip(min_xyz, max_xyz, strict=True)):
        raise ValueError("roi_bounds minimum must be <= maximum on every axis")
    return min_xyz, max_xyz


def _vec3(values: Sequence[float], label: str) -> tuple[float, float, float]:
    if isinstance(values, (str, bytes)) or not isinstance(values, Sequence) or len(values) != 3:
        raise ValueError(f"{label} must contain three coordinates")
    coords: list[float] = []
    for item in values:
        if isinstance(item, bool) or not isinstance(item, (int, float)):
            raise TypeError(f"{label} coordinates must be real numbers")
        num = float(item)
        if not math.isfinite(num):
            raise ValueError(f"{label} coordinates must be finite")
        coords.append(num)
    return (coords[0], coords[1], coords[2])


def _optional_mapping(value: Any, label: str) -> Mapping[str, Any]:
    if value is None:
        return {}
    if not isinstance(value, Mapping):
        raise ContextSliceError(f"graph_dict {label!r} must be an object")
    return value


def _require_nonneg_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{label} must be an int")
    if value < 0:
        raise ValueError(f"{label} must be nonnegative")
    return value


def _resolve_cli_exec(
    cad_cli_bin: str | Path | None,
    *,
    allow_virtual: bool,
) -> str:
    if cad_cli_bin is not None:
        candidate = Path(cad_cli_bin)
        if allow_virtual:
            return str(candidate)
        if not candidate.is_file():
            raise ContextSliceError(f"cad-cli was not found at {candidate}")
        return str(candidate.resolve())
    found = discover_cad_cli()
    if found is not None:
        return str(found)
    if allow_virtual:
        return "cad-cli"
    raise ContextSliceError(
        "cad-cli was not found (pass cad_cli_bin, set CAD_CLI_BIN, or build target/debug/cad-cli)"
    )


def _invoke_runner(
    cmd: list[str],
    *,
    cli_runner: Callable[..., subprocess.CompletedProcess[str]] | None,
) -> subprocess.CompletedProcess[str]:
    try:
        if cli_runner is not None:
            return cli_runner(cmd)
        return subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=_CLI_TIMEOUT_SEC,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise ContextSliceError(f"cad-cli timed out after {_CLI_TIMEOUT_SEC}s") from exc
    except OSError as exc:
        raise ContextSliceError(f"failed to launch cad-cli: {exc}") from exc


def _parse_json_object(stdout: str) -> dict[str, Any] | None:
    text = stdout.strip()
    if not text:
        return None
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _cli_error_text(
    completed: subprocess.CompletedProcess[str],
    payload: Mapping[str, Any] | None,
) -> str:
    if isinstance(payload, Mapping):
        msg = payload.get("message")
        if isinstance(msg, str) and msg.strip():
            return msg.strip()
    detail = (completed.stderr or "").strip() or (completed.stdout or "").strip()
    return detail or f"cad-cli exited with code {completed.returncode}"


__all__ = [
    "ContextSliceError",
    "FocusedModelingContext",
    "build_focused_modeling_context",
    "emit_kcl_from_graph_dict",
    "slice_canonical_graph_dict",
]

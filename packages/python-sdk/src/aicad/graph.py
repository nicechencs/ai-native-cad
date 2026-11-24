"""Build a Canonical Graph IR document and a GraphPatch.

The dictionaries this module returns validate against
``schemas/graph-ir.schema.json``. ``formula`` and ``kernel_op_version`` are
always written, using ``null`` when there is no value, because the Rust
loader requires those keys.
"""

from __future__ import annotations

import copy
import json
import math
import unicodedata
from collections.abc import Mapping, Sequence
from typing import Any

from aicad._schema_constants import IR_SCHEMA_VERSION
from aicad.curves import encode_spline_blob, sha256_hex
from aicad.subd import SubDCage, encode_subd_blob
from aicad.units import Quantity, mm

IR_TYPES = {
    "scalar",
    "int",
    "bool",
    "string",
    "length",
    "angle",
    "force",
    "pressure",
    "mass",
    "density",
    "vec2_length",
    "vec3_length",
    "vec3_dir",
    "solid",
    "surface",
    "subd",
    "scalar_field",
    "vector_field",
    "sketch",
    "assembly",
    "component",
    "topo_ref",
    "blob_ref",
    "external_ref",
}

PAYLOAD_POLICIES = {"eager", "deferred_payload"}

UNIT_NODE_KINDS = {
    "sketch_2d",
    "primitive_box",
    "primitive_cylinder",
    "extrude",
    "revolve",
    "sweep",
    "loft_guided",
    "shell",
    "fillet",
    "chamfer",
    "draft",
    "mirror",
    "pattern_linear",
    "pattern_polar",
    "boolean_union",
    "boolean_subtract",
    "boolean_intersect",
    "hole",
    "direct_face_edit",
    "surface_boundary",
    "surface_trim",
    "surface_knit_to_solid",
    "subd_from_blob",
    "subd_to_nurbs",
    "field_sdf",
    "field_ramp",
    "field_from_fea_blob",
    "implicit_smooth_blend",
    "implicit_lattice_infill",
    "std_lib_call",
    "import_asset",
    "assembly_component",
    "assembly_mate",
    "tag_faces",
    "assert_gate",
}

TPMS_FAMILIES = {"gyroid", "schwarz_p", "diamond", "lidinoid"}

DEFAULT_UNITS = {
    "length": "mm",
    "angle": "deg",
    "force": "N",
    "pressure": "MPa",
    "mass": "kg",
}


def canonical_json(value: object) -> str:
    """Serialize JSON with sorted keys and no insignificant whitespace."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def ir_quantity(quantity: Quantity) -> dict[str, Any]:
    """Wrap a :class:`Quantity` as an IR value."""
    if not isinstance(quantity, Quantity):
        raise TypeError("expected a Quantity")
    return {"type": "quantity", "value": quantity.to_json()}


def ir_node_ref(node_id: str) -> dict[str, Any]:
    """Reference another node's only output port, which is always ``out``."""
    _identifier("node id", node_id)
    return {"type": "node_ref", "value": {"node_id": node_id, "port": "out"}}


def ir_blob_ref(handle: str) -> dict[str, Any]:
    """Reference a blob by the handle stored in the graph blob map."""
    _identifier("blob handle", handle)
    return {"type": "blob_ref", "value": handle}


def ir_external_ref(ref_id: str) -> dict[str, Any]:
    """Reference an external container entry stored in ``external_refs``."""
    _identifier("external ref id", ref_id)
    return {"type": "external_ref", "value": ref_id}


def ir_list(values: Sequence[Any]) -> dict[str, Any]:
    """Wrap a list of IR values."""
    return {"type": "list", "value": [coerce_ir_value(value) for value in values]}


def coerce_ir_value(value: Any) -> dict[str, Any]:
    """Accept a Quantity or an IR value object that already has ``type``."""
    if isinstance(value, Quantity):
        return ir_quantity(value)
    if isinstance(value, Mapping):
        if "type" not in value:
            raise ValueError("an IR value object needs a 'type' field")
        return copy.deepcopy(dict(value))
    raise TypeError(f"cannot encode {type(value).__name__} as an IR value")


class GraphBuilder:
    """Collect parameters, blobs, external refs, nodes, and drawing sheets for one project."""

    def __init__(self, graph_id: str) -> None:
        self.graph_id = _identifier("graph id", graph_id)
        self.units = dict(DEFAULT_UNITS)
        self.parameters: dict[str, dict[str, Any]] = {}
        self.blobs: dict[str, dict[str, Any]] = {}
        self.external_refs: dict[str, dict[str, Any]] = {}
        self.nodes: dict[str, dict[str, Any]] = {}
        self.outputs: list[str] = []
        self._payloads: dict[str, bytes] = {}
        self._drawings: dict[str, dict[str, Any]] = {}

    def add_parameter(
        self,
        name: str,
        ir_type: str,
        value: Any,
        formula: str | None = None,
    ) -> dict[str, Any]:
        """Add a literal parameter. ``value`` may be a Quantity or an IR value."""
        parameter_name = _identifier("parameter name", name)
        if ir_type not in IR_TYPES:
            raise ValueError(f"unknown ir_type {ir_type!r}")
        if formula is not None and not isinstance(formula, str):
            raise TypeError("formula must be a string or None")
        entry = {
            "name": parameter_name,
            "ir_type": ir_type,
            "value": _parameter_value(ir_type, value),
            "formula": formula,
        }
        self.parameters[parameter_name] = entry
        return copy.deepcopy(entry)

    def add_spline_blob(
        self,
        handle: str,
        points: Sequence[Sequence[float]],
        degree: int = 3,
        closed: bool = False,
    ) -> dict[str, Any]:
        """Encode ``points`` and register ``blobs/splines/<sha256>.bin``."""
        blob_handle = _identifier("blob handle", handle)
        payload = encode_spline_blob(points, degree=degree, closed=closed)
        digest = sha256_hex(payload)
        entry = {
            "handle": blob_handle,
            "kind": "spline",
            "sha256": digest,
            "byte_len": len(payload),
            "relative_path": f"blobs/splines/{digest}.bin",
        }
        self.blobs[blob_handle] = entry
        self._payloads[blob_handle] = payload
        return copy.deepcopy(entry)

    def add_subd_blob(self, handle: str, cage: SubDCage) -> dict[str, Any]:
        """Encode ``cage`` and register ``blobs/subd/<sha256>.bin``."""
        blob_handle = _identifier("blob handle", handle)
        payload = encode_subd_blob(cage)
        digest = sha256_hex(payload)
        entry = {
            "handle": blob_handle,
            "kind": "subd",
            "sha256": digest,
            "byte_len": len(payload),
            "relative_path": f"blobs/subd/{digest}.bin",
        }
        self.blobs[blob_handle] = entry
        self._payloads[blob_handle] = payload
        return copy.deepcopy(entry)

    def add_external_ref(
        self,
        ref_id: str,
        container_path: str,
        *,
        payload_policy: str = "eager",
        expected_sha256: str | None = None,
        target_graph_id: str | None = None,
        overrides: Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Register an OpenUSD-style cross-container composition reference."""
        identifier = _identifier("external ref id", ref_id)
        normalized_path = _identifier("container_path", container_path)
        if payload_policy not in PAYLOAD_POLICIES:
            raise ValueError(
                f"payload_policy must be 'eager' or 'deferred_payload', got {payload_policy!r}"
            )
        if expected_sha256 is not None:
            if not isinstance(expected_sha256, str):
                raise TypeError("expected_sha256 must be a 64-character hex string or None")
            if len(expected_sha256) != 64 or any(
                ch not in "0123456789abcdefABCDEF" for ch in expected_sha256
            ):
                raise ValueError(f"invalid expected_sha256 digest {expected_sha256!r}")
        if target_graph_id is not None:
            if not isinstance(target_graph_id, str):
                raise TypeError("target_graph_id must be a string or None")
            target_graph_id = _identifier("target_graph_id", target_graph_id)

        normalized_overrides: dict[str, dict[str, Any]] = {}
        if overrides is not None:
            if not isinstance(overrides, Mapping):
                raise TypeError("overrides must be a mapping of parameter name to IR value")
            for raw_key in sorted(overrides, key=str):
                if not isinstance(raw_key, str):
                    raise TypeError("override parameter name must be a string")
                param_key = _identifier("override parameter name", raw_key)
                normalized_overrides[param_key] = _coerce_override_value(overrides[raw_key])

        entry: dict[str, Any] = {
            "ref_id": identifier,
            "container_path": normalized_path,
            "payload_policy": payload_policy,
        }
        if expected_sha256 is not None:
            entry["expected_sha256"] = expected_sha256
        if target_graph_id is not None:
            entry["target_graph_id"] = target_graph_id
        if normalized_overrides:
            entry["overrides"] = normalized_overrides

        self.external_refs[identifier] = entry
        return copy.deepcopy(entry)

    def external_ref_value(self, ref_id: str) -> dict[str, Any]:
        """Return an ``external_ref`` IR value dictionary for ``ref_id``."""
        return ir_external_ref(ref_id)

    def add_node(
        self,
        node_id: str,
        name: str,
        kind: str | Mapping[str, Any],
        inputs: Mapping[str, Any],
        output_type: str,
        state: Mapping[str, Any] | None = None,
        *,
        metadata: Mapping[str, str] | None = None,
    ) -> dict[str, Any]:
        """Add one graph node. ``kind`` is a name or a kind object."""
        identifier = _identifier("node id", node_id)
        if not isinstance(name, str):
            raise TypeError("node name must be a string")
        if output_type not in IR_TYPES:
            raise ValueError(f"unknown output_type {output_type!r}")
        if not isinstance(inputs, Mapping):
            raise TypeError("inputs must be a mapping of port name to IR value")
        normalized_meta = _normalize_metadata(metadata)
        node = {
            "id": identifier,
            "name": name,
            "kind": _normalize_kind(kind),
            "inputs": {
                str(port): coerce_ir_value(port_value) for port, port_value in inputs.items()
            },
            "output_type": output_type,
            "state": _normalize_state(state),
            "kernel_op_version": None,
            "metadata": normalized_meta,
        }
        self.nodes[identifier] = node
        return copy.deepcopy(node)

    def add_tpms_lattice(
        self,
        node_id: str,
        domain_node_id: str,
        family: str = "gyroid",
        period_mm: float = 6.0,
        thickness_mm: float = 1.2,
    ) -> dict[str, Any]:
        """Add a TPMS scalar field and the lattice solid that fills ``domain_node_id``.

        ``node_id`` is the ``implicit_lattice_infill`` solid. The field node
        is ``{node_id}_field`` and uses ``field_tpms``. ``period_mm`` connects
        to the field ``period`` port. ``thickness_mm`` connects to the lattice
        ``wall_thickness`` port. Both lengths are millimetres.
        """
        lattice_id = _identifier("node id", node_id)
        domain_id = _identifier("domain node id", domain_node_id)
        if family not in TPMS_FAMILIES:
            raise ValueError(
                "field_tpms family must be gyroid, schwarz_p, diamond, or lidinoid"
            )
        field_id = f"{lattice_id}_field"
        if lattice_id in self.nodes or field_id in self.nodes:
            raise ValueError(f"node {lattice_id!r} or {field_id!r} already exists")
        domain = self.nodes.get(domain_id)
        if domain is None:
            raise ValueError(f"domain node {domain_id!r} is not a node")
        if domain["output_type"] != "solid":
            raise ValueError(f"domain node {domain_id!r} must output a solid")
        period = mm(period_mm)
        thickness = mm(thickness_mm)
        label = family.replace("_", " ")
        field = self.add_node(
            field_id,
            f"{label} field",
            {"kind": "field_tpms", "family": family},
            {"period": period},
            "scalar_field",
        )
        lattice = self.add_node(
            lattice_id,
            f"{label} lattice",
            "implicit_lattice_infill",
            {
                "solid": ir_node_ref(domain_id),
                "field": ir_node_ref(field_id),
                "wall_thickness": thickness,
            },
            "solid",
        )
        return {"field": field, "lattice": lattice}

    def add_drawing_sheet(self, sheet: dict[str, Any]) -> dict[str, Any]:
        """Register one associative drawing sheet, keyed by ``sheet_id``.

        :meth:`aicad.container.save_aicad_directory` writes it to
        ``drawings/<sheet_id>.drawing.json``.
        """
        if not isinstance(sheet, Mapping):
            raise TypeError("sheet must be a drawing object")
        sheet_id = _sheet_id(sheet.get("sheet_id"))
        normalized = {
            "sheet_id": sheet_id,
            "title": _required_text(sheet.get("title"), "sheet title"),
            "paper_size": _required_text(sheet.get("paper_size"), "paper size"),
            "views": _drawing_views(sheet.get("views", [])),
            "dimensions": _drawing_dimensions(sheet.get("dimensions", [])),
        }
        # Reject non-finite numbers before the container writer sees the sheet.
        json.dumps(normalized, allow_nan=False)
        self._drawings[sheet_id] = normalized
        return copy.deepcopy(normalized)

    def set_outputs(self, node_ids: Sequence[str]) -> list[str]:
        """Replace the graph output list. Each id must already be a node."""
        outputs: list[str] = []
        seen: set[str] = set()
        for node_id in node_ids:
            identifier = _identifier("output node id", node_id)
            if identifier in seen:
                raise ValueError(f"duplicate output {identifier!r}")
            if identifier not in self.nodes:
                raise ValueError(f"output {identifier!r} is not a node")
            seen.add(identifier)
            outputs.append(identifier)
        self.outputs = outputs
        return list(outputs)

    def blob_payloads(self) -> dict[str, bytes]:
        """Return payload bytes keyed by their container-relative path."""
        payloads: dict[str, bytes] = {}
        for handle, entry in self.blobs.items():
            payloads[entry["relative_path"]] = self._payloads[handle]
        return payloads

    def drawing_sheets(self) -> dict[str, dict[str, Any]]:
        """Return registered drawing sheets keyed by ``sheet_id``."""
        return copy.deepcopy(self._drawings)

    def to_graph_dict(self) -> dict[str, Any]:
        """Return a schema-shaped CanonicalGraph object."""
        graph: dict[str, Any] = {
            "ir_schema_version": IR_SCHEMA_VERSION,
            "graph_id": self.graph_id,
            "units": dict(self.units),
            "parameters": copy.deepcopy(self.parameters),
            "blobs": copy.deepcopy(self.blobs),
        }
        if self.external_refs:
            graph["external_refs"] = {
                key: copy.deepcopy(self.external_refs[key])
                for key in sorted(self.external_refs)
            }
        graph["nodes"] = copy.deepcopy(self.nodes)
        graph["outputs"] = list(self.outputs)
        return graph

    def to_canonical_json(self) -> str:
        """Return the graph as compact JSON with sorted keys."""
        return canonical_json(self.to_graph_dict())

    def build_patch(
        self,
        patch_id: str,
        ops: list[dict[str, Any]],
        description: str = "",
    ) -> dict[str, Any]:
        """Wrap patch operations in the GraphPatch object Rust applies."""
        if not isinstance(description, str):
            raise TypeError("description must be a string")
        if not isinstance(ops, list) or not all(isinstance(op, Mapping) for op in ops):
            raise TypeError("ops must be a list of objects")
        return {
            "patch_id": _identifier("patch id", patch_id),
            "description": description,
            "ops": copy.deepcopy(ops),
        }


def _parameter_value(ir_type: str, value: Any) -> dict[str, Any]:
    if isinstance(value, Quantity):
        if value.dimension != ir_type:
            raise TypeError(
                f"parameter type {ir_type} does not match quantity dimension {value.dimension}"
            )
        return ir_quantity(value)
    if isinstance(value, Mapping):
        return coerce_ir_value(value)
    if ir_type == "scalar" and _is_real(value):
        return {"type": "scalar", "value": float(value)}
    if ir_type == "int" and isinstance(value, int) and not isinstance(value, bool):
        return {"type": "int", "value": value}
    if ir_type == "bool" and isinstance(value, bool):
        return {"type": "bool", "value": value}
    if ir_type == "string" and isinstance(value, str):
        return {"type": "string", "value": value}
    raise TypeError(f"cannot use {type(value).__name__} as a {ir_type} parameter")


def _normalize_kind(kind: str | Mapping[str, Any]) -> dict[str, Any]:
    if isinstance(kind, str):
        raw: dict[str, Any] = {"kind": kind}
    elif isinstance(kind, Mapping) and isinstance(kind.get("kind"), str):
        raw = dict(kind)
    else:
        raise TypeError("kind must be a node kind name or an object with 'kind'")

    name = raw["kind"]
    if name == "field_tpms":
        family = raw.get("family")
        if family not in TPMS_FAMILIES:
            raise ValueError(
                "field_tpms family must be gyroid, schwarz_p, diamond, or lidinoid"
            )
        return {"kind": "field_tpms", "family": family}
    if name == "parameter_ref":
        param_id = raw.get("param_id")
        if not isinstance(param_id, str):
            raise ValueError("parameter_ref requires param_id")
        return {"kind": "parameter_ref", "param_id": _identifier("param_id", param_id)}
    if name not in UNIT_NODE_KINDS:
        raise ValueError(f"unknown node kind {name!r}")
    extra = set(raw) - {"kind"}
    if extra:
        raise ValueError(f"{name} does not accept kind fields {sorted(extra)}")
    return {"kind": name}


def _normalize_state(state: Mapping[str, Any] | None) -> dict[str, Any]:
    if state is None:
        return {"status": "valid"}
    if not isinstance(state, Mapping) or not isinstance(state.get("status"), str):
        raise TypeError("state must be an object with a status string")
    return copy.deepcopy(dict(state))


def _normalize_metadata(metadata: Mapping[str, str] | None) -> dict[str, str]:
    if metadata is None:
        return {}
    if not isinstance(metadata, Mapping):
        raise TypeError("metadata must be a mapping of string keys to string values")
    normalized: dict[str, str] = {}
    for key in sorted(metadata, key=str):
        if not isinstance(key, str):
            raise TypeError("metadata key must be a string")
        value = metadata[key]
        if not isinstance(value, str):
            raise TypeError(f"metadata[{key!r}] must be a string")
        normalized[_identifier("metadata key", key)] = value
    return normalized


def _coerce_override_value(value: Any) -> dict[str, Any]:
    if isinstance(value, Quantity):
        return ir_quantity(value)
    if isinstance(value, Mapping):
        return coerce_ir_value(value)
    if isinstance(value, bool):
        return {"type": "bool", "value": value}
    if isinstance(value, int):
        return {"type": "int", "value": value}
    if _is_real(value):
        number = _finite_number(value, "override scalar")
        return {"type": "scalar", "value": 0.0 if number == 0.0 else number}
    if isinstance(value, str):
        return {"type": "string", "value": value}
    raise TypeError(f"cannot encode {type(value).__name__} as an override IR value")


def _identifier(label: str, value: object) -> str:
    if not isinstance(value, str) or not any(not character.isspace() for character in value):
        raise ValueError(f"{label} must contain a non-space character")
    return value


def _sheet_id(value: object) -> str:
    identifier = _identifier("sheet id", value)
    if (
        "/" in identifier
        or "\\" in identifier
        or identifier in {".", ".."}
        or identifier.endswith((" ", "."))
        or any(
            unicodedata.category(character) == "Cc" or character in ':*?"<>|'
            for character in identifier
        )
    ):
        raise ValueError(f"unsafe sheet id {identifier!r}")
    return identifier


def _required_text(value: object, label: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{label} must be a string")
    return value


def _drawing_views(value: object) -> list[dict[str, Any]]:
    if not isinstance(value, Sequence) or isinstance(value, (str, bytes)):
        raise TypeError("drawing views must be a list")
    return [_drawing_view(view, index) for index, view in enumerate(value)]


def _drawing_view(view: object, index: int) -> dict[str, Any]:
    if not isinstance(view, Mapping):
        raise TypeError(f"drawing view {index} must be an object")
    return {
        "view_id": _identifier("view id", view.get("view_id")),
        "kind": _drawing_view_kind(view.get("kind"), index),
        "placement_mm": _number_list(view.get("placement_mm"), 2, f"view {index} placement_mm"),
        "scale": _finite_number(view.get("scale"), f"view {index} scale"),
    }


def _drawing_view_kind(kind: object, index: int) -> dict[str, Any]:
    if isinstance(kind, str):
        payload: Mapping[str, Any] = {"kind": kind}
    elif isinstance(kind, Mapping):
        payload = kind
    else:
        raise TypeError(f"drawing view {index} kind must be a string or object")
    name = payload.get("kind")
    if name in {"front", "top", "right", "isometric"}:
        return {"kind": name}
    if name == "section":
        return {
            "kind": "section",
            "label": _required_text(payload.get("label"), f"view {index} section label"),
            "plane_origin_mm": _number_list(
                payload.get("plane_origin_mm"), 3, f"view {index} plane_origin_mm"
            ),
            "plane_normal": _number_list(
                payload.get("plane_normal"), 3, f"view {index} plane_normal"
            ),
        }
    if name == "detail":
        return {
            "kind": "detail",
            "scale": _finite_number(payload.get("scale"), f"view {index} detail scale"),
            "center_mm": _number_list(payload.get("center_mm"), 2, f"view {index} center_mm"),
            "radius_mm": _finite_number(payload.get("radius_mm"), f"view {index} radius_mm"),
        }
    raise ValueError(
        f"drawing view {index} kind must be front, top, right, isometric, section, or detail"
    )


def _drawing_dimensions(value: object) -> list[dict[str, Any]]:
    if not isinstance(value, Sequence) or isinstance(value, (str, bytes)):
        raise TypeError("drawing dimensions must be a list")
    return [_drawing_dimension(dimension, index) for index, dimension in enumerate(value)]


def _drawing_dimension(dimension: object, index: int) -> dict[str, Any]:
    if not isinstance(dimension, Mapping):
        raise TypeError(f"drawing dimension {index} must be an object")
    override = dimension.get("override_text")
    if override is not None and not isinstance(override, str):
        raise TypeError(f"drawing dimension {index} override_text must be a string or null")
    anchors = dimension.get("anchors", [])
    if not isinstance(anchors, Sequence) or isinstance(anchors, (str, bytes)):
        raise TypeError(f"drawing dimension {index} anchors must be a list")
    return {
        "id": _identifier("dimension id", dimension.get("id")),
        "label": _required_text(dimension.get("label"), f"drawing dimension {index} label"),
        "anchors": copy.deepcopy(list(anchors)),
        "override_text": override,
        "placement_mm": _number_list(
            dimension.get("placement_mm"), 2, f"drawing dimension {index} placement_mm"
        ),
    }


def _number_list(value: object, count: int, label: str) -> list[float]:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence) or len(value) != count:
        raise ValueError(f"{label} must have {count} numbers")
    return [_finite_number(item, label) for item in value]


def _finite_number(value: object, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"{label} must be a real number")
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"{label} must be finite")
    return number


def _is_real(value: object) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)

"""Pythonic builder for V2 SemanticCommand batches.

Each modeling call records one command in the shape produced by
``crates/cad-api``: ``{"command": "<snake_name>", "args": {...}}``.
Compiling the batch locally builds a CanonicalGraph the ``.aicad`` writer
already knows how to save. A running ``cad-cli`` is optional.
"""

from __future__ import annotations

import copy
import json
import math
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from numbers import Real
from typing import Any

from aicad.curves import decode_spline_blob, encode_spline_blob, sha256_hex
from aicad.graph import GraphBuilder, ir_quantity
from aicad.units import Quantity, deg, mm


def _semantic_command(method: Any) -> Any:
    """Mark a SemanticModelBuilder method whose name is a cad-api command."""
    method._aicad_semantic_command = True
    return method


CONTRACT_NODE_ID = "__design_contract"
_FNV_OFFSET = 0xCBF29CE484222325
_FNV_PRIME = 0x100000001B3
_IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_PARAM_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_PART_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_]*$")
_FORMULA_WORDS = {
    "mm",
    "cm",
    "m",
    "in",
    "um",
    "deg",
    "rad",
    "N",
    "kN",
    "Pa",
    "MPa",
    "g",
    "kg",
    "min",
    "max",
    "abs",
    "sqrt",
    "sin",
    "cos",
    "tan",
}
_BODY_OPS = {
    "new_body": "new_body",
    "new": "new_body",
    "join": "join",
    "cut": "cut",
    "intersect": "intersect",
}
_BOOLEAN_OPS = {
    "union": "union",
    "subtract": "subtract",
    "cut": "subtract",
    "intersect": "intersect",
}
_FASTENER_STANDARDS = {
    "iso4762_counterbore",
    "iso10642_countersink",
    "metric_tap_drill",
    "metric_clearance",
}
_CLEARANCE = {"close", "normal", "loose"}
_MATE_KINDS = {
    "coincident",
    "concentric",
    "parallel",
    "perpendicular",
    "distance",
    "angle",
    "gear",
}
_PLANES = {"xy", "xz", "yz"}
_ANCHORS = {"point", "start", "end", "center", "midpoint"}
_FEATURE_COMMANDS = {
    "create_sketch": "sketch",
    "create_primitive_box": "box",
    "create_primitive_cylinder": "cylinder",
    "extrude_sketch": "extrude",
    "revolve_sketch": "revolve",
    "sweep_profile": "sweep",
    "loft_sections": "loft",
    "create_hole": "hole",
    "fillet_edges": "fillet",
    "chamfer_edges": "chamfer",
    "shell_solid": "shell",
    "draft_faces": "draft",
    "pattern_feature": "pattern",
    "mirror_feature": "mirror",
    "boolean_solids": "boolean",
    "direct_face_edit": "face_edit",
    "insert_component": "component",
    "add_mate": "mate",
    "tag_topology": "tag",
    "add_design_assertion": "assertion",
}


def fnv1a64(data: bytes) -> int:
    """FNV-1a 64-bit hash used by ``cad-api`` patch ids."""
    digest = _FNV_OFFSET
    for byte in data:
        digest ^= byte
        digest = (digest * _FNV_PRIME) & 0xFFFFFFFFFFFFFFFF
    return digest


def semantic_patch_id(commands: Sequence[Mapping[str, Any]]) -> str:
    """Return ``sem-empty`` or ``sem-`` plus the FNV-1a 64 of the command JSON."""
    if len(commands) == 0:
        return "sem-empty"
    payload = _compact_json(list(commands)).encode("utf-8")
    return f"sem-{fnv1a64(payload):016x}"


def line_entity(
    entity_id: str,
    start_mm: Sequence[float],
    end_mm: Sequence[float],
    *,
    construction: bool = False,
) -> dict[str, Any]:
    """One sketch line. Coordinates are millimetres."""
    _flag(construction, "construction")
    return {
        "kind": "line",
        "id": _entity_id(entity_id),
        "start_mm": _vec2(start_mm),
        "end_mm": _vec2(end_mm),
        "construction": construction,
    }


def circle_entity(
    entity_id: str,
    center_mm: Sequence[float],
    radius_mm: float,
    *,
    construction: bool = False,
) -> dict[str, Any]:
    """One sketch circle. ``radius_mm`` must be positive."""
    _flag(construction, "construction")
    radius = _num(radius_mm, "radius_mm")
    if radius <= 0.0:
        raise ValueError("circle radius must be positive")
    return {
        "kind": "circle",
        "id": _entity_id(entity_id),
        "center_mm": _vec2(center_mm),
        "radius_mm": radius,
        "construction": construction,
    }


def rectangle_entity(
    entity_id: str,
    origin_mm: Sequence[float],
    width_mm: float,
    height_mm: float,
    *,
    centered: bool = False,
    construction: bool = False,
) -> dict[str, Any]:
    """One sketch rectangle. Width and height are millimetres."""
    _flag(centered, "centered")
    _flag(construction, "construction")
    width = _num(width_mm, "width_mm")
    height = _num(height_mm, "height_mm")
    if width <= 0.0 or height <= 0.0:
        raise ValueError("rectangle width and height must be positive")
    return {
        "kind": "rectangle",
        "id": _entity_id(entity_id),
        "origin_mm": _vec2(origin_mm),
        "width_mm": width,
        "height_mm": height,
        "centered": centered,
        "construction": construction,
    }


def spline_blob_entity(
    entity_id: str,
    blob: str,
    *,
    closed: bool = False,
    construction: bool = False,
) -> dict[str, Any]:
    """A sketch curve stored in a spline blob. ``blob`` is the blob handle."""
    _flag(closed, "closed")
    _flag(construction, "construction")
    if not isinstance(blob, str) or not blob.strip():
        raise ValueError("spline blob handle cannot be empty")
    return {
        "kind": "spline_blob",
        "id": _entity_id(entity_id),
        "blob": blob,
        "closed": closed,
        "construction": construction,
    }


def profile_loop(
    loop_id: str,
    entity_ids: Sequence[str],
    *,
    is_outer: bool = True,
) -> dict[str, Any]:
    """A profile made from sketch entity ids. ``is_outer`` defaults to true."""
    _flag(is_outer, "is_outer")
    if not isinstance(loop_id, str) or not loop_id.strip():
        raise ValueError("profile id cannot be empty")
    if not entity_ids:
        raise ValueError(f"sketch profile {loop_id!r} cannot be empty")
    ids = []
    for entity_id in entity_ids:
        if not isinstance(entity_id, str) or not entity_id.strip():
            raise ValueError("profile entity id cannot be empty")
        ids.append(entity_id)
    return {"id": loop_id, "entity_ids": ids, "is_outer": is_outer}


def polyline_entities(
    points: Sequence[Sequence[float]],
    *,
    prefix: str = "seg",
    closed: bool = True,
    construction: bool = False,
) -> list[dict[str, Any]]:
    """Turn a 2D point list into connected line entities."""
    _flag(closed, "closed")
    _flag(construction, "construction")
    if not isinstance(prefix, str) or not prefix.strip():
        raise ValueError("polyline prefix cannot be empty")
    coords = [_vec2(point) for point in points]
    if closed and len(coords) < 3:
        raise ValueError("a closed polyline needs at least 3 points")
    if not closed and len(coords) < 2:
        raise ValueError("a polyline needs at least 2 points")
    count = len(coords) if closed else len(coords) - 1
    return [
        line_entity(
            f"{prefix}_{index + 1}",
            coords[index],
            coords[(index + 1) % len(coords)],
            construction=construction,
        )
        for index in range(count)
    ]


@dataclass
class _LiveNode:
    id: str
    name: str
    kind: str
    output_type: str
    inputs: dict[str, Any]
    metadata: dict[str, str] = field(default_factory=dict)
    state: dict[str, str] = field(default_factory=lambda: {"status": "valid"})
    refs: set[str] = field(default_factory=set)
    outline: bool = True


class SemanticModelBuilder:
    """Record a SemanticCommand batch and the graph those commands describe."""

    def __init__(self) -> None:
        self.commands: list[dict[str, Any]] = []
        self.parameters: dict[str, dict[str, Any]] = {}
        self.nodes: dict[str, _LiveNode] = {}
        self.outline: list[str] = []
        self.blobs: dict[str, dict[str, Any]] = {}

    @_semantic_command
    def upsert_parameter(self, name: str, value: Any, formula: str | None = None) -> str:
        """Add or replace a named parameter. ``name`` is the parameter id.

        ``value`` is a :class:`Quantity`, ``int``, ``bool``, or ``str``.
        A bare float is rejected so a length is not stored without a unit.
        ``formula`` is omitted from the command when it is ``None``.
        """
        parameter_name = _parameter_name(name)
        ir_type, ir_value, raw_value = _parameter_binding(value)
        if formula is not None:
            if not isinstance(formula, str) or not formula.strip():
                raise ValueError("formula must be a non-empty string or None")
            for dep in _formula_dependencies(formula, parameter_name):
                if dep not in self.parameters:
                    raise ValueError(f"formula refers to unknown parameter {dep!r}")
        args: dict[str, Any] = {"name": parameter_name, "ir_type": ir_type, "value": ir_value}
        if formula is not None:
            args["formula"] = formula
        self._ingest(
            {"command": "upsert_parameter", "args": args},
            pending_parameter={
                "name": parameter_name,
                "ir_type": ir_type,
                "raw_value": raw_value,
                "formula": formula,
            },
        )
        return parameter_name

    @_semantic_command
    def set_design_contract(
        self,
        part_name: str,
        *,
        summary: str | None = None,
        material: str | None = None,
        manufacturing_method: str | None = None,
        attributes: Mapping[str, str] | None = None,
    ) -> str:
        """Store the part contract on the reserved ``__design_contract`` node."""
        if not isinstance(part_name, str) or _PART_NAME.fullmatch(part_name) is None:
            raise ValueError(
                "design contract part_name must start with a letter and contain only letters, digits, and underscores"
            )
        contract: dict[str, Any] = {"part_name": part_name}
        if summary is not None:
            contract["summary"] = _text(summary, "summary")
        if material is not None:
            contract["material"] = _text(material, "material")
        if manufacturing_method is not None:
            contract["manufacturing_method"] = _text(manufacturing_method, "manufacturing_method")
        if attributes:
            if not isinstance(attributes, Mapping):
                raise TypeError("attributes must be a mapping of strings")
            contract["attributes"] = {
                _text(key, "attribute name"): _text(attributes[key], f"attribute {key!r}")
                for key in sorted(attributes)
            }
        self._ingest({"command": "set_design_contract", "args": {"contract": contract}})
        return CONTRACT_NODE_ID

    def add_spline_blob(
        self,
        handle: str,
        points: Sequence[Sequence[float]],
        degree: int = 3,
        closed: bool = False,
    ) -> str:
        """Store a spline blob on the document. This is not a SemanticCommand.

        Sketch entities reference ``handle``. The bytes are written when the
        project is saved.
        """
        blob_handle = _handle(handle)
        if blob_handle in self.blobs:
            raise ValueError(f"blob {blob_handle!r} already exists")
        payload = encode_spline_blob(points, degree=degree, closed=closed)
        decoded = decode_spline_blob(payload)
        self.blobs[blob_handle] = {
            "points": decoded["points"],
            "degree": decoded["degree"],
            "closed": decoded["closed"],
            "sha256": sha256_hex(payload),
            "byte_len": len(payload),
            "relative_path": f"blobs/splines/{sha256_hex(payload)}.bin",
        }
        return blob_handle

    def create_box(
        self,
        name: str,
        width: Any,
        depth: Any,
        height: Any,
        *,
        node_id: str | None = None,
        origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
    ) -> str:
        """Box primitive. Bare numbers are millimetres. A string names a length parameter."""
        identifier = self._allocate(name, node_id, "box")
        self._ingest(
            {
                "command": "create_primitive_box",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "width": self._dim(width, "length"),
                    "depth": self._dim(depth, "length"),
                    "height": self._dim(height, "length"),
                    "origin_mm": _vec3(origin_mm),
                },
            }
        )
        return identifier

    def create_cylinder(
        self,
        name: str,
        radius: Any,
        height: Any,
        *,
        node_id: str | None = None,
        origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
        axis: Sequence[float] = (0.0, 0.0, 1.0),
    ) -> str:
        """Cylinder primitive. Bare numbers are millimetres."""
        identifier = self._allocate(name, node_id, "cylinder")
        self._ingest(
            {
                "command": "create_primitive_cylinder",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "radius": self._dim(radius, "length"),
                    "height": self._dim(height, "length"),
                    "origin_mm": _vec3(origin_mm),
                    "axis": _direction(axis, "cylinder axis"),
                },
            }
        )
        return identifier

    @_semantic_command
    def create_sketch(
        self,
        name: str,
        plane: str | Mapping[str, Any] = "xy",
        *,
        node_id: str | None = None,
        entities: Sequence[Mapping[str, Any]] | None = None,
        profiles: Sequence[Mapping[str, Any]] | None = None,
        constraints: Sequence[Mapping[str, Any]] | None = None,
    ) -> str:
        """Sketch on ``xy``, ``xz``, ``yz``, or a plane object."""
        identifier = self._allocate(name, node_id, "sketch")
        normalized_entities = _normalize_entities(entities)
        normalized_profiles = _normalize_profiles(profiles, normalized_entities)
        normalized_constraints = _normalize_constraints(constraints, normalized_entities)
        for entity in normalized_entities:
            if entity["kind"] == "spline_blob" and entity["blob"] not in self.blobs:
                raise ValueError(f"blob {entity['blob']!r} was not found")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "plane": _plane(plane),
        }
        if normalized_entities:
            args["entities"] = normalized_entities
        if normalized_profiles:
            args["profiles"] = normalized_profiles
        if normalized_constraints:
            args["constraints"] = normalized_constraints
        self._ingest({"command": "create_sketch", "args": args})
        return identifier

    def extrude(
        self,
        name: str,
        sketch: str,
        distance: Any,
        *,
        node_id: str | None = None,
        direction: Sequence[float] | None = None,
        symmetric: bool = False,
        operation: str = "new_body",
        target: str | None = None,
    ) -> str:
        """Extrude a sketch. A numeric ``distance`` is millimetres."""
        _flag(symmetric, "symmetric")
        identifier = self._allocate(name, node_id, "extrude")
        self._require_node(sketch, "sketch")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "sketch": sketch,
            "distance": self._dim(distance, "length"),
        }
        if direction is not None:
            args["direction"] = _direction(direction, "extrude direction")
        args["symmetric"] = symmetric
        args["operation"] = _body_op(operation)
        if target is not None:
            self._require_node(target, "solid")
            args["target"] = target
        self._ingest({"command": "extrude_sketch", "args": args})
        return identifier

    def revolve(
        self,
        name: str,
        sketch: str,
        angle: Any,
        *,
        node_id: str | None = None,
        axis_origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
        axis_direction: Sequence[float] = (0.0, 0.0, 1.0),
        operation: str = "new_body",
        target: str | None = None,
    ) -> str:
        """Revolve a sketch. A numeric ``angle`` is degrees."""
        identifier = self._allocate(name, node_id, "revolve")
        self._require_node(sketch, "sketch")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "sketch": sketch,
            "axis_origin_mm": _vec3(axis_origin_mm),
            "axis_direction": _direction(axis_direction, "revolve axis"),
            "angle": self._dim(angle, "angle"),
            "operation": _body_op(operation),
        }
        if target is not None:
            self._require_node(target, "solid")
            args["target"] = target
        self._ingest({"command": "revolve_sketch", "args": args})
        return identifier

    def sweep(
        self,
        name: str,
        profile: str,
        path: str | Sequence[Sequence[float]],
        *,
        node_id: str | None = None,
        keep_normal: bool = False,
    ) -> str:
        """Sweep a sketch profile along another sketch or a 3D polyline."""
        _flag(keep_normal, "keep_normal")
        identifier = self._allocate(name, node_id, "sweep")
        self._require_node(profile, "sketch")
        self._ingest(
            {
                "command": "sweep_profile",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "profile": profile,
                    "path": self._sweep_path(path),
                    "keep_normal": keep_normal,
                },
            }
        )
        return identifier

    def loft(
        self,
        name: str,
        sections: Sequence[str],
        *,
        node_id: str | None = None,
        guides: Sequence[str] | None = None,
        closed: bool = False,
        as_surface: bool = False,
    ) -> str:
        """Loft through two or more sketch sections."""
        _flag(closed, "closed")
        _flag(as_surface, "as_surface")
        identifier = self._allocate(name, node_id, "loft")
        section_ids = self._sketch_ids(sections, "loft section")
        if len(section_ids) < 2:
            raise ValueError("loft requires at least 2 section sketches")
        guide_ids = self._sketch_ids(guides or [], "loft guide")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "sections": section_ids,
        }
        if guide_ids:
            args["guides"] = guide_ids
        args["closed"] = closed
        args["as_surface"] = as_surface
        self._ingest({"command": "loft_sections", "args": args})
        return identifier

    @_semantic_command
    def create_hole(
        self,
        name: str,
        solid: str,
        diameter: Any,
        *,
        node_id: str | None = None,
        origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
        axis: Sequence[float] = (0.0, 0.0, -1.0),
        depth: Any = "through_all",
        profile: str = "simple",
        target_face: str | None = None,
        bore_diameter: Any = None,
        bore_depth: Any = None,
        sink_diameter: Any = None,
        sink_angle: Any = None,
    ) -> str:
        """Generic hole. ``profile`` is ``simple``, ``counterbore``, or ``countersink``."""
        identifier = self._allocate(name, node_id, "hole")
        self._require_node(solid, "solid")
        definition: dict[str, Any] = {
            "hole": "generic",
            "diameter": self._dim(diameter, "length"),
            "depth": self._hole_depth(depth),
            "profile": self._hole_profile(
                profile,
                bore_diameter,
                bore_depth,
                sink_diameter,
                sink_angle,
            ),
        }
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "solid": solid,
        }
        face = _optional_text(target_face, "target_face")
        if face is not None:
            args["target_face"] = face
        args["origin_mm"] = _vec3(origin_mm)
        args["axis"] = _direction(axis, "hole axis")
        args["definition"] = definition
        self._ingest({"command": "create_hole", "args": args})
        return identifier

    def create_std_fastener_hole(
        self,
        name: str,
        solid: str,
        standard: str,
        designation: str,
        *,
        node_id: str | None = None,
        clearance_class: str | None = None,
        depth: Any = "through_all",
        thread_depth: Any = None,
        origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
        axis: Sequence[float] = (0.0, 0.0, -1.0),
        target_face: str | None = None,
    ) -> str:
        """Catalog hole such as ``iso4762_counterbore`` or ``metric_tap_drill``.

        The command is still ``create_hole`` with ``hole: standard``.
        """
        if standard not in _FASTENER_STANDARDS:
            raise ValueError(
                "standard must be iso4762_counterbore, iso10642_countersink, "
                f"metric_tap_drill, or metric_clearance, got {standard!r}"
            )
        if not isinstance(designation, str) or not designation.strip():
            raise ValueError("fastener designation cannot be empty")
        if clearance_class is not None and clearance_class not in _CLEARANCE:
            raise ValueError("clearance_class must be close, normal, or loose")
        identifier = self._allocate(name, node_id, "hole")
        self._require_node(solid, "solid")
        definition: dict[str, Any] = {
            "hole": "standard",
            "standard": standard,
            "designation": designation,
        }
        if clearance_class is not None:
            definition["clearance_class"] = clearance_class
        definition["depth"] = self._hole_depth(depth)
        if thread_depth is not None:
            definition["thread_depth"] = self._dim(thread_depth, "length")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "solid": solid,
        }
        face = _optional_text(target_face, "target_face")
        if face is not None:
            args["target_face"] = face
        args["origin_mm"] = _vec3(origin_mm)
        args["axis"] = _direction(axis, "hole axis")
        args["definition"] = definition
        self._ingest({"command": "create_hole", "args": args})
        return identifier

    def fillet(
        self,
        name: str,
        solid: str,
        radius: Any,
        *,
        node_id: str | None = None,
        edges: Sequence[str] | None = None,
    ) -> str:
        """Round edges of a solid. A numeric radius is millimetres."""
        identifier = self._allocate(name, node_id, "fillet")
        self._require_node(solid, "solid")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "solid": solid,
        }
        selectors = _selectors(edges, "edge")
        if selectors:
            args["edges"] = selectors
        args["radius"] = self._dim(radius, "length")
        self._ingest({"command": "fillet_edges", "args": args})
        return identifier

    def chamfer(
        self,
        name: str,
        solid: str,
        distance: Any,
        *,
        node_id: str | None = None,
        edges: Sequence[str] | None = None,
        distance2: Any = None,
        angle: Any = None,
    ) -> str:
        """Chamfer edges. Distances are millimetres. ``angle`` is degrees."""
        identifier = self._allocate(name, node_id, "chamfer")
        self._require_node(solid, "solid")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "solid": solid,
        }
        selectors = _selectors(edges, "edge")
        if selectors:
            args["edges"] = selectors
        args["distance"] = self._dim(distance, "length")
        if distance2 is not None:
            args["distance2"] = self._dim(distance2, "length")
        if angle is not None:
            args["angle"] = self._dim(angle, "angle")
        self._ingest({"command": "chamfer_edges", "args": args})
        return identifier

    def shell(
        self,
        name: str,
        solid: str,
        thickness: Any,
        *,
        node_id: str | None = None,
        open_faces: Sequence[str] | None = None,
        inward: bool = True,
    ) -> str:
        """Hollow a solid. A numeric thickness is millimetres."""
        _flag(inward, "inward")
        identifier = self._allocate(name, node_id, "shell")
        self._require_node(solid, "solid")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "solid": solid,
        }
        faces = _selectors(open_faces, "face")
        if faces:
            args["open_faces"] = faces
        args["thickness"] = self._dim(thickness, "length")
        args["inward"] = inward
        self._ingest({"command": "shell_solid", "args": args})
        return identifier

    def linear_pattern(
        self,
        name: str,
        solid: str,
        direction: Sequence[float],
        count: Any,
        spacing: Any,
        *,
        node_id: str | None = None,
        second_direction: tuple[Sequence[float], Any, Any] | None = None,
    ) -> str:
        """Linear pattern. ``count`` is an int or an int parameter id."""
        definition: dict[str, Any] = {
            "pattern": "linear",
            "direction": direction,
            "count": count,
            "spacing": spacing,
        }
        if second_direction is not None:
            second_dir, second_count, second_spacing = second_direction
            definition["second_direction"] = {
                "direction": second_dir,
                "count": second_count,
                "spacing": second_spacing,
            }
        return self.pattern_feature(name, solid, definition, node_id=node_id)

    def polar_pattern(
        self,
        name: str,
        solid: str,
        count: Any,
        total_angle: Any,
        *,
        node_id: str | None = None,
        axis_origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
        axis_direction: Sequence[float] = (0.0, 0.0, 1.0),
    ) -> str:
        """Polar pattern. A numeric ``total_angle`` is degrees."""
        return self.pattern_feature(
            name,
            solid,
            {
                "pattern": "polar",
                "axis_origin_mm": axis_origin_mm,
                "axis_direction": axis_direction,
                "count": count,
                "total_angle": total_angle,
            },
            node_id=node_id,
        )

    def mirror(
        self,
        name: str,
        solid: str,
        plane: str | Mapping[str, Any],
        *,
        node_id: str | None = None,
        keep_original: bool = True,
    ) -> str:
        """Mirror a solid across a datum plane or a plane object."""
        _flag(keep_original, "keep_original")
        identifier = self._allocate(name, node_id, "mirror")
        self._require_node(solid, "solid")
        self._ingest(
            {
                "command": "mirror_feature",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "solid": solid,
                    "plane": _plane(plane),
                    "keep_original": keep_original,
                },
            }
        )
        return identifier

    def boolean(
        self,
        name: str,
        op: str,
        target: str,
        tools: Sequence[str],
        *,
        node_id: str | None = None,
    ) -> str:
        """Boolean ``union``, ``subtract``, or ``intersect``."""
        identifier = self._allocate(name, node_id, "boolean")
        self._require_node(target, "solid")
        tool_ids = [self._require_node(tool, "solid").id for tool in tools]
        if not tool_ids:
            raise ValueError("boolean feature requires at least one tool solid")
        self._ingest(
            {
                "command": "boolean_solids",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "op": _boolean_op(op),
                    "target": target,
                    "tools": tool_ids,
                },
            }
        )
        return identifier

    def set_suppressed(self, node_id: str, suppressed: bool = True) -> str:
        """Suppress or restore a node."""
        _flag(suppressed, "suppressed")
        self._require_node(node_id)
        self._ingest(
            {
                "command": "set_node_suppressed",
                "args": {"node_id": node_id, "suppressed": suppressed},
            }
        )
        return node_id

    @_semantic_command
    def set_feature_input(
        self,
        node_id: str,
        input_name: str,
        *,
        length_mm: float | None = None,
        vector_mm: Sequence[float] | None = None,
    ) -> str:
        """Update one literal input on an existing ``primitive_box`` or ``hole`` node in place."""
        self._require_node(node_id)
        if not isinstance(input_name, str) or not input_name.strip():
            raise ValueError("set_feature_input requires a non-empty input name")
        args: dict[str, Any] = {"node_id": node_id, "input": input_name.strip()}
        if length_mm is not None:
            if isinstance(length_mm, bool) or not isinstance(length_mm, (int, float)):
                raise TypeError("length_mm must be a finite number greater than zero")
            value = float(length_mm)
            if not math.isfinite(value) or value <= 0.0:
                raise ValueError("length_mm must be a finite number greater than zero")
            args["length_mm"] = value
        if vector_mm is not None:
            args["vector_mm"] = _vec3(vector_mm)
        if "length_mm" not in args and "vector_mm" not in args:
            raise ValueError("set_feature_input requires either length_mm or vector_mm")
        if "length_mm" in args and "vector_mm" in args:
            raise ValueError("set_feature_input accepts length_mm or vector_mm, not both")
        self._ingest({"command": "set_feature_input", "args": args})
        return node_id

    @_semantic_command
    def rename_node(self, node_id: str, name: str) -> str:
        """Rename a node. The id stays the same."""
        self._require_node(node_id)
        self._ingest(
            {
                "command": "rename_node",
                "args": {"node_id": node_id, "name": _feature_name(name)},
            }
        )
        return node_id

    @_semantic_command
    def delete_node(self, node_id: str) -> str:
        """Delete a node that no other node still references."""
        self._require_node(node_id)
        users = [
            other.id
            for other in self.nodes.values()
            if other.id != node_id and node_id in other.refs
        ]
        if users:
            raise ValueError(f"node {node_id!r} is still used by {', '.join(users)}")
        self._ingest({"command": "delete_node", "args": {"node_id": node_id}})
        return node_id

    @_semantic_command
    def insert_component(
        self,
        name: str,
        source: str | Mapping[str, Any],
        *,
        translation_mm: Sequence[float] = (0.0, 0.0, 0.0),
        rotation_deg: Sequence[float] = (0.0, 0.0, 0.0),
        grounded: bool = False,
        node_id: str | None = None,
    ) -> str:
        """Place a solid, surface, or assembly in the assembly as a component."""
        _flag(grounded, "grounded")
        identifier = self._allocate(name, node_id, "component")
        source_spec = self._component_source(source)
        self._ingest(
            {
                "command": "insert_component",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "source": source_spec,
                    "translation_mm": _vec3(translation_mm),
                    "rotation_deg": _vec3(rotation_deg),
                    "grounded": grounded,
                },
            }
        )
        return identifier

    @_semantic_command
    def add_mate(
        self,
        name: str,
        left: str,
        right: str,
        left_ref: str,
        right_ref: str,
        mate_kind: str = "coincident",
        *,
        flip: bool = False,
        node_id: str | None = None,
        value: Any = None,
        ratio: float | None = None,
    ) -> str:
        """Mate two components. ``distance`` and ``angle`` need ``value``. ``gear`` needs ``ratio``."""
        _flag(flip, "flip")
        identifier = self._allocate(name, node_id, "mate")
        self._require_node(left, "component")
        self._require_node(right, "component")
        if mate_kind not in _MATE_KINDS:
            raise ValueError(f"unknown mate kind {mate_kind!r}")
        if not isinstance(left_ref, str) or not left_ref.strip():
            raise ValueError("left_ref cannot be empty")
        if not isinstance(right_ref, str) or not right_ref.strip():
            raise ValueError("right_ref cannot be empty")
        kind: dict[str, Any]
        if mate_kind == "distance":
            if value is None:
                raise ValueError("distance mate requires value")
            kind = {"kind": "distance", "value": self._dim(value, "length")}
        elif mate_kind == "angle":
            if value is None:
                raise ValueError("angle mate requires value")
            kind = {"kind": "angle", "value": self._dim(value, "angle")}
        elif mate_kind == "gear":
            if ratio is None:
                raise ValueError("gear mate requires ratio")
            number = _num(ratio, "gear ratio")
            if number == 0.0:
                raise ValueError("gear mate ratio cannot be zero")
            kind = {"kind": "gear", "ratio": number}
        else:
            if value is not None or ratio is not None:
                raise ValueError(f"{mate_kind} mate does not take value or ratio")
            kind = {"kind": mate_kind}
        self._ingest(
            {
                "command": "add_mate",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "left": left,
                    "right": right,
                    "left_ref": left_ref,
                    "right_ref": right_ref,
                    "mate_kind": kind,
                    "flip": flip,
                },
            }
        )
        return identifier

    @_semantic_command
    def remove_parameter(self, name: str) -> str:
        """Remove a parameter that no formula or feature still uses."""
        parameter_name = _parameter_name(name)
        self._ingest({"command": "remove_parameter", "args": {"name": parameter_name}})
        return parameter_name

    @_semantic_command
    def upsert_sketch_entities(
        self,
        node_id: str,
        entities: Sequence[Mapping[str, Any]],
        *,
        replace: bool = False,
    ) -> str:
        """Add or replace sketch entities. ``replace`` drops the previous entities."""
        _flag(replace, "replace")
        self._require_node(node_id, "sketch")
        normalized = _normalize_entities(entities)
        if not normalized:
            raise ValueError("sketch entity upsert cannot be empty")
        self._ingest(
            {
                "command": "upsert_sketch_entities",
                "args": {"node_id": node_id, "entities": normalized, "replace": replace},
            }
        )
        return node_id

    @_semantic_command
    def upsert_sketch_constraints(
        self,
        node_id: str,
        constraints: Sequence[Mapping[str, Any]],
        *,
        replace: bool = False,
    ) -> str:
        """Add or replace sketch constraints. Named constraints replace the same name."""
        _flag(replace, "replace")
        self._require_node(node_id, "sketch")
        self._ingest(
            {
                "command": "upsert_sketch_constraints",
                "args": {
                    "node_id": node_id,
                    "constraints": list(constraints),
                    "replace": replace,
                },
            }
        )
        return node_id

    @_semantic_command
    def create_primitive_box(
        self,
        name: str,
        width: Any,
        depth: Any,
        height: Any,
        *,
        node_id: str | None = None,
        origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
    ) -> str:
        """Box primitive. Same command as :meth:`create_box`."""
        return self.create_box(
            name, width, depth, height, node_id=node_id, origin_mm=origin_mm
        )

    @_semantic_command
    def create_primitive_cylinder(
        self,
        name: str,
        radius: Any,
        height: Any,
        *,
        node_id: str | None = None,
        origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
        axis: Sequence[float] = (0.0, 0.0, 1.0),
    ) -> str:
        """Cylinder primitive. Same command as :meth:`create_cylinder`."""
        return self.create_cylinder(
            name, radius, height, node_id=node_id, origin_mm=origin_mm, axis=axis
        )

    @_semantic_command
    def extrude_sketch(
        self,
        name: str,
        sketch: str,
        distance: Any,
        *,
        node_id: str | None = None,
        direction: Sequence[float] | None = None,
        symmetric: bool = False,
        operation: str = "new_body",
        target: str | None = None,
    ) -> str:
        """Extrude a sketch. Same command as :meth:`extrude`."""
        return self.extrude(
            name,
            sketch,
            distance,
            node_id=node_id,
            direction=direction,
            symmetric=symmetric,
            operation=operation,
            target=target,
        )

    @_semantic_command
    def revolve_sketch(
        self,
        name: str,
        sketch: str,
        angle: Any,
        *,
        node_id: str | None = None,
        axis_origin_mm: Sequence[float] = (0.0, 0.0, 0.0),
        axis_direction: Sequence[float] = (0.0, 0.0, 1.0),
        operation: str = "new_body",
        target: str | None = None,
    ) -> str:
        """Revolve a sketch. Same command as :meth:`revolve`."""
        return self.revolve(
            name,
            sketch,
            angle,
            node_id=node_id,
            axis_origin_mm=axis_origin_mm,
            axis_direction=axis_direction,
            operation=operation,
            target=target,
        )

    @_semantic_command
    def sweep_profile(
        self,
        name: str,
        profile: str,
        path: str | Sequence[Sequence[float]],
        *,
        node_id: str | None = None,
        keep_normal: bool = False,
    ) -> str:
        """Sweep a profile. Same command as :meth:`sweep`."""
        return self.sweep(name, profile, path, node_id=node_id, keep_normal=keep_normal)

    @_semantic_command
    def loft_sections(
        self,
        name: str,
        sections: Sequence[str],
        *,
        node_id: str | None = None,
        guides: Sequence[str] | None = None,
        closed: bool = False,
        as_surface: bool = False,
    ) -> str:
        """Loft sections. Same command as :meth:`loft`."""
        return self.loft(
            name,
            sections,
            node_id=node_id,
            guides=guides,
            closed=closed,
            as_surface=as_surface,
        )

    @_semantic_command
    def fillet_edges(
        self,
        name: str,
        solid: str,
        radius: Any,
        *,
        node_id: str | None = None,
        edges: Sequence[str] | None = None,
    ) -> str:
        """Fillet edges. Same command as :meth:`fillet`."""
        return self.fillet(name, solid, radius, node_id=node_id, edges=edges)

    @_semantic_command
    def chamfer_edges(
        self,
        name: str,
        solid: str,
        distance: Any,
        *,
        node_id: str | None = None,
        edges: Sequence[str] | None = None,
        distance2: Any = None,
        angle: Any = None,
    ) -> str:
        """Chamfer edges. Same command as :meth:`chamfer`."""
        return self.chamfer(
            name,
            solid,
            distance,
            node_id=node_id,
            edges=edges,
            distance2=distance2,
            angle=angle,
        )

    @_semantic_command
    def shell_solid(
        self,
        name: str,
        solid: str,
        thickness: Any,
        *,
        node_id: str | None = None,
        open_faces: Sequence[str] | None = None,
        inward: bool = True,
    ) -> str:
        """Shell a solid. Same command as :meth:`shell`."""
        return self.shell(
            name, solid, thickness, node_id=node_id, open_faces=open_faces, inward=inward
        )

    @_semantic_command
    def draft_faces(
        self,
        name: str,
        solid: str,
        faces: Sequence[str],
        pull_direction: Sequence[float],
        neutral_plane: str | Mapping[str, Any],
        angle: Any,
        *,
        node_id: str | None = None,
    ) -> str:
        """Draft faces. ``angle`` is degrees. ``neutral_plane`` is xy, xz, yz, or a plane object."""
        selectors = _selectors(faces, "face")
        if not selectors:
            raise ValueError("draft requires at least one face")
        identifier = self._allocate(name, node_id, "draft")
        self._require_node(solid, "solid")
        self._ingest(
            {
                "command": "draft_faces",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "solid": solid,
                    "faces": selectors,
                    "pull_direction": _direction(pull_direction, "pull direction"),
                    "neutral_plane": _plane(neutral_plane),
                    "angle": self._dim(angle, "angle"),
                },
            }
        )
        return identifier

    @_semantic_command
    def pattern_feature(
        self,
        name: str,
        solid: str,
        definition: Mapping[str, Any],
        *,
        node_id: str | None = None,
    ) -> str:
        """Linear or polar pattern. ``definition['pattern']`` is ``linear`` or ``polar``."""
        wire = self._normalize_pattern(definition)
        identifier = self._allocate(name, node_id, "pattern")
        self._require_node(solid, "solid")
        self._ingest(
            {
                "command": "pattern_feature",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "solid": solid,
                    "definition": wire,
                },
            }
        )
        return identifier

    @_semantic_command
    def mirror_feature(
        self,
        name: str,
        solid: str,
        plane: str | Mapping[str, Any],
        *,
        node_id: str | None = None,
        keep_original: bool = True,
    ) -> str:
        """Mirror a solid. Same command as :meth:`mirror`."""
        return self.mirror(name, solid, plane, node_id=node_id, keep_original=keep_original)

    @_semantic_command
    def boolean_solids(
        self,
        name: str,
        op: str,
        target: str,
        tools: Sequence[str],
        *,
        node_id: str | None = None,
    ) -> str:
        """Boolean solids. Same command as :meth:`boolean`."""
        return self.boolean(name, op, target, tools, node_id=node_id)

    @_semantic_command
    def direct_face_edit(
        self,
        name: str,
        solid: str,
        face: str,
        edit: str | Mapping[str, Any],
        *,
        node_id: str | None = None,
        distance: Any = None,
        delta_mm: Sequence[float] | None = None,
    ) -> str:
        """Offset, move, or delete one face. ``edit`` is ``offset``, ``move``, ``delete_and_heal``, or that object."""
        if not isinstance(face, str) or not face.strip():
            raise ValueError("face reference cannot be empty")
        identifier = self._allocate(name, node_id, "face_edit")
        self._require_node(solid, "solid")
        self._ingest(
            {
                "command": "direct_face_edit",
                "args": {
                    "node_id": identifier,
                    "name": _feature_name(name),
                    "solid": solid,
                    "face": face,
                    "edit": self._face_edit(edit, distance=distance, delta_mm=delta_mm),
                },
            }
        )
        return identifier

    @_semantic_command
    def set_node_suppressed(self, node_id: str, suppressed: bool = True) -> str:
        """Suppress or restore a node. Same command as :meth:`set_suppressed`."""
        return self.set_suppressed(node_id, suppressed)

    @_semantic_command
    def reorder_feature(self, node_id: str, before: str | None = None) -> str:
        """Move a feature immediately before ``before``. ``None`` moves it to the end."""
        self._ingest(
            {
                "command": "reorder_feature",
                "args": {"node_id": node_id, "before": before},
            }
        )
        return node_id

    @_semantic_command
    def tag_topology(
        self,
        name: str,
        solid: str,
        tag: str,
        targets: Sequence[str] | None = None,
        *,
        node_id: str | None = None,
    ) -> str:
        """Name topology on a solid or surface. Empty targets are omitted from the wire."""
        if not isinstance(tag, str) or not tag.strip():
            raise ValueError("topology tag cannot be empty")
        selectors = _selectors(targets, "target")
        identifier = self._allocate(name, node_id, "tag")
        self._require_node(solid, "solid", "surface")
        args: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
            "solid": solid,
            "tag": tag,
        }
        if selectors:
            args["targets"] = selectors
        self._ingest({"command": "tag_topology", "args": args})
        return identifier

    @_semantic_command
    def add_design_assertion(
        self,
        name: str,
        assertion_id: str,
        kind: Mapping[str, Any],
        *,
        target: str | None = None,
        node_id: str | None = None,
    ) -> str:
        """Add a design assertion. ``kind['rule']`` selects the Rust rule object."""
        if not isinstance(assertion_id, str) or not assertion_id.strip():
            raise ValueError("assertion id cannot be empty")
        identifier = self._allocate(name, node_id, "assertion")
        wire_kind = self._assertion_kind(kind)
        if target is not None:
            self._require_node(target)
        if wire_kind["rule"] == "boolean_condition":
            self._require_node(wire_kind["condition"], "bool")
        ordered: dict[str, Any] = {
            "node_id": identifier,
            "name": _feature_name(name),
        }
        if target is not None:
            ordered["target"] = target
        ordered["assertion_id"] = assertion_id
        ordered["kind"] = wire_kind
        self._ingest({"command": "add_design_assertion", "args": ordered})
        return identifier

    def to_commands(self) -> list[dict[str, Any]]:
        """Return a deep copy of the command batch."""
        return copy.deepcopy(self.commands)

    def to_commands_json(self, *, indent: int | None = None) -> str:
        """Serialize the command batch. The default encoding is compact."""
        if indent is None:
            return _compact_json(self.to_commands())
        return json.dumps(self.to_commands(), indent=indent, allow_nan=False, ensure_ascii=False)

    def to_patch(self) -> dict[str, Any]:
        """Build the GraphPatch these commands would apply, including spline blobs."""
        ops: list[dict[str, Any]] = []
        for handle in self.blobs:
            blob = self.blobs[handle]
            ops.append(
                {
                    "op": "upsert_blob",
                    "value": {
                        "handle": handle,
                        "kind": "spline",
                        "sha256": blob["sha256"],
                        "byte_len": blob["byte_len"],
                        "relative_path": blob["relative_path"],
                    },
                }
            )
        outline: list[str] = []
        contract_present = False
        sketch_specs: dict[str, dict[str, Any]] = {}
        for command in self.commands:
            command_name = command["command"]
            args = command["args"]
            if command_name == "remove_parameter":
                ops.append({"op": "remove_parameter", "value": {"name": args["name"]}})
                continue
            if command_name == "upsert_parameter":
                ops.append(
                    {
                        "op": "upsert_parameter",
                        "value": {
                            "name": args["name"],
                            "ir_type": args["ir_type"],
                            "value": copy.deepcopy(args["value"]),
                            "formula": args.get("formula"),
                        },
                    }
                )
                continue
            if command_name == "set_design_contract":
                if contract_present:
                    ops.append({"op": "remove_node", "value": {"id": CONTRACT_NODE_ID}})
                ops.append({"op": "add_node", "value": _contract_node(args["contract"])})
                contract_present = True
                continue
            if command_name == "rename_node":
                ops.append(
                    {
                        "op": "update_node",
                        "value": {
                            "id": args["node_id"],
                            "name": args["name"],
                            "inputs": None,
                            "kind": None,
                        },
                    }
                )
                continue
            if command_name == "set_feature_input":
                node_id = args["node_id"]
                live_node = self.nodes.get(node_id)
                updated_inputs = copy.deepcopy(live_node.inputs) if live_node is not None else None
                ops.append(
                    {
                        "op": "update_node",
                        "value": {
                            "id": node_id,
                            "name": None,
                            "inputs": updated_inputs,
                            "kind": None,
                        },
                    }
                )
                continue
            if command_name == "set_node_suppressed":
                ops.append(
                    {
                        "op": "set_node_state",
                        "value": {
                            "id": args["node_id"],
                            "state": {
                                "status": "suppressed" if args["suppressed"] else "valid"
                            },
                        },
                    }
                )
                continue
            if command_name == "delete_node":
                outline = [node_id for node_id in outline if node_id != args["node_id"]]
                ops.append({"op": "remove_node", "value": {"id": args["node_id"]}})
                ops.append({"op": "set_outputs", "value": list(outline)})
                continue
            if command_name in {"upsert_sketch_entities", "upsert_sketch_constraints"}:
                node_id = args["node_id"]
                spec = _next_sketch_spec(sketch_specs[node_id], command)
                sketch_specs[node_id] = spec
                ops.append(
                    {
                        "op": "update_node",
                        "value": {
                            "id": node_id,
                            "name": None,
                            "inputs": _sketch_inputs(spec),
                            "kind": None,
                        },
                    }
                )
                continue
            if command_name == "reorder_feature":
                node_id = args["node_id"]
                outline = [item for item in outline if item != node_id]
                before = args.get("before")
                if before is None:
                    outline.append(node_id)
                else:
                    outline.insert(outline.index(before), node_id)
                ops.append({"op": "set_outputs", "value": list(outline)})
                continue
            node = _materialize_feature(command)
            if command_name == "create_sketch":
                sketch_specs[node.id] = json.loads(node.inputs["feature_spec"]["value"])
            outline.append(node.id)
            ops.append({"op": "add_node", "value": _node_dict(node)})
            ops.append({"op": "set_outputs", "value": list(outline)})
        if self.commands:
            description = ", ".join(command["command"] for command in self.commands)
        else:
            description = "No semantic changes"
        return {
            "patch_id": semantic_patch_id(self.commands),
            "description": description,
            "ops": ops,
        }

    def compile(self, graph_id: str) -> GraphBuilder:
        """Lower the live model into a :class:`GraphBuilder`."""
        builder = GraphBuilder(graph_id)
        for name, parameter in self.parameters.items():
            builder.add_parameter(
                name,
                parameter["ir_type"],
                parameter["raw_value"],
                formula=parameter["formula"],
            )
        for handle, blob in self.blobs.items():
            builder.add_spline_blob(
                handle,
                blob["points"],
                degree=blob["degree"],
                closed=blob["closed"],
            )
        for node in self.nodes.values():
            builder.add_node(
                node.id,
                node.name,
                node.kind,
                node.inputs,
                node.output_type,
                node.state,
                metadata=node.metadata,
            )
        builder.set_outputs(self.outline)
        return builder

    def load_commands(self, commands: Sequence[Mapping[str, Any]]) -> None:
        """Replay a saved batch onto an empty command list. Blobs may already exist."""
        if self.commands or self.nodes or self.parameters:
            raise RuntimeError("load_commands expects a model with no commands yet")
        if not isinstance(commands, Sequence) or isinstance(commands, (str, bytes)):
            raise TypeError("commands must be a list")
        for index, command in enumerate(commands):
            if not isinstance(command, Mapping) or not isinstance(command.get("args"), Mapping):
                raise ValueError(f"command {index} is not a SemanticCommand object")
            name = command.get("command")
            if not isinstance(name, str):
                raise ValueError(f"command {index} is missing a command name")
            self._ingest(
                {"command": name, "args": copy.deepcopy(dict(command["args"]))},
                pending_parameter=_pending_parameter(command) if name == "upsert_parameter" else None,
            )

    def _ingest(
        self,
        command: dict[str, Any],
        *,
        pending_parameter: dict[str, Any] | None = None,
    ) -> None:
        command_name = command["command"]
        args = command["args"]
        if command_name == "upsert_parameter":
            pending = pending_parameter or _pending_parameter(command)
            if pending is None:
                raise ValueError("upsert_parameter is missing a value")
            existing = self.parameters.get(pending["name"])
            if existing is not None and existing["ir_type"] != pending["ir_type"]:
                raise ValueError(
                    f"parameter {pending['name']!r} is {existing['ir_type']}; "
                    f"refusing to change it to {pending['ir_type']}"
                )
            self.parameters[pending["name"]] = pending
            self.commands.append(command)
            return
        if command_name == "set_design_contract":
            node = _contract_live_node(args["contract"])
            self.nodes[node.id] = node
            self.commands.append(command)
            return
        if command_name == "rename_node":
            node = self._require_node(args["node_id"])
            node.name = args["name"]
            self.commands.append(command)
            return
        if command_name == "set_feature_input":
            node = self._require_node(args["node_id"])
            _apply_feature_input_update(node, args)
            self.commands.append(command)
            return
        if command_name == "set_node_suppressed":
            node = self._require_node(args["node_id"])
            node.state = {"status": "suppressed" if args["suppressed"] else "valid"}
            self.commands.append(command)
            return
        if command_name == "delete_node":
            node_id = args["node_id"]
            self._require_node(node_id)
            users = [
                other.id
                for other in self.nodes.values()
                if other.id != node_id and node_id in other.refs
            ]
            if users:
                raise ValueError(f"node {node_id!r} is still used by {', '.join(users)}")
            del self.nodes[node_id]
            self.outline = [item for item in self.outline if item != node_id]
            self.commands.append(command)
            return
        if command_name == "remove_parameter":
            parameter_name = _parameter_name(args["name"])
            if parameter_name not in self.parameters:
                raise ValueError(f"parameter {parameter_name!r} was not found")
            users = self._parameter_users(parameter_name)
            if users:
                raise ValueError(
                    f"parameter {parameter_name!r} is still used by {', '.join(users)}"
                )
            del self.parameters[parameter_name]
            args["name"] = parameter_name
            self.commands.append(command)
            return
        if command_name in {"upsert_sketch_entities", "upsert_sketch_constraints"}:
            self._ingest_sketch_update(command)
            return
        if command_name == "reorder_feature":
            self._ingest_reorder(command)
            return
        if command_name not in _FEATURE_COMMANDS:
            raise ValueError(f"unknown semantic command {command_name!r}")
        node = _materialize_feature(command)
        if node.id in self.nodes or node.id == CONTRACT_NODE_ID:
            raise ValueError(f"node id {node.id!r} already exists")
        self._check_feature_refs(command, node)
        self.nodes[node.id] = node
        self.outline.append(node.id)
        self.commands.append(command)

    def _check_feature_refs(self, command: dict[str, Any], node: _LiveNode) -> None:
        command_name = command["command"]
        args = command["args"]
        if command_name == "create_sketch":
            for entity in args.get("entities", []):
                if entity.get("kind") == "spline_blob" and entity["blob"] not in self.blobs:
                    raise ValueError(f"blob {entity['blob']!r} was not found")
            return
        for ref in node.refs:
            if ref not in self.nodes:
                raise ValueError(f"node {ref!r} was not found")
        if command_name == "add_mate":
            self._require_node(args["left"], "component")
            self._require_node(args["right"], "component")
        elif command_name == "insert_component":
            source = args["source"]
            if source.get("kind") == "local_node":
                self._require_node(source["node_id"], "solid", "surface", "subd", "assembly")
        elif command_name == "tag_topology":
            self._require_node(args["solid"], "solid", "surface")
        elif command_name == "add_design_assertion":
            if "target" in args:
                self._require_node(args["target"])
            if args["kind"]["rule"] == "boolean_condition":
                self._require_node(args["kind"]["condition"], "bool")
        elif command_name in {"extrude_sketch", "revolve_sketch"}:
            self._require_node(args["sketch"], "sketch")
            if "target" in args:
                self._require_node(args["target"], "solid")
        elif command_name == "sweep_profile":
            self._require_node(args["profile"], "sketch")
            path = args["path"]
            if path.get("path") == "sketch":
                self._require_node(path["node_id"], "sketch")
        elif command_name == "loft_sections":
            for section in args["sections"]:
                self._require_node(section, "sketch")
            for guide in args.get("guides", []):
                self._require_node(guide, "sketch")
        elif command_name == "boolean_solids":
            self._require_node(args["target"], "solid")
            for tool in args["tools"]:
                self._require_node(tool, "solid")
        elif command_name in {
            "create_hole",
            "fillet_edges",
            "chamfer_edges",
            "shell_solid",
            "draft_faces",
            "pattern_feature",
            "mirror_feature",
            "direct_face_edit",
        }:
            self._require_node(args["solid"], "solid")

    def _allocate(self, name: str, node_id: str | None, prefix: str) -> str:
        _feature_name(name)
        if node_id is None:
            return self._auto_id(name, prefix)
        return _explicit_id(node_id, self.nodes)

    def _auto_id(self, name: str, prefix: str) -> str:
        raw = re.sub(r"[^0-9A-Za-z_]+", "_", name.strip()).strip("_").lower()
        if not raw or not raw[0].isalpha():
            raw = prefix
        candidate = raw
        number = 2
        while candidate in self.nodes or candidate == CONTRACT_NODE_ID:
            candidate = f"{raw}_{number}"
            number += 1
        return candidate

    def _require_node(self, node_id: str, *output_types: str) -> _LiveNode:
        if not isinstance(node_id, str) or not node_id.strip():
            raise ValueError("node id cannot be empty")
        node = self.nodes.get(node_id)
        if node is None:
            raise ValueError(f"node {node_id!r} was not found")
        if output_types and node.output_type not in output_types:
            expected = " or ".join(output_types)
            raise TypeError(f"node {node_id!r} outputs {node.output_type}, expected {expected}")
        return node

    def _require_param(self, name: str, ir_type: str) -> None:
        parameter = self.parameters.get(name)
        if parameter is None:
            raise ValueError(f"parameter {name!r} was not found")
        if parameter["ir_type"] != ir_type:
            raise TypeError(
                f"parameter {name!r} is {parameter['ir_type']}, expected {ir_type}"
            )

    def _dim(self, value: Any, dimension: str) -> dict[str, Any]:
        if isinstance(value, str):
            self._require_param(value, dimension)
            return {"source": "param", "param_id": value}
        if isinstance(value, Mapping) and value.get("source") == "param":
            self._require_param(str(value["param_id"]), dimension)
            return {"source": "param", "param_id": str(value["param_id"])}
        if isinstance(value, Mapping) and value.get("source") == "literal":
            return _literal_dim(value, dimension)
        quantity = _coerce_quantity(value, dimension)
        payload = {"source": "literal"}
        payload.update(quantity.to_json())
        return payload

    def _int_input(self, value: Any) -> dict[str, Any]:
        if isinstance(value, str):
            self._require_param(value, "int")
            return {"source": "param", "param_id": value}
        if isinstance(value, Mapping) and value.get("source") == "param":
            self._require_param(str(value["param_id"]), "int")
            return {"source": "param", "param_id": str(value["param_id"])}
        if isinstance(value, Mapping) and value.get("source") == "literal":
            number = value.get("value")
            if isinstance(number, bool) or not isinstance(number, int) or number < 1:
                raise ValueError("count must be an int >= 1")
            return {"source": "literal", "value": number}
        if isinstance(value, bool) or not isinstance(value, int):
            raise TypeError("count must be an int >= 1 or a parameter name")
        if value < 1:
            raise ValueError("count must be at least 1")
        return {"source": "literal", "value": value}

    def _hole_depth(self, depth: Any) -> dict[str, Any]:
        if depth is None or depth == "through_all":
            return {"kind": "through_all"}
        if isinstance(depth, Mapping) and depth.get("kind") == "through_all":
            return {"kind": "through_all"}
        if isinstance(depth, Mapping) and depth.get("kind") == "blind":
            return {"kind": "blind", "depth": self._dim(depth.get("depth"), "length")}
        if isinstance(depth, tuple) and len(depth) == 2 and depth[0] == "blind":
            return {"kind": "blind", "depth": self._dim(depth[1], "length")}
        return {"kind": "blind", "depth": self._dim(depth, "length")}

    def _hole_profile(
        self,
        profile: str,
        bore_diameter: Any,
        bore_depth: Any,
        sink_diameter: Any,
        sink_angle: Any,
    ) -> dict[str, Any]:
        if profile in (None, "simple"):
            return {"kind": "simple"}
        if profile == "counterbore":
            if bore_diameter is None or bore_depth is None:
                raise ValueError("counterbore profile requires bore_diameter and bore_depth")
            return {
                "kind": "counterbore",
                "bore_diameter": self._dim(bore_diameter, "length"),
                "bore_depth": self._dim(bore_depth, "length"),
            }
        if profile == "countersink":
            if sink_diameter is None or sink_angle is None:
                raise ValueError("countersink profile requires sink_diameter and sink_angle")
            return {
                "kind": "countersink",
                "sink_diameter": self._dim(sink_diameter, "length"),
                "sink_angle": self._dim(sink_angle, "angle"),
            }
        raise ValueError("profile must be simple, counterbore, or countersink")

    def _sweep_path(self, path: str | Sequence[Sequence[float]]) -> dict[str, Any]:
        if isinstance(path, str):
            self._require_node(path, "sketch")
            return {"path": "sketch", "node_id": path}
        if isinstance(path, Sequence) and not isinstance(path, (str, bytes)):
            points = [_vec3(point) for point in path]
            if len(points) < 2:
                raise ValueError("sweep polyline path requires at least 2 points")
            return {"path": "polyline", "points_mm": points}
        raise TypeError("sweep path must be a sketch id or a list of 3D points")

    def _sketch_ids(self, values: Sequence[str], label: str) -> list[str]:
        if isinstance(values, (str, bytes)) or not isinstance(values, Sequence):
            raise TypeError(f"{label} must be a list of sketch ids")
        ids: list[str] = []
        for value in values:
            if not isinstance(value, str):
                raise TypeError(f"{label} must be a sketch id")
            self._require_node(value, "sketch")
            ids.append(value)
        return ids

    def _component_source(self, source: str | Mapping[str, Any]) -> dict[str, Any]:
        if isinstance(source, str):
            self._require_node(source, "solid", "surface", "subd", "assembly")
            return {"kind": "local_node", "node_id": source}
        if isinstance(source, Mapping):
            kind = source.get("kind")
            if kind == "local_node":
                node_id = source.get("node_id")
                if not isinstance(node_id, str):
                    raise TypeError("local component source needs a node id")
                self._require_node(node_id, "solid", "surface", "subd", "assembly")
                return {"kind": "local_node", "node_id": node_id}
            if kind == "external_ref":
                ref_id = source.get("ref_id")
                if not isinstance(ref_id, str) or not ref_id.strip():
                    raise ValueError("external component source needs a ref id")
                return {"kind": "external_ref", "ref_id": ref_id}
        raise TypeError("component source must be a node id or a source object")

    def _normalize_pattern(self, definition: Mapping[str, Any]) -> dict[str, Any]:
        if not isinstance(definition, Mapping):
            raise TypeError("pattern definition must be an object")
        kind = definition.get("pattern")
        if kind == "linear":
            wire: dict[str, Any] = {
                "pattern": "linear",
                "direction": _direction(definition["direction"], "pattern direction"),
                "count": self._int_input(definition["count"]),
                "spacing": self._dim(definition["spacing"], "length"),
            }
            second = definition.get("second_direction")
            if second is not None:
                if not isinstance(second, Mapping):
                    raise TypeError("second pattern direction must be an object")
                wire["second_direction"] = {
                    "direction": _direction(second["direction"], "second pattern direction"),
                    "count": self._int_input(second["count"]),
                    "spacing": self._dim(second["spacing"], "length"),
                }
            return wire
        if kind == "polar":
            return {
                "pattern": "polar",
                "axis_origin_mm": _vec3(definition.get("axis_origin_mm", (0.0, 0.0, 0.0))),
                "axis_direction": _direction(
                    definition.get("axis_direction", (0.0, 0.0, 1.0)),
                    "pattern axis",
                ),
                "count": self._int_input(definition["count"]),
                "total_angle": self._dim(definition["total_angle"], "angle"),
            }
        raise ValueError("pattern must be linear or polar")

    def _face_edit(
        self,
        edit: str | Mapping[str, Any],
        *,
        distance: Any = None,
        delta_mm: Sequence[float] | None = None,
    ) -> dict[str, Any]:
        payload: Mapping[str, Any]
        if isinstance(edit, str):
            op = edit
            payload = {}
        elif isinstance(edit, Mapping):
            op = edit.get("op")
            payload = edit
        else:
            raise TypeError("face edit must be offset, move, delete_and_heal, or an edit object")
        if op == "offset":
            value = distance if distance is not None else payload.get("distance")
            if value is None:
                raise ValueError("face offset requires distance")
            return {"op": "offset", "distance": self._dim(value, "length")}
        if op == "move":
            delta = delta_mm if delta_mm is not None else payload.get("delta_mm")
            if delta is None:
                raise ValueError("face move requires delta_mm")
            return {"op": "move", "delta_mm": _vec3(delta)}
        if op == "delete_and_heal":
            return {"op": "delete_and_heal"}
        raise ValueError("face edit must be offset, move, or delete_and_heal")

    def _assertion_kind(self, kind: Mapping[str, Any]) -> dict[str, Any]:
        if not isinstance(kind, Mapping):
            raise TypeError("assertion kind must be an object")
        rule = kind.get("rule")
        if rule == "volume_bounds":
            wire: dict[str, Any] = {"rule": "volume_bounds"}
            for key in ("min_mm3", "max_mm3"):
                if key in kind and kind[key] is not None:
                    wire[key] = _num(kind[key], key)
            return wire
        if rule == "min_wall_thickness":
            return {"rule": "min_wall_thickness", "min": self._dim(kind.get("min"), "length")}
        if rule == "boolean_condition":
            condition = kind.get("condition")
            if not isinstance(condition, str) or not condition.strip():
                raise ValueError("boolean condition requires a node id")
            return {"rule": "boolean_condition", "condition": condition}
        if rule == "custom":
            expression = kind.get("expression")
            if not isinstance(expression, str) or not expression.strip():
                raise ValueError("custom assertion expression cannot be empty")
            params = kind.get("params", {})
            if not isinstance(params, Mapping):
                raise TypeError("custom assertion params must be an object")
            ordered: dict[str, str] = {}
            for key in sorted(params, key=str):
                value = params[key]
                if not isinstance(key, str) or not isinstance(value, str):
                    raise TypeError("custom assertion params must be strings")
                ordered[key] = value
            return {"rule": "custom", "expression": expression, "params": ordered}
        raise ValueError(
            "assertion rule must be volume_bounds, min_wall_thickness, boolean_condition, or custom"
        )

    def _parameter_users(self, name: str) -> list[str]:
        users: list[str] = []
        for other, parameter in self.parameters.items():
            if other == name:
                continue
            formula = parameter.get("formula")
            if isinstance(formula, str) and name in _formula_dependencies(formula, other):
                users.append(f"parameter `{other}`")
        for node in self.nodes.values():
            if _tree_mentions_param(node.inputs, name):
                users.append(f"node `{node.id}`")
        return users

    def _ingest_sketch_update(self, command: dict[str, Any]) -> None:
        args = command["args"]
        node = self._require_node(args["node_id"], "sketch")
        current = json.loads(node.inputs["feature_spec"]["value"])
        if "replace" in args:
            _flag(args["replace"], "replace")
        args["replace"] = bool(args.get("replace", False))
        if command["command"] == "upsert_sketch_entities":
            incoming = _normalize_entities(args.get("entities"))
            if not incoming:
                raise ValueError("sketch entity upsert cannot be empty")
            args["entities"] = incoming
        else:
            incoming_constraints = _normalize_constraints(
                args.get("constraints"),
                current.get("entities", []),
            )
            if not incoming_constraints:
                raise ValueError("sketch constraint upsert cannot be empty")
            args["constraints"] = incoming_constraints
        spec = _next_sketch_spec(current, command)
        for entity in spec.get("entities", []):
            if entity.get("kind") == "spline_blob" and entity["blob"] not in self.blobs:
                raise ValueError(f"blob {entity['blob']!r} was not found")
        node.inputs = _sketch_inputs(spec)
        node.refs = _collect_refs(node.inputs)
        self.commands.append(command)

    def _ingest_reorder(self, command: dict[str, Any]) -> None:
        args = command["args"]
        node_id = args["node_id"]
        self._require_node(node_id)
        if node_id not in self.outline:
            raise ValueError(f"feature {node_id!r} is not in the feature tree")
        before = args.get("before")
        if before is None:
            args.pop("before", None)
            self.outline = [item for item in self.outline if item != node_id]
            self.outline.append(node_id)
        else:
            if not isinstance(before, str) or not before.strip():
                raise ValueError("reorder target cannot be empty")
            if before == node_id:
                raise ValueError("reorder target cannot be the same node")
            self._require_node(before)
            if before not in self.outline:
                raise ValueError(f"feature {before!r} is not in the feature tree")
            self.outline = [item for item in self.outline if item != node_id]
            self.outline.insert(self.outline.index(before), node_id)
        self.commands.append(command)


class EngineeringDocument:
    """A named project whose model speaks SemanticCommand and saves ``.aicad``."""

    def __init__(self, project_name: str, *, graph_id: str | None = None) -> None:
        if not isinstance(project_name, str) or not any(not char.isspace() for char in project_name):
            raise ValueError("project name must contain a non-space character")
        self.project_name = project_name
        self.graph_id = graph_id if graph_id is not None else _slug(project_name, "project")
        if not isinstance(self.graph_id, str) or not any(not char.isspace() for char in self.graph_id):
            raise ValueError("graph id must contain a non-space character")
        self.model = SemanticModelBuilder()

    def __getattr__(self, name: str) -> Any:
        return getattr(self.model, name)

    def compile(self) -> GraphBuilder:
        """Lower the document to a graph builder."""
        return self.model.compile(self.graph_id)

    def save(
        self,
        path: str | Any,
        *,
        kernel_fingerprint: Mapping[str, str] | None = None,
    ) -> Any:
        """Write the ``.aicad`` directory plus ``semantic-commands.json``."""
        from pathlib import Path

        from aicad.container import save_aicad_directory

        builder = self.compile()
        dest = save_aicad_directory(
            path,
            self.project_name,
            builder,
            kernel_fingerprint=kernel_fingerprint,
        )
        sidecar = {
            "project_name": self.project_name,
            "graph_id": self.graph_id,
            "commands": self.to_commands(),
        }
        Path(dest, "semantic-commands.json").write_text(
            json.dumps(sidecar, indent=2, allow_nan=False, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        return dest

    @classmethod
    def open(cls, path: str) -> EngineeringDocument:
        """Read a project written by :meth:`save`."""
        from pathlib import Path

        dest = Path(path)
        manifest_path = dest / "manifest.json"
        graph_path = dest / "graph.ir.json"
        commands_path = dest / "semantic-commands.json"
        if not commands_path.is_file():
            raise FileNotFoundError(f"semantic-commands.json is missing in {dest}")
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        graph = json.loads(graph_path.read_text(encoding="utf-8"))
        sidecar = json.loads(commands_path.read_text(encoding="utf-8"))
        document = cls(manifest["project_name"], graph_id=sidecar.get("graph_id", graph["graph_id"]))
        for handle, entry in graph.get("blobs", {}).items():
            if entry.get("kind") != "spline":
                continue
            payload = dest.joinpath(*entry["relative_path"].split("/")).read_bytes()
            decoded = decode_spline_blob(payload)
            document.add_spline_blob(
                handle,
                decoded["points"],
                degree=int(decoded["degree"]),
                closed=bool(decoded["closed"]),
            )
        document.load_commands(sidecar["commands"])
        return document


def _materialize_feature(command: Mapping[str, Any]) -> _LiveNode:
    command_name = str(command["command"])
    args = command["args"]
    node_id = args.get("node_id")
    if not isinstance(node_id, str) or not node_id.strip():
        raise ValueError(f"{command_name} is missing node_id")
    name = str(args["name"])
    if command_name == "create_primitive_box":
        spec = {
            "feature": "primitive_box",
            "width": args["width"],
            "depth": args["depth"],
            "height": args["height"],
            "origin_mm": args["origin_mm"],
        }
        inputs = {
            "width": _dim_ir(args["width"]),
            "depth": _dim_ir(args["depth"]),
            "height": _dim_ir(args["height"]),
            "origin": _vec3_ir(args["origin_mm"]),
        }
        return _feature_node(node_id, name, "primitive_box", "solid", spec, inputs)
    if command_name == "create_primitive_cylinder":
        spec = {
            "feature": "primitive_cylinder",
            "radius": args["radius"],
            "height": args["height"],
            "origin_mm": args["origin_mm"],
            "axis": args["axis"],
        }
        inputs = {
            "radius": _dim_ir(args["radius"]),
            "height": _dim_ir(args["height"]),
            "origin": _vec3_ir(args["origin_mm"]),
            "axis": _vec3_ir(args["axis"]),
        }
        return _feature_node(node_id, name, "primitive_cylinder", "solid", spec, inputs)
    if command_name == "create_sketch":
        entities = list(args.get("entities", []))
        profiles = list(args.get("profiles", []))
        constraints = list(args.get("constraints", []))
        spec = {
            "feature": "sketch_2d",
            "plane": args["plane"],
            "entities": entities,
            "profiles": profiles,
            "constraints": constraints,
        }
        inputs: dict[str, Any] = {}
        handles = [entity["blob"] for entity in entities if entity.get("kind") == "spline_blob"]
        if handles:
            inputs["blobs"] = {
                "type": "list",
                "value": [{"type": "blob_ref", "value": handle} for handle in handles],
            }
        return _feature_node(node_id, name, "sketch_2d", "sketch", spec, inputs)
    if command_name == "extrude_sketch":
        spec = {
            "feature": "extrude",
            "sketch": args["sketch"],
            "distance": args["distance"],
        }
        if "direction" in args:
            spec["direction"] = args["direction"]
        spec["symmetric"] = args.get("symmetric", False)
        spec["operation"] = args.get("operation", "new_body")
        if "target" in args:
            spec["target"] = args["target"]
        inputs = {"sketch": _node_ref(args["sketch"]), "distance": _dim_ir(args["distance"])}
        if "direction" in args:
            inputs["direction"] = _vec3_ir(args["direction"])
        if "target" in args:
            inputs["target"] = _node_ref(args["target"])
        return _feature_node(node_id, name, "extrude", "solid", spec, inputs)
    if command_name == "revolve_sketch":
        spec = {
            "feature": "revolve",
            "sketch": args["sketch"],
            "axis_origin_mm": args["axis_origin_mm"],
            "axis_direction": args["axis_direction"],
            "angle": args["angle"],
            "operation": args.get("operation", "new_body"),
        }
        if "target" in args:
            spec["target"] = args["target"]
        inputs = {
            "sketch": _node_ref(args["sketch"]),
            "origin": _vec3_ir(args["axis_origin_mm"]),
            "axis": _vec3_ir(args["axis_direction"]),
            "angle": _dim_ir(args["angle"]),
        }
        if "target" in args:
            inputs["target"] = _node_ref(args["target"])
        return _feature_node(node_id, name, "revolve", "solid", spec, inputs)
    if command_name == "sweep_profile":
        path = args["path"]
        spec_path = {"kind": path["path"]}
        spec_path.update({key: path[key] for key in path if key != "path"})
        spec = {
            "feature": "sweep",
            "profile": args["profile"],
            "path": spec_path,
            "keep_normal": args.get("keep_normal", False),
        }
        inputs = {"profile": _node_ref(args["profile"])}
        if path["path"] == "sketch":
            inputs["path"] = _node_ref(path["node_id"])
        else:
            inputs["path"] = {
                "type": "list",
                "value": [_vec3_ir(point) for point in path["points_mm"]],
            }
        return _feature_node(node_id, name, "sweep", "solid", spec, inputs)
    if command_name == "loft_sections":
        guides = list(args.get("guides", []))
        spec = {
            "feature": "loft",
            "sections": list(args["sections"]),
            "guides": guides,
            "closed": args.get("closed", False),
            "as_surface": args.get("as_surface", False),
        }
        inputs = {
            "sections": {
                "type": "list",
                "value": [_node_ref(section) for section in args["sections"]],
            }
        }
        if guides:
            inputs["guides"] = {
                "type": "list",
                "value": [_node_ref(guide) for guide in guides],
            }
        output = "surface" if spec["as_surface"] else "solid"
        return _feature_node(node_id, name, "loft_guided", output, spec, inputs)
    if command_name == "create_hole":
        return _hole_node(node_id, name, args)
    if command_name == "fillet_edges":
        edges = list(args.get("edges", []))
        spec = {"feature": "fillet", "solid": args["solid"], "edges": edges, "radius": args["radius"]}
        inputs = {
            "solid": _node_ref(args["solid"]),
            "edges": _topo_list(edges),
            "radius": _dim_ir(args["radius"]),
        }
        return _feature_node(node_id, name, "fillet", "solid", spec, inputs)
    if command_name == "chamfer_edges":
        edges = list(args.get("edges", []))
        spec: dict[str, Any] = {
            "feature": "chamfer",
            "solid": args["solid"],
            "edges": edges,
            "distance": args["distance"],
        }
        inputs = {
            "solid": _node_ref(args["solid"]),
            "edges": _topo_list(edges),
            "distance": _dim_ir(args["distance"]),
        }
        if "distance2" in args:
            spec["distance2"] = args["distance2"]
            inputs["distance2"] = _dim_ir(args["distance2"])
        if "angle" in args:
            spec["angle"] = args["angle"]
            inputs["angle"] = _dim_ir(args["angle"])
        return _feature_node(node_id, name, "chamfer", "solid", spec, inputs)
    if command_name == "shell_solid":
        faces = list(args.get("open_faces", []))
        spec = {
            "feature": "shell",
            "solid": args["solid"],
            "open_faces": faces,
            "thickness": args["thickness"],
            "inward": args.get("inward", True),
        }
        inputs = {
            "solid": _node_ref(args["solid"]),
            "open_faces": _topo_list(faces),
            "thickness": _dim_ir(args["thickness"]),
        }
        return _feature_node(node_id, name, "shell", "solid", spec, inputs)
    if command_name == "pattern_feature":
        return _pattern_node(node_id, name, args)
    if command_name == "mirror_feature":
        spec = {
            "feature": "mirror",
            "solid": args["solid"],
            "plane": args["plane"],
            "keep_original": args.get("keep_original", True),
        }
        inputs = {"solid": _node_ref(args["solid"])}
        return _feature_node(node_id, name, "mirror", "solid", spec, inputs)
    if command_name == "boolean_solids":
        kind = {
            "union": "boolean_union",
            "subtract": "boolean_subtract",
            "intersect": "boolean_intersect",
        }[args["op"]]
        spec = {
            "feature": "boolean",
            "op": args["op"],
            "target": args["target"],
            "tools": list(args["tools"]),
        }
        inputs = {
            "target": _node_ref(args["target"]),
            "tools": {"type": "list", "value": [_node_ref(tool) for tool in args["tools"]]},
        }
        return _feature_node(node_id, name, kind, "solid", spec, inputs)
    if command_name == "insert_component":
        source = args["source"]
        spec = {
            "feature": "component_instance",
            "source": source,
            "translation_mm": args["translation_mm"],
            "rotation_deg": args["rotation_deg"],
            "grounded": args.get("grounded", False),
        }
        inputs = {}
        if source["kind"] == "local_node":
            inputs["geometry"] = _node_ref(source["node_id"])
        elif source["kind"] == "external_ref":
            inputs["external_ref"] = {"type": "external_ref", "value": source["ref_id"]}
        return _feature_node(node_id, name, "assembly_component", "component", spec, inputs)
    if command_name == "add_mate":
        spec = {
            "feature": "mate_constraint",
            "left": args["left"],
            "right": args["right"],
            "left_ref": args["left_ref"],
            "right_ref": args["right_ref"],
            "mate_kind": args["mate_kind"],
            "flip": args.get("flip", False),
        }
        inputs = {"left": _node_ref(args["left"]), "right": _node_ref(args["right"])}
        mate_kind = args["mate_kind"]
        if mate_kind["kind"] == "distance":
            inputs["distance"] = _dim_ir(mate_kind["value"])
        elif mate_kind["kind"] == "angle":
            inputs["angle"] = _dim_ir(mate_kind["value"])
        return _feature_node(node_id, name, "assembly_mate", "assembly", spec, inputs)
    if command_name == "draft_faces":
        spec = {
            "feature": "draft",
            "solid": args["solid"],
            "faces": list(args["faces"]),
            "pull_direction": args["pull_direction"],
            "neutral_plane": args["neutral_plane"],
            "angle": args["angle"],
        }
        inputs = {
            "solid": _node_ref(args["solid"]),
            "faces": _topo_list(args["faces"]),
            "pull_direction": _vec3_ir(args["pull_direction"]),
            "angle": _dim_ir(args["angle"]),
        }
        return _feature_node(node_id, name, "draft", "solid", spec, inputs)
    if command_name == "direct_face_edit":
        edit = args["edit"]
        spec = {
            "feature": "direct_face_edit",
            "solid": args["solid"],
            "face": args["face"],
            "edit": edit,
        }
        inputs = {
            "solid": _node_ref(args["solid"]),
            "face": {"type": "topo_ref_expr", "value": args["face"]},
        }
        if edit["op"] == "offset":
            offset = _dim_ir(edit["distance"])
            if offset is not None:
                inputs["offset"] = offset
        return _feature_node(node_id, name, "direct_face_edit", "solid", spec, inputs)
    if command_name == "tag_topology":
        targets = list(args.get("targets", []))
        spec = {
            "feature": "tag_topology",
            "solid": args["solid"],
            "tag": args["tag"],
            "targets": targets,
        }
        inputs = {
            "solid": _node_ref(args["solid"]),
            "faces": _topo_list(targets),
        }
        return _feature_node(node_id, name, "tag_faces", "solid", spec, inputs)
    if command_name == "add_design_assertion":
        spec = {
            "feature": "design_assertion",
            "assertion_id": args["assertion_id"],
            "kind": args["kind"],
        }
        if "target" in args:
            spec = {
                "feature": "design_assertion",
                "target": args["target"],
                "assertion_id": args["assertion_id"],
                "kind": args["kind"],
            }
        inputs = {}
        if "target" in args:
            inputs["target"] = _node_ref(args["target"])
        kind = args["kind"]
        if kind["rule"] == "min_wall_thickness":
            port = _dim_ir(kind["min"])
            if port is not None:
                inputs["min_wall_thickness"] = port
        elif kind["rule"] == "boolean_condition":
            inputs["condition"] = _node_ref(kind["condition"])
        return _feature_node(node_id, name, "assert_gate", "bool", spec, inputs)
    raise ValueError(f"unknown semantic command {command_name!r}")


def _hole_node(node_id: str, name: str, args: Mapping[str, Any]) -> _LiveNode:
    definition = args["definition"]
    common = {
        "solid": args["solid"],
        "origin_mm": args["origin_mm"],
        "axis": args["axis"],
    }
    if "target_face" in args:
        common = {"solid": args["solid"], "target_face": args["target_face"], **{k: common[k] for k in ("origin_mm", "axis")}}
    if definition["hole"] == "generic":
        spec = {"feature": "hole", **common, "diameter": definition["diameter"], "depth": definition["depth"], "profile": definition["profile"]}
        inputs: dict[str, Any] = {
            "solid": _node_ref(args["solid"]),
            "origin": _vec3_ir(args["origin_mm"]),
            "axis": _vec3_ir(args["axis"]),
            "diameter": _dim_ir(definition["diameter"]),
        }
        depth = definition["depth"]
        if depth["kind"] == "blind":
            inputs["depth"] = _dim_ir(depth["depth"])
        profile = definition["profile"]
        if profile["kind"] == "counterbore":
            inputs["bore_diameter"] = _dim_ir(profile["bore_diameter"])
            inputs["bore_depth"] = _dim_ir(profile["bore_depth"])
        elif profile["kind"] == "countersink":
            inputs["sink_diameter"] = _dim_ir(profile["sink_diameter"])
            inputs["sink_angle"] = _dim_ir(profile["sink_angle"])
        return _feature_node(node_id, name, "hole", "solid", spec, inputs)
    spec = {
        "feature": "std_fastener_hole",
        **common,
        "standard": definition["standard"],
        "designation": definition["designation"],
    }
    if "clearance_class" in definition:
        spec["clearance_class"] = definition["clearance_class"]
    spec["depth"] = definition["depth"]
    if "thread_depth" in definition:
        spec["thread_depth"] = definition["thread_depth"]
    inputs = {
        "solid": _node_ref(args["solid"]),
        "origin": _vec3_ir(args["origin_mm"]),
        "axis": _vec3_ir(args["axis"]),
    }
    if definition["depth"]["kind"] == "blind":
        inputs["depth"] = _dim_ir(definition["depth"]["depth"])
    if "thread_depth" in definition:
        inputs["thread_depth"] = _dim_ir(definition["thread_depth"])
    return _feature_node(node_id, name, "std_lib_call", "solid", spec, inputs)


def _pattern_node(node_id: str, name: str, args: Mapping[str, Any]) -> _LiveNode:
    definition = args["definition"]
    if definition["pattern"] == "linear":
        spec: dict[str, Any] = {
            "feature": "linear_pattern",
            "solid": args["solid"],
            "direction": definition["direction"],
            "count": definition["count"],
            "spacing": definition["spacing"],
        }
        inputs: dict[str, Any] = {
            "solid": _node_ref(args["solid"]),
            "direction": _vec3_ir(definition["direction"]),
            "count": _int_ir(definition["count"]),
            "spacing": _dim_ir(definition["spacing"]),
        }
        if "second_direction" in definition:
            second = definition["second_direction"]
            spec["second_direction"] = second
            inputs["second_direction"] = _vec3_ir(second["direction"])
            inputs["second_count"] = _int_ir(second["count"])
            inputs["second_spacing"] = _dim_ir(second["spacing"])
        _drop_none_ports(inputs)
        return _feature_node(node_id, name, "pattern_linear", "solid", spec, inputs)
    spec = {
        "feature": "polar_pattern",
        "solid": args["solid"],
        "axis_origin_mm": definition["axis_origin_mm"],
        "axis_direction": definition["axis_direction"],
        "count": definition["count"],
        "total_angle": definition["total_angle"],
    }
    inputs = {
        "solid": _node_ref(args["solid"]),
        "axis_origin": _vec3_ir(definition["axis_origin_mm"]),
        "axis": _vec3_ir(definition["axis_direction"]),
        "count": _int_ir(definition["count"]),
        "angle": _dim_ir(definition["total_angle"]),
    }
    _drop_none_ports(inputs)
    return _feature_node(node_id, name, "pattern_polar", "solid", spec, inputs)


def _feature_node(
    node_id: str,
    name: str,
    kind: str,
    output_type: str,
    spec: dict[str, Any],
    inputs: dict[str, Any],
) -> _LiveNode:
    stored = {key: value for key, value in inputs.items() if value is not None}
    stored["feature_spec"] = {"type": "string", "value": _compact_json(spec)}
    return _LiveNode(
        id=node_id,
        name=name,
        kind=kind,
        output_type=output_type,
        inputs=stored,
        refs=_collect_refs(stored),
    )


def _contract_payload_json(contract: Mapping[str, Any]) -> str:
    return _compact_json(dict(contract))


def _contract_live_node(contract: Mapping[str, Any]) -> _LiveNode:
    payload = _node_dict_from_contract(contract)
    return _LiveNode(
        id=payload["id"],
        name=payload["name"],
        kind="assert_gate",
        output_type="bool",
        inputs=payload["inputs"],
        metadata=payload["metadata"],
        state=payload["state"],
        refs=set(),
        outline=False,
    )


def _contract_node(contract: Mapping[str, Any]) -> dict[str, Any]:
    return _node_dict_from_contract(contract)


def _node_dict_from_contract(contract: Mapping[str, Any]) -> dict[str, Any]:
    expression = _contract_payload_json(contract)
    spec = {
        "feature": "design_assertion",
        "assertion_id": "design_contract",
        "kind": {
            "rule": "custom",
            "expression": expression,
            "params": {"role": "design_contract"},
        },
    }
    return {
        "id": CONTRACT_NODE_ID,
        "name": contract["part_name"],
        "kind": {"kind": "assert_gate"},
        "inputs": {"feature_spec": {"type": "string", "value": _compact_json(spec)}},
        "output_type": "bool",
        "state": {"status": "valid"},
        "kernel_op_version": None,
        "metadata": {"contract_json": expression, "role": "design_contract"},
    }


def _apply_feature_input_update(node: _LiveNode, args: Mapping[str, Any]) -> None:
    spec_input = node.inputs.get("feature_spec")
    if not isinstance(spec_input, Mapping) or not isinstance(spec_input.get("value"), str):
        raise ValueError(
            f"node {node.id!r} has no editable literal input; set_feature_input supports primitive_box and hole"
        )
    spec = json.loads(spec_input["value"])
    feature = spec.get("feature")
    field = str(args["input"]).strip()
    if feature == "primitive_box":
        if field in {"width", "depth", "height"}:
            _replace_literal_dim(spec, field, args)
        elif field == "origin_mm":
            spec["origin_mm"] = _require_vector_arg(field, args)
        else:
            raise ValueError(
                f"primitive_box {field!r} is not editable; use width, depth, height, or origin_mm"
            )
    elif feature == "hole":
        if field == "diameter":
            _replace_literal_dim(spec, field, args)
        elif field == "origin_mm":
            spec["origin_mm"] = _require_vector_arg(field, args)
        elif field == "axis":
            vec = _require_vector_arg(field, args)
            if math.sqrt(sum(c * c for c in vec)) <= 1e-12:
                raise ValueError("axis vector must be non-zero")
            spec["axis"] = vec
        else:
            raise ValueError(f"hole {field!r} is not editable; use diameter, origin_mm, or axis")
    else:
        raise ValueError(
            f"node {node.id!r} has no editable literal input; set_feature_input supports primitive_box and hole"
        )
    node.inputs["feature_spec"] = {
        "type": "string",
        "value": json.dumps(spec, separators=(",", ":")),
    }


def _replace_literal_dim(spec: dict[str, Any], field: str, args: Mapping[str, Any]) -> None:
    current = spec.get(field)
    if isinstance(current, Mapping):
        if current.get("source") == "param":
            raise ValueError(
                f"{field!r} is bound to parameter {current.get('param_id')!r}; use upsert_parameter instead"
            )
        if current.get("source") == "node_output":
            raise ValueError(
                f"{field!r} is bound to node {current.get('node_id')!r}; refusing to replace with literal"
            )
    if "vector_mm" in args:
        raise ValueError(f"{field!r} takes length_mm, not vector_mm")
    length_mm = args.get("length_mm")
    if isinstance(length_mm, bool) or not isinstance(length_mm, (int, float)):
        raise ValueError(f"{field!r} requires length_mm")
    value = float(length_mm)
    if not math.isfinite(value) or value <= 0.0:
        raise ValueError(f"{field!r} length_mm must be a finite number greater than zero")
    spec[field] = {"source": "literal", "dimension": "length", "value": value, "unit": "mm"}


def _require_vector_arg(field: str, args: Mapping[str, Any]) -> list[float]:
    if "length_mm" in args:
        raise ValueError(f"{field!r} takes vector_mm, not length_mm")
    vec = args.get("vector_mm")
    if vec is None:
        raise ValueError(f"{field!r} requires vector_mm")
    return _vec3(vec)


def _node_dict(node: _LiveNode) -> dict[str, Any]:
    return {
        "id": node.id,
        "name": node.name,
        "kind": {"kind": node.kind},
        "inputs": copy.deepcopy(node.inputs),
        "output_type": node.output_type,
        "state": dict(node.state),
        "kernel_op_version": None,
        "metadata": dict(node.metadata),
    }


def _pending_parameter(command: Mapping[str, Any]) -> dict[str, Any] | None:
    args = command["args"]
    ir_value = args["value"]
    ir_type = args["ir_type"]
    raw: Any
    if ir_value["type"] == "quantity":
        quantity = ir_value["value"]
        raw = Quantity(quantity["dimension"], quantity["value"], quantity.get("unit"))
    elif ir_value["type"] == "int":
        raw = ir_value["value"]
    elif ir_value["type"] == "bool":
        raw = ir_value["value"]
    elif ir_value["type"] == "string":
        raw = ir_value["value"]
    else:
        return None
    return {
        "name": args["name"],
        "ir_type": ir_type,
        "raw_value": raw,
        "formula": args.get("formula"),
    }


def _dim_ir(value: Mapping[str, Any]) -> dict[str, Any] | None:
    source = value["source"]
    if source == "param":
        return None
    if source == "literal":
        quantity = {"dimension": value["dimension"], "value": value["value"]}
        if "unit" in value:
            quantity["unit"] = value["unit"]
        return {"type": "quantity", "value": quantity}
    if source == "node_output":
        return {
            "type": "node_ref",
            "value": {"node_id": value["node_id"], "port": value.get("port", "out")},
        }
    raise ValueError(f"unknown dimension source {source!r}")


def _int_ir(value: Mapping[str, Any]) -> dict[str, Any] | None:
    source = value["source"]
    if source == "param":
        return None
    if source == "literal":
        return {"type": "int", "value": value["value"]}
    if source == "node_output":
        return {
            "type": "node_ref",
            "value": {"node_id": value["node_id"], "port": value.get("port", "out")},
        }
    raise ValueError(f"unknown integer source {source!r}")


def _drop_none_ports(inputs: dict[str, Any]) -> None:
    for key in [key for key, value in inputs.items() if value is None]:
        del inputs[key]


def _node_ref(node_id: str) -> dict[str, Any]:
    return {"type": "node_ref", "value": {"node_id": node_id, "port": "out"}}


def _vec3_ir(values: Sequence[float]) -> dict[str, Any]:
    return {"type": "vec3", "value": list(values)}


def _selectors(values: Sequence[str] | None, kind: str) -> list[str]:
    """Topology selectors are plain strings on the command wire."""
    if values is None:
        return []
    if isinstance(values, str):
        raise TypeError(f"{kind} selectors must be a sequence of strings")
    selectors: list[str] = []
    for item in values:
        if not isinstance(item, str) or not item.strip():
            raise ValueError(f"{kind} selector must be a non-empty string")
        selectors.append(item)
    return selectors


def _topo_list(values: Sequence[str]) -> dict[str, Any]:
    return {"type": "list", "value": [{"type": "topo_ref_expr", "value": item} for item in values]}


def _collect_refs(value: Any) -> set[str]:
    found: set[str] = set()
    if isinstance(value, Mapping):
        if value.get("type") == "node_ref":
            nested = value.get("value")
            if isinstance(nested, Mapping) and isinstance(nested.get("node_id"), str):
                found.add(nested["node_id"])
        else:
            for child in value.values():
                found.update(_collect_refs(child))
    elif isinstance(value, list):
        for child in value:
            found.update(_collect_refs(child))
    return found


def _parameter_binding(value: Any) -> tuple[str, dict[str, Any], Any]:
    if isinstance(value, Quantity):
        return value.dimension, ir_quantity(value), value
    if isinstance(value, bool):
        return "bool", {"type": "bool", "value": value}, value
    if isinstance(value, int):
        return "int", {"type": "int", "value": value}, value
    if isinstance(value, str):
        return "string", {"type": "string", "value": value}, value
    raise TypeError(
        "parameter value must be a Quantity, int, bool, or str. "
        "Lengths and angles need a unit, for example mm(40) or deg(90)."
    )


def _coerce_quantity(value: Any, dimension: str) -> Quantity:
    if isinstance(value, Quantity):
        if value.dimension != dimension:
            raise TypeError(f"expected a {dimension} quantity, got {value.dimension}")
        return value
    if isinstance(value, bool) or not isinstance(value, Real):
        raise TypeError(
            f"expected a {dimension} Quantity, a number, or a parameter name"
        )
    if dimension == "length":
        return mm(float(value))
    if dimension == "angle":
        return deg(float(value))
    raise TypeError(f"a bare number cannot supply a {dimension}; pass a Quantity")


def _literal_dim(value: Mapping[str, Any], dimension: str) -> dict[str, Any]:
    if value.get("dimension") != dimension:
        raise TypeError(f"expected a {dimension} quantity, got {value.get('dimension')!r}")
    payload: dict[str, Any] = {
        "source": "literal",
        "dimension": dimension,
        "value": _num(value.get("value"), dimension),
    }
    if dimension != "density":
        unit = value.get("unit")
        if not isinstance(unit, str):
            raise ValueError("literal dimension is missing a unit")
        payload["unit"] = unit
    return payload


def _formula_dependencies(formula: str, parameter_name: str) -> list[str]:
    dependencies: list[str] = []
    for match in _IDENT.finditer(formula):
        token = match.group(0)
        if token in _FORMULA_WORDS:
            continue
        if token == parameter_name:
            raise ValueError(f"parameter {parameter_name!r} formula refers to itself")
        if token not in dependencies:
            dependencies.append(token)
    return dependencies


def _parameter_name(name: str) -> str:
    if not isinstance(name, str) or _PARAM_NAME.fullmatch(name) is None:
        raise ValueError(
            "parameter id must start with a letter or underscore and use only letters, digits, and underscores"
        )
    return name


def _feature_name(name: str) -> str:
    if not isinstance(name, str) or not name.strip():
        raise ValueError("name cannot be empty")
    return name


def _explicit_id(node_id: str, nodes: Mapping[str, _LiveNode]) -> str:
    if not isinstance(node_id, str) or not node_id.strip():
        raise ValueError("node id cannot be empty")
    if node_id == CONTRACT_NODE_ID:
        raise ValueError(f"node id {CONTRACT_NODE_ID!r} is reserved for the design contract")
    if node_id in nodes:
        raise ValueError(f"node id {node_id!r} already exists")
    return node_id


def _handle(handle: str) -> str:
    if not isinstance(handle, str) or not any(not char.isspace() for char in handle):
        raise ValueError("blob handle must contain a non-space character")
    return handle


def _text(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{label} must be a string")
    return value


def _optional_text(value: str | None, label: str) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{label} cannot be empty")
    return value


def _entity_id(entity_id: str) -> str:
    if not isinstance(entity_id, str) or not entity_id.strip():
        raise ValueError("sketch entity id cannot be empty")
    return entity_id


def _flag(value: bool, label: str) -> None:
    if not isinstance(value, bool):
        raise TypeError(f"{label} must be a bool")


def _body_op(operation: str) -> str:
    try:
        return _BODY_OPS[operation]
    except KeyError as exc:
        raise ValueError("operation must be new_body, join, cut, or intersect") from exc


def _boolean_op(operation: str) -> str:
    try:
        return _BOOLEAN_OPS[operation]
    except KeyError as exc:
        raise ValueError("boolean op must be union, subtract, or intersect") from exc


def _num(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, Real):
        raise TypeError(f"{label} must be a real number")
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"{label} must be finite")
    return 0.0 if number == 0.0 else number


def _vec2(value: Sequence[float]) -> list[float]:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence) or len(value) != 2:
        raise ValueError("expected 2 numbers")
    return [_num(value[0], "x"), _num(value[1], "y")]


def _vec3(value: Sequence[float]) -> list[float]:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence) or len(value) != 3:
        raise ValueError("expected 3 numbers")
    return [_num(item, "component") for item in value]


def _direction(value: Sequence[float], label: str) -> list[float]:
    vector = _vec3(value)
    if vector == [0.0, 0.0, 0.0]:
        raise ValueError(f"{label} cannot be the zero vector")
    return vector


def _plane(plane: str | Mapping[str, Any]) -> dict[str, Any]:
    if isinstance(plane, str):
        if plane not in _PLANES:
            raise ValueError("plane must be xy, xz, yz, or a plane object")
        return {"kind": "datum", "plane": plane}
    if isinstance(plane, Mapping) and plane.get("kind") in {"datum", "face_ref", "custom"}:
        return copy.deepcopy(dict(plane))
    raise TypeError("plane must be xy, xz, yz, or a plane object")


def _normalize_entities(entities: Sequence[Mapping[str, Any]] | None) -> list[dict[str, Any]]:
    if not entities:
        return []
    normalized: list[dict[str, Any]] = []
    seen: set[str] = set()
    for entity in entities:
        if not isinstance(entity, Mapping) or not isinstance(entity.get("kind"), str):
            raise TypeError("sketch entity must be an object with kind")
        kind = entity["kind"]
        entity_id = _entity_id(str(entity.get("id", "")))
        if entity_id in seen:
            raise ValueError(f"duplicate sketch entity {entity_id!r}")
        seen.add(entity_id)
        if kind == "point":
            item = {
                "kind": "point",
                "id": entity_id,
                "position_mm": _vec2(entity["position_mm"]),
                "fixed": bool(entity.get("fixed", False)),
                "construction": bool(entity.get("construction", False)),
            }
        elif kind == "line":
            item = line_entity(
                entity_id,
                entity["start_mm"],
                entity["end_mm"],
                construction=bool(entity.get("construction", False)),
            )
        elif kind == "arc":
            _flag(bool(entity.get("ccw", False)) or entity.get("ccw", False) is False, "ccw")
            item = {
                "kind": "arc",
                "id": entity_id,
                "center_mm": _vec2(entity["center_mm"]),
                "start_mm": _vec2(entity["start_mm"]),
                "end_mm": _vec2(entity["end_mm"]),
                "ccw": bool(entity.get("ccw", False)),
                "construction": bool(entity.get("construction", False)),
            }
        elif kind == "circle":
            item = circle_entity(
                entity_id,
                entity["center_mm"],
                entity["radius_mm"],
                construction=bool(entity.get("construction", False)),
            )
        elif kind == "rectangle":
            item = rectangle_entity(
                entity_id,
                entity["origin_mm"],
                entity["width_mm"],
                entity["height_mm"],
                centered=bool(entity.get("centered", False)),
                construction=bool(entity.get("construction", False)),
            )
        elif kind == "spline_blob":
            item = spline_blob_entity(
                entity_id,
                str(entity["blob"]),
                closed=bool(entity.get("closed", False)),
                construction=bool(entity.get("construction", False)),
            )
        else:
            raise ValueError(f"unknown sketch entity kind {kind!r}")
        normalized.append(item)
    return normalized


def _normalize_profiles(
    profiles: Sequence[Mapping[str, Any]] | None,
    entities: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    if not profiles:
        return []
    known = {str(entity["id"]) for entity in entities}
    normalized: list[dict[str, Any]] = []
    seen: set[str] = set()
    for profile in profiles:
        if not isinstance(profile, Mapping):
            raise TypeError("sketch profile must be an object")
        item = profile_loop(
            str(profile.get("id", "")),
            list(profile.get("entity_ids", [])),
            is_outer=bool(profile.get("is_outer", False)) if "is_outer" in profile else True,
        )
        if "is_outer" in profile and not isinstance(profile["is_outer"], bool):
            raise TypeError("is_outer must be a bool")
        if item["id"] in seen:
            raise ValueError(f"duplicate sketch profile {item['id']!r}")
        seen.add(item["id"])
        for entity_id in item["entity_ids"]:
            if entity_id not in known:
                raise ValueError(f"profile {item['id']!r} references unknown entity {entity_id!r}")
        normalized.append(item)
    return normalized


def _normalize_constraints(
    constraints: Sequence[Mapping[str, Any]] | None,
    entities: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    if not constraints:
        return []
    known = {str(entity["id"]) for entity in entities}
    normalized: list[dict[str, Any]] = []
    for constraint in constraints:
        if not isinstance(constraint, Mapping) or not isinstance(constraint.get("kind"), str):
            raise TypeError("sketch constraint must be an object with kind")
        kind = constraint["kind"]
        item: dict[str, Any] = {"kind": kind}
        name = constraint.get("name")
        if name is not None:
            if not isinstance(name, str) or not name.strip():
                raise ValueError("constraint name cannot be empty")
            item["name"] = name
        if kind == "coincident":
            item["a"] = _point_ref(constraint["a"], known)
            item["b"] = _point_ref(constraint["b"], known)
        elif kind == "point_on_line":
            item["point"] = _point_ref(constraint["point"], known)
            item["line"] = _known_entity(constraint["line"], known)
        elif kind in {"horizontal", "vertical"}:
            item["entity"] = _known_entity(constraint["entity"], known)
        elif kind in {"parallel", "perpendicular", "tangent", "concentric", "equal"}:
            item["a"] = _known_entity(constraint["a"], known)
            item["b"] = _known_entity(constraint["b"], known)
        elif kind == "symmetric":
            item["a"] = _point_ref(constraint["a"], known)
            item["b"] = _point_ref(constraint["b"], known)
            item["axis_line"] = _known_entity(constraint["axis_line"], known)
        elif kind == "distance":
            item["a"] = _point_ref(constraint["a"], known)
            item["b"] = _point_ref(constraint["b"], known)
            item["value"] = _sketch_dim(constraint["value"], "length")
        elif kind in {"radius", "diameter"}:
            item["entity"] = _known_entity(constraint["entity"], known)
            item["value"] = _sketch_dim(constraint["value"], "length")
        elif kind == "angle":
            item["a"] = _known_entity(constraint["a"], known)
            item["b"] = _known_entity(constraint["b"], known)
            item["value"] = _sketch_dim(constraint["value"], "angle")
        else:
            raise ValueError(f"unknown sketch constraint kind {kind!r}")
        normalized.append(item)
    return normalized


def _known_entity(value: Any, known: set[str]) -> str:
    if not isinstance(value, str) or value not in known:
        raise ValueError(f"unknown sketch entity {value!r}")
    return value


def _point_ref(value: Any, known: set[str]) -> dict[str, str]:
    if isinstance(value, str):
        return {"entity_id": _known_entity(value, known), "anchor": "point"}
    if isinstance(value, Mapping):
        anchor = value.get("anchor", "point")
        if anchor not in _ANCHORS:
            raise ValueError("sketch anchor must be point, start, end, center, or midpoint")
        return {"entity_id": _known_entity(value.get("entity_id"), known), "anchor": str(anchor)}
    raise TypeError("sketch point reference must be an entity id or an object")


def _sketch_dim(value: Any, dimension: str) -> dict[str, Any]:
    if isinstance(value, str):
        if _PARAM_NAME.fullmatch(value) is None:
            raise ValueError(f"parameter id {value!r} is not a valid name")
        return {"binding": "param", "param_id": value}
    if isinstance(value, Mapping) and value.get("binding") == "param":
        return {"binding": "param", "param_id": _parameter_name(str(value["param_id"]))}
    if isinstance(value, Mapping) and value.get("binding") == "literal":
        quantity = _coerce_quantity(
            Quantity(str(value["dimension"]), float(value["value"]), value.get("unit")),
            dimension,
        )
        payload = {"binding": "literal"}
        payload.update(quantity.to_json())
        return payload
    quantity = _coerce_quantity(value, dimension)
    payload = {"binding": "literal"}
    payload.update(quantity.to_json())
    return payload


def _slug(text: str, fallback: str) -> str:
    raw = re.sub(r"[^0-9A-Za-z_]+", "_", text.strip()).strip("_").lower()
    if not raw or not raw[0].isalpha():
        return fallback
    return raw


def _compact_json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), allow_nan=False, ensure_ascii=False)


def semantic_command_names() -> tuple[str, ...]:
    """Names of SemanticModelBuilder methods that emit one SemanticCommand."""
    names = [
        name
        for name, member in SemanticModelBuilder.__dict__.items()
        if getattr(member, "_aicad_semantic_command", False)
    ]
    return tuple(names)


def _merge_sketch_entities(
    current: Sequence[Mapping[str, Any]],
    incoming: Sequence[Mapping[str, Any]],
    replace: bool,
) -> list[dict[str, Any]]:
    if replace:
        return [copy.deepcopy(dict(item)) for item in incoming]
    merged = [copy.deepcopy(dict(item)) for item in current]
    index = {item["id"]: position for position, item in enumerate(merged)}
    for entity in incoming:
        item = copy.deepcopy(dict(entity))
        slot = index.get(item["id"])
        if slot is None:
            index[item["id"]] = len(merged)
            merged.append(item)
        else:
            merged[slot] = item
    return merged


def _merge_sketch_constraints(
    current: Sequence[Mapping[str, Any]],
    incoming: Sequence[Mapping[str, Any]],
    replace: bool,
) -> list[dict[str, Any]]:
    if replace:
        return [copy.deepcopy(dict(item)) for item in incoming]
    merged = [copy.deepcopy(dict(item)) for item in current]
    for constraint in incoming:
        item = copy.deepcopy(dict(constraint))
        name = item.get("name")
        if isinstance(name, str):
            slot = next(
                (position for position, existing in enumerate(merged) if existing.get("name") == name),
                None,
            )
            if slot is not None:
                merged[slot] = item
                continue
        merged.append(item)
    return merged


def _next_sketch_spec(spec: Mapping[str, Any], command: Mapping[str, Any]) -> dict[str, Any]:
    args = command["args"]
    entities = list(spec.get("entities", []))
    constraints = list(spec.get("constraints", []))
    profiles = list(spec.get("profiles", []))
    if command["command"] == "upsert_sketch_entities":
        incoming = _normalize_entities(args.get("entities"))
        if not incoming:
            raise ValueError("sketch entity upsert cannot be empty")
        entities = _merge_sketch_entities(entities, incoming, bool(args.get("replace")))
    elif command["command"] == "upsert_sketch_constraints":
        incoming = _normalize_constraints(args.get("constraints"), entities)
        if not incoming:
            raise ValueError("sketch constraint upsert cannot be empty")
        constraints = _merge_sketch_constraints(constraints, incoming, bool(args.get("replace")))
    else:
        raise ValueError(f"unknown sketch update {command['command']!r}")
    return {
        "feature": "sketch_2d",
        "plane": copy.deepcopy(spec["plane"]),
        "entities": entities,
        "profiles": _normalize_profiles(profiles, entities),
        "constraints": _normalize_constraints(constraints, entities),
    }


def _sketch_inputs(spec: Mapping[str, Any]) -> dict[str, Any]:
    entities = list(spec.get("entities", []))
    inputs: dict[str, Any] = {}
    handles = [entity["blob"] for entity in entities if entity.get("kind") == "spline_blob"]
    if handles:
        inputs["blobs"] = {
            "type": "list",
            "value": [{"type": "blob_ref", "value": handle} for handle in handles],
        }
    inputs["feature_spec"] = {"type": "string", "value": _compact_json(dict(spec))}
    return inputs


def _tree_mentions_param(value: Any, name: str) -> bool:
    if isinstance(value, Mapping):
        if value.get("source") == "param" and value.get("param_id") == name:
            return True
        if value.get("binding") == "param" and value.get("param_id") == name:
            return True
        return any(_tree_mentions_param(child, name) for child in value.values())
    if isinstance(value, list):
        return any(_tree_mentions_param(child, name) for child in value)
    if isinstance(value, str):
        text = value.strip()
        if text.startswith("{") or text.startswith("["):
            try:
                parsed = json.loads(text)
            except json.JSONDecodeError:
                return False
            return _tree_mentions_param(parsed, name)
    return False

"""Data contracts and validators aligned with `schemas/*.schema.json`."""

from __future__ import annotations

import math
import re
from collections.abc import Iterable
from copy import deepcopy
from dataclasses import dataclass, field
from typing import Any, Literal

Severity = Literal["error", "warning", "info"]
CompileStatus = Literal["ok", "error"]
GateLevel = Literal["L0", "L1", "L2", "L3", "L4"]
TriVerdict = Literal["PASS", "FAIL", "UNKNOWN"]
VerificationLayer = Literal[
    "L0_structure_units",
    "L1_geometry_topology",
    "L2_interfaces_constraints",
    "L3_manufacturability_physics",
    "L4_interop_roundtrip",
]

VALID_SEVERITIES: tuple[str, ...] = ("error", "warning", "info")
VALID_STATUSES: tuple[str, ...] = ("ok", "error")
VALID_GATE_LEVELS: tuple[str, ...] = ("L0", "L1", "L2", "L3", "L4")
VALID_TRI_VERDICTS: tuple[TriVerdict, ...] = ("PASS", "FAIL", "UNKNOWN")
VERIFICATION_LAYERS: tuple[VerificationLayer, ...] = (
    "L0_structure_units",
    "L1_geometry_topology",
    "L2_interfaces_constraints",
    "L3_manufacturability_physics",
    "L4_interop_roundtrip",
)
VALID_LENGTH_UNITS: tuple[str, ...] = ("mm", "m", "in")
VALID_ANGLE_UNITS: tuple[str, ...] = ("deg", "rad")
VALID_FORCE_UNITS: tuple[str, ...] = ("N", "kN")
VALID_KEEP_OUT_KINDS: tuple[str, ...] = ("box", "cylinder", "sphere")
VALID_KEEP_OUT_AXES: tuple[str, ...] = (
    "+X",
    "-X",
    "+Y",
    "-Y",
    "+Z",
    "-Z",
    "X_AXIS",
    "Y_AXIS",
    "Z_AXIS",
)
VALID_AXIS_NORMALS: tuple[str, ...] = ("+X", "-X", "+Y", "-Y", "+Z", "-Z")
VALID_INTERFACE_KINDS: tuple[str, ...] = (
    "bore",
    "shaft_seat",
    "mounting_face",
    "bolt_pattern",
    "counterbore_pattern",
    "slot",
    "datum",
)
VALID_LOAD_KINDS: tuple[str, ...] = ("force", "torque", "pressure", "fixed_support")
VALID_LOAD_UNITS: tuple[str, ...] = ("N", "kN", "N*m", "MPa")
VALID_MANUFACTURING_METHODS: tuple[str, ...] = (
    "CNC_3AXIS_MILLING",
    "CNC_5AXIS_MILLING",
    "CNC_TURNING",
    "LASER_CUTTING",
    "SHEET_METAL_BENDING",
    "ADDITIVE_FDM",
    "ADDITIVE_SLM",
    "INJECTION_MOLDING",
    "CASTING",
)

PART_NAME_PATTERN = re.compile(r"^[A-Za-z][A-Za-z0-9_]*$")


def canonical_manufacturing_method(method: str) -> str:
    """Return the exact schema enum value for a process name, or the original text if unknown."""
    text = str(method).strip()
    folded = text.upper().replace("-", "_").replace(" ", "_")
    while "__" in folded:
        folded = folded.replace("__", "_")
    for candidate in VALID_MANUFACTURING_METHODS:
        if candidate.upper() == folded:
            return candidate
    return text


def aggregate_tri_verdicts(statuses: Iterable[TriVerdict | str]) -> TriVerdict:
    """Zero-trust tri-state aggregation: any FAIL -> FAIL; else any UNKNOWN/empty -> UNKNOWN; else PASS."""
    items = list(statuses)
    if not items:
        return "UNKNOWN"
    for item in items:
        if item not in VALID_TRI_VERDICTS:
            raise ValueError(f"Invalid TriVerdict: {item!r}")
    if "FAIL" in items:
        return "FAIL"
    if "UNKNOWN" in items:
        return "UNKNOWN"
    return "PASS"


class ContractValidationError(ValueError):
    """Raised when a contract or compiler payload fails schema validation."""

    def __init__(self, errors: list[str]) -> None:
        self.errors = errors
        super().__init__("; ".join(errors))


@dataclass(slots=True)
class BBox3D:
    """Axis-aligned 3D bounding box in millimeters."""

    min: list[float]
    max: list[float]

    def validate(self, label: str = "bbox_mm") -> list[str]:
        errors: list[str] = []
        if len(self.min) != 3 or len(self.max) != 3:
            errors.append(f"{label} must have 3-element min and max vectors")
            return errors
        for axis_idx, (lo, hi) in enumerate(zip(self.min, self.max)):
            if (
                isinstance(lo, bool)
                or isinstance(hi, bool)
                or not isinstance(lo, (int, float))
                or not isinstance(hi, (int, float))
            ):
                errors.append(f"{label}[{axis_idx}] must be numeric")
            elif lo > hi:
                errors.append(f"{label} min[{axis_idx}]={lo} exceeds max[{axis_idx}]={hi}")
        return errors

    @property
    def dimensions(self) -> tuple[float, float, float]:
        return (
            float(self.max[0]) - float(self.min[0]),
            float(self.max[1]) - float(self.min[1]),
            float(self.max[2]) - float(self.min[2]),
        )

    @property
    def center(self) -> tuple[float, float, float]:
        return (
            (float(self.min[0]) + float(self.max[0])) * 0.5,
            (float(self.min[1]) + float(self.max[1])) * 0.5,
            (float(self.min[2]) + float(self.max[2])) * 0.5,
        )

    def contains(self, inner: BBox3D, tolerance: float = 1e-5) -> tuple[bool, list[str]]:
        """Check whether `inner` stays completely inside this Keep-In bounding box."""
        violations: list[str] = []
        axes = ("X", "Y", "Z")
        for idx, axis in enumerate(axes):
            if inner.min[idx] < self.min[idx] - tolerance:
                violations.append(
                    f"{axis}_min={inner.min[idx]:.4f}mm < keep_in_min={self.min[idx]:.4f}mm"
                )
            if inner.max[idx] > self.max[idx] + tolerance:
                violations.append(
                    f"{axis}_max={inner.max[idx]:.4f}mm > keep_in_max={self.max[idx]:.4f}mm"
                )
        return (len(violations) == 0, violations)

    def intersects(self, other: BBox3D, tolerance: float = 1e-5) -> bool:
        """Return True if two bounding boxes overlap with positive clearance margin."""
        for idx in range(3):
            if self.max[idx] <= other.min[idx] + tolerance:
                return False
            if other.max[idx] <= self.min[idx] + tolerance:
                return False
        return True

    def shrunk(self, margin: float) -> BBox3D:
        """Return a box inset by `margin` on each face (clamped at center)."""
        lo: list[float] = []
        hi: list[float] = []
        for idx in range(3):
            a = float(self.min[idx]) + margin
            b = float(self.max[idx]) - margin
            if a > b:
                mid = (float(self.min[idx]) + float(self.max[idx])) * 0.5
                lo.append(mid)
                hi.append(mid)
            else:
                lo.append(a)
                hi.append(b)
        return BBox3D(min=lo, max=hi)

    def to_dict(self) -> dict[str, list[float]]:
        return {"min": [float(v) for v in self.min], "max": [float(v) for v in self.max]}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> BBox3D:
        return cls(
            min=[float(v) for v in data["min"]],
            max=[float(v) for v in data["max"]],
        )


EXTERNAL_LENGTH_UNITS = ("um", "mm", "cm", "in", "ft", "m")


def _inspect_require(condition: bool, label: str) -> None:
    if not condition:
        raise ContractValidationError([f"Invalid external inspection field: {label}"])


def _inspect_object(value: Any, label: str, required: str, optional: str = "") -> dict:
    keys = set(required.split())
    allowed = keys | set(optional.split())
    _inspect_require(isinstance(value, dict) and keys <= value.keys() <= allowed, label)
    return value


def _inspect_number(value: Any, label: str) -> float:
    try:
        if type(value) in (float, int) and math.isfinite(value):
            return float(value)
    except OverflowError:
        pass
    raise ContractValidationError([f"External inspection {label} must be a finite number"])


def _inspect_vector(value: Any, label: str, *, direction: bool = False) -> list[float]:
    _inspect_require(isinstance(value, list) and len(value) == 3, label)
    vec = [_inspect_number(item, label) for item in value]
    if direction:
        norm = math.hypot(*vec)
        _inspect_require(math.isfinite(norm) and norm > 0, label)
        return [item / norm for item in vec]
    return vec


def _inspect_bbox(value: Any, label: str) -> BBox3D:
    obj = _inspect_object(value, label, "min max")
    bbox = BBox3D(min=_inspect_vector(obj["min"], label), max=_inspect_vector(obj["max"], label))
    _inspect_require(all(math.isfinite(dim) and dim >= 0 for dim in bbox.dimensions), label)
    return bbox


def _inspect_positive(value: Any, label: str) -> float:
    number = _inspect_number(value, label)
    _inspect_require(number > 0, label)
    return number


def _inspect_text(value: Any, label: str) -> None:
    _inspect_require(isinstance(value, str) and bool(value), label)


@dataclass(slots=True)
class ExternalInterfaceSummary:
    """Validated inspect geometry. Asset-controlled text is never retained for prompts."""

    bbox_mm: BBox3D
    datum_planes: list[dict[str, Any]]
    hole_groups: list[dict[str, Any]]
    cylindrical_bosses: list[dict[str, Any]]
    # Host-only identity for rechecking the inspected bytes. Never emit it to prompts.
    exact_asset_path: str | None = field(default=None, repr=False, compare=False)
    exact_asset_sha256: str | None = field(default=None, repr=False, compare=False)

    def to_dict(self) -> dict[str, Any]:
        return {
            "bbox_mm": self.bbox_mm.to_dict(),
            "datum_planes": deepcopy(self.datum_planes),
            "hole_groups": deepcopy(self.hole_groups),
            "cylindrical_bosses": deepcopy(self.cylindrical_bosses),
        }

    @classmethod
    def from_cli_json(cls, data: Any) -> ExternalInterfaceSummary:
        obj = _inspect_object(
            data,
            "summary",
            "status file format sha256 unit_detected watertight bbox_mm "
            "volume_mm3 components datum_planes hole_groups cylindrical_bosses",
        )
        _inspect_require(obj["status"] == "ok", "status")
        _inspect_text(obj["file"], "file")
        _inspect_require(
            obj["format"] in ("STEP", "BREP", "IGES", "STL", "3MF", "OBJ", "GLTF"),
            "format",
        )
        digest = obj["sha256"]
        _inspect_require(
            isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest) is not None,
            "sha256",
        )
        _inspect_require(
            obj["unit_detected"] in (*EXTERNAL_LENGTH_UNITS, "unknown"),
            "unit_detected",
        )
        _inspect_require(type(obj["watertight"]) is bool, "watertight")
        _inspect_require(_inspect_number(obj["volume_mm3"], "volume_mm3") >= 0, "volume_mm3")
        for key in ("components", "datum_planes", "hole_groups", "cylindrical_bosses"):
            _inspect_require(isinstance(obj[key], list), key)
        bbox = _inspect_bbox(obj["bbox_mm"], "bbox_mm")
        for item in obj["components"]:
            comp = _inspect_object(item, "component", "component_id part_name bbox_mm")
            _inspect_text(comp["component_id"], "component_id")
            _inspect_text(comp["part_name"], "part_name")
            _inspect_bbox(comp["bbox_mm"], "component.bbox_mm")
        planes = []
        for item in obj["datum_planes"]:
            plane = _inspect_object(item, "datum_plane", "id normal origin_mm area_mm2")
            _inspect_text(plane["id"], "datum_plane.id")
            planes.append(
                {
                    "normal": _inspect_vector(plane["normal"], "normal", direction=True),
                    "origin_mm": _inspect_vector(plane["origin_mm"], "origin_mm"),
                    "area_mm2": _inspect_positive(plane["area_mm2"], "area_mm2"),
                }
            )
        groups = []
        for index, item in enumerate(obj["hole_groups"], 1):
            group = _inspect_object(
                item,
                "hole_group",
                "group_id pattern count diameter_mm depth_mm axis centers_mm",
                "pitch_circle_diameter_mm",
            )
            _inspect_text(group["group_id"], "group_id")
            _inspect_text(group["pattern"], "pattern")
            count, centers = group["count"], group["centers_mm"]
            _inspect_require(
                type(count) is int
                and count > 0
                and isinstance(centers, list)
                and len(centers) == count,
                "hole_group.count/centers_mm",
            )
            geometry = {
                "tag": f"external_mount_holes_{index}",
                "count": count,
                "diameter_mm": _inspect_positive(group["diameter_mm"], "diameter_mm"),
                "depth_mm": _inspect_positive(group["depth_mm"], "depth_mm"),
                "axis": _inspect_vector(group["axis"], "axis", direction=True),
                "centers_mm": [_inspect_vector(center, "centers_mm") for center in centers],
            }
            if "pitch_circle_diameter_mm" in group:
                geometry["pitch_circle_diameter_mm"] = _inspect_positive(
                    group["pitch_circle_diameter_mm"], "pitch_circle_diameter_mm"
                )
            groups.append(geometry)
        bosses = []
        for item in obj["cylindrical_bosses"]:
            boss = _inspect_object(
                item, "cylindrical_boss", "id diameter_mm height_mm axis base_center_mm"
            )
            _inspect_text(boss["id"], "cylindrical_boss.id")
            bosses.append(
                {
                    "diameter_mm": _inspect_positive(boss["diameter_mm"], "diameter_mm"),
                    "height_mm": _inspect_positive(boss["height_mm"], "height_mm"),
                    "axis": _inspect_vector(boss["axis"], "axis", direction=True),
                    "base_center_mm": _inspect_vector(boss["base_center_mm"], "base_center_mm"),
                }
            )
        return cls(bbox, planes, groups, bosses)


def _derive_zone_bbox(
    *,
    kind: str,
    origin_mm: list[float],
    size_mm: list[float] | None,
    diameter_mm: float | None,
    height_mm: float | None,
    axis: str,
) -> BBox3D:
    ox, oy, oz = (float(origin_mm[0]), float(origin_mm[1]), float(origin_mm[2]))
    if kind == "box" and size_mm is not None and len(size_mm) == 3:
        return BBox3D(
            min=[ox, oy, oz],
            max=[ox + float(size_mm[0]), oy + float(size_mm[1]), oz + float(size_mm[2])],
        )
    if kind == "cylinder" and diameter_mm is not None and height_mm is not None:
        r = float(diameter_mm) * 0.5
        h = float(height_mm)
        ax = axis.upper().lstrip("+")
        if ax.startswith("-"):
            ax = ax[1:]
        if ax.startswith("X"):
            x0, x1 = (ox - h, ox) if axis.startswith("-") else (ox, ox + h)
            return BBox3D(min=[x0, oy - r, oz - r], max=[x1, oy + r, oz + r])
        if ax.startswith("Y"):
            y0, y1 = (oy - h, oy) if axis.startswith("-") else (oy, oy + h)
            return BBox3D(min=[ox - r, y0, oz - r], max=[ox + r, y1, oz + r])
        z0, z1 = (oz - h, oz) if axis.startswith("-") else (oz, oz + h)
        return BBox3D(min=[ox - r, oy - r, z0], max=[ox + r, oy + r, z1])
    if kind == "sphere" and diameter_mm is not None:
        r = float(diameter_mm) * 0.5
        return BBox3D(min=[ox - r, oy - r, oz - r], max=[ox + r, oy + r, oz + r])
    return BBox3D(min=[ox, oy, oz], max=[ox, oy, oz])


@dataclass(slots=True)
class KeepOutZone:
    """Volume reserved for clearance, fasteners, or moving linkages aligned with `schemas/design-contract.schema.json`."""

    zone_id: str = ""
    kind: str = "box"
    origin_mm: list[float] | None = None
    axis: str = "+Z"
    size_mm: list[float] | None = None
    diameter_mm: float | None = None
    height_mm: float | None = None
    description: str = ""
    bbox_mm: BBox3D | None = None

    def resolved_bbox_mm(self) -> BBox3D:
        if self.bbox_mm is not None:
            return self.bbox_mm
        if self.origin_mm is not None and len(self.origin_mm) == 3:
            return _derive_zone_bbox(
                kind=self.kind,
                origin_mm=self.origin_mm,
                size_mm=self.size_mm,
                diameter_mm=self.diameter_mm,
                height_mm=self.height_mm,
                axis=self.axis or "+Z",
            )
        return BBox3D(min=[0.0, 0.0, 0.0], max=[0.0, 0.0, 0.0])

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.zone_id.strip():
            errors.append("keep_out_zone.zone_id must be non-empty")
        if self.kind not in VALID_KEEP_OUT_KINDS:
            errors.append(f"keep_out_zone[{self.zone_id}].kind invalid: {self.kind!r}")
        if self.axis not in VALID_KEEP_OUT_AXES:
            errors.append(
                f"keep_out_zone[{self.zone_id}].axis must be one of {VALID_KEEP_OUT_AXES}, got {self.axis!r}"
            )
        if self.origin_mm is not None and len(self.origin_mm) != 3:
            errors.append(f"keep_out_zone[{self.zone_id}].origin_mm must have 3 elements")
        if self.size_mm is not None and len(self.size_mm) != 3:
            errors.append(f"keep_out_zone[{self.zone_id}].size_mm must have 3 elements")
        if self.diameter_mm is not None and self.diameter_mm <= 0:
            errors.append(f"keep_out_zone[{self.zone_id}].diameter_mm must be > 0")
        if self.height_mm is not None and self.height_mm <= 0:
            errors.append(f"keep_out_zone[{self.zone_id}].height_mm must be > 0")
        bbox = self.resolved_bbox_mm()
        errors.extend(bbox.validate(f"keep_out_zones[{self.zone_id}].bbox_mm"))
        return errors

    def to_dict(self) -> dict[str, Any]:
        bbox = self.resolved_bbox_mm()
        payload: dict[str, Any] = {
            "zone_id": self.zone_id,
            "kind": self.kind,
        }
        if self.origin_mm is not None:
            payload["origin_mm"] = [float(v) for v in self.origin_mm]
        else:
            payload["origin_mm"] = [float(v) for v in bbox.min]
        if self.kind == "box":
            if self.size_mm is not None:
                payload["size_mm"] = [float(v) for v in self.size_mm]
            else:
                payload["size_mm"] = list(bbox.dimensions)
        if self.diameter_mm is not None:
            payload["diameter_mm"] = float(self.diameter_mm)
        if self.height_mm is not None:
            payload["height_mm"] = float(self.height_mm)
        if self.axis:
            payload["axis"] = self.axis
        if self.description:
            payload["description"] = self.description
        if self.bbox_mm is not None:
            payload["bbox_mm"] = self.bbox_mm.to_dict()
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> KeepOutZone:
        zone_id = str(data.get("zone_id") or "")
        bbox = BBox3D.from_dict(data["bbox_mm"]) if isinstance(data.get("bbox_mm"), dict) else None
        origin = [float(v) for v in data["origin_mm"]] if isinstance(data.get("origin_mm"), list) else None
        size = [float(v) for v in data["size_mm"]] if isinstance(data.get("size_mm"), list) else None
        return cls(
            zone_id=zone_id,
            kind=str(data.get("kind", "box")),
            origin_mm=origin,
            axis=str(data.get("axis", "+Z")),
            size_mm=size,
            diameter_mm=float(data["diameter_mm"]) if data.get("diameter_mm") is not None else None,
            height_mm=float(data["height_mm"]) if data.get("height_mm") is not None else None,
            description=str(data.get("description") or ""),
            bbox_mm=bbox,
        )


@dataclass(slots=True)
class MechanicalInterface:
    """Critical mating feature aligned with `schemas/design-contract.schema.json`."""

    interface_id: str = ""
    kind: str = "bore"
    nominal_mm: float | None = None
    diameter_mm: float | None = None
    pitch_circle_diameter_mm: float | None = None
    count: int | None = None
    tolerance: str | None = None
    roughness: str | None = None
    datum_ref: str | None = None
    normal: str | None = None
    preserve_exact_brep: bool = True
    notes: str = ""

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.interface_id.strip():
            errors.append("interface.interface_id must be non-empty")
        if self.kind not in VALID_INTERFACE_KINDS:
            errors.append(f"interface[{self.interface_id}].kind invalid: {self.kind!r}")
        if self.diameter_mm is not None and self.diameter_mm <= 0:
            errors.append(f"interface[{self.interface_id}].diameter_mm must be > 0")
        if self.pitch_circle_diameter_mm is not None and self.pitch_circle_diameter_mm <= 0:
            errors.append(
                f"interface[{self.interface_id}].pitch_circle_diameter_mm must be > 0"
            )
        if self.count is not None and self.count < 1:
            errors.append(f"interface[{self.interface_id}].count must be >= 1")
        if self.normal is not None and self.normal not in VALID_AXIS_NORMALS:
            errors.append(
                f"interface[{self.interface_id}].normal must be one of {VALID_AXIS_NORMALS}, got {self.normal!r}"
            )
        return errors

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "interface_id": self.interface_id,
            "kind": self.kind,
            "preserve_exact_brep": bool(self.preserve_exact_brep),
        }
        if self.nominal_mm is not None:
            payload["nominal_mm"] = float(self.nominal_mm)
        if self.diameter_mm is not None:
            payload["diameter_mm"] = float(self.diameter_mm)
        if self.pitch_circle_diameter_mm is not None:
            payload["pitch_circle_diameter_mm"] = float(self.pitch_circle_diameter_mm)
        if self.count is not None:
            payload["count"] = int(self.count)
        if self.tolerance is not None:
            payload["tolerance"] = self.tolerance
        if self.roughness is not None:
            payload["roughness"] = self.roughness
        if self.datum_ref is not None:
            payload["datum_ref"] = self.datum_ref
        if self.normal is not None:
            payload["normal"] = self.normal
        if self.notes:
            payload["notes"] = self.notes
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> MechanicalInterface:
        iface_id = str(data.get("interface_id") or "")
        preserve = data.get("preserve_exact_brep", True)
        return cls(
            interface_id=iface_id,
            kind=str(data.get("kind", "bore")),
            nominal_mm=float(data["nominal_mm"]) if data.get("nominal_mm") is not None else None,
            diameter_mm=float(data["diameter_mm"]) if data.get("diameter_mm") is not None else None,
            pitch_circle_diameter_mm=(
                float(data["pitch_circle_diameter_mm"])
                if data.get("pitch_circle_diameter_mm") is not None
                else None
            ),
            count=int(data["count"]) if data.get("count") is not None else None,
            tolerance=str(data["tolerance"]) if data.get("tolerance") is not None else None,
            roughness=str(data["roughness"]) if data.get("roughness") is not None else None,
            datum_ref=str(data["datum_ref"]) if data.get("datum_ref") is not None else None,
            normal=str(data["normal"]) if data.get("normal") is not None else None,
            preserve_exact_brep=bool(preserve),
            notes=str(data.get("notes", "")),
        )


@dataclass(slots=True)
class LoadCase:
    """Structural load specification aligned with `schemas/design-contract.schema.json`."""

    load_id: str = ""
    kind: str = "force"
    target_interface: str = ""
    magnitude: float | None = None
    unit: str | None = None
    vector: list[float] = field(default_factory=lambda: [0.0, 0.0, 0.0])
    min_safety_factor: float = 2.0

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.load_id.strip():
            errors.append("load.load_id must be non-empty")
        if self.kind not in VALID_LOAD_KINDS:
            errors.append(f"load[{self.load_id}].kind invalid: {self.kind!r}")
        if self.unit is not None and self.unit not in VALID_LOAD_UNITS:
            errors.append(
                f"load[{self.load_id}].unit must be one of {VALID_LOAD_UNITS}, got {self.unit!r}"
            )
        if len(self.vector) != 3:
            errors.append(f"load[{self.load_id}].vector must be a 3D vector [fx, fy, fz]")
        if self.min_safety_factor <= 0:
            errors.append(f"load[{self.load_id}].min_safety_factor must be > 0")
        return errors

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "load_id": self.load_id,
            "kind": self.kind,
            "target_interface": self.target_interface,
            "vector": [float(v) for v in self.vector],
            "min_safety_factor": float(self.min_safety_factor),
        }
        if self.magnitude is not None:
            payload["magnitude"] = float(self.magnitude)
        if self.unit is not None:
            payload["unit"] = self.unit
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> LoadCase:
        load_id = str(data.get("load_id") or "")
        vec = data.get("vector")
        return cls(
            load_id=load_id,
            kind=str(data.get("kind", "force")),
            target_interface=str(data.get("target_interface", "")),
            magnitude=float(data["magnitude"]) if data.get("magnitude") is not None else None,
            unit=str(data["unit"]) if data.get("unit") is not None else None,
            vector=[float(v) for v in vec] if isinstance(vec, list) else [0.0, 0.0, 0.0],
            min_safety_factor=float(data.get("min_safety_factor", 2.0)),
        )


@dataclass(slots=True)
class DesignContract:
    """Structured Engineering Design Contract matching `schemas/design-contract.schema.json`."""

    part_name: str
    units: str | dict[str, str] = field(
        default_factory=lambda: {"length": "mm", "angle": "deg", "force": "N"}
    )
    material: str | dict[str, Any] = "Al-6061-T6"
    manufacturing_method: str = "CNC_3AXIS_MILLING"
    keep_in_bbox_mm: BBox3D = field(
        default_factory=lambda: BBox3D(min=[-100.0, -100.0, 0.0], max=[100.0, 100.0, 100.0])
    )
    keep_out_zones: list[KeepOutZone] = field(default_factory=list)
    interfaces: list[MechanicalInterface] = field(default_factory=list)
    loads: list[LoadCase] = field(default_factory=list)
    notes: str = ""
    summary: str = ""
    contract_version: str = "0.1"
    dfm_constraints: dict[str, Any] = field(default_factory=dict)
    part_archetype: str = "parametric_part"
    feature_params: dict[str, Any] = field(default_factory=dict)
    # Set by local inspection, not by user/LLM contract JSON.
    external_context: ExternalInterfaceSummary | None = None

    @property
    def length_unit(self) -> str:
        if isinstance(self.units, dict):
            return str(self.units.get("length", "mm"))
        return str(self.units)

    @property
    def angle_unit(self) -> str:
        if isinstance(self.units, dict):
            return str(self.units.get("angle", "deg"))
        return "deg"

    @property
    def force_unit(self) -> str:
        if isinstance(self.units, dict):
            return str(self.units.get("force", "N"))
        return "N"

    @property
    def material_name(self) -> str:
        if isinstance(self.material, dict):
            return str(self.material.get("name", ""))
        return str(self.material)

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.part_name or not PART_NAME_PATTERN.match(self.part_name):
            errors.append(
                "part_name must match ^[A-Za-z][A-Za-z0-9_]*$ "
                "(letter first; no leading underscore, path separators, or punctuation)"
            )
        if isinstance(self.units, dict):
            length = self.units.get("length")
            angle = self.units.get("angle", "deg")
            force = self.units.get("force", "N")
            if length not in VALID_LENGTH_UNITS:
                errors.append(f"units.length must be one of {VALID_LENGTH_UNITS}, got {length!r}")
            if angle not in VALID_ANGLE_UNITS:
                errors.append(f"units.angle must be one of {VALID_ANGLE_UNITS}, got {angle!r}")
            if force not in VALID_FORCE_UNITS:
                errors.append(f"units.force must be one of {VALID_FORCE_UNITS}, got {force!r}")
        elif self.units not in VALID_LENGTH_UNITS:
            errors.append(f"units must be one of {VALID_LENGTH_UNITS}, got {self.units!r}")

        if not self.material_name.strip():
            errors.append("material must be non-empty")
        canonical_method = canonical_manufacturing_method(self.manufacturing_method)
        if canonical_method in VALID_MANUFACTURING_METHODS:
            self.manufacturing_method = canonical_method
        else:
            errors.append(
                f"manufacturing_method must be one of {VALID_MANUFACTURING_METHODS}, "
                f"got {self.manufacturing_method!r}"
            )
        if self.dfm_constraints:
            min_tool = self.dfm_constraints.get("min_tool_radius_mm")
            if min_tool is not None and (isinstance(min_tool, bool) or not isinstance(min_tool, (int, float)) or min_tool < 0):
                errors.append("dfm_constraints.min_tool_radius_mm must be >= 0")
            min_wall = self.dfm_constraints.get("min_wall_thickness_mm")
            if min_wall is not None and (isinstance(min_wall, bool) or not isinstance(min_wall, (int, float)) or min_wall < 0):
                errors.append("dfm_constraints.min_wall_thickness_mm must be >= 0")
            axes = self.dfm_constraints.get("accessible_axes")
            if axes is not None:
                if not isinstance(axes, list) or any(ax not in VALID_AXIS_NORMALS for ax in axes):
                    errors.append(f"dfm_constraints.accessible_axes items must be in {VALID_AXIS_NORMALS}")
        errors.extend(self.keep_in_bbox_mm.validate("keep_in_bbox_mm"))
        for zone in self.keep_out_zones:
            errors.extend(zone.validate())
        for iface in self.interfaces:
            errors.extend(iface.validate())
        for load in self.loads:
            errors.extend(load.validate())
        return errors

    def assert_valid(self) -> None:
        errors = self.validate()
        if errors:
            raise ContractValidationError(errors)

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "contract_version": self.contract_version,
            "part_name": self.part_name,
            "units": dict(self.units) if isinstance(self.units, dict) else self.units,
            "material": dict(self.material) if isinstance(self.material, dict) else self.material,
            "manufacturing_method": canonical_manufacturing_method(self.manufacturing_method),
            "keep_in_bbox_mm": self.keep_in_bbox_mm.to_dict(),
            "keep_out_zones": [z.to_dict() for z in self.keep_out_zones],
            "interfaces": [i.to_dict() for i in self.interfaces],
            "loads": [l.to_dict() for l in self.loads],
        }
        if self.summary or self.notes:
            payload["summary"] = self.summary or self.notes
        if self.notes:
            payload["notes"] = self.notes
        if self.dfm_constraints:
            payload["dfm_constraints"] = dict(self.dfm_constraints)
        if self.part_archetype != "parametric_part":
            payload["part_archetype"] = self.part_archetype
        if self.feature_params:
            payload["feature_params"] = dict(self.feature_params)
        if self.external_context is not None:
            payload["external_context"] = self.external_context.to_dict()
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> DesignContract:
        raw_units = data.get("units", {"length": "mm", "angle": "deg", "force": "N"})
        units: str | dict[str, str]
        if isinstance(raw_units, dict):
            units = {str(k): str(v) for k, v in raw_units.items()}
        else:
            units = str(raw_units)

        raw_material = data.get("material", "Al-6061-T6")
        material: str | dict[str, Any] = (
            dict(raw_material) if isinstance(raw_material, dict) else str(raw_material)
        )

        return cls(
            part_name=str(data["part_name"]),
            units=units,
            material=material,
            manufacturing_method=canonical_manufacturing_method(
                str(data.get("manufacturing_method", "CNC_3AXIS_MILLING"))
            ),
            keep_in_bbox_mm=BBox3D.from_dict(data["keep_in_bbox_mm"]),
            keep_out_zones=[KeepOutZone.from_dict(z) for z in data.get("keep_out_zones", [])],
            interfaces=[MechanicalInterface.from_dict(i) for i in data.get("interfaces", [])],
            loads=[LoadCase.from_dict(l) for l in data.get("loads", [])],
            notes=str(data.get("notes", "")),
            summary=str(data.get("summary", "")),
            contract_version=str(data.get("contract_version", "0.1")),
            dfm_constraints=dict(data.get("dfm_constraints", {})),
            part_archetype=str(data.get("part_archetype", "parametric_part")),
            feature_params=dict(data.get("feature_params", {})),
        )


@dataclass(slots=True)
class SourceSpan:
    """Source code span returned by `cad-cli` diagnostics (`schemas/diagnostics.schema.json`)."""

    start_line: int
    start_col: int
    end_line: int
    end_col: int
    byte_start: int | None = None
    byte_end: int | None = None

    def validate(self) -> list[str]:
        errors: list[str] = []
        if self.start_line < 0 or self.start_col < 0:
            errors.append("span start_line and start_col must be >= 0")
        if self.end_line < 0 or self.end_col < 0:
            errors.append("span end_line and end_col must be >= 0")
        if self.end_line < self.start_line:
            errors.append("span end_line must be >= start_line")
        if self.end_line == self.start_line and self.end_col < self.start_col:
            errors.append("span end_col must be >= start_col on single-line span")
        if self.byte_start is not None and self.byte_start < 0:
            errors.append("span byte_start must be >= 0")
        if self.byte_end is not None and self.byte_end < 0:
            errors.append("span byte_end must be >= 0")
        if (
            self.byte_start is not None
            and self.byte_end is not None
            and self.byte_end < self.byte_start
        ):
            errors.append("span byte_end must be >= byte_start")
        return errors

    def to_dict(self) -> dict[str, int]:
        payload: dict[str, int] = {
            "start_line": int(self.start_line),
            "start_col": int(self.start_col),
            "end_line": int(self.end_line),
            "end_col": int(self.end_col),
        }
        if self.byte_start is not None:
            payload["byte_start"] = int(self.byte_start)
        if self.byte_end is not None:
            payload["byte_end"] = int(self.byte_end)
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> SourceSpan:
        start_line = int(data.get("start_line", 0))
        start_col = int(data.get("start_col", 0))
        return cls(
            start_line=start_line,
            start_col=start_col,
            end_line=int(data.get("end_line", start_line)),
            end_col=int(data.get("end_col", start_col)),
            byte_start=int(data["byte_start"]) if data.get("byte_start") is not None else None,
            byte_end=int(data["byte_end"]) if data.get("byte_end") is not None else None,
        )


@dataclass(slots=True)
class Diagnostic:
    """Single compiler or geometric diagnostic aligned with `schemas/diagnostics.schema.json`."""

    code: str
    severity: Severity
    message: str
    span: SourceSpan
    hint: str | None = None
    rule_layer: GateLevel | None = None

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.code.strip():
            errors.append("diagnostic.code must be non-empty")
        if self.severity not in VALID_SEVERITIES:
            errors.append(f"diagnostic.severity invalid: {self.severity!r}")
        if not self.message.strip():
            errors.append("diagnostic.message must be non-empty")
        if self.rule_layer is not None and self.rule_layer not in VALID_GATE_LEVELS:
            errors.append(f"diagnostic.rule_layer invalid: {self.rule_layer!r}")
        errors.extend(self.span.validate())
        return errors

    def format_line(self) -> str:
        return (
            f"[{self.severity.upper()}] {self.code} "
            f"(L{self.span.start_line}:C{self.span.start_col}-L{self.span.end_line}:C{self.span.end_col}): "
            f"{self.message}"
        )

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "span": self.span.to_dict(),
        }
        if self.hint is not None:
            payload["hint"] = self.hint
        if self.rule_layer is not None:
            payload["rule_layer"] = self.rule_layer
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Diagnostic:
        return cls(
            code=str(data["code"]),
            severity=data["severity"],
            message=str(data["message"]),
            span=SourceSpan.from_dict(data.get("span", {})),
            hint=str(data["hint"]) if data.get("hint") is not None else None,
            rule_layer=data.get("rule_layer"),
        )


@dataclass(slots=True)
class CompileMetrics:
    """Geometric metrics returned by `cad-cli check/build --json`."""

    watertight: bool
    volume_mm3: float
    bbox_mm: BBox3D
    face_count: int
    edge_count: int
    surface_area_mm2: float | None = None
    open_edge_count: int | None = None
    compile_time_ms: float | None = None
    kernel_backend: str | None = None

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not isinstance(self.watertight, bool):
            errors.append("metrics.watertight must be boolean")
        if self.volume_mm3 < 0:
            errors.append(f"metrics.volume_mm3 must be >= 0, got {self.volume_mm3}")
        if self.face_count < 0 or self.edge_count < 0:
            errors.append("metrics.face_count and edge_count must be >= 0")
        if self.surface_area_mm2 is not None and self.surface_area_mm2 < 0:
            errors.append("metrics.surface_area_mm2 must be >= 0")
        if self.open_edge_count is not None and self.open_edge_count < 0:
            errors.append("metrics.open_edge_count must be >= 0")
        if self.compile_time_ms is not None and self.compile_time_ms < 0:
            errors.append("metrics.compile_time_ms must be >= 0")
        errors.extend(self.bbox_mm.validate("metrics.bbox_mm"))
        return errors

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "watertight": self.watertight,
            "volume_mm3": float(self.volume_mm3),
            "bbox_mm": self.bbox_mm.to_dict(),
            "face_count": int(self.face_count),
            "edge_count": int(self.edge_count),
        }
        if self.surface_area_mm2 is not None:
            payload["surface_area_mm2"] = float(self.surface_area_mm2)
        if self.open_edge_count is not None:
            payload["open_edge_count"] = int(self.open_edge_count)
        if self.compile_time_ms is not None:
            payload["compile_time_ms"] = float(self.compile_time_ms)
        if self.kernel_backend is not None:
            payload["kernel_backend"] = str(self.kernel_backend)
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> CompileMetrics:
        return cls(
            watertight=bool(data["watertight"]),
            volume_mm3=float(data["volume_mm3"]),
            bbox_mm=BBox3D.from_dict(data["bbox_mm"]),
            face_count=int(data["face_count"]),
            edge_count=int(data["edge_count"]),
            surface_area_mm2=(
                float(data["surface_area_mm2"])
                if data.get("surface_area_mm2") is not None
                else None
            ),
            open_edge_count=(
                int(data["open_edge_count"])
                if data.get("open_edge_count") is not None
                else None
            ),
            compile_time_ms=(
                float(data["compile_time_ms"])
                if data.get("compile_time_ms") is not None
                else None
            ),
            kernel_backend=(
                str(data["kernel_backend"])
                if data.get("kernel_backend") is not None
                else None
            ),
        )


@dataclass(slots=True)
class CompileResult:
    """Structured output of `cad-cli check` / `cad-cli build` aligned with `schemas/diagnostics.schema.json`."""

    status: CompileStatus
    file: str
    diagnostics: list[Diagnostic] = field(default_factory=list)
    metrics: CompileMetrics | None = None
    step_path: str | None = None
    mesh_path: str | None = None
    views_dir: str | None = None
    dxf_path: str | None = None
    dxf_view_path: str | None = None
    drawing_path: str | None = None
    part_name: str | None = None
    kind: str | None = None
    components: list[dict[str, Any]] | None = None
    exit_code: int = 0
    raw_stderr: str = ""
    # Full cad-cli JSON object. Geometry, PMI, and hole records live here when the CLI emits them.
    cli_payload: dict[str, Any] | None = None

    @property
    def has_errors(self) -> bool:
        return self.status != "ok" or any(d.severity == "error" for d in self.diagnostics)

    @property
    def error_diagnostics(self) -> list[Diagnostic]:
        return [d for d in self.diagnostics if d.severity == "error"]

    @property
    def artifacts(self) -> dict[str, str]:
        out: dict[str, str] = {}
        if self.step_path is not None:
            out["step_path"] = self.step_path
        if self.mesh_path is not None:
            out["mesh_path"] = self.mesh_path
        if self.views_dir is not None:
            out["views_dir"] = self.views_dir
        if self.dxf_path is not None:
            out["dxf_path"] = self.dxf_path
        if self.dxf_view_path is not None:
            out["dxf_view_path"] = self.dxf_view_path
        if self.drawing_path is not None:
            out["drawing_path"] = self.drawing_path
        return out

    def validate(self) -> list[str]:
        errors: list[str] = []
        if self.status not in VALID_STATUSES:
            errors.append(f"status must be one of {VALID_STATUSES}, got {self.status!r}")
        if not self.file:
            errors.append("file must be non-empty")
        if self.kind is not None and self.kind not in ("part", "assembly"):
            errors.append(f"kind must be 'part' or 'assembly', got {self.kind!r}")
        for diag in self.diagnostics:
            errors.extend(diag.validate())
        if self.metrics is not None:
            errors.extend(self.metrics.validate())
        elif self.status == "ok":
            errors.append("metrics is required when status == 'ok'")
        return errors

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "status": self.status,
            "file": self.file,
            "diagnostics": [d.to_dict() for d in self.diagnostics],
            "metrics": self.metrics.to_dict() if self.metrics is not None else None,
        }
        if self.part_name is not None:
            payload["part_name"] = self.part_name
        if self.kind is not None:
            payload["kind"] = self.kind
        if self.components is not None:
            payload["components"] = deepcopy(self.components)
        artifacts = self.artifacts
        if artifacts:
            payload["artifacts"] = artifacts
        return payload

    @classmethod
    def from_cli_json(
        cls,
        data: dict[str, Any],
        *,
        step_path: str | None = None,
        mesh_path: str | None = None,
        views_dir: str | None = None,
        dxf_path: str | None = None,
        dxf_view_path: str | None = None,
        drawing_path: str | None = None,
        exit_code: int = 0,
        raw_stderr: str = "",
    ) -> CompileResult:
        metrics_raw = data.get("metrics")
        metrics = CompileMetrics.from_dict(metrics_raw) if isinstance(metrics_raw, dict) else None
        raw_artifacts = data.get("artifacts") if isinstance(data.get("artifacts"), dict) else {}

        def _pick_path(explicit: str | None, key: str) -> str | None:
            val = explicit or raw_artifacts.get(key) or data.get(key)
            return str(val) if val is not None else None

        raw_components = data.get("components")
        components = (
            [dict(item) for item in raw_components if isinstance(item, dict)]
            if isinstance(raw_components, list)
            else None
        )

        return cls(
            status=data["status"],
            file=str(data.get("file", "")),
            diagnostics=[Diagnostic.from_dict(d) for d in data.get("diagnostics", [])],
            metrics=metrics,
            step_path=_pick_path(step_path, "step_path"),
            mesh_path=_pick_path(mesh_path, "mesh_path"),
            views_dir=_pick_path(views_dir, "views_dir"),
            dxf_path=_pick_path(dxf_path, "dxf_path"),
            dxf_view_path=_pick_path(dxf_view_path, "dxf_view_path"),
            drawing_path=_pick_path(drawing_path, "drawing_path"),
            part_name=str(data["part_name"]) if data.get("part_name") is not None else None,
            kind=str(data["kind"]) if data.get("kind") is not None else None,
            components=components,
            exit_code=exit_code,
            raw_stderr=raw_stderr,
            cli_payload=dict(data),
        )


@dataclass(slots=True)
class KernelIdentity:
    """Geometry kernel provenance aligned with `schemas/verification-report.schema.json`."""

    provider: str = "occt"
    version: str = "7.9.0"
    authoritative: bool = True

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not isinstance(self.provider, str) or not self.provider.strip():
            errors.append("kernel_identity.provider must be a non-empty string")
        if not isinstance(self.version, str) or not self.version.strip():
            errors.append("kernel_identity.version must be a non-empty string")
        if not isinstance(self.authoritative, bool):
            errors.append("kernel_identity.authoritative must be a boolean")
        return errors

    def to_dict(self) -> dict[str, Any]:
        return {
            "provider": str(self.provider),
            "version": str(self.version),
            "authoritative": bool(self.authoritative),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> KernelIdentity:
        return cls(
            provider=str(data.get("provider", "occt")),
            version=str(data.get("version", "7.9.0")),
            authoritative=bool(data.get("authoritative", True)),
        )


@dataclass(slots=True)
class VerificationCheck:
    """Single verification evidence item aligned with `schemas/verification-report.schema.json`."""

    code: str
    layer: VerificationLayer
    status: TriVerdict
    message: str
    evidence: dict[str, Any] = field(default_factory=dict)

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not isinstance(self.code, str) or not self.code.strip():
            errors.append("verification_check.code must be a non-empty string")
        if self.layer not in VERIFICATION_LAYERS:
            errors.append(
                f"verification_check.layer must be one of {VERIFICATION_LAYERS}, got {self.layer!r}"
            )
        if self.status not in VALID_TRI_VERDICTS:
            errors.append(
                f"verification_check.status must be one of {VALID_TRI_VERDICTS}, got {self.status!r}"
            )
        if not isinstance(self.message, str) or not self.message.strip():
            errors.append("verification_check.message must be a non-empty string")
        if not isinstance(self.evidence, dict):
            errors.append("verification_check.evidence must be an object")
        return errors

    def to_dict(self) -> dict[str, Any]:
        return {
            "code": str(self.code),
            "layer": self.layer,
            "status": self.status,
            "message": str(self.message),
            "evidence": deepcopy(self.evidence),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> VerificationCheck:
        raw_evidence = data.get("evidence")
        return cls(
            code=str(data["code"]),
            layer=data["layer"],
            status=data["status"],
            message=str(data["message"]),
            evidence=deepcopy(raw_evidence) if isinstance(raw_evidence, dict) else {},
        )


@dataclass(slots=True)
class VerificationReport:
    """Five-layer tri-state verification report aligned with `schemas/verification-report.schema.json`."""

    kernel_identity: KernelIdentity = field(default_factory=KernelIdentity)
    layers: dict[VerificationLayer, TriVerdict] = field(default_factory=dict)
    checks: list[VerificationCheck] = field(default_factory=list)
    overall_status: TriVerdict | None = None
    schema_version: int = 1
    summary: str | None = None

    def __post_init__(self) -> None:
        grouped: dict[VerificationLayer, list[TriVerdict]] = {
            layer: [] for layer in VERIFICATION_LAYERS
        }
        for chk in self.checks:
            if chk.layer in grouped and chk.status in VALID_TRI_VERDICTS:
                grouped[chk.layer].append(chk.status)

        resolved_layers: dict[VerificationLayer, TriVerdict] = {}
        for layer in VERIFICATION_LAYERS:
            explicit = self.layers.get(layer) if isinstance(self.layers, dict) else None
            layer_checks = grouped[layer]
            if explicit in VALID_TRI_VERDICTS and layer_checks:
                resolved_layers[layer] = aggregate_tri_verdicts([*layer_checks, explicit])
            elif explicit in VALID_TRI_VERDICTS:
                resolved_layers[layer] = explicit
            elif explicit is not None:
                resolved_layers[layer] = explicit  # preserved so validate() can flag invalid value
            else:
                resolved_layers[layer] = aggregate_tri_verdicts(layer_checks)
        self.layers = resolved_layers

        if self.overall_status is None:
            valid_layer_values = [
                self.layers[layer]
                for layer in VERIFICATION_LAYERS
                if self.layers.get(layer) in VALID_TRI_VERDICTS
            ]
            if len(valid_layer_values) == len(VERIFICATION_LAYERS):
                self.overall_status = aggregate_tri_verdicts(valid_layer_values)
            else:
                self.overall_status = "UNKNOWN"

    @property
    def passed(self) -> bool:
        """Strictly True only when overall_status == 'PASS' (never folds UNKNOWN into PASS)."""
        return (
            self.overall_status == "PASS"
            and all(self.layers.get(layer) == "PASS" for layer in VERIFICATION_LAYERS)
        )

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not isinstance(self.schema_version, int) or self.schema_version < 1:
            errors.append("verification_report.schema_version must be an integer >= 1")
        errors.extend(self.kernel_identity.validate())
        if self.overall_status not in VALID_TRI_VERDICTS:
            errors.append(
                f"verification_report.overall_status must be one of {VALID_TRI_VERDICTS}, got {self.overall_status!r}"
            )
        for layer in VERIFICATION_LAYERS:
            status = self.layers.get(layer)
            if status not in VALID_TRI_VERDICTS:
                errors.append(
                    f"verification_report.layers[{layer}] must be one of {VALID_TRI_VERDICTS}, got {status!r}"
                )
        for chk in self.checks:
            errors.extend(chk.validate())
        if not errors:
            expected_overall = aggregate_tri_verdicts(
                [self.layers[layer] for layer in VERIFICATION_LAYERS]
            )
            if self.overall_status != expected_overall:
                errors.append(
                    f"verification_report.overall_status={self.overall_status!r} does not match "
                    f"aggregated layer status {expected_overall!r}"
                )
        return errors

    def assert_valid(self) -> None:
        errors = self.validate()
        if errors:
            raise ContractValidationError(errors)

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "schema_version": int(self.schema_version),
            "overall_status": self.overall_status,
            "kernel_identity": self.kernel_identity.to_dict(),
            "layers": {layer: self.layers[layer] for layer in VERIFICATION_LAYERS},
            "checks": [chk.to_dict() for chk in self.checks],
        }
        if self.summary is not None:
            payload["summary"] = self.summary
        return payload

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> VerificationReport:
        raw_kernel = data.get("kernel_identity")
        kernel = (
            KernelIdentity.from_dict(raw_kernel)
            if isinstance(raw_kernel, dict)
            else KernelIdentity()
        )
        raw_layers = data.get("layers")
        layers: dict[VerificationLayer, TriVerdict] = {}
        if isinstance(raw_layers, dict):
            for layer in VERIFICATION_LAYERS:
                if layer in raw_layers:
                    layers[layer] = raw_layers[layer]
        raw_checks = data.get("checks")
        checks = (
            [VerificationCheck.from_dict(c) for c in raw_checks if isinstance(c, dict)]
            if isinstance(raw_checks, list)
            else []
        )
        return cls(
            schema_version=int(data.get("schema_version", 1)),
            overall_status=data.get("overall_status"),
            kernel_identity=kernel,
            layers=layers,
            checks=checks,
            summary=str(data["summary"]) if data.get("summary") is not None else None,
        )


@dataclass(slots=True)
class GateCheckResult:
    """Result of an individual L0-L4 verification gate."""

    gate: GateLevel
    name: str
    passed: bool
    details: str
    status: TriVerdict | None = None

    def __post_init__(self) -> None:
        if self.status is None:
            self.status = "PASS" if self.passed else "FAIL"
        elif self.status not in VALID_TRI_VERDICTS:
            raise ValueError(f"Invalid GateCheckResult.status: {self.status!r}")

    def to_dict(self) -> dict[str, Any]:
        return {
            "gate": self.gate,
            "name": self.name,
            "passed": self.passed,
            "status": self.status,
            "details": self.details,
        }


@dataclass(slots=True)
class CriticVerdict:
    """Self-inspection report emitted by `VisualGeometricCritic` with five-layer tri-state `VerificationReport`."""

    passed: bool
    iteration: int
    gate_results: list[GateCheckResult] = field(default_factory=list)
    issues: list[str] = field(default_factory=list)
    repair_instructions: str = ""
    visual_summary: str = ""
    metrics: CompileMetrics | None = None
    verification_report: VerificationReport | None = None

    @property
    def report(self) -> VerificationReport | None:
        return self.verification_report

    @property
    def overall_status(self) -> TriVerdict:
        if self.verification_report is not None and self.verification_report.overall_status is not None:
            return self.verification_report.overall_status
        return "PASS" if self.passed else "FAIL"

    @property
    def layers(self) -> dict[VerificationLayer, TriVerdict]:
        if self.verification_report is not None:
            return self.verification_report.layers
        default_status: TriVerdict = "PASS" if self.passed else "FAIL"
        return {layer: default_status for layer in VERIFICATION_LAYERS}

    @property
    def checks(self) -> list[VerificationCheck]:
        if self.verification_report is not None:
            return self.verification_report.checks
        return []

    @property
    def kernel_identity(self) -> KernelIdentity:
        if self.verification_report is not None:
            return self.verification_report.kernel_identity
        return KernelIdentity()

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "passed": self.passed,
            "overall_status": self.overall_status,
            "iteration": self.iteration,
            "gate_results": [g.to_dict() for g in self.gate_results],
            "issues": list(self.issues),
            "repair_instructions": self.repair_instructions,
            "visual_summary": self.visual_summary,
            "metrics": self.metrics.to_dict() if self.metrics is not None else None,
        }
        if self.verification_report is not None:
            payload["verification_report"] = self.verification_report.to_dict()
        return payload

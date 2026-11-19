"""Data contracts and validators aligned with `schemas/*.schema.json`."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any, Literal

Severity = Literal["error", "warning", "info"]
CompileStatus = Literal["ok", "error"]
GateLevel = Literal["L0", "L1", "L2", "L3", "L4"]

VALID_SEVERITIES: tuple[str, ...] = ("error", "warning", "info")
VALID_STATUSES: tuple[str, ...] = ("ok", "error")
VALID_LENGTH_UNITS: tuple[str, ...] = ("mm", "m", "in")
VALID_ANGLE_UNITS: tuple[str, ...] = ("deg", "rad")
VALID_FORCE_UNITS: tuple[str, ...] = ("N", "kN")
VALID_KEEP_OUT_KINDS: tuple[str, ...] = ("box", "cylinder", "sphere")
VALID_INTERFACE_KINDS: tuple[str, ...] = (
    "bore",
    "shaft_seat",
    "mounting_face",
    "bolt_pattern",
    "bolt_circle",
    "counterbore_pattern",
    "slot",
    "datum",
)
VALID_LOAD_KINDS: tuple[str, ...] = ("force", "torque", "pressure", "fixed_support")
VALID_MANUFACTURING_METHODS: tuple[str, ...] = (
    "CNC_3AXIS_MILLING",
    "CNC_5AXIS_MILLING",
    "CNC_TURNING",
    "LASER_CUTTING",
    "SHEET_METAL_BENDING",
    "SHEET_METAL",
    "ADDITIVE_FDM",
    "ADDITIVE_SLM",
    "INJECTION_MOLDING",
    "CASTING",
)

PART_NAME_PATTERN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


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
            if not isinstance(lo, (int, float)) or not isinstance(hi, (int, float)):
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
    """Volume reserved for clearance, fasteners, or moving linkages.

    Supports both `schemas/design-contract.schema.json` fields (`zone_id`, `kind`,
    `origin_mm`, `size_mm`, `diameter_mm`, `height_mm`, `axis`, `description`) and
    shorthand fields (`name`, `bbox_mm`, `reason`).
    """

    zone_id: str = ""
    kind: str = "box"
    origin_mm: list[float] | None = None
    axis: str = "+Z"
    size_mm: list[float] | None = None
    diameter_mm: float | None = None
    height_mm: float | None = None
    description: str = ""
    bbox_mm: BBox3D | None = None

    def __init__(
        self,
        zone_id: str = "",
        *,
        name: str = "",
        kind: str = "box",
        origin_mm: list[float] | None = None,
        axis: str = "+Z",
        size_mm: list[float] | None = None,
        diameter_mm: float | None = None,
        height_mm: float | None = None,
        description: str = "",
        reason: str = "",
        bbox_mm: BBox3D | None = None,
    ) -> None:
        resolved_id = zone_id or name
        self.zone_id = resolved_id
        self.kind = kind
        self.origin_mm = list(origin_mm) if origin_mm is not None else None
        self.axis = axis
        self.size_mm = list(size_mm) if size_mm is not None else None
        self.diameter_mm = float(diameter_mm) if diameter_mm is not None else None
        self.height_mm = float(height_mm) if height_mm is not None else None
        self.description = description or reason
        self.bbox_mm = bbox_mm

    @property
    def name(self) -> str:
        return self.zone_id

    @property
    def reason(self) -> str:
        return self.description

    def resolved_bbox_mm(self) -> BBox3D:
        if self.bbox_mm is not None:
            return self.bbox_mm
        if self.origin_mm is not None:
            return _derive_zone_bbox(
                kind=self.kind,
                origin_mm=self.origin_mm,
                size_mm=self.size_mm,
                diameter_mm=self.diameter_mm,
                height_mm=self.height_mm,
                axis=self.axis,
            )
        return BBox3D(min=[0.0, 0.0, 0.0], max=[0.0, 0.0, 0.0])

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.zone_id.strip():
            errors.append("keep_out_zone.zone_id must be non-empty")
        if self.kind not in VALID_KEEP_OUT_KINDS:
            errors.append(f"keep_out_zone[{self.zone_id}].kind invalid: {self.kind!r}")
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
        zone_id = str(data.get("zone_id") or data.get("name") or "")
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
            description=str(data.get("description") or data.get("reason") or ""),
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

    def __init__(
        self,
        interface_id: str = "",
        *,
        name: str = "",
        kind: str = "bore",
        nominal_mm: float | None = None,
        diameter_mm: float | None = None,
        pitch_circle_diameter_mm: float | None = None,
        count: int | None = None,
        tolerance: str | None = None,
        roughness: str | None = None,
        datum_ref: str | None = None,
        normal: str | None = None,
        preserve_exact_brep: bool | None = None,
        preserve_exact: bool | None = None,
        notes: str = "",
    ) -> None:
        self.interface_id = interface_id or name
        self.kind = kind
        self.nominal_mm = float(nominal_mm) if nominal_mm is not None else None
        self.diameter_mm = float(diameter_mm) if diameter_mm is not None else None
        self.pitch_circle_diameter_mm = (
            float(pitch_circle_diameter_mm) if pitch_circle_diameter_mm is not None else None
        )
        self.count = int(count) if count is not None else None
        self.tolerance = tolerance
        self.roughness = roughness
        self.datum_ref = datum_ref
        self.normal = normal
        if preserve_exact_brep is not None:
            self.preserve_exact_brep = bool(preserve_exact_brep)
        elif preserve_exact is not None:
            self.preserve_exact_brep = bool(preserve_exact)
        else:
            self.preserve_exact_brep = True
        self.notes = notes

    @property
    def name(self) -> str:
        return self.interface_id

    @property
    def preserve_exact(self) -> bool:
        return self.preserve_exact_brep

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.interface_id.strip():
            errors.append("interface.interface_id must be non-empty")
        if self.kind not in VALID_INTERFACE_KINDS:
            errors.append(f"interface[{self.interface_id}].kind invalid: {self.kind!r}")
        if self.diameter_mm is not None and self.diameter_mm <= 0:
            errors.append(f"interface[{self.interface_id}].diameter_mm must be > 0")
        if self.count is not None and self.count < 1:
            errors.append(f"interface[{self.interface_id}].count must be >= 1")
        return errors

    def to_dict(self) -> dict[str, Any]:
        schema_kind = "bolt_pattern" if self.kind == "bolt_circle" else self.kind
        payload: dict[str, Any] = {
            "interface_id": self.interface_id,
            "kind": schema_kind,
            "preserve_exact_brep": self.preserve_exact_brep,
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
        iface_id = str(data.get("interface_id") or data.get("name") or "")
        preserve = data.get("preserve_exact_brep")
        if preserve is None:
            preserve = data.get("preserve_exact", True)
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

    def __init__(
        self,
        load_id: str = "",
        *,
        name: str = "",
        kind: str = "force",
        target_interface: str = "",
        magnitude: float | None = None,
        unit: str | None = None,
        vector: list[float] | None = None,
        force_n: list[float] | None = None,
        min_safety_factor: float = 2.0,
    ) -> None:
        self.load_id = load_id or name
        self.kind = kind
        self.target_interface = target_interface
        self.magnitude = float(magnitude) if magnitude is not None else None
        self.unit = unit
        raw_vec = vector if vector is not None else force_n
        self.vector = [float(v) for v in raw_vec] if raw_vec is not None else [0.0, 0.0, 0.0]
        self.min_safety_factor = float(min_safety_factor)

    @property
    def name(self) -> str:
        return self.load_id

    @property
    def force_n(self) -> list[float]:
        return self.vector

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.load_id.strip():
            errors.append("load.load_id must be non-empty")
        if self.kind not in VALID_LOAD_KINDS:
            errors.append(f"load[{self.load_id}].kind invalid: {self.kind!r}")
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
        load_id = str(data.get("load_id") or data.get("name") or "")
        vec = data.get("vector") if data.get("vector") is not None else data.get("force_n")
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
                "part_name must match ^[A-Za-z_][A-Za-z0-9_]*$ (no path separators or punctuation)"
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
        if self.manufacturing_method not in VALID_MANUFACTURING_METHODS:
            errors.append(
                f"manufacturing_method must be one of {VALID_MANUFACTURING_METHODS}, "
                f"got {self.manufacturing_method!r}"
            )
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
            "manufacturing_method": self.manufacturing_method,
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
            manufacturing_method=str(data.get("manufacturing_method", "CNC_3AXIS_MILLING")),
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
    """1-based source code span returned by `cad-cli` diagnostics."""

    start_line: int
    start_col: int
    end_line: int
    end_col: int

    def validate(self) -> list[str]:
        errors: list[str] = []
        if self.start_line < 1 or self.start_col < 1:
            errors.append("span start_line and start_col must be >= 1")
        if self.end_line < self.start_line:
            errors.append("span end_line must be >= start_line")
        if self.end_line == self.start_line and self.end_col < self.start_col:
            errors.append("span end_col must be >= start_col on single-line span")
        return errors

    def to_dict(self) -> dict[str, int]:
        return {
            "start_line": int(self.start_line),
            "start_col": int(self.start_col),
            "end_line": int(self.end_line),
            "end_col": int(self.end_col),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> SourceSpan:
        return cls(
            start_line=int(data.get("start_line", 1)),
            start_col=int(data.get("start_col", 1)),
            end_line=int(data.get("end_line", data.get("start_line", 1))),
            end_col=int(data.get("end_col", data.get("start_col", 1))),
        )


@dataclass(slots=True)
class Diagnostic:
    """Single compiler or geometric diagnostic aligned with `schemas/diagnostics.schema.json`."""

    code: str
    severity: Severity
    message: str
    span: SourceSpan

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.code.strip():
            errors.append("diagnostic.code must be non-empty")
        if self.severity not in VALID_SEVERITIES:
            errors.append(f"diagnostic.severity invalid: {self.severity!r}")
        if not self.message.strip():
            errors.append("diagnostic.message must be non-empty")
        errors.extend(self.span.validate())
        return errors

    def format_line(self) -> str:
        return (
            f"[{self.severity.upper()}] {self.code} "
            f"(L{self.span.start_line}:C{self.span.start_col}-L{self.span.end_line}:C{self.span.end_col}): "
            f"{self.message}"
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "code": self.code,
            "severity": self.severity,
            "message": self.message,
            "span": self.span.to_dict(),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Diagnostic:
        return cls(
            code=str(data["code"]),
            severity=data["severity"],
            message=str(data["message"]),
            span=SourceSpan.from_dict(data.get("span", {})),
        )


@dataclass(slots=True)
class CompileMetrics:
    """Geometric metrics returned by `cad-cli check/build --json`."""

    watertight: bool
    volume_mm3: float
    bbox_mm: BBox3D
    face_count: int
    edge_count: int

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not isinstance(self.watertight, bool):
            errors.append("metrics.watertight must be boolean")
        if self.volume_mm3 < 0:
            errors.append(f"metrics.volume_mm3 must be >= 0, got {self.volume_mm3}")
        if self.face_count < 0 or self.edge_count < 0:
            errors.append("metrics.face_count and edge_count must be >= 0")
        errors.extend(self.bbox_mm.validate("metrics.bbox_mm"))
        return errors

    def to_dict(self) -> dict[str, Any]:
        return {
            "watertight": self.watertight,
            "volume_mm3": float(self.volume_mm3),
            "bbox_mm": self.bbox_mm.to_dict(),
            "face_count": int(self.face_count),
            "edge_count": int(self.edge_count),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> CompileMetrics:
        return cls(
            watertight=bool(data["watertight"]),
            volume_mm3=float(data["volume_mm3"]),
            bbox_mm=BBox3D.from_dict(data["bbox_mm"]),
            face_count=int(data["face_count"]),
            edge_count=int(data["edge_count"]),
        )


@dataclass(slots=True)
class CompileResult:
    """Structured output of `cad-cli check` / `cad-cli build`."""

    status: CompileStatus
    file: str
    diagnostics: list[Diagnostic] = field(default_factory=list)
    metrics: CompileMetrics | None = None
    step_path: str | None = None
    mesh_path: str | None = None
    views_dir: str | None = None
    dxf_path: str | None = None
    drawing_path: str | None = None
    exit_code: int = 0
    raw_stderr: str = ""

    @property
    def has_errors(self) -> bool:
        return self.status != "ok" or any(d.severity == "error" for d in self.diagnostics)

    @property
    def error_diagnostics(self) -> list[Diagnostic]:
        return [d for d in self.diagnostics if d.severity == "error"]

    def validate(self) -> list[str]:
        errors: list[str] = []
        if self.status not in VALID_STATUSES:
            errors.append(f"status must be one of {VALID_STATUSES}, got {self.status!r}")
        if not self.file:
            errors.append("file must be non-empty")
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
        if self.step_path is not None:
            payload["step_path"] = self.step_path
        if self.mesh_path is not None:
            payload["mesh_path"] = self.mesh_path
        if self.views_dir is not None:
            payload["views_dir"] = self.views_dir
        if self.dxf_path is not None:
            payload["dxf_path"] = self.dxf_path
        if self.drawing_path is not None:
            payload["drawing_path"] = self.drawing_path
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
        drawing_path: str | None = None,
        exit_code: int = 0,
        raw_stderr: str = "",
    ) -> CompileResult:
        metrics_raw = data.get("metrics")
        metrics = CompileMetrics.from_dict(metrics_raw) if isinstance(metrics_raw, dict) else None
        return cls(
            status=data["status"],
            file=str(data.get("file", "")),
            diagnostics=[Diagnostic.from_dict(d) for d in data.get("diagnostics", [])],
            metrics=metrics,
            step_path=step_path or data.get("step_path"),
            mesh_path=mesh_path or data.get("mesh_path"),
            views_dir=views_dir or data.get("views_dir"),
            dxf_path=dxf_path or data.get("dxf_path"),
            drawing_path=drawing_path or data.get("drawing_path"),
            exit_code=exit_code,
            raw_stderr=raw_stderr,
        )


@dataclass(slots=True)
class GateCheckResult:
    """Result of an individual L0-L4 verification gate."""

    gate: GateLevel
    name: str
    passed: bool
    details: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "gate": self.gate,
            "name": self.name,
            "passed": self.passed,
            "details": self.details,
        }


@dataclass(slots=True)
class CriticVerdict:
    """Self-inspection report emitted by `VisualGeometricCritic`."""

    passed: bool
    iteration: int
    gate_results: list[GateCheckResult] = field(default_factory=list)
    issues: list[str] = field(default_factory=list)
    repair_instructions: str = ""
    visual_summary: str = ""
    metrics: CompileMetrics | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "passed": self.passed,
            "iteration": self.iteration,
            "gate_results": [g.to_dict() for g in self.gate_results],
            "issues": list(self.issues),
            "repair_instructions": self.repair_instructions,
            "visual_summary": self.visual_summary,
            "metrics": self.metrics.to_dict() if self.metrics is not None else None,
        }

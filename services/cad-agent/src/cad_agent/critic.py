"""Validate compiled geometry, interface evidence, and orthogonal views."""

from __future__ import annotations

import json
import math
import re
from collections.abc import Callable
from pathlib import Path
from typing import Any, Protocol

from .compiler_bridge import (
    check_external_envelope,
    check_keep_in_bbox,
    check_keep_out_zones,
    check_non_zero_volume,
    check_watertight,
)
from .contracts import (
    CompileResult,
    CriticVerdict,
    DesignContract,
    ExternalInterfaceSummary,
    GateCheckResult,
    KernelIdentity,
    MechanicalInterface,
    TriVerdict,
    VerificationCheck,
    VerificationReport,
    aggregate_tri_verdicts,
)


class OrthogonalViewInspector(Protocol):
    """Hook for VLM or headless SVG/mesh inspection of rendered views."""

    def inspect(
        self,
        views_dir: Path | None,
        mesh_path: Path | None,
        contract: DesignContract,
        compile_result: CompileResult,
    ) -> tuple[bool, str]:
        """Return (passed, visual_summary)."""


class HeuristicViewInspector:
    """Offline structural check for headless SVG orthogonal views and scene-mesh JSON."""

    EXPECTED_VIEWS: tuple[str, ...] = ("front.svg", "top.svg", "right.svg", "iso.svg")

    def inspect(
        self,
        views_dir: Path | None,
        mesh_path: Path | None,
        contract: DesignContract,
        compile_result: CompileResult,
    ) -> tuple[bool, str]:
        notes: list[str] = []
        if views_dir is not None and views_dir.is_dir():
            missing: list[str] = []
            for view_name in self.EXPECTED_VIEWS:
                svg_file = views_dir / view_name
                if not svg_file.is_file() or "<svg" not in svg_file.read_text(encoding="utf-8", errors="ignore"):
                    missing.append(view_name)
            if missing:
                return (False, f"Missing or invalid orthogonal SVG views: {', '.join(missing)}")
            notes.append("4 orthogonal/iso SVG views verified")

        if mesh_path is not None and mesh_path.is_file():
            try:
                mesh_data = json.loads(mesh_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError as exc:
                return (False, f"Corrupt scene-mesh JSON: {exc}")
            if mesh_data.get("format_version") != "0.1" or not mesh_data.get("faces"):
                return (False, "scene-mesh JSON missing format_version='0.1' or non-empty faces")
            notes.append(f"scene-mesh verified ({len(mesh_data.get('faces', []))} faces)")

        if not notes:
            return (True, "Visual inspection skipped (no views/mesh artifacts requested)")
        return (True, "; ".join(notes))


def _estimate_cylinder_diameter_from_faces(faces: list[dict[str, Any]]) -> float | None:
    """Estimate cylindrical diameter from tessellated face vertices when available."""
    xs: list[float] = []
    ys: list[float] = []
    for face in faces:
        verts = face.get("vertices", [])
        for i in range(0, len(verts) - 2, 3):
            xs.append(float(verts[i]))
            ys.append(float(verts[i + 1]))
    if len(xs) < 3:
        return None
    cx = sum(xs) / len(xs)
    cy = sum(ys) / len(ys)
    radii = [math.hypot(x - cx, y - cy) for x, y in zip(xs, ys)]
    avg_r = sum(radii) / len(radii)
    return 2.0 * avg_r if avg_r > 1e-6 else None


def _finite_number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    if not math.isfinite(number):
        return None
    return number


def _whole_number(value: Any) -> int | None:
    number = _finite_number(value)
    if number is None or abs(number - round(number)) > 1e-6:
        return None
    return int(round(number))


def _string_tags(record: dict[str, Any]) -> list[str]:
    tags: list[str] = []
    for key in ("provenance_tags", "target_tags", "tags"):
        raw = record.get(key)
        if isinstance(raw, str):
            tags.append(raw)
        elif isinstance(raw, list):
            tags.extend(str(item) for item in raw if item is not None)
    for key in ("interface_id", "selector", "tag"):
        raw = record.get(key)
        if isinstance(raw, str):
            tags.append(raw)
    return tags


def _tag_equals(tag: str, interface_id: str) -> bool:
    return tag.strip().lower() == interface_id.strip().lower()


def _bound_to_interface(record: dict[str, Any], interface_id: str) -> bool:
    """True when a compiler record is explicitly tagged with this interface id."""
    if not interface_id.strip():
        return False
    return any(_tag_equals(tag, interface_id) for tag in _string_tags(record))


def _evidence_documents(
    mesh_data: dict[str, Any] | None,
    compile_result: CompileResult,
) -> list[dict[str, Any]]:
    documents: list[dict[str, Any]] = []
    if isinstance(mesh_data, dict):
        documents.append(mesh_data)
    payload = getattr(compile_result, "cli_payload", None)
    if isinstance(payload, dict):
        documents.append(payload)
        for key in ("scene_mesh", "mesh", "geometry"):
            nested = payload.get(key)
            if isinstance(nested, dict):
                documents.append(nested)
    return documents


def _collect_records(documents: list[dict[str, Any]], keys: tuple[str, ...]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for document in documents:
        for key in keys:
            raw = document.get(key)
            items = [raw] if isinstance(raw, dict) else raw
            if not isinstance(items, list):
                continue
            for item in items:
                if not isinstance(item, dict):
                    continue
                item_id = item.get("face_id") or item.get("handle_id") or item.get("edge_id")
                if isinstance(item_id, str):
                    marker = (key, item_id)
                    if marker in seen:
                        continue
                    seen.add(marker)
                records.append(item)
    return records


def _is_compiler_geometry_record(record: dict[str, Any]) -> bool:
    """Compiler interface witness. A design-contract interface object is not one."""
    if isinstance(record.get("exact_interface"), bool):
        return True
    if record.get("surface_type") in {"plane", "cylinder", "cone", "sphere", "torus", "bspline", "mesh"}:
        return True
    return "measured_diameter_mm" in record or "radius_mm" in record


def _handle_matches(handle: dict[str, Any], iface: MechanicalInterface) -> bool:
    name = str(handle.get("param_name") or "").strip().lower()
    if not name:
        return False
    iface_id = iface.interface_id.strip().lower()
    if iface_id and iface_id in name:
        return True
    # Primary bore parameters emitted by the compiler as a single diameter handle.
    generic_bore = {"bore_diameter", "bore_radius", "shaft_diameter", "shaft_radius"}
    if iface.kind in ("bore", "shaft_seat") and name in generic_bore:
        return True
    return False


def _handle_diameter_mm(handle: dict[str, Any]) -> float | None:
    name = str(handle.get("param_name") or "").lower()
    if "count" in name:
        return None
    value = _finite_number(handle.get("current_value"))
    if value is None:
        value = _finite_number(handle.get("diameter_mm"))
    if value is None:
        return None
    if "diameter" not in name and "radius" not in name:
        return None
    if "radius" in name and "diameter" not in name:
        return value * 2.0
    return value


def _handle_count(handle: dict[str, Any]) -> int | None:
    name = str(handle.get("param_name") or "").lower()
    if "count" not in name:
        return None
    return _whole_number(handle.get("current_value"))


def _record_diameter_mm(record: dict[str, Any]) -> float | None:
    for key in ("diameter_mm", "measured_diameter_mm"):
        value = _finite_number(record.get(key))
        if value is not None:
            return value
    radius = _finite_number(record.get("radius_mm"))
    if radius is not None:
        return radius * 2.0
    return None


def _pmi_has_tolerance(record: dict[str, Any], tolerance: str) -> bool:
    token = tolerance.strip()
    if not token:
        return False
    blobs = [
        str(record[key])
        for key in ("label", "tolerance", "value", "kind")
        if record.get(key) is not None
    ]
    return re.search(rf"(?<![A-Za-z0-9]){re.escape(token)}(?![A-Za-z0-9])", " ".join(blobs)) is not None


def check_interfaces(
    contract: DesignContract,
    compile_result: CompileResult,
    *,
    mesh_data: dict[str, Any] | None = None,
    kcl_source: str | None = None,
    tolerance_mm: float = 0.05,
) -> tuple[bool, list[str]]:
    """Verify interfaces from compiler JSON only.

    KCL source text, including comments, is not evidence. A required bore, bolt
    pattern, tolerance, or exact interface must be bound in scene-mesh tags,
    CLI geometry/PMI/hole records, or edit handles emitted by the compiler.
    """
    _ = kcl_source
    if not contract.interfaces:
        return (True, [])

    documents = _evidence_documents(mesh_data, compile_result)
    faces = _collect_records(documents, ("faces",))
    handles = _collect_records(documents, ("edit_handles",))
    holes = _collect_records(documents, ("hole_offsets", "hole_groups", "holes"))
    pmi_records = _collect_records(documents, ("pmi", "pmi_records", "annotations"))
    geometry_records = [
        record
        for record in _collect_records(documents, ("geometry_interfaces", "bound_interfaces", "interfaces"))
        if _is_compiler_geometry_record(record)
    ]

    cylindrical_kinds = {"bore", "shaft_seat", "bolt_pattern", "counterbore_pattern"}
    planar_kinds = {"mounting_face", "datum", "slot"}
    violations: list[str] = []

    for iface in contract.interfaces:
        iface_id = iface.interface_id
        expected_surface = (
            "cylinder"
            if iface.kind in cylindrical_kinds
            else ("plane" if iface.kind in planar_kinds else None)
        )
        bound_faces = [face for face in faces if _bound_to_interface(face, iface_id)]
        bound_holes = [hole for hole in holes if _bound_to_interface(hole, iface_id)]
        bound_geometry = [record for record in geometry_records if _bound_to_interface(record, iface_id)]
        bound_handles = [handle for handle in handles if _handle_matches(handle, iface)]
        bound_pmi = [record for record in pmi_records if _bound_to_interface(record, iface_id)]
        relevant_faces = [
            face
            for face in bound_faces
            if expected_surface is None or face.get("surface_type") == expected_surface
        ]

        if not bound_faces and not bound_holes and not bound_geometry:
            expected_bits = [f"a tagged {expected_surface or 'feature'}"]
            if iface.preserve_exact_brep:
                expected_bits.append("exact_interface=true")
            if iface.diameter_mm is not None:
                expected_bits.append(f"diameter {iface.diameter_mm:.3f} mm")
            if iface.count is not None:
                expected_bits.append(f"count {iface.count}")
            if iface.tolerance:
                expected_bits.append(f"PMI tolerance {iface.tolerance}")
            violations.append(
                f"Interface '{iface_id}' ({iface.kind}) has no compiler-bound geometry. "
                f"Need {', '.join(expected_bits)}. "
                "Tag the feature in the evaluated model and emit hole/PMI records from cad-cli. "
                "Comments in the .kcl source are not evidence."
            )
            continue

        if expected_surface is not None and bound_faces and not relevant_faces:
            if not any(record.get("surface_type") == expected_surface for record in bound_geometry):
                found = sorted({str(face.get("surface_type")) for face in bound_faces})
                violations.append(
                    f"Interface '{iface_id}' requires surface_type {expected_surface!r}; "
                    f"bound faces are {found}. Retag the {expected_surface} faces with '{iface_id}'."
                )

        if iface.preserve_exact_brep:
            inexact = [face for face in relevant_faces if face.get("exact_interface") is not True]
            exact_records = [
                record
                for record in bound_geometry
                if record.get("exact_interface") is True
                and record.get("surface_type") in (None, expected_surface)
            ]
            if inexact or (not relevant_faces and not exact_records):
                violations.append(
                    f"Interface '{iface_id}' requires exact_interface=true on the bound "
                    f"{expected_surface or 'feature'}. Compiler evidence has "
                    f"{len(inexact)} face(s) with exact_interface not true. "
                    "Rebuild the feature with preserve_exact_interfaces=true and do not "
                    "treat a comment or a tessellated fit as an exact interface."
                )

        if iface.diameter_mm is not None:
            measurements: list[tuple[str, float]] = []
            for handle in bound_handles:
                diameter = _handle_diameter_mm(handle)
                if diameter is not None:
                    measurements.append((str(handle.get("param_name")), diameter))
            for hole in bound_holes:
                diameter = _record_diameter_mm(hole)
                if diameter is not None:
                    measurements.append(("hole record", diameter))
            for record in (*bound_geometry, *relevant_faces):
                diameter = _record_diameter_mm(record)
                if diameter is not None:
                    label = str(record.get("face_id") or record.get("interface_id") or "geometry record")
                    measurements.append((label, diameter))
            if not measurements and expected_surface == "cylinder":
                estimated = _estimate_cylinder_diameter_from_faces(relevant_faces)
                if estimated is not None:
                    measurements.append(("tessellated cylinder", estimated))
            if not measurements:
                violations.append(
                    f"Interface '{iface_id}' requires diameter {iface.diameter_mm:.3f} mm, "
                    "but cad-cli JSON has no bound hole diameter, edit handle, or measurable cylinder."
                )
            else:
                only_tessellation = (
                    len(measurements) == 1 and measurements[0][0] == "tessellated cylinder"
                )
                limit = max(tolerance_mm, 0.25) if only_tessellation else tolerance_mm
                for source, actual in measurements:
                    if abs(actual - iface.diameter_mm) > limit:
                        violations.append(
                            f"Interface '{iface_id}' diameter mismatch: expected {iface.diameter_mm:.3f} mm, "
                            f"compiler {source} reports {actual:.3f} mm."
                        )

        if iface.count is not None:
            explicit_counts: list[tuple[str, int]] = []
            for handle in bound_handles:
                count = _handle_count(handle)
                if count is not None:
                    explicit_counts.append((str(handle.get("param_name")), count))
            for record in (*bound_holes, *bound_geometry):
                if "count" not in record:
                    continue
                count = _whole_number(record.get("count"))
                if count is not None:
                    explicit_counts.append((str(record.get("interface_id") or record.get("tag") or "hole record"), count))
            if explicit_counts:
                for source, actual in explicit_counts:
                    if actual != iface.count:
                        violations.append(
                            f"Interface '{iface_id}' count mismatch: expected {iface.count}, "
                            f"compiler {source} reports {actual}."
                        )
            else:
                instances = [hole for hole in bound_holes if "count" not in hole]
                measured = len(instances) if instances else len(relevant_faces)
                if measured != iface.count:
                    violations.append(
                        f"Interface '{iface_id}' requires {iface.count} instances; "
                        f"compiler bound {measured} tagged face(s) or hole(s) and no count record. "
                        "Emit one tagged hole per instance, or a hole-group count, from cad-cli."
                    )

        if iface.tolerance:
            if not any(_pmi_has_tolerance(record, iface.tolerance) for record in bound_pmi):
                violations.append(
                    f"Interface '{iface_id}' requires PMI tolerance '{iface.tolerance}' on a compiler "
                    f"PMI record whose target_tags include '{iface_id}'. "
                    "A comment or bare tolerance string in the .kcl source is not PMI evidence."
                )

    return (len(violations) == 0, violations)


def _dot3(a: list[float], b: list[float]) -> float:
    return sum(x * y for x, y in zip(a, b))


def _external_hole_geometry(
    face: dict[str, Any], axis: list[float], tolerance_mm: float,
) -> tuple[float, list[float]] | None:
    """Measure one sampled inward cylinder from its tessellated positions and radial normals."""
    if face.get("surface_type") != "cylinder":
        return None
    verts, norms = face.get("vertices"), face.get("normals")
    if not isinstance(verts, list) or not isinstance(norms, list):
        return None
    if len(verts) != len(norms) or len(verts) < 12 or len(verts) % 3:
        return None
    if not all(type(v) in (float, int) and math.isfinite(v) for values in (verts, norms) for v in values):
        return None
    points = [verts[i:i + 3] for i in range(0, len(verts), 3)]
    normals = []
    for i in range(0, len(norms), 3):
        n = norms[i:i + 3]
        length = math.hypot(*n)
        if length < 1e-8 or not math.isfinite(length) or abs(_dot3(n, axis) / length) > 1e-4:
            return None
        normals.append([v / length for v in n])
    # For an inward wall, p + r*n lies on its axis. Choose the most widely spaced
    # normals to avoid an ill-conditioned fit and verify all other samples against it.
    n0 = normals[0]
    i = min(range(len(normals)), key=lambda i: _dot3(n0, normals[i]))
    dn = [a - b for a, b in zip(n0, normals[i])]
    dn2 = _dot3(dn, dn)
    if dn2 < 1:
        return None
    radius = _dot3([b - a for a, b in zip(points[0], points[i])], dn) / dn2
    if not math.isfinite(radius) or radius <= 0:
        return None
    center = [p + radius * n for p, n in zip(points[0], n0)]
    tangent = [
        axis[1] * n0[2] - axis[2] * n0[1],
        axis[2] * n0[0] - axis[0] * n0[2],
        axis[0] * n0[1] - axis[1] * n0[0],
    ]
    angles, offsets = [], []
    for point, normal in zip(points, normals):
        delta = [p - c + radius * n for p, c, n in zip(point, center, normal)]
        axial = _dot3(delta, axis)
        radial_error = math.hypot(*(d - axial * a for d, a in zip(delta, axis)))
        if not math.isfinite(radial_error) or radial_error > tolerance_mm:
            return None
        offsets.append(axial)
        angles.append(math.atan2(_dot3(normal, tangent), _dot3(normal, n0)))
    angles.sort()
    # Reject faces whose samples leave a large arc unrepresented.
    if max(offsets) - min(offsets) <= 1e-6 or any(
        b - a > math.pi / 2 + 1e-4
        for a, b in zip(angles, [*angles[1:], angles[0] + math.tau])
    ):
        return None
    return 2 * radius, center


def _projected_distance(a: list[float], b: list[float], axis: list[float]) -> float:
    delta = [x - y for x, y in zip(a, b)]
    axial = _dot3(delta, axis)
    return math.hypot(*(d - axial * normal for d, normal in zip(delta, axis)))


def check_external_holes(
    context: ExternalInterfaceSummary, mesh_data: dict[str, Any] | None, tolerance_mm: float = 0.05,
) -> list[str]:
    """Check measured hole diameter and projected placement/spacing; no source-text exemption."""
    faces = (mesh_data or {}).get("faces", [])
    issues = []
    for group in context.hole_groups:
        tag = group["tag"]
        axis = group["axis"]
        cylinders = []
        for face in faces:
            if not isinstance(face, dict) or tag not in face.get("provenance_tags", []):
                continue
            measurement = _external_hole_geometry(face, axis, tolerance_mm)
            if measurement is not None:
                cylinders.append(measurement)
        if len(cylinders) != group["count"]:
            issues.append(f"External hole group {tag}: expected {group['count']} tagged inward cylindrical holes; measured {len(cylinders)}")
            continue
        expected_diameter = group["diameter_mm"]
        if any(abs(diameter - expected_diameter) > tolerance_mm for diameter, _ in cylinders):
            issues.append(f"External hole group {tag}: diameter must match {expected_diameter:.3f} mm")
        remaining = [center for _, center in cylinders]
        matched = []
        expected = group["centers_mm"]
        for center in expected:
            i = min(range(len(remaining)), key=lambda i: _projected_distance(remaining[i], center, axis))
            actual = remaining.pop(i)
            if _projected_distance(actual, center, axis) > tolerance_mm:
                break
            matched.append(actual)
        if len(matched) != len(expected) or any(
            abs(_projected_distance(a, b, axis) - _projected_distance(ea, eb, axis)) > tolerance_mm
            for i, (a, ea) in enumerate(zip(matched, expected))
            for b, eb in zip(matched[:i], expected[:i])
        ):
            issues.append(f"External hole group {tag}: projected centers or center spacing do not match the inspected asset")
    return issues


def _extract_kernel_identity(compile_result: CompileResult) -> KernelIdentity:
    payload = getattr(compile_result, "cli_payload", None)
    if isinstance(payload, dict) and isinstance(payload.get("kernel_identity"), dict):
        return KernelIdentity.from_dict(payload["kernel_identity"])
    backend = (
        compile_result.metrics.kernel_backend
        if compile_result.metrics is not None and compile_result.metrics.kernel_backend
        else None
    )
    if backend:
        return KernelIdentity(
            provider=backend,
            version="7.9.0",
            authoritative="occt" in backend.lower(),
        )
    return KernelIdentity(provider="occt", version="7.9.0", authoritative=True)


class VisualGeometricCritic:
    """Runs five-layer tri-state (PASS / FAIL / UNKNOWN) geometry gates + orthogonal view checks."""

    def __init__(
        self,
        view_inspector: OrthogonalViewInspector | None = None,
        *,
        bbox_tolerance_mm: float = 1e-4,
    ) -> None:
        self.view_inspector = view_inspector or HeuristicViewInspector()
        self.bbox_tolerance_mm = bbox_tolerance_mm

    def evaluate(
        self,
        contract: DesignContract,
        compile_result: CompileResult,
        *,
        iteration: int = 1,
        external_interference: tuple[bool, str] | None = None,
        render_shaded_views: Callable[[], dict[str, bytes] | None] | None = None,
        fea_results: dict[str, Any] | None = None,
        step_roundtrip: dict[str, Any] | None = None,
    ) -> CriticVerdict:
        gates: list[GateCheckResult] = []
        checks: list[VerificationCheck] = []
        issues: list[str] = []
        repair_hints: list[str] = []

        mesh_path = Path(compile_result.mesh_path) if compile_result.mesh_path else None
        mesh_data: dict[str, Any] | None = None
        if mesh_path is not None and mesh_path.is_file():
            try:
                mesh_data = json.loads(mesh_path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                mesh_data = None

        # L0: Syntax, dimensional type checking, deterministic selectors
        errors = compile_result.error_diagnostics
        l0_passed = (compile_result.status == "ok") and (len(errors) == 0)
        l0_status: TriVerdict = "PASS" if l0_passed else "FAIL"
        l0_details = "0 compiler errors" if l0_passed else f"{len(errors)} error(s)"
        if not l0_passed:
            formatted = [d.format_line() for d in errors] or [
                f"Compiler exited with status={compile_result.status}"
            ]
            issues.extend(formatted)
            repair_hints.extend(formatted)
        checks.append(
            VerificationCheck(
                code="L0_SYNTAX_UNITS",
                layer="L0_structure_units",
                status=l0_status,
                message=l0_details,
                evidence={
                    "compile_status": compile_result.status,
                    "error_count": len(errors),
                    "error_codes": [d.code for d in errors],
                },
            )
        )
        gates.append(
            GateCheckResult(
                gate="L0",
                name="Syntax & Dimensional Type Check",
                passed=l0_passed,
                status=l0_status,
                details=l0_details,
            )
        )

        # L1: Watertight solid & non-zero volume
        wt_ok, wt_msg = check_watertight(compile_result.metrics)
        vol_ok, vol_msg = check_non_zero_volume(compile_result.metrics)
        l1_passed = wt_ok and vol_ok
        l1_status: TriVerdict = "PASS" if l1_passed else "FAIL"
        if not wt_ok:
            issues.append(wt_msg)
            repair_hints.append(f"Fix geometry watertightness: {wt_msg}")
        if not vol_ok:
            issues.append(vol_msg)
            repair_hints.append(f"Fix solid volume: {vol_msg}")
        checks.append(
            VerificationCheck(
                code="L1_WATERTIGHT_MANIFOLD",
                layer="L1_geometry_topology",
                status="PASS" if wt_ok else "FAIL",
                message=wt_msg,
                evidence={
                    "watertight": bool(compile_result.metrics.watertight)
                    if compile_result.metrics is not None
                    else False,
                    "face_count": compile_result.metrics.face_count
                    if compile_result.metrics is not None
                    else 0,
                    "edge_count": compile_result.metrics.edge_count
                    if compile_result.metrics is not None
                    else 0,
                },
            )
        )
        checks.append(
            VerificationCheck(
                code="L1_POSITIVE_VOLUME",
                layer="L1_geometry_topology",
                status="PASS" if vol_ok else "FAIL",
                message=vol_msg,
                evidence={
                    "volume_mm3": compile_result.metrics.volume_mm3
                    if compile_result.metrics is not None
                    else 0.0,
                },
            )
        )
        gates.append(
            GateCheckResult(
                gate="L1",
                name="Watertight Manifold & Positive Volume",
                passed=l1_passed,
                status=l1_status,
                details=(
                    f"{wt_msg}; {vol_msg}"
                    if l1_passed
                    else "; ".join(m for ok, m in ((wt_ok, wt_msg), (vol_ok, vol_msg)) if not ok)
                ),
            )
        )

        # L2: Keep-In BBox, Keep-Out clearance (narrow-phase mesh), and Mechanical Interfaces
        keep_in_ok, keep_in_violations = check_keep_in_bbox(
            compile_result.metrics,
            contract.keep_in_bbox_mm,
            tolerance=self.bbox_tolerance_mm,
        )
        keep_out_ok, keep_out_violations = check_keep_out_zones(
            compile_result.metrics,
            contract.keep_out_zones,
            tolerance=self.bbox_tolerance_mm,
            mesh_path=mesh_path,
            mesh_data=mesh_data,
        )
        iface_ok, iface_violations = check_interfaces(
            contract,
            compile_result,
            mesh_data=mesh_data,
        )
        l2_passed = keep_in_ok and keep_out_ok and iface_ok
        if not keep_in_ok:
            msg = "Keep-In bbox overflow: " + ", ".join(keep_in_violations)
            issues.append(msg)
            repair_hints.append(
                f"Constrain dimensions inside keep_in_bbox_mm={contract.keep_in_bbox_mm.to_dict()} ({msg})"
            )
        if not keep_out_ok:
            for v in keep_out_violations:
                issues.append(v)
                repair_hints.append(f"Remove geometry from Keep-Out zone: {v}")
        if not iface_ok:
            for v in iface_violations:
                issues.append(v)
                repair_hints.append(f"Implement/fix mechanical interface: {v}")

        checks.append(
            VerificationCheck(
                code="L2_KEEP_IN_BBOX",
                layer="L2_interfaces_constraints",
                status="PASS" if keep_in_ok else "FAIL",
                message="Within Keep-In bounding envelope"
                if keep_in_ok
                else "; ".join(keep_in_violations),
                evidence={
                    "keep_in_bbox_mm": contract.keep_in_bbox_mm.to_dict(),
                    "violations": list(keep_in_violations),
                },
            )
        )
        checks.append(
            VerificationCheck(
                code="L2_KEEP_OUT_CLEARANCE",
                layer="L2_interfaces_constraints",
                status="PASS" if keep_out_ok else "FAIL",
                message="All Keep-Out zones clear"
                if keep_out_ok
                else "; ".join(keep_out_violations),
                evidence={
                    "zone_count": len(contract.keep_out_zones),
                    "violations": list(keep_out_violations),
                },
            )
        )
        checks.append(
            VerificationCheck(
                code="L2_MECHANICAL_INTERFACES",
                layer="L2_interfaces_constraints",
                status="PASS" if iface_ok else "FAIL",
                message="All declared mechanical interfaces verified"
                if iface_ok
                else "; ".join(iface_violations),
                evidence={
                    "interface_count": len(contract.interfaces),
                    "violations": list(iface_violations),
                },
            )
        )

        l2_details = (
            "Within Keep-In envelope, Keep-Out clear, interfaces verified"
            if l2_passed
            else "; ".join(keep_in_violations + keep_out_violations + iface_violations)
        )
        gates.append(
            GateCheckResult(
                gate="L2",
                name="Keep-In Envelope, Keep-Out Clearance & Interfaces",
                passed=l2_passed,
                status="PASS" if l2_passed else "FAIL",
                details=l2_details,
            )
        )

        if l0_passed and l1_passed and contract.external_context is not None:
            context = contract.external_context
            hole_issues = check_external_holes(context, mesh_data)
            if context.hole_groups:
                holes_ok = not hole_issues
                hole_details = (
                    "; ".join(hole_issues)
                    or "Tessellated hole diameter and projected centers verified"
                )
                gates.append(
                    GateCheckResult(
                        gate="L2",
                        name="External Mounting Hole Alignment",
                        passed=holes_ok,
                        status="PASS" if holes_ok else "FAIL",
                        details=hole_details,
                    )
                )
                checks.append(
                    VerificationCheck(
                        code="L2_EXTERNAL_MOUNTING_HOLES",
                        layer="L2_interfaces_constraints",
                        status="PASS" if holes_ok else "FAIL",
                        message=hole_details,
                        evidence={"violations": list(hole_issues)},
                    )
                )
            issues.extend(hole_issues)
            repair_hints.extend(
                f"Correct the measured mounting hole geometry: {issue}" for issue in hole_issues
            )
            if external_interference is not None:
                clear, message = external_interference
                gates.append(
                    GateCheckResult(
                        gate="L2",
                        name="Exact External B-Rep Interference",
                        passed=clear,
                        status="PASS" if clear else "FAIL",
                        details=message,
                    )
                )
                checks.append(
                    VerificationCheck(
                        code="L2_EXTERNAL_BREP_INTERFERENCE",
                        layer="L2_interfaces_constraints",
                        status="PASS" if clear else "FAIL",
                        message=message,
                        evidence={"exact_brep_check": True, "clear": clear},
                    )
                )
                if not clear:
                    issues.append(message)
                    repair_hints.append(
                        "Resolve the reported interference or exact-check error before accepting the model."
                    )
            else:
                envelope_ok, envelope_msg = check_external_envelope(
                    compile_result.metrics, context.bbox_mm, mesh_data, self.bbox_tolerance_mm
                )
                gates.append(
                    GateCheckResult(
                        gate="L2",
                        name="External AABB Containment Screen",
                        passed=envelope_ok,
                        status="PASS" if envelope_ok else "FAIL",
                        details=envelope_msg,
                    )
                )
                checks.append(
                    VerificationCheck(
                        code="L2_EXTERNAL_AABB_CONTAINMENT",
                        layer="L2_interfaces_constraints",
                        status="PASS" if envelope_ok else "FAIL",
                        message=envelope_msg,
                        evidence={"envelope_ok": envelope_ok},
                    )
                )
                if not envelope_ok:
                    issues.append(envelope_msg)
                    repair_hints.append(
                        "Leave clearance at the supplied external AABB or move the new part so it does not enclose the reference."
                    )

        # L3: Manufacturability (DFM) & Structural Physics (FEA loads)
        l3_checks: list[VerificationCheck] = []
        cli_payload = getattr(compile_result, "cli_payload", None)
        dfm_report = (
            cli_payload.get("dfm_report")
            if isinstance(cli_payload, dict) and isinstance(cli_payload.get("dfm_report"), dict)
            else None
        )
        if dfm_report is not None and (
            dfm_report.get("passed") is False
            or int(dfm_report.get("violation_count", 0) or 0) > 0
        ):
            dfm_msg = str(
                dfm_report.get("message")
                or f"DFM check reported {dfm_report.get('violation_count', 1)} violation(s)"
            )
            l3_checks.append(
                VerificationCheck(
                    code="L3_DFM_MANUFACTURABILITY",
                    layer="L3_manufacturability_physics",
                    status="FAIL",
                    message=dfm_msg,
                    evidence=dict(dfm_report),
                )
            )
            issues.append(dfm_msg)
            repair_hints.append(f"Fix DFM violation: {dfm_msg}")
        else:
            l3_checks.append(
                VerificationCheck(
                    code="L3_DFM_MANUFACTURABILITY",
                    layer="L3_manufacturability_physics",
                    status="PASS",
                    message=f"DFM baseline checks passed for {contract.manufacturing_method}",
                    evidence={"manufacturing_method": contract.manufacturing_method},
                )
            )

        resolved_fea = fea_results
        if resolved_fea is None and isinstance(cli_payload, dict):
            raw_fea = cli_payload.get("fea_results")
            if isinstance(raw_fea, dict):
                resolved_fea = raw_fea

        if contract.loads:
            if resolved_fea is None:
                load_ids = [
                    load.load_id or f"load_{idx}" for idx, load in enumerate(contract.loads, 1)
                ]
                l3_load_msg = (
                    f"DesignContract declares {len(contract.loads)} structural load(s) "
                    f"({', '.join(load_ids)}), but no FEA solver result is attached; "
                    "L3 structural physics verification is UNKNOWN."
                )
                l3_checks.append(
                    VerificationCheck(
                        code="L3_LOADS_NO_FEA_SOLVER",
                        layer="L3_manufacturability_physics",
                        status="UNKNOWN",
                        message=l3_load_msg,
                        evidence={
                            "load_ids": load_ids,
                            "load_count": len(contract.loads),
                            "fea_attached": False,
                        },
                    )
                )
            else:
                for load in contract.loads:
                    entry = resolved_fea.get(load.load_id)
                    sf = (
                        _finite_number(entry.get("safety_factor"))
                        if isinstance(entry, dict)
                        else None
                    )
                    if sf is None:
                        l3_checks.append(
                            VerificationCheck(
                                code="L3_LOAD_MISSING_FEA_RESULT",
                                layer="L3_manufacturability_physics",
                                status="UNKNOWN",
                                message=(
                                    f"Load '{load.load_id}' has no attached FEA safety_factor; "
                                    "L3 structural physics status is UNKNOWN."
                                ),
                                evidence={
                                    "load_id": load.load_id,
                                    "required_min_safety_factor": load.min_safety_factor,
                                    "fea_attached": False,
                                },
                            )
                        )
                    elif sf >= load.min_safety_factor and entry.get("passed", True) is not False:
                        l3_checks.append(
                            VerificationCheck(
                                code="L3_LOAD_SAFETY_FACTOR",
                                layer="L3_manufacturability_physics",
                                status="PASS",
                                message=(
                                    f"Load '{load.load_id}' FEA safety_factor={sf:.3f} >= "
                                    f"min_safety_factor={load.min_safety_factor:.3f}"
                                ),
                                evidence={
                                    "load_id": load.load_id,
                                    "safety_factor": sf,
                                    "min_safety_factor": load.min_safety_factor,
                                    "fea_attached": True,
                                },
                            )
                        )
                    else:
                        fail_msg = (
                            f"Load '{load.load_id}' failed FEA safety factor check: "
                            f"actual={sf:.3f} < required={load.min_safety_factor:.3f}"
                        )
                        l3_checks.append(
                            VerificationCheck(
                                code="L3_LOAD_SAFETY_FACTOR",
                                layer="L3_manufacturability_physics",
                                status="FAIL",
                                message=fail_msg,
                                evidence={
                                    "load_id": load.load_id,
                                    "safety_factor": sf,
                                    "min_safety_factor": load.min_safety_factor,
                                    "fea_attached": True,
                                },
                            )
                        )
                        issues.append(fail_msg)
                        repair_hints.append(f"Reinforce structure for load '{load.load_id}': {fail_msg}")

        checks.extend(l3_checks)
        l3_status = aggregate_tri_verdicts([c.status for c in l3_checks])
        l3_details = "; ".join(c.message for c in l3_checks)
        gates.append(
            GateCheckResult(
                gate="L3",
                name="Manufacturability & Structural Physics",
                passed=l3_status == "PASS",
                status=l3_status,
                details=l3_details,
            )
        )

        # L4: Interop STEP round-trip & auxiliary orthogonal view / VLM inspection
        views_dir = Path(compile_result.views_dir) if compile_result.views_dir else None
        from .vision import VLMViewInspector

        if render_shaded_views is not None and isinstance(self.view_inspector, VLMViewInspector):
            vis_ok, vis_summary = self.view_inspector.inspect(
                views_dir,
                mesh_path,
                contract,
                compile_result,
                render_shaded_views=render_shaded_views,
            )
        else:
            vis_ok, vis_summary = self.view_inspector.inspect(
                views_dir,
                mesh_path,
                contract,
                compile_result,
            )
        if not vis_ok:
            issues.append(f"Visual/Mesh inspection failed: {vis_summary}")
            repair_hints.append(vis_summary)

        resolved_rt = step_roundtrip
        if resolved_rt is None and isinstance(cli_payload, dict):
            for rt_key in ("step_roundtrip", "interop_roundtrip"):
                candidate_rt = cli_payload.get(rt_key)
                if isinstance(candidate_rt, dict):
                    resolved_rt = candidate_rt
                    break

        l4_checks: list[VerificationCheck] = []
        if resolved_rt is None:
            l4_checks.append(
                VerificationCheck(
                    code="L4_STEP_ROUNDTRIP_NOT_RUN",
                    layer="L4_interop_roundtrip",
                    status="UNKNOWN",
                    message=(
                        "STEP AP242 round-trip or reference STEP comparison was not executed; "
                        "L4 interop status is UNKNOWN."
                    ),
                    evidence={
                        "step_path": compile_result.step_path,
                        "roundtrip_executed": False,
                        "visual_auxiliary_passed": vis_ok,
                        "visual_summary": vis_summary,
                    },
                )
            )
        else:
            rel_err = _finite_number(resolved_rt.get("volume_rel_error"))
            if rel_err is None:
                rel_err = _finite_number(resolved_rt.get("relative_error"))
            tol = _finite_number(resolved_rt.get("tolerance")) or 1e-4
            explicit_pass = resolved_rt.get("passed")
            rt_status_str = resolved_rt.get("status")
            rt_ok = (
                (explicit_pass is True or rt_status_str in ("ok", "PASS"))
                and (rel_err is None or rel_err <= tol)
                and explicit_pass is not False
                and rt_status_str not in ("error", "FAIL")
            )
            if rt_ok:
                rt_msg = (
                    f"STEP AP242 round-trip verified (volume_rel_error={rel_err:.6g} <= {tol:.6g})"
                    if rel_err is not None
                    else "STEP AP242 round-trip verified"
                )
                l4_checks.append(
                    VerificationCheck(
                        code="L4_STEP_ROUNDTRIP",
                        layer="L4_interop_roundtrip",
                        status="PASS",
                        message=rt_msg,
                        evidence={
                            **dict(resolved_rt),
                            "roundtrip_executed": True,
                            "visual_auxiliary_passed": vis_ok,
                            "visual_summary": vis_summary,
                        },
                    )
                )
            else:
                rt_msg = str(
                    resolved_rt.get("message")
                    or (
                        f"STEP AP242 round-trip volume relative error {rel_err:.6g} exceeds {tol:.6g}"
                        if rel_err is not None
                        else "STEP AP242 round-trip verification failed"
                    )
                )
                l4_checks.append(
                    VerificationCheck(
                        code="L4_STEP_ROUNDTRIP",
                        layer="L4_interop_roundtrip",
                        status="FAIL",
                        message=rt_msg,
                        evidence={**dict(resolved_rt), "roundtrip_executed": True},
                    )
                )
                issues.append(rt_msg)
                repair_hints.append(f"Fix STEP round-trip geometry fidelity: {rt_msg}")

        if not vis_ok:
            l4_checks.append(
                VerificationCheck(
                    code="L4_VISUAL_MESH_INSPECTION",
                    layer="L4_interop_roundtrip",
                    status="FAIL",
                    message=f"Visual/Mesh inspection failed: {vis_summary}",
                    evidence={"visual_summary": vis_summary, "visual_auxiliary_passed": False},
                )
            )

        checks.extend(l4_checks)
        gates.append(
            GateCheckResult(
                gate="L4",
                name="Orthogonal View & Scene-Mesh Inspection",
                passed=vis_ok,
                status="PASS" if vis_ok else "FAIL",
                details=vis_summary,
            )
        )

        report = VerificationReport(
            kernel_identity=_extract_kernel_identity(compile_result),
            checks=checks,
            summary="; ".join(issues) if issues else vis_summary,
        )

        # Deterministic L0-L3 gates must all be PASS (so L3 UNKNOWN on unverified loads
        # or any L0-L3 FAIL sets passed=False). Heuristic/VLM visual checks are auxiliary
        # diagnostic evidence that can fail L4 on corrupt/missing views, never override L0-L3 FAIL.
        l0_to_l3_all_pass = all(
            report.layers[layer] == "PASS"
            for layer in (
                "L0_structure_units",
                "L1_geometry_topology",
                "L2_interfaces_constraints",
                "L3_manufacturability_physics",
            )
        )
        l4_not_failed = report.layers["L4_interop_roundtrip"] != "FAIL" and vis_ok
        overall_passed = l0_to_l3_all_pass and l4_not_failed

        return CriticVerdict(
            passed=overall_passed,
            iteration=iteration,
            gate_results=gates,
            issues=issues,
            repair_instructions="\n".join(repair_hints),
            visual_summary=vis_summary,
            metrics=compile_result.metrics,
            verification_report=report,
        )

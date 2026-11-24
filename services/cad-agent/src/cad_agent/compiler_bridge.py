"""Subprocess bridge to the headless `cad-cli` Rust compiler and geometry checks."""

from __future__ import annotations

import json
import os
import shutil
import struct
import subprocess
import tempfile
import zlib
from functools import partial
from pathlib import Path
from typing import Any, Callable, Sequence

from . import observe
from .contracts import (
    EXTERNAL_LENGTH_UNITS,
    BBox3D,
    CompileMetrics,
    CompileResult,
    ContractValidationError,
    ExternalInterfaceSummary,
    KeepOutZone,
)


ORTHOGONAL_VIEW_NAMES = ("front", "top", "right", "iso")
_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def _normalize_view_stem(view_name: str) -> str:
    """Normalize a view identifier like 'front.png', 'front.svg', or 'front' to 'front'."""
    stem = Path(view_name).name.lower()
    for suffix in (".semantic.json", ".mask.png", ".png", ".svg"):
        if stem.endswith(suffix):
            return stem[: -len(suffix)]
    return stem


class ShadedViewsBundle(dict[str, bytes]):
    """`dict[str, bytes]` of 4 shaded view PNGs with optional semantic masks and legend."""

    def __init__(
        self,
        images: dict[str, bytes] | None = None,
        *,
        semantic_masks: dict[str, bytes] | None = None,
        semantic_legend: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(images or {})
        self.semantic_masks: dict[str, bytes] = dict(semantic_masks or {})
        self.semantic_legend: dict[str, Any] | None = semantic_legend

    def get_view_semantic(
        self, view_name: str,
    ) -> tuple[bytes | None, dict[str, Any] | None]:
        """Return `(mask_png_bytes, legend_doc)` for the requested view stem."""
        stem = _normalize_view_stem(view_name)
        mask = self.semantic_masks.get(stem)
        legend = _extract_view_legend_payload(self.semantic_legend, stem)
        return (mask, legend)


def _has_complete_png_chunks(data: bytes) -> bool:
    if not data.startswith(_PNG_SIGNATURE):
        return False
    offset = len(_PNG_SIGNATURE)
    has_image_data = False
    while offset + 12 <= len(data):
        size, kind = struct.unpack_from(">I4s", data, offset)
        payload_end = offset + 8 + size
        if payload_end + 4 > len(data):
            return False
        crc, = struct.unpack_from(">I", data, payload_end)
        if zlib.crc32(memoryview(data)[offset + 4 : payload_end]) != crc:
            return False
        if offset == len(_PNG_SIGNATURE):
            if kind != b"IHDR" or size != 13:
                return False
            width, height = struct.unpack_from(">II", data, offset + 8)
            if width == 0 or height == 0:
                return False
        if kind == b"IDAT" and size:
            has_image_data = True
        if kind == b"IEND":
            return size == 0 and has_image_data and payload_end + 4 == len(data)
        offset = payload_end + 4
    return False


def _extract_view_legend_payload(
    raw_doc: dict[str, Any] | None,
    view_stem: str | None = None,
) -> dict[str, Any] | None:
    """Normalize `semantic.json` or `<view>.semantic.json` into a single-view legend dict."""
    if not isinstance(raw_doc, dict):
        return None
    if view_stem is None:
        return raw_doc

    views = raw_doc.get("views")
    if isinstance(views, list):
        for entry in views:
            if not isinstance(entry, dict):
                continue
            entry_view = entry.get("view")
            if isinstance(entry_view, str) and _normalize_view_stem(entry_view) == view_stem:
                return {
                    "width": raw_doc.get("width"),
                    "height": raw_doc.get("height"),
                    "view": view_stem,
                    "image": entry.get("image"),
                    "mask": entry.get("mask"),
                    "entries": list(entry.get("entries", []))
                    if isinstance(entry.get("entries"), list)
                    else [],
                }
        return None

    if isinstance(raw_doc.get("entries"), list):
        doc_view = raw_doc.get("view")
        if not isinstance(doc_view, str) or _normalize_view_stem(doc_view) == view_stem:
            return {
                "width": raw_doc.get("width"),
                "height": raw_doc.get("height"),
                "view": view_stem,
                "image": raw_doc.get("image"),
                "mask": raw_doc.get("mask"),
                "entries": list(raw_doc["entries"]),
            }
    return None


def load_semantic_mask(directory: Path, view_name: str) -> bytes | None:
    """Load `<view>.mask.png` from `directory`, returning None if missing or corrupt."""
    stem = _normalize_view_stem(view_name)
    if not stem:
        return None
    mask_path = directory / f"{stem}.mask.png"
    try:
        if not mask_path.is_file():
            return None
        data = mask_path.read_bytes()
    except OSError:
        return None
    if not _has_complete_png_chunks(data):
        return None
    return data


def load_semantic_legend(
    path_or_dir: Path,
    view_name: str | None = None,
) -> dict[str, Any] | None:
    """Load semantic legend JSON from a file or render directory (graceful None on miss)."""
    stem = _normalize_view_stem(view_name) if view_name is not None else None
    try:
        if path_or_dir.is_file():
            raw = json.loads(path_or_dir.read_text(encoding="utf-8"))
            if not isinstance(raw, dict):
                return None
            return _extract_view_legend_payload(raw, stem)

        if not path_or_dir.is_dir():
            return None

        if stem is not None:
            per_view_file = path_or_dir / f"{stem}.semantic.json"
            if per_view_file.is_file():
                raw = json.loads(per_view_file.read_text(encoding="utf-8"))
                if isinstance(raw, dict):
                    extracted = _extract_view_legend_payload(raw, stem)
                    if extracted is not None:
                        return extracted

        shared_file = path_or_dir / "semantic.json"
        if shared_file.is_file():
            raw = json.loads(shared_file.read_text(encoding="utf-8"))
            if isinstance(raw, dict):
                return _extract_view_legend_payload(raw, stem)

        # Merge per-view semantic files when shared `semantic.json` is absent.
        merged_views: list[dict[str, Any]] = []
        width: Any = None
        height: Any = None
        for name in ORTHOGONAL_VIEW_NAMES:
            per_view_file = path_or_dir / f"{name}.semantic.json"
            if not per_view_file.is_file():
                continue
            raw = json.loads(per_view_file.read_text(encoding="utf-8"))
            if not isinstance(raw, dict):
                continue
            extracted = _extract_view_legend_payload(raw, name)
            if extracted is not None:
                width = width if width is not None else extracted.get("width")
                height = height if height is not None else extracted.get("height")
                merged_views.append(extracted)
        if merged_views:
            return {"width": width, "height": height, "views": merged_views}
        return None
    except (OSError, json.JSONDecodeError):
        return None


def load_view_semantic_artifacts(
    directory: Path,
    view_name: str,
) -> tuple[bytes, dict[str, Any] | None] | None:
    """Load `(mask_png_bytes, view_legend)` for a single view from `directory`."""
    mask_bytes = load_semantic_mask(directory, view_name)
    if mask_bytes is None:
        return None
    legend = load_semantic_legend(directory, view_name)
    return (mask_bytes, legend)


def load_shaded_views(directory: Path) -> dict[str, bytes] | None:
    """Read all four shaded views together, rejecting missing or incomplete PNGs."""
    images: dict[str, bytes] = {}
    try:
        for name in ORTHOGONAL_VIEW_NAMES:
            filename = f"{name}.png"
            data = (directory / filename).read_bytes()
            if not _has_complete_png_chunks(data):
                return None
            images[filename] = data
    except OSError:
        return None
    return images


def load_shaded_views_with_masks(directory: Path) -> ShadedViewsBundle | None:
    """Read all four shaded views and best-effort attach any semantic masks and legend."""
    images = load_shaded_views(directory)
    if images is None:
        return None
    masks: dict[str, bytes] = {}
    for name in ORTHOGONAL_VIEW_NAMES:
        mask_data = load_semantic_mask(directory, name)
        if mask_data is not None:
            masks[name] = mask_data
    legend = load_semantic_legend(directory)
    return ShadedViewsBundle(images, semantic_masks=masks, semantic_legend=legend)


class CompilerBridgeError(RuntimeError):
    """Raised when `cad-cli` cannot be found or emits malformed output."""


def find_workspace_root(start: Path | None = None) -> Path:
    """Walk upward from `start` (or this module) to locate the repo root."""
    cur = (start or Path(__file__)).resolve()
    if cur.is_file():
        cur = cur.parent
    for candidate in (cur, *cur.parents):
        if (
            (candidate / "dsl-spec").is_dir()
            or (candidate / "Cargo.toml").is_file()
            or (candidate / ".git").exists()
        ):
            return candidate.resolve()
    return Path.cwd().resolve()


def discover_cad_cli(
    explicit_bin: str | Path | None = None,
    workspace_root: Path | None = None,
) -> Path | None:
    """Auto-detect `cad-cli` from explicit arg, `CAD_CLI_BIN`, newest target build, or PATH."""
    if explicit_bin:
        explicit_path = Path(explicit_bin)
        if explicit_path.is_file():
            return explicit_path.resolve()
        which_explicit = shutil.which(str(explicit_bin))
        if which_explicit:
            return Path(which_explicit).resolve()

    env_bin = os.environ.get("CAD_CLI_BIN")
    if env_bin:
        env_path = Path(env_bin)
        if env_path.is_file():
            return env_path.resolve()

    root = (workspace_root or find_workspace_root()).resolve()
    exe_names = ("cad-cli.exe", "cad-cli") if os.name == "nt" else ("cad-cli", "cad-cli.exe")
    search_roots = [root]
    git_entry = root / ".git"
    if git_entry.is_file():
        try:
            git_text = git_entry.read_text(encoding="utf-8", errors="ignore").strip()
            if git_text.lower().startswith("gitdir:"):
                gitdir = Path(git_text.split(":", 1)[1].strip())
                if not gitdir.is_absolute():
                    gitdir = (root / gitdir).resolve()
                # Worktree gitdir is <main_repo>/.git/worktrees/<worktree_name>
                if gitdir.parent.name == "worktrees" and gitdir.parent.parent.name == ".git":
                    main_repo = gitdir.parent.parent.parent.resolve()
                    if main_repo not in search_roots:
                        search_roots.append(main_repo)
        except OSError:
            pass

    for candidate_root in search_roots:
        built_candidates: list[Path] = []
        for profile in ("release", "debug"):
            for name in exe_names:
                candidate = candidate_root / "target" / profile / name
                if candidate.is_file():
                    built_candidates.append(candidate.resolve())
                    break
        if built_candidates:
            built_candidates.sort(key=lambda p: p.stat().st_mtime, reverse=True)
            return built_candidates[0]

    which_path = shutil.which("cad-cli")
    if which_path:
        return Path(which_path).resolve()
    return None


def check_watertight(metrics: CompileMetrics | None) -> tuple[bool, str]:
    """Verify L1 watertight manifold requirement."""
    if metrics is None:
        return (False, "Missing geometric metrics from compiler")
    if not metrics.watertight:
        return (False, "B-Rep shape is not watertight (open shell detected)")
    if metrics.face_count <= 0 or metrics.edge_count <= 0:
        return (
            False,
            f"Degenerate topology: face_count={metrics.face_count}, edge_count={metrics.edge_count}",
        )
    return (True, f"Watertight solid ({metrics.face_count} faces, {metrics.edge_count} edges)")


def check_non_zero_volume(
    metrics: CompileMetrics | None,
    min_volume_mm3: float = 1e-6,
) -> tuple[bool, str]:
    """Verify compiled solid has strictly positive volume."""
    if metrics is None:
        return (False, "Missing geometric metrics from compiler")
    if metrics.volume_mm3 <= min_volume_mm3:
        return (False, f"Solid volume is zero or degenerate: {metrics.volume_mm3:.6f} mm^3")
    return (True, f"Volume={metrics.volume_mm3:.3f} mm^3")


def check_keep_in_bbox(
    metrics: CompileMetrics | None,
    keep_in_bbox: BBox3D,
    tolerance: float = 1e-5,
) -> tuple[bool, list[str]]:
    """Verify compiled geometry fits inside the contract's Keep-In envelope."""
    if metrics is None:
        return (False, ["Missing geometric metrics for Keep-In bounding box check"])
    return keep_in_bbox.contains(metrics.bbox_mm, tolerance=tolerance)


Vec3 = tuple[float, float, float]


def _sub(a: Vec3, b: Vec3) -> Vec3:
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def _cross(a: Vec3, b: Vec3) -> Vec3:
    return (
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    )


def _dot(a: Vec3, b: Vec3) -> float:
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def _triangle_intersects_aabb(
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    box_min: Vec3,
    box_max: Vec3,
) -> bool:
    """Exact Separating Axis Theorem (SAT) triangle-AABB overlap test."""
    cx = (box_min[0] + box_max[0]) * 0.5
    cy = (box_min[1] + box_max[1]) * 0.5
    cz = (box_min[2] + box_max[2]) * 0.5
    hx = (box_max[0] - box_min[0]) * 0.5
    hy = (box_max[1] - box_min[1]) * 0.5
    hz = (box_max[2] - box_min[2]) * 0.5

    p0 = (v0[0] - cx, v0[1] - cy, v0[2] - cz)
    p1 = (v1[0] - cx, v1[1] - cy, v1[2] - cz)
    p2 = (v2[0] - cx, v2[1] - cy, v2[2] - cz)

    # 1. Box face normals (X, Y, Z)
    if max(p0[0], p1[0], p2[0]) < -hx or min(p0[0], p1[0], p2[0]) > hx:
        return False
    if max(p0[1], p1[1], p2[1]) < -hy or min(p0[1], p1[1], p2[1]) > hy:
        return False
    if max(p0[2], p1[2], p2[2]) < -hz or min(p0[2], p1[2], p2[2]) > hz:
        return False

    e0 = _sub(p1, p0)
    e1 = _sub(p2, p1)
    e2 = _sub(p0, p2)

    # 2. 9 cross-product axes (edge_i x box_axis_j)
    for edge in (e0, e1, e2):
        ex, ey, ez = edge
        # Axis (0, -ez, ey)
        a0 = p0[1] * (-ez) + p0[2] * ey
        a1 = p1[1] * (-ez) + p1[2] * ey
        a2 = p2[1] * (-ez) + p2[2] * ey
        r = hy * abs(ez) + hz * abs(ey)
        if min(a0, a1, a2) > r or max(a0, a1, a2) < -r:
            return False

        # Axis (ez, 0, -ex)
        a0 = p0[0] * ez + p0[2] * (-ex)
        a1 = p1[0] * ez + p1[2] * (-ex)
        a2 = p2[0] * ez + p2[2] * (-ex)
        r = hx * abs(ez) + hz * abs(ex)
        if min(a0, a1, a2) > r or max(a0, a1, a2) < -r:
            return False

        # Axis (-ey, ex, 0)
        a0 = p0[0] * (-ey) + p0[1] * ex
        a1 = p1[0] * (-ey) + p1[1] * ex
        a2 = p2[0] * (-ey) + p2[1] * ex
        r = hx * abs(ey) + hy * abs(ex)
        if min(a0, a1, a2) > r or max(a0, a1, a2) < -r:
            return False

    # 3. Triangle plane normal
    normal = _cross(e0, e1)
    d = _dot(normal, p0)
    r_plane = hx * abs(normal[0]) + hy * abs(normal[1]) + hz * abs(normal[2])
    if abs(d) > r_plane:
        return False

    return True


def _extract_mesh_triangles(mesh_data: dict[str, Any]) -> list[tuple[Vec3, Vec3, Vec3]]:
    triangles: list[tuple[Vec3, Vec3, Vec3]] = []
    for face in mesh_data.get("faces", []):
        verts = face.get("vertices", [])
        indices = face.get("indices", [])
        if not verts or not indices:
            continue
        for i in range(0, len(indices) - 2, 3):
            i0, i1, i2 = int(indices[i]), int(indices[i + 1]), int(indices[i + 2])
            if max(i0, i1, i2) * 3 + 2 >= len(verts):
                continue
            v0 = (float(verts[i0 * 3]), float(verts[i0 * 3 + 1]), float(verts[i0 * 3 + 2]))
            v1 = (float(verts[i1 * 3]), float(verts[i1 * 3 + 1]), float(verts[i1 * 3 + 2]))
            v2 = (float(verts[i2 * 3]), float(verts[i2 * 3 + 1]), float(verts[i2 * 3 + 2]))
            triangles.append((v0, v1, v2))
    return triangles


def _point_inside_closed_mesh(point: Vec3, triangles: list[tuple[Vec3, Vec3, Vec3]]) -> bool:
    """Moller-Trumbore ray-casting parity test along (+1, 0.137, 0.271) to avoid edge hits."""
    if not triangles:
        return False
    px, py, pz = point
    dx, dy, dz = (1.0, 0.13719, 0.27183)
    hits = 0
    eps = 1e-8
    for v0, v1, v2 in triangles:
        e1 = _sub(v1, v0)
        e2 = _sub(v2, v0)
        pvec = _cross((dx, dy, dz), e2)
        det = _dot(e1, pvec)
        if abs(det) < eps:
            continue
        inv_det = 1.0 / det
        tvec = (px - v0[0], py - v0[1], pz - v0[2])
        u = _dot(tvec, pvec) * inv_det
        if u < 0.0 or u > 1.0:
            continue
        qvec = _cross(tvec, e1)
        v = _dot((dx, dy, dz), qvec) * inv_det
        if v < 0.0 or u + v > 1.0:
            continue
        t = _dot(e2, qvec) * inv_det
        if t > eps:
            hits += 1
    return (hits % 2) == 1


def _point_segment_distance_sq(point: Vec3, start: Vec3, end: Vec3) -> float:
    """Return the squared distance from a point to a finite 3D segment."""
    edge = _sub(end, start)
    length_sq = _dot(edge, edge)
    if length_sq == 0.0:
        delta = _sub(point, start)
        return _dot(delta, delta)
    t = _dot(_sub(point, start), edge) / length_sq
    t = max(0.0, min(1.0, t))
    closest = (
        start[0] + edge[0] * t,
        start[1] + edge[1] * t,
        start[2] + edge[2] * t,
    )
    delta = _sub(point, closest)
    return _dot(delta, delta)


def _point_triangle_distance_sq(point: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> float:
    """Closest point distance for a 3D triangle using Ericson's region tests."""
    edge01 = _sub(v1, v0)
    edge02 = _sub(v2, v0)
    normal = _cross(edge01, edge02)
    if _dot(normal, normal) == 0.0:
        return min(
            _point_segment_distance_sq(point, v0, v1),
            _point_segment_distance_sq(point, v1, v2),
            _point_segment_distance_sq(point, v2, v0),
        )

    to_point = _sub(point, v0)
    d1 = _dot(edge01, to_point)
    d2 = _dot(edge02, to_point)
    if d1 <= 0.0 and d2 <= 0.0:
        delta = _sub(point, v0)
        return _dot(delta, delta)

    to_v1 = _sub(point, v1)
    d3 = _dot(edge01, to_v1)
    d4 = _dot(edge02, to_v1)
    if d3 >= 0.0 and d4 <= d3:
        delta = _sub(point, v1)
        return _dot(delta, delta)

    vc = d1 * d4 - d3 * d2
    if vc <= 0.0 and d1 >= 0.0 and d3 <= 0.0:
        t = d1 / (d1 - d3)
        closest = (v0[0] + edge01[0] * t, v0[1] + edge01[1] * t, v0[2] + edge01[2] * t)
        delta = _sub(point, closest)
        return _dot(delta, delta)

    to_v2 = _sub(point, v2)
    d5 = _dot(edge01, to_v2)
    d6 = _dot(edge02, to_v2)
    if d6 >= 0.0 and d5 <= d6:
        delta = _sub(point, v2)
        return _dot(delta, delta)

    vb = d5 * d2 - d1 * d6
    if vb <= 0.0 and d2 >= 0.0 and d6 <= 0.0:
        t = d2 / (d2 - d6)
        closest = (v0[0] + edge02[0] * t, v0[1] + edge02[1] * t, v0[2] + edge02[2] * t)
        delta = _sub(point, closest)
        return _dot(delta, delta)

    va = d3 * d6 - d5 * d4
    if va <= 0.0 and (d4 - d3) >= 0.0 and (d5 - d6) >= 0.0:
        edge12 = _sub(v2, v1)
        t = (d4 - d3) / ((d4 - d3) + (d5 - d6))
        closest = (v1[0] + edge12[0] * t, v1[1] + edge12[1] * t, v1[2] + edge12[2] * t)
        delta = _sub(point, closest)
        return _dot(delta, delta)

    barycentric_sum = va + vb + vc
    if barycentric_sum == 0.0:
        return min(
            _point_segment_distance_sq(point, v0, v1),
            _point_segment_distance_sq(point, v1, v2),
            _point_segment_distance_sq(point, v2, v0),
        )
    denominator = 1.0 / barycentric_sum
    v = vb * denominator
    w = vc * denominator
    closest = (
        v0[0] + edge01[0] * v + edge02[0] * w,
        v0[1] + edge01[1] * v + edge02[1] * w,
        v0[2] + edge01[2] * v + edge02[2] * w,
    )
    delta = _sub(point, closest)
    return _dot(delta, delta)


def _cylinder_local_point(point: Vec3, origin: Vec3, axis: str) -> Vec3:
    """Map a world point to the cylinder's local (u, v, axial) frame."""
    dx, dy, dz = _sub(point, origin)
    direction = -1.0 if axis.startswith("-") else 1.0
    cardinal = axis.upper().lstrip("+-")
    if cardinal.startswith("X"):
        return (dy, dz, direction * dx)
    if cardinal.startswith("Y"):
        return (dx, dz, direction * dy)
    return (dx, dy, direction * dz)


def _clip_polygon_axis(
    polygon: list[Vec3], value: float, *, keep_above: bool,
) -> list[Vec3]:
    """Clip a 3D polygon against one axial half-space."""
    if not polygon:
        return []
    result: list[Vec3] = []
    previous = polygon[-1]
    previous_inside = previous[2] >= value if keep_above else previous[2] <= value
    for current in polygon:
        current_inside = current[2] >= value if keep_above else current[2] <= value
        if current_inside != previous_inside:
            t = (value - previous[2]) / (current[2] - previous[2])
            result.append(
                (
                    previous[0] + (current[0] - previous[0]) * t,
                    previous[1] + (current[1] - previous[1]) * t,
                    value,
                )
            )
        if current_inside:
            result.append(current)
        previous = current
        previous_inside = current_inside
    return result


def _origin_inside_convex_polygon(polygon: list[Vec3]) -> bool:
    """Check origin containment in a convex polygon, including collinear edges."""
    signs: list[bool] = []
    for index, current in enumerate(polygon):
        previous = polygon[index - 1]
        cross_z = previous[0] * current[1] - previous[1] * current[0]
        if cross_z != 0.0:
            signs.append(cross_z > 0.0)
    return bool(signs) and all(sign == signs[0] for sign in signs)


def _triangle_intersects_sphere(
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    *,
    center: Vec3,
    radius_sq: float,
) -> bool:
    return _point_triangle_distance_sq(center, v0, v1, v2) < radius_sq


def _triangle_intersects_cylinder(
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    *,
    origin: Vec3,
    axis: str,
    radius_sq: float,
    height: float,
) -> bool:
    local = [_cylinder_local_point(vertex, origin, axis) for vertex in (v0, v1, v2)]
    axial_values = [point[2] for point in local]
    if max(axial_values) <= 0.0 or min(axial_values) >= height:
        return False
    # With open-slab overlap and a strictly interior radial distance, a
    # closest point on a cap also implies an intersection just inside the cap.
    clipped = _clip_polygon_axis(local, 0.0, keep_above=True)
    clipped = _clip_polygon_axis(clipped, height, keep_above=False)
    if not clipped:
        return False
    projected = [(u, v, 0.0) for u, v, _ in clipped]
    if _origin_inside_convex_polygon(projected):
        return True
    distance_sq = min(
        _point_segment_distance_sq((0.0, 0.0, 0.0), projected[index - 1], point)
        for index, point in enumerate(projected)
    )
    return distance_sq < radius_sq


def _keep_out_cylinder_height(zone: KeepOutZone) -> float | None:
    if zone.kind == "cylinder" and zone.origin_mm is not None and zone.diameter_mm is not None:
        return zone.height_mm
    return None


def _mesh_intersects_keep_out_zone(
    triangles: list[tuple[Vec3, Vec3, Vec3]],
    zone: KeepOutZone,
    tolerance: float,
) -> bool:
    zone_bbox = zone.resolved_bbox_mm()
    origin = zone.origin_mm
    diameter = zone.diameter_mm
    height = _keep_out_cylinder_height(zone)
    intersects: Callable[[Vec3, Vec3, Vec3], bool] | None = None
    if origin is not None and diameter is not None and (zone.kind == "sphere" or height is not None):
        radius = diameter * 0.5 - tolerance
        if radius <= 0.0 or (height is not None and height <= 0.0):
            return False
        center = (float(origin[0]), float(origin[1]), float(origin[2]))
        radius_sq = radius * radius
        if height is None:
            intersects = partial(_triangle_intersects_sphere, center=center, radius_sq=radius_sq)
        else:
            intersects = partial(
                _triangle_intersects_cylinder,
                origin=center,
                axis=zone.axis,
                radius_sq=radius_sq,
                height=height,
            )
    else:
        # Box zones and legacy zones specified only by bounds use the inset box.
        zone_bbox = zone_bbox.shrunk(tolerance)

    bmin = (zone_bbox.min[0], zone_bbox.min[1], zone_bbox.min[2])
    bmax = (zone_bbox.max[0], zone_bbox.max[1], zone_bbox.max[2])
    for v0, v1, v2 in triangles:
        if _triangle_intersects_aabb(v0, v1, v2, bmin, bmax):
            if intersects is None or intersects(v0, v1, v2):
                return True

    # Catch cases where the Keep-Out zone is completely enclosed inside solid material.
    return _point_inside_closed_mesh(zone_bbox.center, triangles)


def check_keep_out_zones(
    metrics: CompileMetrics | None,
    keep_out_zones: list[KeepOutZone],
    tolerance: float = 1e-5,
    *,
    mesh_path: str | Path | None = None,
    mesh_data: dict[str, Any] | None = None,
) -> tuple[bool, list[str]]:
    """Broad-phase AABB + narrow-phase mesh intersection check against Keep-Out zones."""
    if not keep_out_zones:
        return (True, [])
    if metrics is None:
        return (False, ["Missing geometric metrics for Keep-Out zone check"])

    loaded_mesh = mesh_data
    if loaded_mesh is None and mesh_path is not None:
        mp = Path(mesh_path)
        if mp.is_file():
            try:
                loaded_mesh = json.loads(mp.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                loaded_mesh = None

    triangles: list[tuple[Vec3, Vec3, Vec3]] | None = None
    if isinstance(loaded_mesh, dict) and loaded_mesh.get("faces"):
        extracted = _extract_mesh_triangles(loaded_mesh)
        if extracted:
            triangles = extracted

    violations: list[str] = []
    for zone in keep_out_zones:
        zone_bbox = zone.resolved_bbox_mm()
        # Broad-phase AABB screening
        # An exact cylinder uses tolerance only in the radial direction.
        exact_cylinder = triangles is not None and _keep_out_cylinder_height(zone) is not None
        broadphase_tolerance = 0.0 if exact_cylinder else tolerance
        if not metrics.bbox_mm.intersects(zone_bbox, tolerance=broadphase_tolerance):
            continue
        # Narrow-phase exact mesh interference check when mesh triangles are available
        if triangles is not None:
            if _mesh_intersects_keep_out_zone(triangles, zone, tolerance):
                violations.append(
                    f"Solid geometry intersects Keep-Out zone '{zone.zone_id}' "
                    f"({zone.description or 'clearance'})"
                )
        else:
            violations.append(
                f"Solid bounding box intersects Keep-Out zone '{zone.zone_id}' "
                f"({zone.description or 'clearance'}; no scene-mesh for narrow-phase check)"
            )
    return (len(violations) == 0, violations)


def check_external_envelope(
    metrics: CompileMetrics | None,
    bbox: BBox3D,
    mesh_data: dict[str, Any] | None,
    tolerance: float,
) -> tuple[bool, str]:
    """Phase A containment screen, not exact interference or minimum clearance."""
    if metrics is None:
        return False, "External envelope could not be checked: missing compiled bounds"
    if not metrics.bbox_mm.contains(bbox, tolerance=tolerance)[0]:
        return True, "External envelope not fully enclosed (partial overlaps are not checked)"
    if not mesh_data or not mesh_data.get("faces"):
        return False, "Generated bounds enclose the external AABB; missing mesh evidence of clearance"

    if min(bbox.dimensions) <= tolerance * 2:
        return False, "Generated bounds enclose a degenerate external AABB; clearance cannot be verified"
    envelope = KeepOutZone(zone_id="external_asset", bbox_mm=bbox)
    try:
        # Separate components so overlapping solid shells cannot cancel each other's inside parity.
        faces_by_component: dict[str | None, list[dict[str, Any]]] = {}
        for face in mesh_data["faces"]:
            if not isinstance(face, dict):
                raise ValueError("invalid face")
            faces_by_component.setdefault(face.get("component_id"), []).append(face)
        for faces in faces_by_component.values():
            triangles = _extract_mesh_triangles({"faces": faces})
            if not triangles:
                return False, "Generated bounds enclose the external AABB; missing mesh evidence of clearance"
            if _mesh_intersects_keep_out_zone(triangles, envelope, tolerance):
                return False, "Generated bounds enclose the external AABB and the compiled mesh occupies it"
    except (IndexError, TypeError, ValueError):
        return False, "Generated bounds enclose the external AABB; invalid clearance mesh"
    return True, "Compiled mesh shows no occupancy inside the enclosed external AABB (Phase A approximation)"


SubprocessRunner = Callable[..., subprocess.CompletedProcess[str]]


class CompilerBridge:
    """Subprocess wrapper around `cad-cli check` and `cad-cli build`."""

    def __init__(
        self,
        cli_bin: str | Path | None = None,
        *,
        workspace_root: Path | None = None,
        timeout_sec: float = 30.0,
        runner: SubprocessRunner | None = None,
    ) -> None:
        self.repo_root = find_workspace_root()
        self._asset_root = Path(workspace_root).resolve() if workspace_root is not None else None
        if self._asset_root is not None and not self._asset_root.is_dir():
            raise CompilerBridgeError("--workspace-root must be an existing directory")
        self.workspace_root = self._asset_root or self.repo_root
        self._explicit_bin = (
            Path(cli_bin).resolve()
            if cli_bin is not None and (os.sep in str(cli_bin) or "/" in str(cli_bin) or Path(cli_bin).exists())
            else (Path(cli_bin) if cli_bin is not None else None)
        )
        self.timeout_sec = timeout_sec
        self._runner = runner or subprocess.run

    def resolve_binary(self) -> Path:
        resolved = discover_cad_cli(self._explicit_bin, self.repo_root)
        if resolved is None:
            raise CompilerBridgeError(
                "Could not locate `cad-cli`. Build it with `cargo build -p cad-cli` "
                "or set CAD_CLI_BIN."
            )
        return resolved

    def _workspace_args(self) -> list[str]:
        return ["--workspace-root", str(self._asset_root)] if self._asset_root is not None else []

    def inspect_external_asset(
        self, asset_path: str | Path, *, source_unit: str | None = None,
    ) -> ExternalInterfaceSummary:
        """Inspect a file under an explicit trusted root; never use the caller's CWD as that root."""
        if self._asset_root is None:
            raise CompilerBridgeError("External inspection requires an explicit --workspace-root")
        if source_unit is not None and source_unit not in EXTERNAL_LENGTH_UNITS:
            raise CompilerBridgeError("Unsupported external asset source unit")
        relative_or_abs = Path(asset_path)
        if ".." in relative_or_abs.parts or (relative_or_abs.anchor and not relative_or_abs.is_absolute()):
            raise CompilerBridgeError("Context asset must stay within --workspace-root")
        try:
            path = (self._asset_root / relative_or_abs).resolve(strict=True)
        except (OSError, RuntimeError, ValueError) as exc:
            raise CompilerBridgeError("Cannot resolve context asset within --workspace-root") from exc
        if not path.is_relative_to(self._asset_root):
            raise CompilerBridgeError("Context asset must stay within --workspace-root")
        if not path.is_file():
            raise CompilerBridgeError("Context asset must be a file")
        # The CLI resolves again against the explicit root while opening the asset.
        relative_path = path.relative_to(self._asset_root).as_posix()
        cmd = [str(self.resolve_binary()), "inspect", "--json", *self._workspace_args()]
        if source_unit is not None:
            cmd.extend(["--source-unit", source_unit])
        cmd.extend(["--", relative_path])
        payload, proc = self._run_json(cmd, subject=path)
        if proc.returncode != 0 or payload.get("status") != "ok":
            raise CompilerBridgeError(f"cad-cli inspect failed (exit={proc.returncode}); check the asset format, geometry and CLI features")
        try:
            summary = ExternalInterfaceSummary.from_cli_json(payload)
        except ContractValidationError as exc:
            raise CompilerBridgeError(str(exc)) from exc
        if payload["format"] in ("STEP", "BREP", "IGES"):
            summary.exact_asset_path = relative_path
            summary.exact_asset_sha256 = payload["sha256"]
        return summary

    def check_external_interference(
        self, kcl_file: Path, context: ExternalInterfaceSummary,
    ) -> CompileResult:
        if self._asset_root is None or context.exact_asset_path is None or context.exact_asset_sha256 is None:
            raise CompilerBridgeError("Exact interference requires an inspected asset and a workspace root")
        kcl_path = kcl_file.resolve()
        cmd = [
            str(self.resolve_binary()), "check", str(kcl_path), "--json", *self._workspace_args(),
            "--against-asset", context.exact_asset_path, "--against-sha256", context.exact_asset_sha256,
        ]
        return self._invoke_json(cmd, kcl_path=kcl_path)

    def check(self, kcl_file: str | Path) -> CompileResult:
        """Execute `cad-cli check <file> --json` and parse diagnostics + metrics."""
        kcl_path = Path(kcl_file).resolve()
        cli_path = self.resolve_binary()
        cmd = [str(cli_path), "check", str(kcl_path), "--json", *self._workspace_args()]
        return self._invoke_json(cmd, kcl_path=kcl_path)

    def build(
        self,
        kcl_file: str | Path,
        *,
        out_step: str | Path | None = None,
        out_mesh: str | Path | None = None,
        out_views: str | Path | None = None,
        out_dxf: str | Path | None = None,
        out_drawing: str | Path | None = None,
        component: str | None = None,
    ) -> CompileResult:
        """Execute `cad-cli build <file> ... --json` and parse outputs + metrics."""
        kcl_path = Path(kcl_file).resolve()
        cli_path = self.resolve_binary()
        cmd: list[str] = [str(cli_path), "build", str(kcl_path)]

        step_str = str(Path(out_step).resolve()) if out_step is not None else None
        mesh_str = str(Path(out_mesh).resolve()) if out_mesh is not None else None
        views_str = str(Path(out_views).resolve()) if out_views is not None else None
        dxf_str = str(Path(out_dxf).resolve()) if out_dxf is not None else None
        drawing_str = str(Path(out_drawing).resolve()) if out_drawing is not None else None

        if step_str is not None:
            Path(step_str).parent.mkdir(parents=True, exist_ok=True)
            cmd.extend(["--out-step", step_str])
        if mesh_str is not None:
            Path(mesh_str).parent.mkdir(parents=True, exist_ok=True)
            cmd.extend(["--out-mesh", mesh_str])
        if views_str is not None:
            Path(views_str).mkdir(parents=True, exist_ok=True)
            cmd.extend(["--out-views", views_str])
        if dxf_str is not None:
            Path(dxf_str).parent.mkdir(parents=True, exist_ok=True)
            cmd.extend(["--out-dxf", dxf_str])
        if drawing_str is not None:
            Path(drawing_str).parent.mkdir(parents=True, exist_ok=True)
            cmd.extend(["--out-drawing", drawing_str])
        if component:
            cmd.extend(["--component", component])
        cmd.extend(["--json", *self._workspace_args()])

        return self._invoke_json(
            cmd,
            kcl_path=kcl_path,
            step_path=step_str,
            mesh_path=mesh_str,
            views_dir=views_str,
            dxf_path=dxf_str,
            drawing_path=drawing_str,
        )

    def render_shaded_views(
        self,
        kcl_file: str | Path,
        *,
        emit_semantic_mask: bool = True,
    ) -> dict[str, bytes] | None:
        """Try a separate, disposable wgpu build (with semantic masks) for multimodal review."""
        try:
            kcl_path = Path(kcl_file).resolve()
            binary = str(self.resolve_binary())
            workspace_args = self._workspace_args()
            with tempfile.TemporaryDirectory(prefix="cad_agent_render_") as render_dir:
                cmd = [
                    binary, "build", str(kcl_path),
                    "--out-render", render_dir,
                    *(["--emit-semantic-mask"] if emit_semantic_mask else []),
                    "--json",
                    *workspace_args,
                ]
                try:
                    payload, proc = self._run_json(cmd, subject=kcl_path)
                except CompilerBridgeError as exc:
                    msg = str(exc).lower()
                    if emit_semantic_mask and "semantic" in msg and "--out-render" not in msg:
                        return self._render_shaded_views_without_mask(
                            binary, kcl_path, workspace_args,
                        )
                    return None
                if proc.returncode != 0 or payload.get("status") != "ok":
                    diag_blob = (
                        json.dumps(payload, ensure_ascii=False).lower()
                        + " "
                        + (proc.stderr or "").lower()
                    )
                    if emit_semantic_mask and "semantic" in diag_blob and "--out-render" not in diag_blob:
                        return self._render_shaded_views_without_mask(
                            binary, kcl_path, workspace_args,
                        )
                    return None
                return load_shaded_views_with_masks(Path(render_dir))
        except (CompilerBridgeError, OSError):
            return None

    def _render_shaded_views_without_mask(
        self,
        binary: str,
        kcl_path: Path,
        workspace_args: list[str],
    ) -> dict[str, bytes] | None:
        with tempfile.TemporaryDirectory(prefix="cad_agent_render_nomask_") as fallback_dir:
            cmd = [
                binary, "build", str(kcl_path),
                "--out-render", fallback_dir,
                "--json",
                *workspace_args,
            ]
            payload, proc = self._run_json(cmd, subject=kcl_path)
            if proc.returncode != 0 or payload.get("status") != "ok":
                return None
            return load_shaded_views_with_masks(Path(fallback_dir))

    def _invoke_json(
        self,
        cmd: Sequence[str],
        *,
        kcl_path: Path,
        step_path: str | None = None,
        mesh_path: str | None = None,
        views_dir: str | None = None,
        dxf_path: str | None = None,
        drawing_path: str | None = None,
    ) -> CompileResult:
        payload, proc = self._run_json(cmd, subject=kcl_path)
        return CompileResult.from_cli_json(
            payload,
            step_path=step_path,
            mesh_path=mesh_path,
            views_dir=views_dir,
            dxf_path=dxf_path,
            drawing_path=drawing_path,
            exit_code=proc.returncode,
            raw_stderr=(proc.stderr or "").strip(),
        )

    def _run_json(
        self, cmd: Sequence[str], *, subject: Path,
    ) -> tuple[dict[str, Any], subprocess.CompletedProcess[str]]:
        try:
            proc = self._runner(
                list(cmd),
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=self.timeout_sec,
                check=False,
                cwd=str(self.repo_root),
                env=observe.subprocess_env(),
            )
        except OSError as exc:
            raise CompilerBridgeError(f"Failed to launch cad-cli ({cmd[0]}): {exc}") from exc
        except subprocess.TimeoutExpired as exc:
            raise CompilerBridgeError(
                f"cad-cli timed out after {self.timeout_sec}s on {subject}"
            ) from exc
        except UnicodeError as exc:
            raise CompilerBridgeError("cad-cli output was not valid UTF-8") from exc

        stdout = (proc.stdout or "").strip()
        stderr = (proc.stderr or "").strip()
        if not stdout:
            raise CompilerBridgeError(
                f"cad-cli exited with code {proc.returncode} and produced empty stdout. "
                f"stderr={stderr!r}"
            )

        try:
            payload = json.loads(stdout)
        except json.JSONDecodeError as exc:
            raise CompilerBridgeError(
                f"cad-cli returned non-JSON stdout (exit={proc.returncode}): {exc}. "
                f"stdout={stdout[:300]!r}"
            ) from exc

        if not isinstance(payload, dict) or "status" not in payload:
            raise CompilerBridgeError(f"Unexpected cad-cli JSON shape: {payload!r}")

        return payload, proc

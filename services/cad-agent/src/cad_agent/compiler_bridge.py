"""Subprocess bridge to the headless `cad-cli` Rust compiler and geometry checks."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any, Callable, Sequence

from .contracts import (
    BBox3D,
    CompileMetrics,
    CompileResult,
    KeepOutZone,
)


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
    built_candidates: list[Path] = []
    for profile in ("release", "debug"):
        for name in exe_names:
            candidate = root / "target" / profile / name
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


def _point_in_zone(point: Vec3, zone: KeepOutZone, tolerance: float) -> bool:
    bbox = zone.resolved_bbox_mm().shrunk(tolerance)
    px, py, pz = point
    if not (bbox.min[0] <= px <= bbox.max[0] and bbox.min[1] <= py <= bbox.max[1] and bbox.min[2] <= pz <= bbox.max[2]):
        return False
    if zone.kind == "cylinder" and zone.origin_mm is not None and zone.diameter_mm is not None:
        r = max(0.0, zone.diameter_mm * 0.5 - tolerance)
        ox, oy, oz = zone.origin_mm
        ax = zone.axis.upper().lstrip("+-")
        if ax.startswith("X"):
            dist2 = (py - oy) ** 2 + (pz - oz) ** 2
        elif ax.startswith("Y"):
            dist2 = (px - ox) ** 2 + (pz - oz) ** 2
        else:
            dist2 = (px - ox) ** 2 + (py - oy) ** 2
        return dist2 <= r * r
    if zone.kind == "sphere" and zone.origin_mm is not None and zone.diameter_mm is not None:
        r = max(0.0, zone.diameter_mm * 0.5 - tolerance)
        ox, oy, oz = zone.origin_mm
        return ((px - ox) ** 2 + (py - oy) ** 2 + (pz - oz) ** 2) <= r * r
    return True


def _mesh_intersects_keep_out_zone(
    triangles: list[tuple[Vec3, Vec3, Vec3]],
    zone: KeepOutZone,
    tolerance: float,
) -> bool:
    shrunk = zone.resolved_bbox_mm().shrunk(tolerance)
    bmin: Vec3 = (shrunk.min[0], shrunk.min[1], shrunk.min[2])
    bmax: Vec3 = (shrunk.max[0], shrunk.max[1], shrunk.max[2])

    for v0, v1, v2 in triangles:
        if not _triangle_intersects_aabb(v0, v1, v2, bmin, bmax):
            continue
        if zone.kind == "box":
            return True
        # For cylinder/sphere, verify triangle vertices or centroid lie inside exact analytic shape
        centroid = (
            (v0[0] + v1[0] + v2[0]) / 3.0,
            (v0[1] + v1[1] + v2[1]) / 3.0,
            (v0[2] + v1[2] + v2[2]) / 3.0,
        )
        if any(_point_in_zone(p, zone, tolerance) for p in (v0, v1, v2, centroid)):
            return True
        # Also sample edge midpoints for large triangles spanning across a cylinder
        m01 = ((v0[0] + v1[0]) * 0.5, (v0[1] + v1[1]) * 0.5, (v0[2] + v1[2]) * 0.5)
        m12 = ((v1[0] + v2[0]) * 0.5, (v1[1] + v2[1]) * 0.5, (v1[2] + v2[2]) * 0.5)
        m20 = ((v2[0] + v0[0]) * 0.5, (v2[1] + v0[1]) * 0.5, (v2[2] + v0[2]) * 0.5)
        if any(_point_in_zone(p, zone, tolerance) for p in (m01, m12, m20)):
            return True

    # Catch cases where the Keep-Out zone is completely enclosed inside solid material
    return _point_inside_closed_mesh(shrunk.center, triangles)


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
        if not metrics.bbox_mm.intersects(zone_bbox, tolerance=tolerance):
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
        self.workspace_root = (
            Path(workspace_root).resolve() if workspace_root is not None else find_workspace_root()
        )
        self._explicit_bin = (
            Path(cli_bin).resolve()
            if cli_bin is not None and (os.sep in str(cli_bin) or "/" in str(cli_bin) or Path(cli_bin).exists())
            else (Path(cli_bin) if cli_bin is not None else None)
        )
        self.timeout_sec = timeout_sec
        self._runner = runner or subprocess.run

    def resolve_binary(self) -> Path:
        resolved = discover_cad_cli(self._explicit_bin, self.workspace_root)
        if resolved is None:
            raise CompilerBridgeError(
                "Could not locate `cad-cli`. Build it with `cargo build -p cad-cli` "
                "or set CAD_CLI_BIN."
            )
        return resolved

    def check(self, kcl_file: str | Path) -> CompileResult:
        """Execute `cad-cli check <file> --json` and parse diagnostics + metrics."""
        kcl_path = Path(kcl_file).resolve()
        cli_path = self.resolve_binary()
        cmd = [str(cli_path), "check", str(kcl_path), "--json"]
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
        cmd.append("--json")

        return self._invoke_json(
            cmd,
            kcl_path=kcl_path,
            step_path=step_str,
            mesh_path=mesh_str,
            views_dir=views_str,
            dxf_path=dxf_str,
            drawing_path=drawing_str,
        )

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
        try:
            proc = self._runner(
                list(cmd),
                capture_output=True,
                text=True,
                timeout=self.timeout_sec,
                check=False,
                cwd=str(self.workspace_root),
            )
        except OSError as exc:
            raise CompilerBridgeError(f"Failed to launch cad-cli ({cmd[0]}): {exc}") from exc
        except subprocess.TimeoutExpired as exc:
            raise CompilerBridgeError(
                f"cad-cli timed out after {self.timeout_sec}s on {kcl_path}"
            ) from exc

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

        return CompileResult.from_cli_json(
            payload,
            step_path=step_path,
            mesh_path=mesh_path,
            views_dir=views_dir,
            dxf_path=dxf_path,
            drawing_path=drawing_path,
            exit_code=proc.returncode,
            raw_stderr=stderr,
        )

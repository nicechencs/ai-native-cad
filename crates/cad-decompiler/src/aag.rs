//! Kernel-agnostic Attributed Adjacency Graph (AAG) extracted from `&SolidShape`.
//!
//! Works deterministically on both `AnalyticKernel` and `occt-ffi` native B-Rep solids,
//! recording per-face surface geometry (plane normal/origin, cylinder axis/origin/radius/sweep,
//! area, bounding box, loops) and pairwise edge dihedral/convexity classifications.

use std::collections::{BTreeMap, HashMap};

use cad_kernel_occt::{AnalyticKernel, BBox3, BRepFace, GeometryKernel, SolidShape, SurfaceType};
use serde::{Deserialize, Serialize};

use crate::{EdgeId, FaceId};

pub(crate) type Vec3 = [f64; 3];

const COORD_QUANT_MM: f64 = 1e-3;
const SMOOTH_DOT_THRESHOLD: f64 = 0.939_692_620_785_908_4; // cos(20 deg)

/// Surface type recorded on each AAG node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AagSurfaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    BSpline,
    Mesh,
    Other,
}

impl From<SurfaceType> for AagSurfaceKind {
    fn from(value: SurfaceType) -> Self {
        match value {
            SurfaceType::Plane => Self::Plane,
            SurfaceType::Cylinder => Self::Cylinder,
            SurfaceType::Cone => Self::Cone,
            SurfaceType::Sphere => Self::Sphere,
            SurfaceType::Torus => Self::Torus,
            SurfaceType::Bspline => Self::BSpline,
            SurfaceType::Mesh => Self::Mesh,
        }
    }
}

/// Dihedral convexity classification of an edge shared between two faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeConvexity {
    Convex,
    Concave,
    Smooth,
    NonManifold,
    Unknown,
}

/// Extracted geometric attributes for a planar face.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaneAttributes {
    pub normal: Vec3,
    pub origin: Vec3,
    pub offset: f64,
}

/// Extracted geometric attributes for a cylindrical face.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CylinderAttributes {
    /// Canonical unit axis vector.
    pub axis: Vec3,
    /// Point on the cylinder axis at the minimum axial bound (`axial_min`).
    pub origin: Vec3,
    /// Point on the cylinder axis at the maximum axial bound (`axial_max`).
    pub end_point: Vec3,
    pub radius_mm: f64,
    pub height_mm: f64,
    pub axial_min: f64,
    pub axial_max: f64,
    /// `true` when surface normals point radially inward (hole/bore wall).
    pub is_inward: bool,
    /// Estimated angular span in radians (`0 ..= 2*PI`).
    pub sweep_angle_rad: f64,
}

/// Face node in an [`AttributedAdjacencyGraph`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AagFace {
    pub id: FaceId,
    pub face_id: String,
    pub surface_kind: AagSurfaceKind,
    pub area_mm2: f64,
    pub bbox: BBox3,
    pub centroid: Vec3,
    pub plane: Option<PlaneAttributes>,
    pub cylinder: Option<CylinderAttributes>,
    pub outer_loop: Vec<Vec3>,
    pub inner_loops: Vec<Vec<Vec3>>,
    pub component_id: Option<String>,
}

/// Adjacency edge between two distinct faces in an [`AttributedAdjacencyGraph`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AagEdge {
    pub id: EdgeId,
    pub face_a: FaceId,
    pub face_b: FaceId,
    pub convexity: EdgeConvexity,
    pub dihedral_angle_deg: f64,
    pub midpoint: Vec3,
    pub length_mm: f64,
    pub is_circular: bool,
}

/// Kernel-agnostic Attributed Adjacency Graph for a single solid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributedAdjacencyGraph {
    pub part_name: String,
    pub solid_bbox: BBox3,
    pub solid_volume_mm3: f64,
    pub is_watertight: bool,
    pub faces: Vec<AagFace>,
    pub edges: Vec<AagEdge>,
    pub incident_edges: Vec<Vec<EdgeId>>,
}

impl AttributedAdjacencyGraph {
    /// Build an AAG directly from a [`SolidShape`] (Analytic, OCCT, or TriMesh).
    pub fn from_solid(solid: &SolidShape) -> Self {
        let check = AnalyticKernel::new().check_solid(solid);
        let solid_bbox = if check.bbox_mm.is_valid() {
            check.bbox_mm
        } else {
            solid.csg.bbox()
        };

        let faces: Vec<AagFace> = solid
            .faces
            .iter()
            .enumerate()
            .map(|(idx, face)| extract_face_attrs(FaceId(idx), face))
            .collect();

        let mut edges = build_geometric_edges(&faces, &solid.faces);

        // Enrich with native OCCT incidence when available and not already linked geometrically.
        #[cfg(feature = "occt-ffi")]
        if let Ok(native_fag) = crate::FaceAdjacencyGraph::from_occt(solid) {
            merge_native_incidences(&faces, &native_fag, &mut edges);
        }

        let mut incident_edges = vec![Vec::new(); faces.len()];
        for (idx, edge) in edges.iter_mut().enumerate() {
            edge.id = EdgeId(idx);
            if edge.face_a.0 < incident_edges.len() {
                incident_edges[edge.face_a.0].push(EdgeId(idx));
            }
            if edge.face_b.0 < incident_edges.len() && edge.face_b != edge.face_a {
                incident_edges[edge.face_b.0].push(EdgeId(idx));
            }
        }

        Self {
            part_name: solid.part_name.clone(),
            solid_bbox,
            solid_volume_mm3: check.volume_mm3.max(0.0),
            is_watertight: check.watertight,
            faces,
            edges,
            incident_edges,
        }
    }

    pub fn face(&self, id: FaceId) -> Option<&AagFace> {
        self.faces.get(id.0)
    }

    pub fn incident_edges(&self, id: FaceId) -> &[EdgeId] {
        self.incident_edges
            .get(id.0)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Return all `(neighbor_face_id, edge)` pairs adjacent to `id`.
    pub fn neighbors(&self, id: FaceId) -> Vec<(FaceId, &AagEdge)> {
        self.incident_edges(id)
            .iter()
            .filter_map(|eid| {
                let edge = self.edges.get(eid.0)?;
                let other = if edge.face_a == id {
                    edge.face_b
                } else {
                    edge.face_a
                };
                Some((other, edge))
            })
            .collect()
    }

    /// Return the edge(s) connecting two faces, if any.
    pub fn edges_between(&self, a: FaceId, b: FaceId) -> Vec<&AagEdge> {
        self.incident_edges(a)
            .iter()
            .filter_map(|eid| {
                let e = self.edges.get(eid.0)?;
                ((e.face_a == a && e.face_b == b) || (e.face_a == b && e.face_b == a)).then_some(e)
            })
            .collect()
    }
}

fn extract_face_attrs(id: FaceId, face: &BRepFace) -> AagFace {
    let surface_kind = AagSurfaceKind::from(face.surface_type);
    let bbox = compute_face_bbox(face);
    let (centroid, computed_area) = compute_face_centroid_and_area(face);
    let area_mm2 = if face.area_mm2.is_finite() && face.area_mm2 > 0.0 {
        face.area_mm2
    } else {
        computed_area
    };

    let plane = if surface_kind == AagSurfaceKind::Plane {
        extract_plane_attrs(face, centroid)
    } else {
        None
    };

    let cylinder = if surface_kind == AagSurfaceKind::Cylinder {
        extract_cylinder_attrs(face, area_mm2)
    } else {
        None
    };

    AagFace {
        id,
        face_id: face.face_id.clone(),
        surface_kind,
        area_mm2,
        bbox,
        centroid,
        plane,
        cylinder,
        outer_loop: face.outer_loop.clone(),
        inner_loops: face.inner_loops.clone(),
        component_id: face.component_id.clone(),
    }
}

fn compute_face_bbox(face: &BRepFace) -> BBox3 {
    let mut bb = BBox3::empty();
    for chunk in face.vertices.chunks_exact(3) {
        bb.expand_point([chunk[0], chunk[1], chunk[2]]);
    }
    for &p in &face.outer_loop {
        bb.expand_point(p);
    }
    if let Some(top) = &face.top_loop {
        for &p in top {
            bb.expand_point(p);
        }
    }
    for inner in &face.inner_loops {
        for &p in inner {
            bb.expand_point(p);
        }
    }
    if bb.is_valid() {
        bb
    } else {
        BBox3 {
            min: face.center_hint,
            max: face.center_hint,
        }
    }
}

fn compute_face_centroid_and_area(face: &BRepFace) -> (Vec3, f64) {
    let mut area_sum = 0.0;
    let mut weighted = [0.0; 3];
    for [a, b, c] in face_triangles(face) {
        let tri_area = 0.5 * norm(cross(sub(b, a), sub(c, a)));
        if tri_area > 0.0 && tri_area.is_finite() {
            let tri_c = scale(add(add(a, b), c), 1.0 / 3.0);
            area_sum += tri_area;
            for k in 0..3 {
                weighted[k] += tri_c[k] * tri_area;
            }
        }
    }
    if area_sum > 1e-12 {
        (scale(weighted, 1.0 / area_sum), area_sum)
    } else if !face.outer_loop.is_empty() {
        let n = face.outer_loop.len() as f64;
        let mut sum = [0.0; 3];
        for p in &face.outer_loop {
            for k in 0..3 {
                sum[k] += p[k];
            }
        }
        (scale(sum, 1.0 / n), face.area_mm2.max(0.0))
    } else {
        (face.center_hint, face.area_mm2.max(0.0))
    }
}

fn extract_plane_attrs(face: &BRepFace, centroid: Vec3) -> Option<PlaneAttributes> {
    let mut normal = unit(face.normal_hint);
    // Cross-check with area-weighted triangle normals when available.
    let mut tri_normal_sum = [0.0; 3];
    for [a, b, c] in face_triangles(face) {
        let n_area = cross(sub(b, a), sub(c, a));
        tri_normal_sum = add(tri_normal_sum, n_area);
    }
    if let Some(tri_n) = unit(tri_normal_sum) {
        if let Some(hint_n) = normal {
            if dot(hint_n, tri_n) < 0.0 {
                normal = Some(tri_n);
            }
        } else {
            normal = Some(tri_n);
        }
    }
    let normal = normal?;
    let origin = if face.outer_loop.is_empty() {
        centroid
    } else {
        let first = face.outer_loop[0];
        if first.iter().all(|v| v.is_finite()) {
            centroid
        } else {
            face.center_hint
        }
    };
    let offset = dot(normal, origin);
    Some(PlaneAttributes {
        normal,
        origin,
        offset,
    })
}

fn extract_cylinder_attrs(face: &BRepFace, area_mm2: f64) -> Option<CylinderAttributes> {
    let radius = face.radius_hint.filter(|r| r.is_finite() && *r > 1e-4)?;

    // Determine the true cylinder axis reliably for both Analytic and OCCT faces.
    // Note: Analytic `make_cyl_wall_face` stores `u_dir` (radial) in `normal_hint`,
    // whereas OCCT stores the cylinder axis in `normal_hint`.
    let (mut axis, axis_point_opt) = infer_cylinder_axis_and_point(face, radius)?;
    axis = canonical_axis(axis);

    let line_anchor = axis_point_opt.unwrap_or(face.center_hint);
    let line_point = sub(line_anchor, scale(axis, dot(line_anchor, axis)));

    let mut axial_min = f64::INFINITY;
    let mut axial_max = f64::NEG_INFINITY;
    let mut radial_score = 0.0;
    let mut area_twice = 0.0;

    for chunk in face.vertices.chunks_exact(3) {
        let p = [chunk[0], chunk[1], chunk[2]];
        let h = dot(p, axis);
        axial_min = axial_min.min(h);
        axial_max = axial_max.max(h);
    }
    for &p in &face.outer_loop {
        let h = dot(p, axis);
        axial_min = axial_min.min(h);
        axial_max = axial_max.max(h);
    }
    if let Some(top) = &face.top_loop {
        for &p in top {
            let h = dot(p, axis);
            axial_min = axial_min.min(h);
            axial_max = axial_max.max(h);
        }
    }

    for [a, b, c] in face_triangles(face) {
        let mid = scale(add(add(a, b), c), 1.0 / 3.0);
        let radial = sub(sub(mid, line_point), scale(axis, dot(mid, axis)));
        if let Some(r_dir) = unit(radial) {
            let n_area = cross(sub(b, a), sub(c, a));
            radial_score += dot(r_dir, n_area);
            area_twice += norm(n_area);
        }
    }

    if !axial_min.is_finite() || !axial_max.is_finite() || axial_max - axial_min <= 1e-4 {
        return None;
    }

    // Fallback to vertex normals if triangles were degenerate/absent.
    let is_inward = if area_twice > 1e-9 && radial_score.abs() > area_twice * 0.2 {
        radial_score < 0.0
    } else {
        let mut vn_score = 0.0;
        for (p_chunk, n_chunk) in face
            .vertices
            .chunks_exact(3)
            .zip(face.normals.chunks_exact(3))
        {
            let p = [p_chunk[0], p_chunk[1], p_chunk[2]];
            let n = [n_chunk[0], n_chunk[1], n_chunk[2]];
            let radial = sub(sub(p, line_point), scale(axis, dot(p, axis)));
            if let Some(r_dir) = unit(radial) {
                vn_score += dot(r_dir, n);
            }
        }
        vn_score < 0.0
    };

    let height_mm = axial_max - axial_min;
    let origin = add(line_point, scale(axis, axial_min));
    let end_point = add(line_point, scale(axis, axial_max));
    let full_area = std::f64::consts::TAU * radius * height_mm;
    let sweep_angle_rad = if full_area > 1e-9 {
        ((area_mm2 / full_area) * std::f64::consts::TAU).clamp(0.0, std::f64::consts::TAU)
    } else {
        0.0
    };

    Some(CylinderAttributes {
        axis,
        origin,
        end_point,
        radius_mm: radius,
        height_mm,
        axial_min,
        axial_max,
        is_inward,
        sweep_angle_rad,
    })
}

fn infer_cylinder_axis_and_point(face: &BRepFace, radius: f64) -> Option<(Vec3, Option<Vec3>)> {
    // 1. If both circular rim loops exist (all Analytic cylinders & bores), their centroids
    //    directly give the exact axis direction and exact axis point.
    if face.outer_loop.len() >= 3
        && let Some(top) = face.top_loop.as_ref()
        && top.len() >= 3
    {
        let c0 = mean_point(&face.outer_loop);
        let c1 = mean_point(top);
        let delta = sub(c1, c0);
        if let Some(ax) = unit(delta)
            && norm(delta) > 1e-4
        {
            // Verify ring points are equidistant (~radius) from the axis line through c0.
            let tol = (radius * 0.08).max(0.05);
            let ring_ok = face
                .outer_loop
                .iter()
                .take(8)
                .all(|&p| (radial_distance(p, c0, ax) - radius).abs() <= tol);
            if ring_ok {
                return Some((ax, Some(c0)));
            }
        }
    }

    // 2. Check if `normal_hint` is already the cylinder axis (true for OCCT native cylinders):
    //    every triangle/vertex normal on a cylinder is perpendicular to the axis.
    if let Some(hint) = unit(face.normal_hint) {
        let mut dot_sum = 0.0;
        let mut count = 0usize;
        for [a, b, c] in face_triangles(face) {
            if let Some(tn) = unit(cross(sub(b, a), sub(c, a))) {
                dot_sum += dot(tn, hint).abs();
                count += 1;
            }
        }
        let hint_is_axis = if count > 0 {
            (dot_sum / count as f64) < 0.15
        } else {
            true
        };
        if hint_is_axis {
            let center_ok = face
                .vertices
                .chunks_exact(3)
                .take(12)
                .all(|c| {
                    let p = [c[0], c[1], c[2]];
                    (radial_distance(p, face.center_hint, hint) - radius).abs()
                        <= (radius * 0.08).max(0.05)
                });
            let pt = if center_ok {
                Some(face.center_hint)
            } else {
                estimate_cylinder_axis_point(face, hint, radius)
            };
            return Some((hint, pt));
        }
    }

    // 3. Fallback: recover cylinder axis from cross products of distinct triangle normals.
    let mut normals = Vec::new();
    for [a, b, c] in face_triangles(face) {
        if let Some(tn) = unit(cross(sub(b, a), sub(c, a))) {
            normals.push(tn);
        }
    }
    if normals.len() >= 2 {
        let mut accum = [0.0; 3];
        let step = (normals.len() / 16).max(1);
        for i in (0..normals.len()).step_by(step) {
            for j in ((i + 1)..normals.len()).step_by(step) {
                let c = cross(normals[i], normals[j]);
                if norm(c) > 0.1 {
                    let aligned = if dot(accum, c) < 0.0 {
                        scale(c, -1.0)
                    } else {
                        c
                    };
                    accum = add(accum, aligned);
                }
            }
        }
        if let Some(ax) = unit(accum) {
            let pt = estimate_cylinder_axis_point(face, ax, radius).or(Some(face.center_hint));
            return Some((ax, pt));
        }
    }

    unit(face.normal_hint).map(|ax| (ax, Some(face.center_hint)))
}

fn estimate_cylinder_axis_point(face: &BRepFace, axis: Vec3, radius: f64) -> Option<Vec3> {
    // Check if center_hint already works.
    let tol = (radius * 0.08).max(0.05);
    if !face.vertices.is_empty()
        && face.vertices.chunks_exact(3).take(12).all(|c| {
            let p = [c[0], c[1], c[2]];
            (radial_distance(p, face.center_hint, axis) - radius).abs() <= tol
        })
    {
        return Some(face.center_hint);
    }

    // Try both +radius and -radius offsets along vertex normals and pick the cluster with minimal spread.
    let mut best_center = None;
    let mut best_err = f64::INFINITY;
    for sign in [-1.0, 1.0] {
        let mut sum = [0.0; 3];
        let mut count = 0usize;
        for (pc, nc) in face
            .vertices
            .chunks_exact(3)
            .zip(face.normals.chunks_exact(3))
        {
            let p = [pc[0], pc[1], pc[2]];
            let n = [nc[0], nc[1], nc[2]];
            let n_perp = sub(n, scale(axis, dot(n, axis)));
            if let Some(nu) = unit(n_perp) {
                let cand = add(p, scale(nu, sign * radius));
                let proj = sub(cand, scale(axis, dot(cand, axis)));
                sum = add(sum, proj);
                count += 1;
            }
        }
        if count > 0 {
            let mean = scale(sum, 1.0 / count as f64);
            let max_err = face
                .vertices
                .chunks_exact(3)
                .take(24)
                .map(|c| (radial_distance([c[0], c[1], c[2]], mean, axis) - radius).abs())
                .fold(0.0_f64, f64::max);
            if max_err < best_err {
                best_err = max_err;
                best_center = Some(mean);
            }
        }
    }
    best_center
}

fn build_geometric_edges(faces: &[AagFace], raw_faces: &[BRepFace]) -> Vec<AagEdge> {
    // Collect boundary segments per face, then match shared segments between face pairs.
    type QuantPt = [i64; 3];
    type EdgeKey = (QuantPt, QuantPt);

    let mut seg_map: HashMap<EdgeKey, Vec<(FaceId, Vec3, Vec3)>> = HashMap::new();
    for (idx, raw) in raw_faces.iter().enumerate() {
        let fid = FaceId(idx);
        let loops = collect_face_boundary_loops(raw);
        for lp in loops {
            if lp.len() < 2 {
                continue;
            }
            for i in 0..lp.len() {
                let a = lp[i];
                let b = lp[(i + 1) % lp.len()];
                if norm(sub(b, a)) <= 1e-6 {
                    continue;
                }
                let qa = quantize_pt(a);
                let qb = quantize_pt(b);
                if qa == qb {
                    continue;
                }
                let key = if qa < qb { (qa, qb) } else { (qb, qa) };
                seg_map.entry(key).or_default().push((fid, a, b));
            }
        }
    }

    // Aggregate shared segments per unordered face pair.
    let mut pair_acc: BTreeMap<(FaceId, FaceId), (Vec3, f64, usize, bool)> = BTreeMap::new();
    for entries in seg_map.values() {
        // Deduplicate by FaceId within a segment bucket.
        let mut distinct: Vec<(FaceId, Vec3, Vec3)> = Vec::new();
        for &(fid, a, b) in entries {
            if !distinct.iter().any(|(existing, _, _)| *existing == fid) {
                distinct.push((fid, a, b));
            }
        }
        if distinct.len() < 2 {
            continue;
        }
        let non_manifold = distinct.len() > 2;
        for i in 0..distinct.len() {
            for j in (i + 1)..distinct.len() {
                let (fa, a, b) = distinct[i];
                let (fb, _, _) = distinct[j];
                let pair = if fa < fb { (fa, fb) } else { (fb, fa) };
                let mid = scale(add(a, b), 0.5);
                let len = norm(sub(b, a));
                let entry = pair_acc.entry(pair).or_insert(([0.0; 3], 0.0, 0, false));
                entry.0 = add(entry.0, scale(mid, len));
                entry.1 += len;
                entry.2 += 1;
                entry.3 |= non_manifold;
            }
        }
    }

    // Also link cylinder-plane rims when discretization differs (e.g., 32-seg Analytic vs OCCT).
    for (ci, cf) in faces.iter().enumerate() {
        let Some(cyl) = &cf.cylinder else {
            continue;
        };
        for (pi, pf) in faces.iter().enumerate() {
            if ci == pi {
                continue;
            }
            let Some(pl) = &pf.plane else {
                continue;
            };
            if dot(pl.normal, cyl.axis).abs() < 0.995 {
                continue;
            }
            for rim_center in [cyl.origin, cyl.end_point] {
                if (dot(pl.normal, rim_center) - pl.offset).abs() > 0.02 {
                    continue;
                }
                if !point_near_bbox(rim_center, &pf.bbox, cyl.radius_mm + 0.05) {
                    continue;
                }
                let pair = if FaceId(ci) < FaceId(pi) {
                    (FaceId(ci), FaceId(pi))
                } else {
                    (FaceId(pi), FaceId(ci))
                };
                pair_acc.entry(pair).or_insert_with(|| {
                    let (u, _) = ortho_basis(cyl.axis);
                    let rim_pt = add(rim_center, scale(u, cyl.radius_mm));
                    let circ = std::f64::consts::TAU * cyl.radius_mm;
                    (scale(rim_pt, circ), circ, 16, false)
                });
            }
        }
    }

    // Also detect collinear overlapping boundary segments between planar faces (handles T-junctions).
    let planar_segments: Vec<(FaceId, Vec<(Vec3, Vec3)>)> = faces
        .iter()
        .enumerate()
        .filter_map(|(idx, f)| {
            if f.surface_kind != AagSurfaceKind::Plane {
                return None;
            }
            let mut segs = Vec::new();
            for lp in std::iter::once(&f.outer_loop).chain(&f.inner_loops) {
                if lp.len() < 3 {
                    continue;
                }
                for i in 0..lp.len() {
                    let a = lp[i];
                    let b = lp[(i + 1) % lp.len()];
                    if norm(sub(b, a)) > 1e-4 {
                        segs.push((a, b));
                    }
                }
            }
            Some((FaceId(idx), segs))
        })
        .collect();

    for i in 0..planar_segments.len() {
        let (fa, ref segs_a) = planar_segments[i];
        let bb_a = &faces[fa.0].bbox;
        for (fb, segs_b) in planar_segments.iter().skip(i + 1) {
            let pair = if fa < *fb { (fa, *fb) } else { (*fb, fa) };
            if pair_acc.contains_key(&pair) {
                continue;
            }
            if !bboxes_touch(bb_a, &faces[fb.0].bbox, 0.02) {
                continue;
            }
            if let Some((mid, len)) = find_collinear_overlap(segs_a, segs_b) {
                pair_acc.insert(pair, (scale(mid, len), len, 1, false));
            }
        }
    }

    let mut edges = Vec::with_capacity(pair_acc.len());
    for ((fa, fb), (weighted_mid, total_len, seg_count, non_manifold)) in pair_acc {
        let midpoint = if total_len > 1e-9 {
            scale(weighted_mid, 1.0 / total_len)
        } else {
            scale(add(faces[fa.0].centroid, faces[fb.0].centroid), 0.5)
        };
        let is_circular = seg_count >= 6
            && (faces[fa.0].surface_kind == AagSurfaceKind::Cylinder
                || faces[fb.0].surface_kind == AagSurfaceKind::Cylinder);
        let (convexity, dihedral_angle_deg) = if non_manifold {
            (EdgeConvexity::NonManifold, 0.0)
        } else {
            classify_edge_convexity(&faces[fa.0], &faces[fb.0], midpoint)
        };
        edges.push(AagEdge {
            id: EdgeId(edges.len()),
            face_a: fa,
            face_b: fb,
            convexity,
            dihedral_angle_deg,
            midpoint,
            length_mm: total_len,
            is_circular,
        });
    }
    edges
}

#[cfg(feature = "occt-ffi")]
fn merge_native_incidences(
    faces: &[AagFace],
    native: &crate::FaceAdjacencyGraph,
    edges: &mut Vec<AagEdge>,
) {
    use std::collections::BTreeSet;
    let mut existing: BTreeSet<(FaceId, FaceId)> = edges
        .iter()
        .map(|e| {
            if e.face_a < e.face_b {
                (e.face_a, e.face_b)
            } else {
                (e.face_b, e.face_a)
            }
        })
        .collect();
    for e in native.edges() {
        if e.is_degenerate || e.face_uses.len() < 2 {
            continue;
        }
        let non_manifold = e.face_uses.len() > 2;
        for i in 0..e.face_uses.len() {
            for j in (i + 1)..e.face_uses.len() {
                let fa = e.face_uses[i].face;
                let fb = e.face_uses[j].face;
                if fa == fb || fa.0 >= faces.len() || fb.0 >= faces.len() {
                    continue;
                }
                let pair = if fa < fb { (fa, fb) } else { (fb, fa) };
                if !existing.insert(pair) {
                    continue;
                }
                let mid = scale(add(faces[pair.0.0].centroid, faces[pair.1.0].centroid), 0.5);
                let (convexity, dihedral_angle_deg) = if non_manifold {
                    (EdgeConvexity::NonManifold, 0.0)
                } else {
                    classify_edge_convexity(&faces[pair.0.0], &faces[pair.1.0], mid)
                };
                edges.push(AagEdge {
                    id: EdgeId(edges.len()),
                    face_a: pair.0,
                    face_b: pair.1,
                    convexity,
                    dihedral_angle_deg,
                    midpoint: mid,
                    length_mm: 0.0,
                    is_circular: e.curve_kind == crate::CurveKind::Circle,
                });
            }
        }
    }
}

fn collect_face_boundary_loops(face: &BRepFace) -> Vec<Vec<Vec3>> {
    let mut loops = Vec::new();
    if face.outer_loop.len() >= 3 {
        loops.push(face.outer_loop.clone());
    }
    if let Some(top) = &face.top_loop
        && top.len() >= 3
    {
        loops.push(top.clone());
    }
    for inner in &face.inner_loops {
        if inner.len() >= 3 {
            loops.push(inner.clone());
        }
    }
    if !loops.is_empty() {
        return loops;
    }

    // Fallback: extract boundary half-edges from the triangle mesh (used by OCCT faces).
    let mut edge_counts: HashMap<(u32, u32), usize> = HashMap::new();
    for tri in face.indices.chunks_exact(3) {
        for &(u, v) in &[(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let key = if u < v { (u, v) } else { (v, u) };
            *edge_counts.entry(key).or_insert(0) += 1;
        }
    }
    let mut boundary_segs = Vec::new();
    for ((u, v), count) in edge_counts {
        if count == 1
            && let (Some(pa), Some(pb)) = (face_vertex(face, u), face_vertex(face, v))
        {
            boundary_segs.push(vec![pa, pb]);
        }
    }
    boundary_segs
}

fn classify_edge_convexity(fa: &AagFace, fb: &AagFace, midpoint: Vec3) -> (EdgeConvexity, f64) {
    let Some(na) = surface_normal_at(fa, midpoint) else {
        return (EdgeConvexity::Unknown, 0.0);
    };
    let Some(nb) = surface_normal_at(fb, midpoint) else {
        return (EdgeConvexity::Unknown, 0.0);
    };

    let cos_angle = dot(na, nb).clamp(-1.0, 1.0);
    let angle_deg = cos_angle.acos().to_degrees();
    if cos_angle >= SMOOTH_DOT_THRESHOLD {
        return (EdgeConvexity::Smooth, angle_deg);
    }

    let ta = face_inward_tangent(fa, midpoint, na);
    let tb = face_inward_tangent(fb, midpoint, nb);

    let score = match (ta, tb) {
        (Some(ta), Some(tb)) => 0.5 * (dot(ta, nb) + dot(tb, na)),
        (Some(ta), None) => dot(ta, nb),
        (None, Some(tb)) => dot(tb, na),
        (None, None) => return (EdgeConvexity::Unknown, angle_deg),
    };

    if score > 0.05 {
        (EdgeConvexity::Concave, angle_deg)
    } else if score < -0.05 {
        (EdgeConvexity::Convex, angle_deg)
    } else {
        (EdgeConvexity::Smooth, angle_deg)
    }
}

fn surface_normal_at(face: &AagFace, point: Vec3) -> Option<Vec3> {
    if let Some(p) = &face.plane {
        return Some(p.normal);
    }
    if let Some(c) = &face.cylinder {
        let d = sub(point, c.origin);
        let radial = sub(d, scale(c.axis, dot(d, c.axis)));
        let r_dir = unit(radial)?;
        return Some(if c.is_inward {
            scale(r_dir, -1.0)
        } else {
            r_dir
        });
    }
    None
}

fn face_inward_tangent(face: &AagFace, midpoint: Vec3, normal: Vec3) -> Option<Vec3> {
    // For a cylinder at a rim, the inward tangent points along the cylinder axis toward its middle.
    if let Some(c) = &face.cylinder {
        let mid_axis = scale(add(c.origin, c.end_point), 0.5);
        let delta = sub(mid_axis, midpoint);
        let axial = scale(c.axis, dot(delta, c.axis));
        if let Some(dir) = unit(axial) {
            return Some(dir);
        }
    }
    // For a planar face with an inner hole/boss loop containing `midpoint`, tangent points AWAY
    // from the loop center onto the planar material.
    if let Some(p) = &face.plane {
        for inner in &face.inner_loops {
            if inner.len() >= 3 {
                let lc = mean_point(inner);
                let dist_to_loop = inner
                    .iter()
                    .map(|&v| norm(sub(v, midpoint)))
                    .fold(f64::INFINITY, f64::min);
                if dist_to_loop < 0.5 {
                    let away = sub(midpoint, lc);
                    let tang = sub(away, scale(p.normal, dot(away, p.normal)));
                    if let Some(dir) = unit(tang) {
                        return Some(dir);
                    }
                }
            }
        }
    }
    let to_centroid = sub(face.centroid, midpoint);
    let tang = sub(to_centroid, scale(normal, dot(to_centroid, normal)));
    unit(tang)
}

fn find_collinear_overlap(segs_a: &[(Vec3, Vec3)], segs_b: &[(Vec3, Vec3)]) -> Option<(Vec3, f64)> {
    const TOL: f64 = 0.02;
    let mut best_len = 0.0;
    let mut best_mid = [0.0; 3];
    for &(a0, a1) in segs_a {
        let da = sub(a1, a0);
        let la = norm(da);
        let Some(ua) = unit(da) else { continue };
        for &(b0, b1) in segs_b {
            let db = sub(b1, b0);
            let Some(ub) = unit(db) else { continue };
            if dot(ua, ub).abs() < 0.999 {
                continue;
            }
            if radial_distance(b0, a0, ua) > TOL || radial_distance(b1, a0, ua) > TOL {
                continue;
            }
            let t0 = dot(sub(b0, a0), ua);
            let t1 = dot(sub(b1, a0), ua);
            let lo = t0.min(t1).max(0.0);
            let hi = t0.max(t1).min(la);
            if hi - lo > TOL {
                let len = hi - lo;
                if len > best_len {
                    best_len = len;
                    best_mid = add(a0, scale(ua, 0.5 * (lo + hi)));
                }
            }
        }
    }
    (best_len > TOL).then_some((best_mid, best_len))
}

fn bboxes_touch(a: &BBox3, b: &BBox3, tol: f64) -> bool {
    (0..3).all(|k| a.min[k] <= b.max[k] + tol && b.min[k] <= a.max[k] + tol)
}

fn point_near_bbox(p: Vec3, bb: &BBox3, tol: f64) -> bool {
    (0..3).all(|k| p[k] >= bb.min[k] - tol && p[k] <= bb.max[k] + tol)
}

fn quantize_pt(p: Vec3) -> [i64; 3] {
    p.map(|v| (v / COORD_QUANT_MM).round() as i64)
}

pub(crate) fn face_vertex(face: &BRepFace, index: u32) -> Option<Vec3> {
    let start = (index as usize).checked_mul(3)?;
    let slice = face.vertices.get(start..start + 3)?;
    Some([slice[0], slice[1], slice[2]])
}

pub(crate) fn face_triangles(face: &BRepFace) -> impl Iterator<Item = [Vec3; 3]> + '_ {
    face.indices.chunks_exact(3).filter_map(|t| {
        Some([
            face_vertex(face, t[0])?,
            face_vertex(face, t[1])?,
            face_vertex(face, t[2])?,
        ])
    })
}

pub(crate) fn canonical_axis(v: Vec3) -> Vec3 {
    let dominant = (0..3)
        .max_by(|&i, &j| v[i].abs().total_cmp(&v[j].abs()))
        .unwrap_or(2);
    if v[dominant] < 0.0 { scale(v, -1.0) } else { v }
}

pub(crate) fn mean_point(pts: &[Vec3]) -> Vec3 {
    if pts.is_empty() {
        return [0.0; 3];
    }
    let mut sum = [0.0; 3];
    for p in pts {
        for k in 0..3 {
            sum[k] += p[k];
        }
    }
    scale(sum, 1.0 / pts.len() as f64)
}

pub(crate) fn radial_distance(p: Vec3, line_pt: Vec3, axis: Vec3) -> f64 {
    let d = sub(p, line_pt);
    let perp = sub(d, scale(axis, dot(d, axis)));
    norm(perp)
}

pub(crate) fn ortho_basis(n: Vec3) -> (Vec3, Vec3) {
    let ref_v = if n[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let u = unit(cross(ref_v, n)).unwrap_or([1.0, 0.0, 0.0]);
    let v = unit(cross(n, u)).unwrap_or([0.0, 1.0, 0.0]);
    (u, v)
}

pub(crate) fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn scale(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn norm(a: Vec3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

pub(crate) fn unit(a: Vec3) -> Option<Vec3> {
    let len = norm(a);
    (len.is_finite() && len > 1e-12).then(|| [a[0] / len, a[1] / len, a[2] / len])
}

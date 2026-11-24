//! CPU reference implementation for Pipeline A interference screening.
//!
//! The GPU implementation uses the same broad/narrow phase contract.  This
//! module intentionally keeps the reference path free of external geometry
//! dependencies so that it can run in CI without a GPU or an OCCT build.

use serde::{Deserialize, Serialize};

use crate::lbvh::{LbvhTree, TriRef, cross, dot, sub};
use crate::roi::RoiAnchor;

const ROUNDING_TOLERANCE: f64 = 16.0 * f64::EPSILON;

/// A triangle pair that either penetrates or is closer than the requested
/// safety clearance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterferencePair {
    pub instance_a: u32,
    pub instance_b: u32,
    pub face_a: u32,
    pub face_b: u32,
    pub min_distance_mm: f64,
    pub penetrating: bool,
}

/// Result of an assembly interference/clearance pass.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InterferenceReport {
    pub pairs: Vec<InterferencePair>,
}

impl InterferenceReport {
    /// Returns true when the surface triangles have no reported violations.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.pairs.is_empty()
    }
}

/// Runs the LBVH broad phase followed by exact triangle narrow phase tests.
///
/// Reports the closest triangle witness per distinct instance/face pair, sorted
/// by instance and face IDs. Surface contact counts as an intersection with
/// zero distance. Fully enclosed solids require a separate containment check.
#[must_use]
pub fn check_assembly_interference(tris: &[TriRef], min_clearance_mm: f64) -> InterferenceReport {
    if tris.is_empty() {
        return InterferenceReport::default();
    }

    let clearance = min_clearance_mm.max(0.0);
    let anchor = RoiAnchor::from_triangles(tris);
    let local_tris = anchor.rebase_triangles(tris);
    let tree = LbvhTree::build(&local_tris);
    let bounds: Vec<_> = local_tris.iter().map(TriRef::aabb).collect();
    let mut pairs = Vec::new();

    for (index_a, tri_a) in local_tris.iter().enumerate() {
        let query_box = bounds[index_a].expand(clearance);
        for index_b in tree.query_aabb(&query_box) {
            if index_b <= index_a {
                continue;
            }
            let tri_b = &local_tris[index_b];
            if tri_a.instance_id == tri_b.instance_id {
                continue;
            }
            let lower_bound = bounds[index_a].min_distance_sq(&bounds[index_b]);
            if lower_bound > 0.0 && lower_bound >= clearance * clearance {
                continue;
            }

            let penetrating = lower_bound == 0.0 && triangles_intersect(tri_a, tri_b);
            let distance = if penetrating {
                0.0
            } else {
                triangle_distance_sq(tri_a, tri_b).max(lower_bound).sqrt()
            };
            if penetrating || distance < clearance {
                let (instance_a, face_a, instance_b, face_b) = canonical_pair(tri_a, tri_b);
                pairs.push(InterferencePair {
                    instance_a,
                    instance_b,
                    face_a,
                    face_b,
                    min_distance_mm: distance,
                    penetrating,
                });
            }
        }
    }

    pairs.sort_by(|left, right| {
        (left.instance_a, left.instance_b, left.face_a, left.face_b)
            .cmp(&(
                right.instance_a,
                right.instance_b,
                right.face_a,
                right.face_b,
            ))
            .then_with(|| right.penetrating.cmp(&left.penetrating))
            .then_with(|| left.min_distance_mm.total_cmp(&right.min_distance_mm))
    });
    pairs.dedup_by(|left, right| {
        left.instance_a == right.instance_a
            && left.instance_b == right.instance_b
            && left.face_a == right.face_a
            && left.face_b == right.face_b
    });

    InterferenceReport { pairs }
}

fn canonical_pair(a: &TriRef, b: &TriRef) -> (u32, u32, u32, u32) {
    if (a.instance_id, a.face_slot_id) <= (b.instance_id, b.face_slot_id) {
        (a.instance_id, a.face_slot_id, b.instance_id, b.face_slot_id)
    } else {
        (b.instance_id, b.face_slot_id, a.instance_id, a.face_slot_id)
    }
}

fn triangles_intersect(a: &TriRef, b: &TriRef) -> bool {
    let a_edges = [(a.a, a.b), (a.b, a.c), (a.c, a.a)];
    let b_edges = [(b.a, b.b), (b.b, b.c), (b.c, b.a)];
    let edge_a0 = sub(a.b, a.a);
    let edge_a1 = sub(a.c, a.a);
    let edge_b0 = sub(b.b, b.a);
    let edge_b1 = sub(b.c, b.a);
    let normal_a = cross(edge_a0, edge_a1);
    let normal_b = cross(edge_b0, edge_b1);
    for &(start, end) in &a_edges {
        if segment_triangle_intersection(start, end, b) {
            return true;
        }
    }
    for &(start, end) in &b_edges {
        if segment_triangle_intersection(start, end, a) {
            return true;
        }
    }

    if normal_a == [0.0; 3] || normal_b == [0.0; 3] {
        // Segment/triangle crossings were checked above. For the remaining
        // point or collinear cases, a zero closest distance is contact.
        return triangle_distance_sq(a, b) == 0.0;
    }

    triangle_on_plane(a, b.a, normal_b)
        && triangle_on_plane(b, a.a, normal_a)
        && coplanar_triangles_intersect(a, b, normal_a)
}

fn triangle_on_plane(triangle: &TriRef, origin: [f64; 3], normal: [f64; 3]) -> bool {
    [triangle.a, triangle.b, triangle.c]
        .into_iter()
        .all(|point| {
            let offset = sub(point, origin);
            let rounding_bound = dot(offset.map(f64::abs), normal.map(f64::abs));
            dot(offset, normal).abs() <= ROUNDING_TOLERANCE * rounding_bound
        })
}

fn segment_triangle_intersection(start: [f64; 3], end: [f64; 3], tri: &TriRef) -> bool {
    let direction = sub(end, start);
    let edge_1 = sub(tri.b, tri.a);
    let edge_2 = sub(tri.c, tri.a);
    let pvec = cross(direction, edge_2);
    let determinant = dot(edge_1, pvec);
    if determinant == 0.0 {
        return false;
    }
    let inverse = 1.0 / determinant;
    let tvec = sub(start, tri.a);
    let u = dot(tvec, pvec) * inverse;
    if !(-ROUNDING_TOLERANCE..=1.0 + ROUNDING_TOLERANCE).contains(&u) {
        return false;
    }
    let qvec = cross(tvec, edge_1);
    let v = dot(direction, qvec) * inverse;
    if v < -ROUNDING_TOLERANCE || u + v > 1.0 + ROUNDING_TOLERANCE {
        return false;
    }
    let t = dot(edge_2, qvec) * inverse;
    (-ROUNDING_TOLERANCE..=1.0 + ROUNDING_TOLERANCE).contains(&t)
}

fn coplanar_triangles_intersect(a: &TriRef, b: &TriRef, normal: [f64; 3]) -> bool {
    let axis = dominant_axis(normal);
    let a_points = [project(a.a, axis), project(a.b, axis), project(a.c, axis)];
    let b_points = [project(b.a, axis), project(b.b, axis), project(b.c, axis)];
    for i in 0..3 {
        let a_start = a_points[i];
        let a_end = a_points[(i + 1) % 3];
        for j in 0..3 {
            if segments_intersect_2d(a_start, a_end, b_points[j], b_points[(j + 1) % 3]) {
                return true;
            }
        }
    }
    point_in_triangle_2d(a_points[0], &b_points) || point_in_triangle_2d(b_points[0], &a_points)
}

fn triangle_distance_sq(a: &TriRef, b: &TriRef) -> f64 {
    let a_edges = [(a.a, a.b), (a.b, a.c), (a.c, a.a)];
    let b_edges = [(b.a, b.b), (b.b, b.c), (b.c, b.a)];
    let mut best = f64::INFINITY;
    for &point in &[a.a, a.b, a.c] {
        best = best.min(point_triangle_distance_sq(point, b));
    }
    for &point in &[b.a, b.b, b.c] {
        best = best.min(point_triangle_distance_sq(point, a));
    }
    for &(start, end) in &a_edges {
        for &(other_start, other_end) in &b_edges {
            best = best.min(segment_segment_distance_sq(
                start,
                end,
                other_start,
                other_end,
            ));
        }
    }
    best
}

pub(crate) fn point_triangle_distance_sq(point: [f64; 3], tri: &TriRef) -> f64 {
    let ab = sub(tri.b, tri.a);
    let ac = sub(tri.c, tri.a);
    let normal = cross(ab, ac);
    let normal_sq = norm_sq(normal);
    if normal_sq == 0.0 {
        return segment_point_distance_sq(point, tri.a, tri.b)
            .min(segment_point_distance_sq(point, tri.b, tri.c))
            .min(segment_point_distance_sq(point, tri.c, tri.a));
    }
    let ap = sub(point, tri.a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return norm_sq(ap);
    }

    let bp = sub(point, tri.b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return norm_sq(bp);
    }

    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let denominator = d1 - d3;
        if denominator != 0.0 {
            let v = d1 / denominator;
            return norm_sq(sub(ap, scale(ab, v)));
        }
    }

    let cp = sub(point, tri.c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return norm_sq(cp);
    }

    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let denominator = d2 - d6;
        if denominator != 0.0 {
            let w = d2 / denominator;
            return norm_sq(sub(ap, scale(ac, w)));
        }
    }

    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let edge = sub(tri.c, tri.b);
        let denominator = (d4 - d3) + (d5 - d6);
        if denominator != 0.0 {
            let w = (d4 - d3) / denominator;
            return norm_sq(sub(bp, scale(edge, w)));
        }
    }

    let distance = dot(ap, normal);
    distance * distance / normal_sq
}

fn segment_segment_distance_sq(
    first_start: [f64; 3],
    first_end: [f64; 3],
    second_start: [f64; 3],
    second_end: [f64; 3],
) -> f64 {
    let direction_a = sub(first_end, first_start);
    let direction_b = sub(second_end, second_start);
    let offset = sub(first_start, second_start);
    let a = dot(direction_a, direction_a);
    let e = dot(direction_b, direction_b);
    let f = dot(direction_b, offset);

    if a == 0.0 && e == 0.0 {
        return norm_sq(offset);
    }
    if a == 0.0 {
        let t = (f / e).clamp(0.0, 1.0);
        return norm_sq(sub(offset, scale(direction_b, t)));
    }
    let c = dot(direction_a, offset);
    if e == 0.0 {
        let s = (-c / a).clamp(0.0, 1.0);
        return norm_sq(add(offset, scale(direction_a, s)));
    }

    let b = dot(direction_a, direction_b);
    let perpendicular = cross(direction_a, direction_b);
    let denominator = norm_sq(perpendicular);
    let mut s;
    let t;
    if denominator > 0.0 {
        s = (dot(cross(direction_b, offset), perpendicular) / denominator).clamp(0.0, 1.0);
    } else {
        s = 0.0;
    }
    let t_nominal = b * s + f;
    if t_nominal <= 0.0 {
        t = 0.0;
        s = (-c / a).clamp(0.0, 1.0);
    } else if t_nominal >= e {
        t = 1.0;
        s = ((b - c) / a).clamp(0.0, 1.0);
    } else {
        t = t_nominal / e;
    }
    let delta = sub(add(offset, scale(direction_a, s)), scale(direction_b, t));
    norm_sq(delta)
}

pub(crate) fn segment_point_distance_sq(point: [f64; 3], start: [f64; 3], end: [f64; 3]) -> f64 {
    let direction = sub(end, start);
    let length_sq = norm_sq(direction);
    if length_sq == 0.0 {
        return norm_sq(sub(point, start));
    }
    let offset = sub(point, start);
    let parameter = dot(offset, direction) / length_sq;
    norm_sq(sub(offset, scale(direction, parameter.clamp(0.0, 1.0))))
}

fn segments_intersect_2d(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let bounds_overlap = (0..2).all(|axis| {
        a[axis].min(b[axis]) <= c[axis].max(d[axis]) && c[axis].min(d[axis]) <= a[axis].max(b[axis])
    });
    if !bounds_overlap {
        return false;
    }
    let ab_c = orient_2d(a, b, c);
    let ab_d = orient_2d(a, b, d);
    let cd_a = orient_2d(c, d, a);
    let cd_b = orient_2d(c, d, b);
    opposite_or_zero(ab_c, ab_d) && opposite_or_zero(cd_a, cd_b)
}

fn opposite_or_zero(first: f64, second: f64) -> bool {
    (first >= 0.0 && second <= 0.0) || (first <= 0.0 && second >= 0.0)
}

pub(crate) fn point_in_triangle_2d(point: [f64; 2], tri: &[[f64; 2]; 3]) -> bool {
    let first = orient_2d(tri[0], tri[1], point);
    let second = orient_2d(tri[1], tri[2], point);
    let third = orient_2d(tri[2], tri[0], point);
    (first >= 0.0 && second >= 0.0 && third >= 0.0)
        || (first <= 0.0 && second <= 0.0 && third <= 0.0)
}

fn dominant_axis(normal: [f64; 3]) -> usize {
    let abs = [normal[0].abs(), normal[1].abs(), normal[2].abs()];
    if abs[0] >= abs[1] && abs[0] >= abs[2] {
        0
    } else if abs[1] >= abs[2] {
        1
    } else {
        2
    }
}

fn project(point: [f64; 3], axis: usize) -> [f64; 2] {
    match axis {
        0 => [point[1], point[2]],
        1 => [point[0], point[2]],
        _ => [point[0], point[1]],
    }
}

fn orient_2d(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(value: [f64; 3], factor: f64) -> [f64; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

fn norm_sq(value: [f64; 3]) -> f64 {
    dot(value, value)
}

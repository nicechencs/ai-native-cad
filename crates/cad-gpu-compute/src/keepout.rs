//! Exact full-triangle Keep-Out zone intersection and multi-body containment checks.
//!
//! Addresses the R13 sparse-sampling counterexample: instead of probing vertices,
//! edge midpoints, or centroids, every LBVH broad-phase candidate triangle is
//! tested analytically against the full triangle area (13-axis SAT for boxes,
//! exact point-to-triangle distance for spheres, exact axial slab clipping plus
//! 2D polygon-to-origin distance for finite cylinders), followed by an exact
//! opposing-ray signed-crossing containment test for zones buried wholly inside
//! a closed solid.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::interference::{
    InterferencePair, InterferenceReport, check_assembly_interference, point_in_triangle_2d,
    point_triangle_distance_sq, segment_point_distance_sq,
};
use crate::lbvh::{Aabb3, LbvhTree, TriRef, cross, dot, sub};
use crate::roi::RoiAnchor;

const OPEN_INTERIOR_REL_EPS: f64 = 1.0e-12;

/// Analytic geometry of a Keep-Out clearance volume in world millimeters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeepOutShape {
    Box {
        min_mm: [f64; 3],
        max_mm: [f64; 3],
    },
    Sphere {
        center_mm: [f64; 3],
        radius_mm: f64,
    },
    Cylinder {
        origin_mm: [f64; 3],
        axis: [f64; 3],
        radius_mm: f64,
        height_mm: f64,
    },
}

/// Named clearance volume that must not be penetrated by part geometry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeepOutZone {
    pub zone_id: String,
    pub shape: KeepOutShape,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl KeepOutZone {
    /// Creates an axis-aligned box keep-out zone from min/max corners in millimeters.
    #[must_use]
    pub fn box_mm(zone_id: impl Into<String>, min_mm: [f64; 3], max_mm: [f64; 3]) -> Self {
        let min = std::array::from_fn(|axis| min_mm[axis].min(max_mm[axis]));
        let max = std::array::from_fn(|axis| min_mm[axis].max(max_mm[axis]));
        Self {
            zone_id: zone_id.into(),
            shape: KeepOutShape::Box {
                min_mm: min,
                max_mm: max,
            },
            description: None,
        }
    }

    /// Creates a spherical keep-out zone from center and radius in millimeters.
    #[must_use]
    pub fn sphere_mm(zone_id: impl Into<String>, center_mm: [f64; 3], radius_mm: f64) -> Self {
        Self {
            zone_id: zone_id.into(),
            shape: KeepOutShape::Sphere {
                center_mm,
                radius_mm,
            },
            description: None,
        }
    }

    /// Creates a finite cylindrical keep-out zone starting at `origin_mm` and
    /// extending by `height_mm` along `axis`.
    #[must_use]
    pub fn cylinder_mm(
        zone_id: impl Into<String>,
        origin_mm: [f64; 3],
        axis: [f64; 3],
        radius_mm: f64,
        height_mm: f64,
    ) -> Self {
        Self {
            zone_id: zone_id.into(),
            shape: KeepOutShape::Cylinder {
                origin_mm,
                axis,
                radius_mm,
                height_mm,
            },
            description: None,
        }
    }

    /// Attaches an optional human-readable description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Computes a tight world-space bounding box for the keep-out volume.
    /// Returns `None` if the zone has invalid or non-positive dimensions.
    #[must_use]
    pub fn aabb(&self) -> Option<Aabb3> {
        match self.shape {
            KeepOutShape::Box { min_mm, max_mm } => {
                if !min_mm.into_iter().chain(max_mm).all(f64::is_finite) {
                    return None;
                }
                let min = std::array::from_fn(|axis| min_mm[axis].min(max_mm[axis]));
                let max = std::array::from_fn(|axis| min_mm[axis].max(max_mm[axis]));
                if (0..3).any(|axis| max[axis] <= min[axis]) {
                    return None;
                }
                Some(Aabb3 { min, max })
            }
            KeepOutShape::Sphere {
                center_mm,
                radius_mm,
            } => {
                if !center_mm.into_iter().all(f64::is_finite)
                    || !radius_mm.is_finite()
                    || radius_mm <= 0.0
                {
                    return None;
                }
                Some(Aabb3 {
                    min: center_mm.map(|v| v - radius_mm),
                    max: center_mm.map(|v| v + radius_mm),
                })
            }
            KeepOutShape::Cylinder {
                origin_mm,
                axis,
                radius_mm,
                height_mm,
            } => {
                if !origin_mm.into_iter().chain(axis).all(f64::is_finite)
                    || !radius_mm.is_finite()
                    || !height_mm.is_finite()
                    || radius_mm <= 0.0
                    || height_mm <= 0.0
                {
                    return None;
                }
                let u = normalize(axis)?;
                let end = [
                    origin_mm[0] + u[0] * height_mm,
                    origin_mm[1] + u[1] * height_mm,
                    origin_mm[2] + u[2] * height_mm,
                ];
                // Exact cylinder AABB radial extent along each cardinal axis: r * sqrt(1 - u_i^2).
                let radial: [f64; 3] =
                    std::array::from_fn(|i| radius_mm * (1.0 - u[i] * u[i]).max(0.0).sqrt());
                Some(Aabb3 {
                    min: std::array::from_fn(|i| origin_mm[i].min(end[i]) - radial[i]),
                    max: std::array::from_fn(|i| origin_mm[i].max(end[i]) + radial[i]),
                })
            }
        }
    }

    /// Returns a strictly interior reference point of the keep-out volume.
    #[must_use]
    pub fn interior_point(&self) -> Option<[f64; 3]> {
        match self.shape {
            KeepOutShape::Box { min_mm, max_mm } => {
                let bounds = self.aabb()?;
                let _ = (min_mm, max_mm);
                Some(std::array::from_fn(|i| bounds.min[i].midpoint(bounds.max[i])))
            }
            KeepOutShape::Sphere {
                center_mm,
                radius_mm,
            } => {
                if !center_mm.into_iter().all(f64::is_finite)
                    || !radius_mm.is_finite()
                    || radius_mm <= 0.0
                {
                    return None;
                }
                Some(center_mm)
            }
            KeepOutShape::Cylinder {
                origin_mm,
                axis,
                radius_mm,
                height_mm,
            } => {
                if !origin_mm.into_iter().chain(axis).all(f64::is_finite)
                    || !radius_mm.is_finite()
                    || !height_mm.is_finite()
                    || radius_mm <= 0.0
                    || height_mm <= 0.0
                {
                    return None;
                }
                let u = normalize(axis)?;
                Some([
                    origin_mm[0] + u[0] * (0.5 * height_mm),
                    origin_mm[1] + u[1] * (0.5 * height_mm),
                    origin_mm[2] + u[2] * (0.5 * height_mm),
                ])
            }
        }
    }

    /// Rebases this keep-out zone into ROI-local `f64` coordinates.
    #[must_use]
    pub fn rebase(&self, anchor: &RoiAnchor) -> Self {
        let shape = match self.shape {
            KeepOutShape::Box { min_mm, max_mm } => KeepOutShape::Box {
                min_mm: anchor.to_local_f64(min_mm),
                max_mm: anchor.to_local_f64(max_mm),
            },
            KeepOutShape::Sphere {
                center_mm,
                radius_mm,
            } => KeepOutShape::Sphere {
                center_mm: anchor.to_local_f64(center_mm),
                radius_mm,
            },
            KeepOutShape::Cylinder {
                origin_mm,
                axis,
                radius_mm,
                height_mm,
            } => KeepOutShape::Cylinder {
                origin_mm: anchor.to_local_f64(origin_mm),
                axis,
                radius_mm,
                height_mm,
            },
        };
        Self {
            zone_id: self.zone_id.clone(),
            shape,
            description: self.description.clone(),
        }
    }
}

/// One detected Keep-Out zone intrusion witness.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeepOutViolation {
    pub zone_id: String,
    pub instance_id: u32,
    pub face_slot_id: Option<u32>,
    /// Positive penetration depth into the keep-out interior, in millimeters.
    pub penetration_mm: f64,
    /// World-space witness point inside the violated keep-out zone, in millimeters.
    pub witness_point_mm: [f64; 3],
    /// True when the keep-out zone lies wholly inside the closed solid with no
    /// surface triangle intersecting the zone boundary.
    pub enclosed: bool,
}

/// Result of checking a triangle set against all Keep-Out zones.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KeepOutReport {
    pub violations: Vec<KeepOutViolation>,
}

impl KeepOutReport {
    /// Returns true when no keep-out zone is violated.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.violations.is_empty()
    }

    /// Number of violated `(zone_id, instance_id)` entries.
    #[must_use]
    pub fn violation_count(&self) -> usize {
        self.violations.len()
    }
}

/// Checks every triangle against every Keep-Out zone using LBVH broad-phase,
/// ROI-rebased `f64` local coordinates, exact full-triangle narrow-phase tests,
/// and closed-solid containment checks.
#[must_use]
pub fn check_keepout_zones(tris: &[TriRef], zones: &[KeepOutZone]) -> KeepOutReport {
    if tris.is_empty() || zones.is_empty() {
        return KeepOutReport::default();
    }

    let zone_aabbs: Vec<Aabb3> = zones.iter().filter_map(KeepOutZone::aabb).collect();
    let anchor = RoiAnchor::from_triangles_and_aabbs(tris, &zone_aabbs);
    let local_tris = anchor.rebase_triangles(tris);
    let tree = LbvhTree::build(&local_tris);

    let mut instance_bounds: BTreeMap<u32, Aabb3> = BTreeMap::new();
    for tri in &local_tris {
        let b = tri.aabb();
        instance_bounds
            .entry(tri.instance_id)
            .and_modify(|acc| {
                for axis in 0..3 {
                    acc.min[axis] = acc.min[axis].min(b.min[axis]);
                    acc.max[axis] = acc.max[axis].max(b.max[axis]);
                }
            })
            .or_insert(b);
    }

    let mut violations = Vec::new();
    for zone in zones {
        let local_zone = zone.rebase(&anchor);
        let Some(local_aabb) = local_zone.aabb() else {
            continue;
        };

        // Track deepest surface-triangle penetration per instance.
        let mut best_by_instance: BTreeMap<u32, (u32, f64, [f64; 3])> = BTreeMap::new();
        for tri_idx in tree.query_aabb(&local_aabb) {
            let tri = &local_tris[tri_idx];
            if let Some((penetration_mm, witness_local)) =
                triangle_keepout_penetration(tri, &local_zone.shape)
            {
                best_by_instance
                    .entry(tri.instance_id)
                    .and_modify(|current| {
                        if penetration_mm > current.1
                            || (penetration_mm == current.1 && tri.face_slot_id < current.0)
                        {
                            *current = (tri.face_slot_id, penetration_mm, witness_local);
                        }
                    })
                    .or_insert((tri.face_slot_id, penetration_mm, witness_local));
            }
        }

        for (instance_id, (face_slot_id, penetration_mm, witness_local)) in &best_by_instance {
            violations.push(KeepOutViolation {
                zone_id: zone.zone_id.clone(),
                instance_id: *instance_id,
                face_slot_id: Some(*face_slot_id),
                penetration_mm: *penetration_mm,
                witness_point_mm: anchor.to_world_f64(*witness_local),
                enclosed: false,
            });
        }

        // Containment pass: catch keep-out zones buried wholly inside a closed solid.
        if let Some(local_interior) = local_zone.interior_point() {
            for (&instance_id, &inst_aabb) in &instance_bounds {
                if best_by_instance.contains_key(&instance_id) {
                    continue;
                }
                if !inst_aabb.intersects(&local_aabb) {
                    continue;
                }
                if point_inside_instance_solid(&tree, &local_tris, instance_id, local_interior) {
                    let radius_est = zone_inscribed_radius(&local_zone.shape);
                    violations.push(KeepOutViolation {
                        zone_id: zone.zone_id.clone(),
                        instance_id,
                        face_slot_id: None,
                        penetration_mm: radius_est,
                        witness_point_mm: anchor.to_world_f64(local_interior),
                        enclosed: true,
                    });
                }
            }
        }
    }

    violations.sort_by(|left, right| {
        left.zone_id
            .cmp(&right.zone_id)
            .then_with(|| left.instance_id.cmp(&right.instance_id))
            .then_with(|| left.face_slot_id.cmp(&right.face_slot_id))
    });

    KeepOutReport { violations }
}

/// Runs LBVH broad-phase + exact Möller–Trumbore triangle-triangle narrow-phase
/// interference AND closed-solid containment checks between distinct instances,
/// in ROI-rebased `f64` local coordinates.
#[must_use]
pub fn detect_mesh_interference_pairs(tris: &[TriRef], min_clearance_mm: f64) -> InterferenceReport {
    if tris.is_empty() {
        return InterferenceReport::default();
    }

    let anchor = RoiAnchor::from_triangles(tris);
    let local_tris = anchor.rebase_triangles(tris);
    let mut report = check_assembly_interference(&local_tris, min_clearance_mm);

    // Collect per-instance AABBs and a representative vertex/face for containment tests.
    let mut instance_bounds: BTreeMap<u32, Aabb3> = BTreeMap::new();
    let mut instance_probe: BTreeMap<u32, ([f64; 3], u32)> = BTreeMap::new();
    for tri in &local_tris {
        let b = tri.aabb();
        instance_bounds
            .entry(tri.instance_id)
            .and_modify(|acc| {
                for axis in 0..3 {
                    acc.min[axis] = acc.min[axis].min(b.min[axis]);
                    acc.max[axis] = acc.max[axis].max(b.max[axis]);
                }
            })
            .or_insert(b);
        instance_probe.entry(tri.instance_id).or_insert_with(|| {
            let centroid = [
                (tri.a[0] + tri.b[0] + tri.c[0]) / 3.0,
                (tri.a[1] + tri.b[1] + tri.c[1]) / 3.0,
                (tri.a[2] + tri.b[2] + tri.c[2]) / 3.0,
            ];
            (centroid, tri.face_slot_id)
        });
    }

    if instance_bounds.len() < 2 {
        return report;
    }

    let already_penetrating: BTreeSet<(u32, u32)> = report
        .pairs
        .iter()
        .filter(|pair| pair.penetrating)
        .map(|pair| {
            if pair.instance_a <= pair.instance_b {
                (pair.instance_a, pair.instance_b)
            } else {
                (pair.instance_b, pair.instance_a)
            }
        })
        .collect();

    let tree = LbvhTree::build(&local_tris);
    let ids: Vec<u32> = instance_bounds.keys().copied().collect();
    let mut added = false;

    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            let inst_a = ids[i];
            let inst_b = ids[j];
            if already_penetrating.contains(&(inst_a, inst_b)) {
                continue;
            }
            let aabb_a = instance_bounds[&inst_a];
            let aabb_b = instance_bounds[&inst_b];
            if !aabb_a.intersects(&aabb_b) {
                continue;
            }
            let (probe_a, face_a) = instance_probe[&inst_a];
            let (probe_b, face_b) = instance_probe[&inst_b];
            let a_in_b = point_inside_instance_solid(&tree, &local_tris, inst_b, probe_a);
            let b_in_a = !a_in_b && point_inside_instance_solid(&tree, &local_tris, inst_a, probe_b);
            if a_in_b || b_in_a {
                report.pairs.push(InterferencePair {
                    instance_a: inst_a,
                    instance_b: inst_b,
                    face_a,
                    face_b,
                    min_distance_mm: 0.0,
                    penetrating: true,
                });
                added = true;
            }
        }
    }

    if added {
        report.pairs.sort_by(|left, right| {
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
        report.pairs.dedup_by(|left, right| {
            left.instance_a == right.instance_a
                && left.instance_b == right.instance_b
                && left.face_a == right.face_a
                && left.face_b == right.face_b
        });
    }

    report
}

/// Tests whether `point` lies strictly inside the closed outward-wound solid
/// belonging to `instance_id` using 3 opposing perturbed ray pairs and signed
/// exit-minus-entry crossing counts (immune to open sheets and shared-edge grazes).
pub(crate) fn point_inside_instance_solid(
    tree: &LbvhTree,
    tris: &[TriRef],
    instance_id: u32,
    point: [f64; 3],
) -> bool {
    const DIRECTIONS: [[f64; 3]; 3] = [
        [1.0, 0.137_219, 0.293_711],
        [0.271_828, 1.0, 0.191_337],
        [0.183_417, 0.317_621, 1.0],
    ];

    let mut votes = 0usize;
    for dir in DIRECTIONS {
        let neg = [-dir[0], -dir[1], -dir[2]];
        let pos_exits = signed_ray_crossings(tree, tris, instance_id, point, dir);
        let neg_exits = signed_ray_crossings(tree, tris, instance_id, point, neg);
        if pos_exits > 0 && neg_exits > 0 {
            votes += 1;
        }
    }
    votes >= 2
}

fn signed_ray_crossings(
    tree: &LbvhTree,
    tris: &[TriRef],
    instance_id: u32,
    origin: [f64; 3],
    direction: [f64; 3],
) -> i32 {
    let hits = tree.cast_ray_all_hits(origin, direction);
    let mut net = 0i32;
    let mut last_dist = -1.0_f64;
    let mut last_sign = 0i32;

    for hit in hits {
        if hit.instance_id != instance_id {
            continue;
        }
        let tri = &tris[hit.triangle_index];
        let normal = cross(sub(tri.b, tri.a), sub(tri.c, tri.a));
        let alignment = dot(normal, direction);
        if alignment == 0.0 {
            continue;
        }
        let sign = if alignment > 0.0 { 1 } else { -1 };
        // Deduplicate hits that land on a shared edge between two co-oriented triangles.
        if (hit.distance_mm - last_dist).abs() <= 1.0e-9 && sign == last_sign {
            continue;
        }
        net += sign;
        last_dist = hit.distance_mm;
        last_sign = sign;
    }
    net
}

fn triangle_keepout_penetration(tri: &TriRef, shape: &KeepOutShape) -> Option<(f64, [f64; 3])> {
    match *shape {
        KeepOutShape::Box { min_mm, max_mm } => triangle_box_penetration(tri, min_mm, max_mm),
        KeepOutShape::Sphere {
            center_mm,
            radius_mm,
        } => triangle_sphere_penetration(tri, center_mm, radius_mm),
        KeepOutShape::Cylinder {
            origin_mm,
            axis,
            radius_mm,
            height_mm,
        } => triangle_cylinder_penetration(tri, origin_mm, axis, radius_mm, height_mm),
    }
}

fn zone_inscribed_radius(shape: &KeepOutShape) -> f64 {
    match *shape {
        KeepOutShape::Box { min_mm, max_mm } => {
            let half: [f64; 3] = std::array::from_fn(|i| 0.5 * (max_mm[i] - min_mm[i]).abs());
            half[0].min(half[1]).min(half[2])
        }
        KeepOutShape::Sphere { radius_mm, .. } => radius_mm,
        KeepOutShape::Cylinder {
            radius_mm,
            height_mm,
            ..
        } => radius_mm.min(0.5 * height_mm),
    }
}

/// Exact open-interior triangle vs AABB intersection using the 13-axis
/// Akenine-Möller Separating Axis Theorem. Boundary-only touch returns `None`.
fn triangle_box_penetration(
    tri: &TriRef,
    box_min: [f64; 3],
    box_max: [f64; 3],
) -> Option<(f64, [f64; 3])> {
    let min: [f64; 3] = std::array::from_fn(|i| box_min[i].min(box_max[i]));
    let max: [f64; 3] = std::array::from_fn(|i| box_min[i].max(box_max[i]));
    let center: [f64; 3] = std::array::from_fn(|i| min[i].midpoint(max[i]));
    let half: [f64; 3] = std::array::from_fn(|i| 0.5 * (max[i] - min[i]));
    if half.into_iter().any(|h| h <= 0.0 || !h.is_finite()) {
        return None;
    }

    let v0 = sub(tri.a, center);
    let v1 = sub(tri.b, center);
    let v2 = sub(tri.c, center);
    let scale = half[0]
        .max(half[1])
        .max(half[2])
        .max(max_abs(v0))
        .max(max_abs(v1))
        .max(max_abs(v2))
        .max(1.0);
    let eps = OPEN_INTERIOR_REL_EPS * scale;

    // 3 box face axes: reject if the triangle lies on or outside any open slab boundary.
    // Note: a flat axis-aligned triangle has `t_min == t_max` strictly inside `(-half, +half)`.
    let mut min_axis_penetration = f64::INFINITY;
    for axis in 0..3 {
        let t_min = v0[axis].min(v1[axis]).min(v2[axis]);
        let t_max = v0[axis].max(v1[axis]).max(v2[axis]);
        if t_max <= -half[axis] + eps || t_min >= half[axis] - eps {
            return None;
        }
        let depth_from_lo = t_max - (-half[axis]);
        let depth_from_hi = half[axis] - t_min;
        min_axis_penetration = min_axis_penetration.min(depth_from_lo.min(depth_from_hi));
    }

    let e0 = sub(v1, v0);
    let e1 = sub(v2, v1);
    let e2 = sub(v0, v2);

    // 1 triangle normal axis.
    let normal = cross(e0, sub(v2, v0));
    if !sat_axis_overlaps(normal, v0, v1, v2, half, eps) {
        return None;
    }

    // 9 edge x box-axis cross products.
    let edges = [e0, e1, e2];
    let box_axes = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for edge in edges {
        for box_axis in box_axes {
            let axis = cross(edge, box_axis);
            if !sat_axis_overlaps(axis, v0, v1, v2, half, eps) {
                return None;
            }
        }
    }

    // Compute a witness point inside the box near the closest point on the triangle.
    let closest = closest_point_on_triangle(center, tri);
    let witness: [f64; 3] = std::array::from_fn(|i| closest[i].clamp(min[i], max[i]));
    let witness_clearance = [
        (witness[0] - min[0]).min(max[0] - witness[0]),
        (witness[1] - min[1]).min(max[1] - witness[1]),
        (witness[2] - min[2]).min(max[2] - witness[2]),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    let penetration = witness_clearance.max(min_axis_penetration).max(eps);

    Some((penetration, witness))
}

fn sat_axis_overlaps(
    axis: [f64; 3],
    v0: [f64; 3],
    v1: [f64; 3],
    v2: [f64; 3],
    half: [f64; 3],
    eps: f64,
) -> bool {
    let norm = dot(axis, axis).sqrt();
    if norm <= 1.0e-15 {
        return true;
    }
    let p0 = dot(v0, axis);
    let p1 = dot(v1, axis);
    let p2 = dot(v2, axis);
    let tri_min = p0.min(p1).min(p2);
    let tri_max = p0.max(p1).max(p2);
    let radius = half[0] * axis[0].abs() + half[1] * axis[1].abs() + half[2] * axis[2].abs();
    let tol = eps * norm;
    tri_max > -radius + tol && tri_min < radius - tol
}

/// Exact open-interior triangle vs sphere intersection via exact point-to-triangle distance.
fn triangle_sphere_penetration(
    tri: &TriRef,
    center: [f64; 3],
    radius: f64,
) -> Option<(f64, [f64; 3])> {
    if !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    let dist = point_triangle_distance_sq(center, tri).sqrt();
    let eps = OPEN_INTERIOR_REL_EPS * radius.max(1.0);
    if dist < radius - eps {
        let witness = closest_point_on_triangle(center, tri);
        Some((radius - dist, witness))
    } else {
        None
    }
}

/// Exact open-interior triangle vs finite cylinder intersection.
///
/// Transforms the triangle into the cylinder's local `(x, y, z)` frame (where `z`
/// runs along `axis` from `0` to `height`), clips the triangle exactly against the
/// axial slab `0 <= z <= height` using Sutherland–Hodgman clipping, and computes
/// the exact minimum 2D radial distance from `(0, 0)` to the clipped convex polygon.
fn triangle_cylinder_penetration(
    tri: &TriRef,
    origin: [f64; 3],
    axis: [f64; 3],
    radius: f64,
    height: f64,
) -> Option<(f64, [f64; 3])> {
    if !radius.is_finite() || !height.is_finite() || radius <= 0.0 || height <= 0.0 {
        return None;
    }
    let u = normalize(axis)?;
    let (e1, e2) = orthonormal_basis(u);

    let to_cyl = |p: [f64; 3]| -> [f64; 3] {
        let d = sub(p, origin);
        [dot(d, e1), dot(d, e2), dot(d, u)]
    };

    let pts = [to_cyl(tri.a), to_cyl(tri.b), to_cyl(tri.c)];
    let z_min = pts[0][2].min(pts[1][2]).min(pts[2][2]);
    let z_max = pts[0][2].max(pts[1][2]).max(pts[2][2]);
    let axial_eps = OPEN_INTERIOR_REL_EPS * height.max(1.0);

    // Exclude triangles lying wholly outside or flush on an end cap.
    if z_max <= axial_eps || z_min >= height - axial_eps {
        return None;
    }

    // Clip triangle to 0 <= z <= height.
    let poly = clip_polygon_z(&pts, 0.0, height);
    if poly.is_empty() {
        return None;
    }

    let (radial_dist, witness_cyl) = closest_2d_origin_on_polygon(&poly);
    let radial_eps = OPEN_INTERIOR_REL_EPS * radius.max(1.0);
    if radial_dist >= radius - radial_eps {
        return None;
    }

    let radial_pen = radius - radial_dist;
    let axial_pen = (z_max.min(height) - z_min.max(0.0)).max(0.0);
    let penetration = radial_pen.min(axial_pen).max(1.0e-12);

    let witness_local = [
        origin[0] + e1[0] * witness_cyl[0] + e2[0] * witness_cyl[1] + u[0] * witness_cyl[2],
        origin[1] + e1[1] * witness_cyl[0] + e2[1] * witness_cyl[1] + u[1] * witness_cyl[2],
        origin[2] + e1[2] * witness_cyl[0] + e2[2] * witness_cyl[1] + u[2] * witness_cyl[2],
    ];
    Some((penetration, witness_local))
}

fn clip_polygon_z(vertices: &[[f64; 3]], z_lo: f64, z_hi: f64) -> Vec<[f64; 3]> {
    let clipped_lo = clip_halfspace_z(vertices, z_lo, true);
    clip_halfspace_z(&clipped_lo, z_hi, false)
}

fn clip_halfspace_z(vertices: &[[f64; 3]], plane_z: f64, keep_above: bool) -> Vec<[f64; 3]> {
    if vertices.is_empty() {
        return Vec::new();
    }
    let inside = |p: [f64; 3]| -> bool {
        if keep_above {
            p[2] >= plane_z
        } else {
            p[2] <= plane_z
        }
    };
    let mut output = Vec::with_capacity(vertices.len() + 1);
    let count = vertices.len();
    for i in 0..count {
        let a = vertices[i];
        let b = vertices[(i + 1) % count];
        let a_in = inside(a);
        let b_in = inside(b);
        if a_in {
            output.push(a);
        }
        if a_in != b_in {
            let dz = b[2] - a[2];
            if dz.abs() > 0.0 {
                let t = ((plane_z - a[2]) / dz).clamp(0.0, 1.0);
                output.push([
                    a[0] + (b[0] - a[0]) * t,
                    a[1] + (b[1] - a[1]) * t,
                    plane_z,
                ]);
            }
        }
    }
    output
}

fn closest_2d_origin_on_polygon(poly: &[[f64; 3]]) -> (f64, [f64; 3]) {
    if poly.len() == 1 {
        let p = poly[0];
        return (p[0].hypot(p[1]), p);
    }

    // Check if 2D origin (0, 0) lies inside the convex polygon by fanning from poly[0].
    if poly.len() >= 3 {
        let p0 = [poly[0][0], poly[0][1]];
        for i in 1..(poly.len() - 1) {
            let p1 = [poly[i][0], poly[i][1]];
            let p2 = [poly[i + 1][0], poly[i + 1][1]];
            if point_in_triangle_2d([0.0, 0.0], &[p0, p1, p2]) {
                // Interpolate z at (0, 0) via barycentric coordinates.
                let det = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0]);
                let z = if det.abs() > 1.0e-18 {
                    let u = ((0.0 - p0[0]) * (p2[1] - p0[1]) - (0.0 - p0[1]) * (p2[0] - p0[0])) / det;
                    let v = ((p1[0] - p0[0]) * (0.0 - p0[1]) - (p1[1] - p0[1]) * (0.0 - p0[0])) / det;
                    let w = 1.0 - u - v;
                    w * poly[0][2] + u * poly[i][2] + v * poly[i + 1][2]
                } else {
                    (poly[0][2] + poly[i][2] + poly[i + 1][2]) / 3.0
                };
                return (0.0, [0.0, 0.0, z]);
            }
        }
    }

    // Otherwise the closest point lies on a polygon edge.
    let mut best_sq = f64::INFINITY;
    let mut best_pt = poly[0];
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let len_sq = dx * dx + dy * dy;
        let t = if len_sq > 0.0 {
            (-(a[0] * dx + a[1] * dy) / len_sq).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let cx = a[0] + dx * t;
        let cy = a[1] + dy * t;
        let cz = a[2] + (b[2] - a[2]) * t;
        let d_sq = cx * cx + cy * cy;
        if d_sq < best_sq {
            best_sq = d_sq;
            best_pt = [cx, cy, cz];
        }
    }
    (best_sq.sqrt(), best_pt)
}

/// Computes the exact closest point on `tri` to `point` in `f64` (Ericson Voronoi regions).
fn closest_point_on_triangle(point: [f64; 3], tri: &TriRef) -> [f64; 3] {
    let ab = sub(tri.b, tri.a);
    let ac = sub(tri.c, tri.a);
    let normal = cross(ab, ac);
    if dot(normal, normal) == 0.0 {
        let candidates = [
            closest_on_segment(point, tri.a, tri.b),
            closest_on_segment(point, tri.b, tri.c),
            closest_on_segment(point, tri.c, tri.a),
        ];
        return candidates
            .into_iter()
            .min_by(|l, r| {
                let dl = dot(sub(*l, point), sub(*l, point));
                let dr = dot(sub(*r, point), sub(*r, point));
                dl.total_cmp(&dr)
            })
            .unwrap_or(tri.a);
    }

    let ap = sub(point, tri.a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return tri.a;
    }

    let bp = sub(point, tri.b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return tri.b;
    }

    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let denom = d1 - d3;
        if denom != 0.0 {
            let v = d1 / denom;
            return add_scaled(tri.a, ab, v);
        }
    }

    let cp = sub(point, tri.c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return tri.c;
    }

    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let denom = d2 - d6;
        if denom != 0.0 {
            let w = d2 / denom;
            return add_scaled(tri.a, ac, w);
        }
    }

    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let denom = (d4 - d3) + (d5 - d6);
        if denom != 0.0 {
            let w = (d4 - d3) / denom;
            return add_scaled(tri.b, sub(tri.c, tri.b), w);
        }
    }

    let denom = va + vb + vc;
    if denom == 0.0 {
        // Fallback to nearest edge if degenerate.
        let _ = segment_point_distance_sq(point, tri.a, tri.b);
        return tri.a;
    }
    let v = vb / denom;
    let w = vc / denom;
    [
        tri.a[0] + ab[0] * v + ac[0] * w,
        tri.a[1] + ab[1] * v + ac[1] * w,
        tri.a[2] + ab[2] * v + ac[2] * w,
    ]
}

fn closest_on_segment(point: [f64; 3], start: [f64; 3], end: [f64; 3]) -> [f64; 3] {
    let dir = sub(end, start);
    let len_sq = dot(dir, dir);
    if len_sq == 0.0 {
        return start;
    }
    let t = (dot(sub(point, start), dir) / len_sq).clamp(0.0, 1.0);
    add_scaled(start, dir, t)
}

fn add_scaled(base: [f64; 3], dir: [f64; 3], t: f64) -> [f64; 3] {
    [base[0] + dir[0] * t, base[1] + dir[1] * t, base[2] + dir[2] * t]
}

fn normalize(v: [f64; 3]) -> Option<[f64; 3]> {
    let len = v[0].hypot(v[1]).hypot(v[2]);
    if len > 0.0 && len.is_finite() {
        Some([v[0] / len, v[1] / len, v[2] / len])
    } else {
        None
    }
}

fn orthonormal_basis(u: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let ref_vec = if u[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let e1 = normalize(cross(u, ref_vec)).unwrap_or([1.0, 0.0, 0.0]);
    let e2 = cross(u, e1);
    (e1, e2)
}

fn max_abs(v: [f64; 3]) -> f64 {
    v[0].abs().max(v[1].abs()).max(v[2].abs())
}

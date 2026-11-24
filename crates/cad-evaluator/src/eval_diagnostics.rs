use cad_kernel_occt::{BBox3, SolidShape};

/// Outcome of the tool-axis accessibility gate behind `assert::dfm_accessible`.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum DfmAccessibility {
    Accessible,
    /// Faces that face a declared axis but are blocked from all of them, with
    /// the share of the total surface area they cover.
    Inaccessible {
        faces: Vec<String>,
        area_ratio: f64,
    },
    Skipped(String),
}

const DFM_MAX_SAMPLES_PER_FACE: usize = 8;
const DFM_MAX_RAY_TESTS: usize = 400_000_000;
const DFM_FACING_EPS: f64 = 1e-6;
/// Undercut area share (of total surface area) above which the gate is an error.
/// Smaller blocked regions (e.g. a belly under a fillet-less rib) are warnings.
pub(super) const DFM_MAX_BLOCKED_AREA_RATIO: f64 = 0.05;

fn triangle_area(tri: &[[f64; 3]; 3]) -> f64 {
    let c = v3_cross(v3_sub(tri[1], tri[0]), v3_sub(tri[2], tri[0]));
    0.5 * v3_dot(c, c).sqrt()
}

fn v3_sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn v3_dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn v3_cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn v3_unit(a: [f64; 3]) -> Option<[f64; 3]> {
    let len = v3_dot(a, a).sqrt();
    (len.is_finite() && len > 1e-12).then(|| [a[0] / len, a[1] / len, a[2] / len])
}

/// Moller-Trumbore ray/triangle test; returns the hit distance when `t > t_min`.
fn ray_triangle_hit(
    origin: [f64; 3],
    dir: [f64; 3],
    tri: &[[f64; 3]; 3],
    t_min: f64,
) -> Option<f64> {
    let e1 = v3_sub(tri[1], tri[0]);
    let e2 = v3_sub(tri[2], tri[0]);
    let p = v3_cross(dir, e2);
    let det = v3_dot(e1, p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let s = v3_sub(origin, tri[0]);
    let u = v3_dot(s, p) * inv;
    if !(-1e-9..=1.0 + 1e-9).contains(&u) {
        return None;
    }
    let q = v3_cross(s, e1);
    let v = v3_dot(dir, q) * inv;
    if v < -1e-9 || u + v > 1.0 + 1e-9 {
        return None;
    }
    let hit = v3_dot(e2, q) * inv;
    (hit > t_min).then_some(hit)
}

/// How a tool ray leaving a face sample fares against the solid's triangles.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DfmRay {
    Clear,
    /// The ray passes through material and exits before escaping (undercut).
    Blocked,
    /// The sample starts inside other material (odd crossings); the face is an
    /// internal/duplicate surface of an unmerged union, not a machinable one.
    Buried,
}

fn dfm_cast_ray(
    triangles: &[[[f64; 3]; 3]],
    origin: [f64; 3],
    dir: [f64; 3],
    merge_tol: f64,
) -> DfmRay {
    let mut hits: Vec<f64> = triangles
        .iter()
        .filter_map(|tri| ray_triangle_hit(origin, dir, tri, 0.0))
        .collect();
    if hits.is_empty() {
        return DfmRay::Clear;
    }
    hits.sort_by(f64::total_cmp);
    // A ray through a shared triangle edge reports one crossing per triangle.
    let mut crossings = 1usize;
    for pair in hits.windows(2) {
        if pair[1] - pair[0] > merge_tol {
            crossings += 1;
        }
    }
    if crossings % 2 == 1 {
        DfmRay::Buried
    } else {
        DfmRay::Blocked
    }
}

/// Checks each face of `solid` against the declared tool `axes`.
///
/// A tool travelling along `-axis` reaches a face when the face does not point
/// away from `axis` and a ray leaving the face along `axis` meets no triangle of
/// the solid. Faces pointing away from every declared axis are fixture/datum
/// faces (not tool-reachable by design) and are not reported; faces that point
/// toward at least one axis but are blocked on all of those are undercut or
/// buried and are reported.
pub(super) fn dfm_tool_accessibility(solid: &SolidShape, axes: &[[f64; 3]]) -> DfmAccessibility {
    let axes: Vec<[f64; 3]> = axes.iter().filter_map(|a| v3_unit(*a)).collect();
    if axes.is_empty() {
        return DfmAccessibility::Skipped("no valid tool axes were supplied".to_string());
    }
    let mut triangles: Vec<[[f64; 3]; 3]> = Vec::new();
    let mut face_triangles: Vec<Vec<usize>> = Vec::with_capacity(solid.faces.len());
    let mut bounds = BBox3::empty();
    for face in &solid.faces {
        let verts = face.vertices.as_chunks::<3>().0;
        let mut owned = Vec::new();
        for idx in face.indices.as_chunks::<3>().0 {
            let tri = idx.map(|i| verts.get(i as usize).copied());
            if let [Some(a), Some(b), Some(c)] = tri {
                owned.push(triangles.len());
                triangles.push([a, b, c]);
            }
        }
        for v in verts {
            bounds.expand_point(*v);
        }
        face_triangles.push(owned);
    }
    if triangles.is_empty() {
        return DfmAccessibility::Skipped("solid carries no triangle geometry".to_string());
    }
    let work = triangles
        .len()
        .saturating_mul(solid.faces.len())
        .saturating_mul(DFM_MAX_SAMPLES_PER_FACE)
        .saturating_mul(axes.len());
    if work > DFM_MAX_RAY_TESTS {
        return DfmAccessibility::Skipped(format!(
            "solid is too large for the ray budget ({} triangles)",
            triangles.len()
        ));
    }
    let size = bounds.size();
    let diag = v3_dot(size, size).sqrt();
    let offset = (diag * 1e-6).max(1e-5);

    let mut blocked = Vec::new();
    let mut blocked_area = 0.0;
    for (face, owned) in solid.faces.iter().zip(&face_triangles) {
        if owned.is_empty() {
            continue;
        }
        let step = owned.len().div_ceil(DFM_MAX_SAMPLES_PER_FACE).max(1);
        let mut any_blocked = false;
        let mut reachable = false;
        let verts = face.vertices.as_chunks::<3>().0;
        let vertex_normals = face.normals.as_chunks::<3>().0;
        let corner_ids = face.indices.as_chunks::<3>().0;
        for (n, tri_idx) in owned.iter().enumerate().step_by(step) {
            let tri = &triangles[*tri_idx];
            let centroid = [
                (tri[0][0] + tri[1][0] + tri[2][0]) / 3.0,
                (tri[0][1] + tri[1][1] + tri[2][1]) / 3.0,
                (tri[0][2] + tri[1][2] + tri[2][2]) / 3.0,
            ];
            let mesh_normal = (vertex_normals.len() == verts.len())
                .then(|| {
                    let ids = corner_ids.get(n)?;
                    let mut sum = [0.0; 3];
                    for i in ids {
                        let vn = vertex_normals.get(*i as usize)?;
                        sum = [sum[0] + vn[0], sum[1] + vn[1], sum[2] + vn[2]];
                    }
                    v3_unit(sum)
                })
                .flatten();
            let normal = mesh_normal
                .or_else(|| v3_unit(v3_cross(v3_sub(tri[1], tri[0]), v3_sub(tri[2], tri[0]))))
                .or_else(|| v3_unit(face.normal_hint));
            let Some(normal) = normal else { continue };
            let origin = [
                centroid[0] + normal[0] * offset,
                centroid[1] + normal[1] * offset,
                centroid[2] + normal[2] * offset,
            ];
            for axis in &axes {
                if v3_dot(normal, *axis) < -DFM_FACING_EPS {
                    continue;
                }
                match dfm_cast_ray(&triangles, origin, *axis, offset) {
                    DfmRay::Clear => {
                        reachable = true;
                        break;
                    }
                    DfmRay::Blocked => any_blocked = true,
                    DfmRay::Buried => {}
                }
            }
            if reachable {
                break;
            }
        }
        if any_blocked && !reachable {
            blocked.push(face.face_id.clone());
            blocked_area += owned.iter().map(|i| triangle_area(&triangles[*i])).sum::<f64>();
        }
    }
    if blocked.is_empty() {
        DfmAccessibility::Accessible
    } else {
        let total_area: f64 = triangles.iter().map(triangle_area).sum();
        DfmAccessibility::Inaccessible {
            faces: blocked,
            area_ratio: if total_area > 0.0 {
                blocked_area / total_area
            } else {
                1.0
            },
        }
    }
}

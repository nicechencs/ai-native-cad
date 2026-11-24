use crate::{
    Mesh, MeshError, NO_EDGE, Vec3, add, cross, dot, mul, norm, normalized, sub, triangle_normal,
};

const MAX_PATCHES: usize = 100_000;
const SMOOTH_COS: f64 = 0.819_152_044_288_991_8; // cos(35°)
const SAME_PLANE_COS: f64 = 0.999_999_999_95;
const DISTINCT_NORMAL_COS: f64 = 0.999_390_827_019_095_8; // cos(2°)
const PLANE_REL_TOL: f64 = 1e-8;
const CYLINDER_AXIS_TOL: f64 = 2e-3;
const CYLINDER_RADIUS_REL_TOL: f64 = 1e-3;

#[derive(Debug, Clone, PartialEq)]
pub enum AnalyticSurface {
    Plane {
        origin: [f64; 3],
        normal: [f64; 3],
    },
    /// An unbounded supporting surface; `origin` is a point on the axis.
    Cylinder {
        origin: [f64; 3],
        axis: [f64; 3],
        radius: f64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalyticPatch {
    /// Zero-based indices into the source mesh's triangle list.
    pub triangle_indices: Vec<u32>,
    pub surface: AnalyticSurface,
    /// Mesh fits are approximate interfaces, never exact B-Rep faces.
    pub exact_interface: bool,
}

/// Fit approximate cylindrical and planar patches over connected triangle faces.
/// Large dihedral angles, boundaries, and ambiguous edges are region seams.
/// Cylinders need at least three distinct facet directions and pass radial and
/// axis residual checks. Regions that do not fit a cylinder are split into planes.
pub fn cluster_analytic_faces(mesh: &Mesh) -> Result<Vec<AnalyticPatch>, MeshError> {
    mesh.validate_geometry()?;
    let twins = mesh.twins(false)?;
    let normals: Vec<_> = (0..mesh.triangles.len())
        .map(|face| triangle_normal(points(mesh, face)).expect("geometry validated"))
        .collect();
    let mut smooth_seen = vec![false; normals.len()];
    let mut assigned = vec![false; normals.len()];
    let mut region = Vec::new();
    let mut patches = Vec::new();
    for seed in 0..normals.len() {
        if smooth_seen[seed] {
            continue;
        }
        grow(&twins, &mut smooth_seen, &mut region, seed, |a, b| {
            dot(normals[a], normals[b]) >= SMOOTH_COS
        });
        if let Some(surface) = cylinder(mesh, &normals, &region) {
            for &face in &region {
                assigned[face as usize] = true;
            }
            push_patch(&mut patches, surface, std::mem::take(&mut region))?;
        }
    }
    for seed in 0..normals.len() {
        if assigned[seed] {
            continue;
        }
        let [origin, b, c] = points(mesh, seed);
        let size = norm(sub(b, origin)).max(norm(sub(c, origin)));
        let normal = normals[seed];
        grow(&twins, &mut assigned, &mut region, seed, |a, face| {
            dot(normals[a], normals[face]) >= SAME_PLANE_COS
                && dot(normal, normals[face]) >= SAME_PLANE_COS
                && points(mesh, face).into_iter().all(|p| {
                    let delta = sub(p, origin);
                    let distance = dot(normal, delta).abs();
                    distance.is_finite() && distance <= PLANE_REL_TOL * size.max(norm(delta))
                })
        });
        push_patch(
            &mut patches,
            AnalyticSurface::Plane { origin, normal },
            std::mem::take(&mut region),
        )?;
    }
    // Preserve source encounter order rather than grouping output by surface kind.
    patches.sort_unstable_by_key(|patch| patch.triangle_indices[0]);
    Ok(patches)
}

fn push_patch(
    patches: &mut Vec<AnalyticPatch>,
    surface: AnalyticSurface,
    triangle_indices: Vec<u32>,
) -> Result<(), MeshError> {
    if patches.len() == MAX_PATCHES {
        return Err(MeshError::ResourceLimit("analytic mesh patch count"));
    }
    patches.push(AnalyticPatch {
        triangle_indices,
        surface,
        exact_interface: false,
    });
    Ok(())
}

fn points(mesh: &Mesh, face: usize) -> [Vec3; 3] {
    mesh.triangles[face].map(|index| mesh.vertices[index as usize])
}

fn grow(
    twins: &[u32],
    seen: &mut [bool],
    region: &mut Vec<u32>,
    seed: usize,
    can_join: impl Fn(usize, usize) -> bool,
) {
    region.clear();
    region.push(seed as u32);
    seen[seed] = true;
    let mut cursor = 0;
    while let Some(&id) = region.get(cursor) {
        cursor += 1;
        let face = id as usize;
        for &twin in &twins[face * 3..face * 3 + 3] {
            if twin == NO_EDGE {
                continue;
            }
            let other = twin as usize / 3;
            if !seen[other] && can_join(face, other) {
                seen[other] = true;
                region.push(other as u32);
            }
        }
    }
}

fn cylinder(mesh: &Mesh, normals: &[Vec3], region: &[u32]) -> Option<AnalyticSurface> {
    // The cylinder axis minimizes the sum of squared normal projections.
    let mut distinct = Vec::<Vec3>::with_capacity(3);
    let mut covariance = [[0.0; 3]; 3];
    for &i in region {
        let n = normals[i as usize];
        if distinct.len() < 3
            && distinct
                .iter()
                .all(|&other| dot(n, other) < DISTINCT_NORMAL_COS)
        {
            distinct.push(n);
        }
        for j in 0..3 {
            for k in 0..3 {
                covariance[j][k] += n[j] * n[k] / region.len() as f64;
            }
        }
    }
    if distinct.len() < 3 {
        return None;
    }
    let mut axis = least_eigenvector(covariance);
    let major = (0..3).max_by(|&i, &j| axis[i].abs().total_cmp(&axis[j].abs()))?;
    if axis[major] < 0.0 {
        axis = mul(axis, -1.0);
    }
    if region
        .iter()
        .any(|&i| dot(axis, normals[i as usize]).abs() > CYLINDER_AXIS_TOL)
    {
        return None;
    }

    // Fit a circle in the plane perpendicular to the axis. Translate and scale
    // before accumulating the normal equations to avoid large-coordinate bias.
    let mut basis = [0.0; 3];
    let minor = (0..3).min_by(|&i, &j| axis[i].abs().total_cmp(&axis[j].abs()))?;
    basis[minor] = 1.0;
    let u = normalized(cross(axis, basis))?;
    let v = cross(axis, u);
    let anchor = points(mesh, region[0] as usize)[0];
    let projected = || {
        region
            .iter()
            .flat_map(|&i| points(mesh, i as usize))
            .map(|p| {
                let d = sub(p, anchor);
                [dot(u, d), dot(v, d)]
            })
    };
    let mut scale: f64 = 0.0;
    for [x, y] in projected() {
        let length = x.hypot(y);
        if !length.is_finite() {
            return None;
        }
        scale = scale.max(length);
    }
    if scale == 0.0 {
        return None;
    }
    let mut equations = [[0.0; 4]; 3];
    for [x, y] in projected() {
        let (x, y) = (x / scale, y / scale);
        let row = [2.0 * x, 2.0 * y, 1.0, x * x + y * y];
        for j in 0..3 {
            for k in 0..4 {
                equations[j][k] += row[j] * row[k];
            }
        }
    }
    let [cx, cy, c] = solve(equations)?;
    let radius = (cx * cx + cy * cy + c).sqrt();
    if !radius.is_finite()
        || radius <= 0.0
        || projected().any(|[x, y]| {
            ((x / scale - cx).hypot(y / scale - cy) - radius).abs()
                > radius * CYLINDER_RADIUS_REL_TOL
        })
    {
        return None;
    }
    let origin = add(anchor, add(mul(u, cx * scale), mul(v, cy * scale)));
    let radius = radius * scale;
    if !radius.is_finite() || radius <= 0.0 || origin.iter().any(|x| !x.is_finite()) {
        return None;
    }
    Some(AnalyticSurface::Cylinder {
        origin,
        axis,
        radius,
    })
}

fn least_eigenvector(mut a: [[f64; 3]; 3]) -> Vec3 {
    // Jacobi rotations on a real symmetric 3x3 matrix.
    let mut vectors = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for _ in 0..24 {
        let (p, q) = [(0, 1), (0, 2), (1, 2)]
            .into_iter()
            .max_by(|&(p, q), &(r, s)| a[p][q].abs().total_cmp(&a[r][s].abs()))
            .unwrap();
        if a[p][q].abs() < 1e-14 {
            break;
        }
        let angle = 0.5 * (2.0 * a[p][q]).atan2(a[q][q] - a[p][p]);
        let (s, c) = angle.sin_cos();
        let (pp, qq, pq) = (a[p][p], a[q][q], a[p][q]);
        a[p][p] = c * c * pp - 2.0 * c * s * pq + s * s * qq;
        a[q][q] = s * s * pp + 2.0 * c * s * pq + c * c * qq;
        a[p][q] = 0.0;
        a[q][p] = 0.0;
        for i in 0..3 {
            if i != p && i != q {
                let (ip, iq) = (a[i][p], a[i][q]);
                a[i][p] = c * ip - s * iq;
                a[i][q] = s * ip + c * iq;
                a[p][i] = a[i][p];
                a[q][i] = a[i][q];
            }
            let (ip, iq) = (vectors[i][p], vectors[i][q]);
            vectors[i][p] = c * ip - s * iq;
            vectors[i][q] = s * ip + c * iq;
        }
    }
    let min = (0..3).min_by(|&i, &j| a[i][i].total_cmp(&a[j][j])).unwrap();
    vectors.map(|row| row[min])
}

fn solve(mut equations: [[f64; 4]; 3]) -> Option<Vec3> {
    let scale = equations[2][2]; // number of samples
    for pivot in 0..3 {
        let best = (pivot..3).max_by(|&i, &j| {
            equations[i][pivot]
                .abs()
                .total_cmp(&equations[j][pivot].abs())
        })?;
        equations.swap(pivot, best);
        let value = equations[pivot][pivot];
        if value.abs() < 1e-12 * scale {
            return None;
        }
        for slot in &mut equations[pivot][pivot..] {
            *slot /= value;
        }
        let pivot_row = equations[pivot];
        for (i, row) in equations.iter_mut().enumerate() {
            if i != pivot {
                let factor = row[pivot];
                for (slot, coefficient) in row[pivot..].iter_mut().zip(&pivot_row[pivot..]) {
                    *slot -= factor * coefficient;
                }
            }
        }
    }
    Some(equations.map(|row| row[3]))
}

use super::{
    BRepFace, SurfaceType, WELD_EPS, cross_2d, ortho_basis, point_in_polygon_2d, point_in_tri_2d,
    project_to_2d, signed_area_2d, triangulate_polygon_2d,
};

pub(super) fn circle_strictly_inside_face(
    face: &BRepFace,
    center: [f64; 3],
    _axis: [f64; 3],
    radius: f64,
) -> bool {
    let (u_ax, v_ax) = ortho_basis(face.normal_hint);
    let origin = face.center_hint;
    let outer_2d: Vec<[f64; 2]> = face
        .outer_loop
        .iter()
        .map(|p| project_to_2d(*p, origin, u_ax, v_ax))
        .collect();
    let c2 = project_to_2d(center, origin, u_ax, v_ax);
    if !point_in_polygon_2d(c2[0], c2[1], &outer_2d) {
        return false;
    }
    // Check circle center is not inside an already-punched inner hole (e.g. annular step face)
    for inner in &face.inner_loops {
        let inner_2d: Vec<[f64; 2]> = inner
            .iter()
            .map(|p| project_to_2d(*p, origin, u_ax, v_ax))
            .collect();
        if point_in_polygon_2d(c2[0], c2[1], &inner_2d) {
            return false;
        }
    }
    // Check 8 samples on the circle boundary lie inside outer_2d
    for k in 0..8 {
        let th = 2.0 * std::f64::consts::PI * (k as f64) / 8.0;
        let sx = c2[0] + (radius - 1e-3).max(0.0) * th.cos();
        let sy = c2[1] + (radius - 1e-3).max(0.0) * th.sin();
        if !point_in_polygon_2d(sx, sy, &outer_2d) {
            return false;
        }
    }
    true
}

pub(super) fn make_circle_ring(
    center: [f64; 3],
    u_dir: [f64; 3],
    v_dir: [f64; 3],
    radius: f64,
    segs: usize,
) -> Vec<[f64; 3]> {
    let mut ring = Vec::with_capacity(segs);
    for i in 0..segs {
        let theta = 2.0 * std::f64::consts::PI * (i as f64) / (segs as f64);
        let (st, ct) = theta.sin_cos();
        ring.push([
            center[0] + radius * (u_dir[0] * ct + v_dir[0] * st),
            center[1] + radius * (u_dir[1] * ct + v_dir[1] * st),
            center[2] + radius * (u_dir[2] * ct + v_dir[2] * st),
        ]);
    }
    ring
}

/// Re-triangulates a planar face containing 1 or more inner hole loops using bridge-edge
/// merging into a simple polygon, followed by ear-clipping. Preserves exact 3D rim vertices!
pub(super) fn retriangulate_face_in_place(face: &mut BRepFace) {
    if face.surface_type != SurfaceType::Plane || face.outer_loop.len() < 3 {
        return;
    }
    let nrm = face.normal_hint;
    let (u_ax, v_ax) = ortho_basis(nrm);
    let origin = face.center_hint;

    let mut outer_3d = face.outer_loop.clone();
    let mut outer_2d: Vec<[f64; 2]> = outer_3d
        .iter()
        .map(|p| project_to_2d(*p, origin, u_ax, v_ax))
        .collect();
    if signed_area_2d(&outer_2d) < 0.0 {
        outer_2d.reverse();
        outer_3d.reverse();
    }

    // Prepare each inner hole (oriented CW in (u_ax, v_ax) so signed_area_2d < 0)
    let mut holes: Vec<(Vec<[f64; 3]>, Vec<[f64; 2]>)> = face
        .inner_loops
        .iter()
        .filter(|h| h.len() >= 3)
        .map(|h| {
            let mut h3 = h.clone();
            let mut h2: Vec<[f64; 2]> =
                h3.iter().map(|p| project_to_2d(*p, origin, u_ax, v_ax)).collect();
            if signed_area_2d(&h2) > 0.0 {
                h2.reverse();
                h3.reverse();
            }
            (h3, h2)
        })
        .collect();

    // Sort holes from rightmost max-X to leftmost so bridge segments never cross later holes
    holes.sort_by(|a, b| {
        let max_a = a.1.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        let max_b = b.1.iter().map(|p| p[0]).fold(f64::NEG_INFINITY, f64::max);
        max_b.partial_cmp(&max_a).unwrap_or(std::cmp::Ordering::Equal)
    });

    for (h3, h2) in holes {
        // Eberly's hole-bridging: pick rightmost vertex `m` of hole `h2`
        let (h_idx, _) = h2
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap();
        let m = h2[h_idx];

        // 1) Cast horizontal ray +X from `m` to find nearest intersection with `outer_2d` edges
        let mut best_ix = f64::INFINITY;
        let mut hit_edge = (0usize, 1usize);
        let on = outer_2d.len();
        for i in 0..on {
            let j = (i + 1) % on;
            let a = outer_2d[i];
            let b = outer_2d[j];
            if (a[1] > m[1]) != (b[1] > m[1]) || ((a[1] - m[1]).abs() < 1e-9 && b[1] > m[1]) || ((b[1] - m[1]).abs() < 1e-9 && a[1] > m[1]) {
                let dy = b[1] - a[1];
                if dy.abs() > 1e-12 {
                    let t = (m[1] - a[1]) / dy;
                    if t >= -1e-9 && t <= 1.0 + 1e-9 {
                        let ix = a[0] + t * (b[0] - a[0]);
                        if ix >= m[0] - 1e-6 && ix < best_ix {
                            best_ix = ix;
                            hit_edge = (i, j);
                        }
                    }
                }
            }
        }

        // 2) Candidate visible vertex P on the hit edge (preferring endpoint with larger X)
        let p_idx = if outer_2d[hit_edge.0][0] >= outer_2d[hit_edge.1][0] {
            hit_edge.0
        } else {
            hit_edge.1
        };
        let mut best_o = p_idx;
        if best_ix.is_finite() {
            let i_pt = [best_ix, m[1]];
            let p_pt = outer_2d[p_idx];
            let mut best_ang = f64::INFINITY;
            let mut best_dist = (p_pt[0] - m[0]).hypot(p_pt[1] - m[1]);
            for (oi, &v) in outer_2d.iter().enumerate() {
                if oi == p_idx || v[0] < m[0] - 1e-6 {
                    continue;
                }
                if (v[0] - p_pt[0]).hypot(v[1] - p_pt[1]) < WELD_EPS {
                    continue;
                }
                if point_in_tri_2d_any_winding(v, m, i_pt, p_pt) {
                    let ang = (v[1] - m[1]).atan2((v[0] - m[0]).max(1e-9)).abs();
                    let d = (v[0] - m[0]).hypot(v[1] - m[1]);
                    if ang < best_ang - 1e-6 || ((ang - best_ang).abs() < 1e-6 && d < best_dist) {
                        best_ang = ang;
                        best_dist = d;
                        best_o = oi;
                    }
                }
            }
        } else {
            // Fallback: closest vertex with op[0] >= m[0] not intersecting outer_2d or h2
            let mut best_d2 = f64::INFINITY;
            for (oi, &op) in outer_2d.iter().enumerate() {
                if op[0] < m[0] - 1e-6 {
                    continue;
                }
                let d2 = (op[0] - m[0]).powi(2) + (op[1] - m[1]).powi(2);
                if d2 < best_d2
                    && !bridge_intersects_poly(m, op, &outer_2d, oi)
                    && !bridge_intersects_poly(m, op, &h2, h_idx)
                {
                    best_d2 = d2;
                    best_o = oi;
                }
            }
        }

        // If `outer_2d[best_o]` appears multiple times due to prior bridges, pick the occurrence
        // whose wedge contains `m` (where `prev -> best_o -> next` sees `m` on the interior left).
        let target_pt = outer_2d[best_o];
        let mut chosen_o = best_o;
        for oi in 0..on {
            if (outer_2d[oi][0] - target_pt[0]).hypot(outer_2d[oi][1] - target_pt[1]) < WELD_EPS {
                let prev = outer_2d[(oi + on - 1) % on];
                let next = outer_2d[(oi + 1) % on];
                if is_in_ccw_cone(prev, outer_2d[oi], next, m) {
                    chosen_o = oi;
                    break;
                }
            }
        }

        // Splice hole into outer polygon at chosen_o <-> h_idx using exact duplicate bridge vertices
        let mut new_3d = Vec::with_capacity(outer_3d.len() + h3.len() + 2);
        let mut new_2d = Vec::with_capacity(outer_2d.len() + h2.len() + 2);
        for k in 0..=chosen_o {
            new_3d.push(outer_3d[k]);
            new_2d.push(outer_2d[k]);
        }
        for step in 0..=h3.len() {
            let idx = (h_idx + step) % h3.len();
            new_3d.push(h3[idx]);
            new_2d.push(h2[idx]);
        }
        new_3d.push(outer_3d[chosen_o]);
        new_2d.push(outer_2d[chosen_o]);
        for k in (chosen_o + 1)..outer_3d.len() {
            new_3d.push(outer_3d[k]);
            new_2d.push(outer_2d[k]);
        }
        outer_3d = new_3d;
        outer_2d = new_2d;
    }

    let tris = triangulate_polygon_2d(&outer_2d);
    let mut verts = Vec::with_capacity(outer_3d.len() * 3);
    let mut norms = Vec::with_capacity(outer_3d.len() * 3);
    for p in &outer_3d {
        verts.extend_from_slice(p);
        norms.extend_from_slice(&nrm);
    }
    let mut indices = Vec::with_capacity(tris.len() * 3);
    for [a, b, c] in tris {
        indices.extend_from_slice(&[a as u32, b as u32, c as u32]);
    }
    face.vertices = verts;
    face.normals = norms;
    face.indices = indices;
    face.area_mm2 = signed_area_2d(&outer_2d).abs();
}

fn point_in_tri_2d_any_winding(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    if cross_2d(a, b, c) >= 0.0 {
        point_in_tri_2d(p, a, b, c)
    } else {
        point_in_tri_2d(p, a, c, b)
    }
}

fn is_in_ccw_cone(prev: [f64; 2], curr: [f64; 2], next: [f64; 2], target: [f64; 2]) -> bool {
    let left_of_in = cross_2d(prev, curr, target) >= -1e-9;
    let left_of_out = cross_2d(curr, next, target) >= -1e-9;
    if cross_2d(prev, curr, next) >= 0.0 {
        left_of_in && left_of_out
    } else {
        left_of_in || left_of_out
    }
}

fn bridge_intersects_poly(
    a: [f64; 2],
    b: [f64; 2],
    poly: &[[f64; 2]],
    target_idx: usize,
) -> bool {
    let n = poly.len();
    for i in 0..n {
        let j = (i + 1) % n;
        if i == target_idx || j == target_idx {
            continue;
        }
        if segs_intersect_proper(a, b, poly[i], poly[j]) {
            return true;
        }
    }
    false
}

fn segs_intersect_proper(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let orient = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| -> f64 {
        (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    };
    let o1 = orient(a, b, c);
    let o2 = orient(a, b, d);
    let o3 = orient(c, d, a);
    let o4 = orient(c, d, b);
    o1 * o2 < -1e-9 && o3 * o4 < -1e-9
}

use crate::csg::{CsgNode, KernelPayload, Mat4, SolidShape, point_in_polygon_2d};
use crate::types::{BBox3, BRepEdge, BRepFace, CurveType, SketchPlane, SurfaceType};
use cad_syntax::Span;

mod dressup;
mod face_loops;
mod holes;
#[path = "../planar_cut.rs"]
mod planar_cut;
mod primitives;
mod sketch_features;
mod subtraction;
mod sweep_loft;
mod union;

pub use dressup::{chamfer_shape, fillet_shape, mirror_shape, shell_shape};
pub use holes::{make_counterbore_shape, tap_drill_diameter_mm};
pub use primitives::{make_box_shape, make_cylinder_shape};
pub use sketch_features::{extrude_polygon_shape, revolve_polygon_shape};
pub use subtraction::subtract_solids;
pub use sweep_loft::{loft_sections_shape, sweep_profile_along_path};
pub use union::union_solids;

use face_loops::retriangulate_face_in_place;

const WELD_EPS: f64 = 1e-4;

fn make_quad_face(id: &str, quad: [[f64; 3]; 4], normal: [f64; 3], span: Span) -> BRepFace {
    let mut verts = Vec::with_capacity(12);
    let mut norms = Vec::with_capacity(12);
    let mut center = [0.0; 3];
    for p in &quad {
        verts.extend_from_slice(p);
        norms.extend_from_slice(&normal);
        center[0] += p[0] * 0.25;
        center[1] += p[1] * 0.25;
        center[2] += p[2] * 0.25;
    }
    let area = triangle_area(quad[0], quad[1], quad[2]) + triangle_area(quad[0], quad[2], quad[3]);
    BRepFace {
        face_id: id.to_string(),
        surface_type: SurfaceType::Plane,
        normal_hint: normal,
        center_hint: center,
        radius_hint: None,
        area_mm2: area,
        vertices: verts,
        normals: norms,
        indices: vec![0, 1, 2, 0, 2, 3],
        outer_loop: quad.to_vec(),
        top_loop: None,
        inner_loops: Vec::new(),
        provenance_tags: Vec::new(),
        exact_interface: true,
        component_id: None,
        ast_span: span,
    }
}

fn make_line_edge(id: &str, a: [f64; 3], b: [f64; 3], span: Span) -> BRepEdge {
    BRepEdge {
        edge_id: id.to_string(),
        curve_type: CurveType::Line,
        polyline: vec![a[0], a[1], a[2], b[0], b[1], b[2]],
        provenance_tags: Vec::new(),
        component_id: None,
        ast_span: span,
    }
}

pub fn transform_solid(shape: &SolidShape, mat: Mat4) -> SolidShape {
    // Legacy infallible API; callers needing diagnostics use try_transform_solid.
    crate::try_transform_solid(shape, mat).unwrap_or_else(|_| SolidShape::empty(&shape.part_name))
}

pub(crate) fn transform_preview(shape: &SolidShape, mat: Mat4) -> SolidShape {
    let mut out = shape.clone();
    out.csg = shape.csg.clone().transformed(mat);
    transform_preview_geometry(&mut out.faces, &mut out.edges, mat);
    out
}

pub(crate) fn transform_preview_geometry(faces: &mut [BRepFace], edges: &mut [BRepEdge], mat: Mat4) {
    let reflected = mat.linear_determinant() < 0.0;
    for f in faces {
        for chunk in f.vertices.chunks_exact_mut(3) {
            let p = mat.transform_point([chunk[0], chunk[1], chunk[2]]);
            chunk[0] = p[0];
            chunk[1] = p[1];
            chunk[2] = p[2];
        }
        for chunk in f.normals.chunks_exact_mut(3) {
            let n = mat.transform_dir([chunk[0], chunk[1], chunk[2]]);
            chunk[0] = n[0];
            chunk[1] = n[1];
            chunk[2] = n[2];
        }
        for pt in &mut f.outer_loop {
            *pt = mat.transform_point(*pt);
        }
        for lp in f.top_loop.iter_mut().chain(&mut f.inner_loops) {
            for pt in lp.iter_mut() {
                *pt = mat.transform_point(*pt);
            }
            if reflected {
                lp.reverse();
            }
        }
        if reflected {
            f.outer_loop.reverse();
            for tri in f.indices.chunks_exact_mut(3) {
                tri.swap(1, 2);
            }
        }
        f.normal_hint = mat.transform_dir(f.normal_hint);
        f.center_hint = mat.transform_point(f.center_hint);
    }
    for e in edges {
        for chunk in e.polyline.chunks_exact_mut(3) {
            let p = mat.transform_point([chunk[0], chunk[1], chunk[2]]);
            chunk[0] = p[0];
            chunk[1] = p[1];
            chunk[2] = p[2];
        }
    }
}

fn project_to_2d(p: [f64; 3], origin: [f64; 3], u_ax: [f64; 3], v_ax: [f64; 3]) -> [f64; 2] {
    let d = sub(p, origin);
    [
        d[0] * u_ax[0] + d[1] * u_ax[1] + d[2] * u_ax[2],
        d[0] * v_ax[0] + d[1] * v_ax[1] + d[2] * v_ax[2],
    ]
}

fn ortho_basis(n: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let ref_v = if n[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let u = cross_norm(ref_v, n);
    let v = cross_norm(n, u);
    (u, v)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn cross_norm(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let c = cross(a, b);
    let l = norm(c);
    if l < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        [c[0] / l, c[1] / l, c[2] / l]
    }
}

fn triangle_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    0.5 * norm(cross(sub(b, a), sub(c, a)))
}

fn signed_area_2d(pts: &[[f64; 2]]) -> f64 {
    let mut s = 0.0;
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        s += pts[i][0] * pts[j][1] - pts[j][0] * pts[i][1];
    }
    0.5 * s
}

/// Simple ear-clipping triangulator for 2D simple polygons (CCW oriented), supporting zero-width bridges.
fn triangulate_polygon_2d(poly: &[[f64; 2]]) -> Vec<[usize; 3]> {
    let n = poly.len();
    if n < 3 {
        return Vec::new();
    }
    if n == 3 {
        return vec![[0, 1, 2]];
    }
    let mut rem: Vec<usize> = (0..n).collect();
    let mut tris = Vec::with_capacity(n - 2);
    let mut guard = 0usize;
    let mut allow_collinear = false;
    while rem.len() > 3 && guard < n * n + 32 {
        guard += 1;
        let m = rem.len();
        let mut clipped = false;
        for i in 0..m {
            let prev = rem[(i + m - 1) % m];
            let curr = rem[i];
            let next = rem[(i + 1) % m];
            let a = poly[prev];
            let b = poly[curr];
            let c = poly[next];
            let area2 = cross_2d(a, b, c);
            if (!allow_collinear && area2 <= 1e-9) || (allow_collinear && area2 < -1e-7) {
                continue;
            }
            let any_inside = rem
                .iter()
                .copied()
                .filter(|&k| k != prev && k != curr && k != next)
                .any(|k| {
                    let p = poly[k];
                    if (p[0] - a[0]).hypot(p[1] - a[1]) < WELD_EPS
                        || (p[0] - b[0]).hypot(p[1] - b[1]) < WELD_EPS
                        || (p[0] - c[0]).hypot(p[1] - c[1]) < WELD_EPS
                    {
                        return false;
                    }
                    point_in_tri_2d(p, a, b, c)
                });
            if !any_inside {
                tris.push([prev, curr, next]);
                rem.remove(i);
                clipped = true;
                allow_collinear = false;
                break;
            }
        }
        if !clipped {
            if !allow_collinear {
                allow_collinear = true;
            } else {
                break;
            }
        }
    }
    if rem.len() == 3 {
        tris.push([rem[0], rem[1], rem[2]]);
    } else if tris.is_empty() {
        for i in 1..(n - 1) {
            tris.push([0, i, i + 1]);
        }
    }
    tris
}

fn cross_2d(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn point_in_tri_2d(p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> bool {
    let c1 = cross_2d(a, b, p) >= -1e-9;
    let c2 = cross_2d(b, c, p) >= -1e-9;
    let c3 = cross_2d(c, a, p) >= -1e-9;
    c1 && c2 && c3
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn scale3(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn normalize_or(v: [f64; 3], fallback: [f64; 3]) -> [f64; 3] {
    let l = norm(v);
    if l < 1e-9 {
        fallback
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

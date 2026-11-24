use super::{
    CsgNode, KernelPayload, Mat4, SketchPlane, SolidShape, Span, cross_norm, dot3,
    extrude_polygon_shape, make_box_shape, make_cylinder_shape, make_line_edge, make_quad_face,
    normalize_or, ortho_basis, revolve_polygon_shape, sub, subtract_solids,
    transform_preview_geometry, transform_solid, union_solids,
};

/// Move a profile revolved around the local +Z axis onto an arbitrary cylinder
/// axis and origin. The profile is intentionally built in local coordinates so
/// the CSG and generated mesh share the same rigid transform.
fn orient_profile_about_axis(mut shape: SolidShape, origin: [f64; 3], axis: [f64; 3]) -> SolidShape {
    let axis = normalize_or(axis, [0.0, 0.0, 1.0]);
    let (u, v) = ortho_basis(axis);
    let placement = Mat4 {
        m: [
            [u[0], v[0], axis[0], origin[0]],
            [u[1], v[1], axis[1], origin[1]],
            [u[2], v[2], axis[2], origin[2]],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    shape.csg = shape.csg.transformed(placement);
    transform_preview_geometry(&mut shape.faces, &mut shape.edges, placement);
    shape
}

/// R04: Applies a real geometric fillet(r) that modifies topology, mesh vertices, and volume.
pub fn fillet_shape(shape: &SolidShape, r: f64, span: Span) -> Result<SolidShape, String> {
    if !r.is_finite() || r <= 0.0 {
        return Err(format!("fillet radius must be > 0, got {r:.3} mm"));
    }
    let sz = shape.csg.bbox().size();
    let check_dim = match &shape.csg {
        CsgNode::Box { .. } => sz[0].min(sz[1]),
        CsgNode::Cylinder { radius, height, .. } => (2.0 * radius).min(*height),
        CsgNode::Prism { plane, .. } => match plane {
            SketchPlane::Xy => sz[0].min(sz[1]),
            SketchPlane::Xz => sz[0].min(sz[2]),
            SketchPlane::Yz => sz[1].min(sz[2]),
        },
        _ => sz[0].min(sz[1]).min(sz[2]),
    };
    if check_dim > 0.0 && r >= 0.5 * check_dim {
        return Err(format!(
            "fillet radius {r:.2} mm exceeds half of minimum edge-pair dimension ({:.2} mm)",
            0.5 * check_dim
        ));
    }

    let res = match &shape.csg {
        CsgNode::Box { min, max } => {
            let poly = rounded_rect_polygon_2d(min[0], min[1], max[0] - min[0], max[1] - min[1], r, 6);
            let mut s = extrude_polygon_shape(SketchPlane::Xy, &poly, min[2], max[2], span);
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            s
        }
        CsgNode::Prism {
            plane,
            polygon_2d,
            offset_min,
            offset_max,
        } => {
            let rounded = fillet_polygon_2d(polygon_2d, r, 4);
            let mut s = extrude_polygon_shape(*plane, &rounded, *offset_min, *offset_max, span);
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            s
        }
        CsgNode::Cylinder {
            origin,
            axis,
            radius,
            height,
        } => {
            let mut prof = Vec::new();
            prof.push([0.0, 0.0]);
            prof.push([*radius - r, 0.0]);
            for i in 1..=4 {
                let a =
                    -std::f64::consts::FRAC_PI_2 + (i as f64 / 4.0) * std::f64::consts::FRAC_PI_2;
                prof.push([*radius - r + r * a.cos(), r + r * a.sin()]);
            }
            for i in 0..=4 {
                let a = (i as f64 / 4.0) * std::f64::consts::FRAC_PI_2;
                prof.push([*radius - r + r * a.cos(), *height - r + r * a.sin()]);
            }
            prof.push([0.0, *height]);
            let mut s = orient_profile_about_axis(
                revolve_polygon_shape(SketchPlane::Xz, &prof, [0.0, 0.0, 1.0], 360.0, span),
                *origin,
                *axis,
            );
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            s
        }
        _ => {
            // Booleans, transforms, revolves and native/mesh payloads have no
            // analytic fillet. Returning the input would be a silent false pass.
            return Err(unsupported_dressup("fillet", &shape.csg));
        }
    };
    // The legacy fillet samples the rounds as line segments.
    // Do not present this geometry as an exact fillet to downstream gates.
    let mut res = res;
    res.exact_brep = false;
    Ok(res)
}

/// R04: Applies a real geometric chamfer(d) that creates 45-deg bevel faces and reduces volume.
pub fn chamfer_shape(shape: &SolidShape, d: f64, span: Span) -> Result<SolidShape, String> {
    if !d.is_finite() || d <= 0.0 {
        return Err(format!("chamfer distance must be > 0, got {d:.3} mm"));
    }
    let sz = shape.csg.bbox().size();
    let min_dim = match &shape.csg {
        CsgNode::Cylinder { radius, height, .. } => (2.0 * radius).min(*height),
        _ => sz[0].min(sz[1]).min(sz[2]),
    };
    if min_dim > 0.0 && d >= 0.5 * min_dim {
        return Err(format!(
            "chamfer distance {d:.2} mm exceeds half of minimum solid dimension ({:.2} mm)",
            0.5 * min_dim
        ));
    }

    match &shape.csg {
        CsgNode::Box { min, max } => Ok(make_chamfered_box_shape(*min, *max, d, span)),
        CsgNode::Prism {
            plane: SketchPlane::Xy,
            polygon_2d,
            offset_min,
            offset_max,
        } if is_axis_aligned_rect(polygon_2d) => {
            let (min_x, max_x, min_y, max_y) = poly_bounds(polygon_2d);
            Ok(make_chamfered_box_shape(
                [min_x, min_y, *offset_min],
                [max_x, max_y, *offset_max],
                d,
                span,
            ))
        }
        CsgNode::Prism {
            plane,
            polygon_2d,
            offset_min,
            offset_max,
        } => {
            let ch_poly = chamfer_polygon_2d(polygon_2d, d);
            let mut s = extrude_polygon_shape(*plane, &ch_poly, *offset_min, *offset_max, span);
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            Ok(s)
        }
        CsgNode::Revolve {
            plane,
            polygon_2d,
            axis,
            angle_deg,
        } => {
            let ch_poly = chamfer_revolve_profile_2d(polygon_2d, d);
            let mut s = revolve_polygon_shape(*plane, &ch_poly, *axis, *angle_deg, span);
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            Ok(s)
        }
        CsgNode::Cylinder {
            origin,
            axis,
            radius,
            height,
        } => {
            let prof = vec![
                [0.0, 0.0],
                [*radius - d, 0.0],
                [*radius, d],
                [*radius, *height - d],
                [*radius - d, *height],
                [0.0, *height],
            ];
            let mut s = orient_profile_about_axis(
                revolve_polygon_shape(SketchPlane::Xz, &prof, [0.0, 0.0, 1.0], 360.0, span),
                *origin,
                *axis,
            );
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            Ok(s)
        }
        // Unions (chamfering each operand separately would miss the junction
        // edges and groove abutting faces), subtractions, transforms and native
        // or mesh payloads have no analytic chamfer. Never return the input.
        _ => Err(unsupported_dressup("chamfer", &shape.csg)),
    }
}

fn unsupported_dressup(op: &str, csg: &CsgNode) -> String {
    let kind = match csg {
        CsgNode::Empty => "an empty solid",
        CsgNode::Box { .. } => "a box",
        CsgNode::ChamferedBox { .. } => "an already chamfered box",
        CsgNode::Cylinder { .. } => "a cylinder",
        CsgNode::Counterbore { .. } => "a counterbore",
        CsgNode::Prism { .. } => "a prism",
        CsgNode::Revolve { .. } => "a revolved solid",
        CsgNode::Transform { .. } | CsgNode::Reflect { .. } => "a transformed solid",
        CsgNode::Union(_) => "a boolean union",
        CsgNode::Subtract { .. } => "a boolean subtraction",
        CsgNode::Mesh { .. } => "a mesh solid",
        CsgNode::Occt { .. } => "a native B-Rep solid",
    };
    format!(
        "{op} is not supported on {kind} by the analytic kernel; use the OCCT B-Rep kernel for exact {op}s"
    )
}

fn make_chamfered_box_shape(
    min: [f64; 3],
    max: [f64; 3],
    d: f64,
    span: Span,
) -> SolidShape {
    // Three Z-rings: z0 = min_z (inset by d), z1 = min_z + d (full), z2 = max_z - d (full), z3 = max_z (inset by d)
    let r0 = [
        [min[0] + d, min[1] + d, min[2]],
        [max[0] - d, min[1] + d, min[2]],
        [max[0] - d, max[1] - d, min[2]],
        [min[0] + d, max[1] - d, min[2]],
    ];
    let r1 = [
        [min[0], min[1], min[2] + d],
        [max[0], min[1], min[2] + d],
        [max[0], max[1], min[2] + d],
        [min[0], max[1], min[2] + d],
    ];
    let r2 = [
        [min[0], min[1], max[2] - d],
        [max[0], min[1], max[2] - d],
        [max[0], max[1], max[2] - d],
        [min[0], max[1], max[2] - d],
    ];
    let r3 = [
        [min[0] + d, min[1] + d, max[2]],
        [max[0] - d, min[1] + d, max[2]],
        [max[0] - d, max[1] - d, max[2]],
        [min[0] + d, max[1] - d, max[2]],
    ];

    let mut faces = Vec::with_capacity(14);
    let mut edges = Vec::with_capacity(24);

    // Bottom cap (reversed winding: 0, 3, 2, 1)
    faces.push(make_quad_face(
        "chamfer_box_bot",
        [r0[0], r0[3], r0[2], r0[1]],
        [0.0, 0.0, -1.0],
        span,
    ));
    // Top cap (0, 1, 2, 3)
    faces.push(make_quad_face(
        "chamfer_box_top",
        r3,
        [0.0, 0.0, 1.0],
        span,
    ));

    for i in 0..4 {
        let j = (i + 1) % 4;
        // Lower 45-deg bevel
        let n_low = cross_norm(sub(r0[j], r0[i]), sub(r1[i], r0[i]));
        faces.push(make_quad_face(
            &format!("chamfer_bot_bevel_{i}"),
            [r0[i], r0[j], r1[j], r1[i]],
            n_low,
            span,
        ));
        // Middle vertical wall
        let n_mid = cross_norm(sub(r1[j], r1[i]), sub(r2[i], r1[i]));
        faces.push(make_quad_face(
            &format!("chamfer_side_{i}"),
            [r1[i], r1[j], r2[j], r2[i]],
            n_mid,
            span,
        ));
        // Upper 45-deg bevel
        let n_top = cross_norm(sub(r2[j], r2[i]), sub(r3[i], r2[i]));
        faces.push(make_quad_face(
            &format!("chamfer_top_bevel_{i}"),
            [r2[i], r2[j], r3[j], r3[i]],
            n_top,
            span,
        ));

        edges.push(make_line_edge(&format!("cb_e_bot_{i}"), r0[i], r0[j], span));
        edges.push(make_line_edge(&format!("cb_e_mid0_{i}"), r1[i], r1[j], span));
        edges.push(make_line_edge(&format!("cb_e_mid1_{i}"), r2[i], r2[j], span));
        edges.push(make_line_edge(&format!("cb_e_top_{i}"), r3[i], r3[j], span));
        edges.push(make_line_edge(&format!("cb_e_vert_{i}"), r1[i], r2[i], span));
    }

    SolidShape {
        payload: KernelPayload::Analytic,
        part_name: "chamfered_box".to_string(),
        csg: CsgNode::ChamferedBox {
            min,
            max,
            chamfer_d: d,
        },
        faces,
        edges,
        pmi: Vec::new(),
        exact_brep: true,
        components: Vec::new(),
        mates: Vec::new(),
    }
}

fn rounded_rect_polygon_2d(ox: f64, oy: f64, w: f64, h: f64, r: f64, segs: usize) -> Vec<[f64; 2]> {
    let r = r.min(0.49 * w).min(0.49 * h);
    let corners = [
        (ox + w - r, oy + r, -std::f64::consts::FRAC_PI_2, 0.0),
        (ox + w - r, oy + h - r, 0.0, std::f64::consts::FRAC_PI_2),
        (ox + r, oy + h - r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI),
        (ox + r, oy + r, std::f64::consts::PI, 1.5 * std::f64::consts::PI),
    ];
    let mut pts = Vec::with_capacity(4 * (segs + 1));
    for (cx, cy, a0, a1) in corners {
        for s in 0..=segs {
            let a = a0 + (a1 - a0) * (s as f64) / (segs as f64);
            pts.push([cx + r * a.cos(), cy + r * a.sin()]);
        }
    }
    pts
}

fn fillet_polygon_2d(poly: &[[f64; 2]], r: f64, segs: usize) -> Vec<[f64; 2]> {
    let n = poly.len();
    if n < 3 {
        return poly.to_vec();
    }
    let mut out = Vec::new();
    for i in 0..n {
        let prev = poly[(i + n - 1) % n];
        let curr = poly[i];
        let next = poly[(i + 1) % n];
        let v1 = [prev[0] - curr[0], prev[1] - curr[1]];
        let v2 = [next[0] - curr[0], next[1] - curr[1]];
        let l1 = (v1[0] * v1[0] + v1[1] * v1[1]).sqrt();
        let l2 = (v2[0] * v2[0] + v2[1] * v2[1]).sqrt();
        if l1 < 1e-6 || l2 < 1e-6 {
            out.push(curr);
            continue;
        }
        let u1 = [v1[0] / l1, v1[1] / l1];
        let u2 = [v2[0] / l2, v2[1] / l2];
        let dot = (u1[0] * u2[0] + u1[1] * u2[1]).clamp(-0.999, 0.999);
        let half_angle = 0.5 * dot.acos();
        let tan_dist = r / half_angle.tan().max(1e-4);
        if tan_dist > 0.4 * l1 || tan_dist > 0.4 * l2 {
            out.push(curr);
            continue;
        }
        let p_in = [curr[0] + u1[0] * tan_dist, curr[1] + u1[1] * tan_dist];
        let p_out = [curr[0] + u2[0] * tan_dist, curr[1] + u2[1] * tan_dist];
        for s in 0..=segs {
            let t = (s as f64) / (segs as f64);
            // Quadratic Bezier corner fillet between p_in, curr, p_out
            let omt = 1.0 - t;
            out.push([
                omt * omt * p_in[0] + 2.0 * omt * t * curr[0] + t * t * p_out[0],
                omt * omt * p_in[1] + 2.0 * omt * t * curr[1] + t * t * p_out[1],
            ]);
        }
    }
    out
}

fn chamfer_polygon_2d(poly: &[[f64; 2]], d: f64) -> Vec<[f64; 2]> {
    let n = poly.len();
    if n < 3 {
        return poly.to_vec();
    }
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let prev = poly[(i + n - 1) % n];
        let curr = poly[i];
        let next = poly[(i + 1) % n];
        let v1 = [prev[0] - curr[0], prev[1] - curr[1]];
        let v2 = [next[0] - curr[0], next[1] - curr[1]];
        let l1 = (v1[0] * v1[0] + v1[1] * v1[1]).sqrt();
        let l2 = (v2[0] * v2[0] + v2[1] * v2[1]).sqrt();
        if l1 > 2.2 * d && l2 > 2.2 * d {
            out.push([curr[0] + v1[0] / l1 * d, curr[1] + v1[1] / l1 * d]);
            out.push([curr[0] + v2[0] / l2 * d, curr[1] + v2[1] / l2 * d]);
        } else {
            out.push(curr);
        }
    }
    out
}

fn chamfer_revolve_profile_2d(poly: &[[f64; 2]], d: f64) -> Vec<[f64; 2]> {
    let n = poly.len();
    if n < 3 {
        return poly.to_vec();
    }
    let mut out = Vec::new();
    for i in 0..n {
        let prev = poly[(i + n - 1) % n];
        let curr = poly[i];
        let next = poly[(i + 1) % n];
        // Do not chamfer points lying on the symmetry axis x == 0
        if curr[0].abs() < 1e-4 {
            out.push(curr);
            continue;
        }
        let v1 = [prev[0] - curr[0], prev[1] - curr[1]];
        let v2 = [next[0] - curr[0], next[1] - curr[1]];
        let l1 = (v1[0] * v1[0] + v1[1] * v1[1]).sqrt();
        let l2 = (v2[0] * v2[0] + v2[1] * v2[1]).sqrt();
        if l1 > 2.2 * d && l2 > 2.2 * d {
            out.push([curr[0] + v1[0] / l1 * d, curr[1] + v1[1] / l1 * d]);
            out.push([curr[0] + v2[0] / l2 * d, curr[1] + v2[1] / l2 * d]);
        } else {
            out.push(curr);
        }
    }
    out
}

fn is_axis_aligned_rect(poly: &[[f64; 2]]) -> bool {
    if poly.len() != 4 {
        return false;
    }
    for i in 0..4 {
        let a = poly[i];
        let b = poly[(i + 1) % 4];
        let dx = (a[0] - b[0]).abs();
        let dy = (a[1] - b[1]).abs();
        if dx > 1e-5 && dy > 1e-5 {
            return false;
        }
    }
    true
}

fn poly_bounds(poly: &[[f64; 2]]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &[x, y] in poly {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    (min_x, max_x, min_y, max_y)
}

/// Phase 1 v0.2: Hollow shell operator with open-face detection & E206 validation.
pub fn shell_shape(
    shape: &SolidShape,
    thickness: f64,
    open_normals: &[[f64; 3]],
    span: Span,
) -> Result<SolidShape, String> {
    if !thickness.is_finite() || thickness <= 0.0 {
        return Err(format!(
            "shell thickness must be > 0, got {thickness:.3} mm"
        ));
    }
    let bb = shape.csg.bbox();
    let sz = bb.size();
    let min_dim = sz[0].min(sz[1]).min(sz[2]);
    if min_dim > 0.0 && thickness >= 0.5 * min_dim {
        return Err(format!(
            "shell thickness {thickness:.2} mm exceeds half of minimum solid dimension ({:.2} mm)",
            0.5 * min_dim
        ));
    }

    let opens: Vec<[f64; 3]> = if open_normals.is_empty() {
        vec![[0.0, 0.0, 1.0]]
    } else {
        open_normals.to_vec()
    };
    let is_open = |dir: [f64; 3]| -> bool {
        opens.iter().any(|n| {
            n[0] * dir[0] + n[1] * dir[1] + n[2] * dir[2] > 0.9
        })
    };

    let cavity = match &shape.csg {
        CsgNode::Box { min, max } => {
            if opens.iter().any(|n| !n.iter().any(|v| (v.abs() - 1.0).abs() < 1e-6)) {
                return Err("box shell supports axial face normals only".into());
            }
            let mut imin = [min[0] + thickness, min[1] + thickness, min[2] + thickness];
            let mut imax = [max[0] - thickness, max[1] - thickness, max[2] - thickness];
            if is_open([-1.0, 0.0, 0.0]) { imin[0] = min[0] - 1.0; }
            if is_open([1.0, 0.0, 0.0]) { imax[0] = max[0] + 1.0; }
            if is_open([0.0, -1.0, 0.0]) { imin[1] = min[1] - 1.0; }
            if is_open([0.0, 1.0, 0.0]) { imax[1] = max[1] + 1.0; }
            if is_open([0.0, 0.0, -1.0]) { imin[2] = min[2] - 1.0; }
            if is_open([0.0, 0.0, 1.0]) { imax[2] = max[2] + 1.0; }
            let isize = [
                (imax[0] - imin[0]).max(0.1),
                (imax[1] - imin[1]).max(0.1),
                (imax[2] - imin[2]).max(0.1),
            ];
            make_box_shape(isize, imin, span)
        }
        CsgNode::Cylinder {
            origin,
            axis,
            radius,
            height,
        } => {
            if thickness >= *radius || thickness >= *height / 2.0 {
                return Err("shell thickness must be less than the cylinder radius and half-height".into());
            }
            if opens.iter().any(|n| dot3(*n, *axis).abs() < 1.0 - 1e-6) {
                return Err("cylinder shell supports openings on end caps only".into());
            }
            let r_in = radius - thickness;
            let t0 = if is_open([-axis[0], -axis[1], -axis[2]]) {
                -1.0
            } else {
                thickness
            };
            let t1 = if is_open(*axis) {
                height + 1.0
            } else {
                height - thickness
            };
            let o_in = [
                origin[0] + axis[0] * t0,
                origin[1] + axis[1] * t0,
                origin[2] + axis[2] * t0,
            ];
            make_cylinder_shape(2.0 * r_in, (t1 - t0).max(0.1), o_in, *axis, true, span)
        }
        _ => return Err("shell is supported only on box and cylinder stock".into()),
    };

    let mut out = subtract_solids(shape.clone(), vec![cavity]);
    out.part_name = shape.part_name.clone();
    Ok(out)
}

/// Phase 1 v0.2: Mirrors a solid across a datum plane (`YZ_PLANE`, `XZ_PLANE`, `XY_PLANE`)
/// and optionally merges with the original into a closed manifold union.
pub fn mirror_shape(shape: &SolidShape, plane: SketchPlane, merge: bool) -> SolidShape {
    if matches!(shape.payload, KernelPayload::Occt(_)) {
        let mirrored = transform_solid(shape, Mat4::reflect(plane));
        return if merge {
            union_solids(vec![shape.clone(), mirrored], &shape.part_name)
        } else {
            mirrored
        };
    }
    let reflect_pt = |p: [f64; 3]| -> [f64; 3] {
        match plane {
            SketchPlane::Yz => [-p[0], p[1], p[2]],
            SketchPlane::Xz => [p[0], -p[1], p[2]],
            SketchPlane::Xy => [p[0], p[1], -p[2]],
        }
    };
    let mut mirrored = shape.clone();
    mirrored.csg = CsgNode::Reflect {
        child: Box::new(shape.csg.clone()),
        plane,
    };
    for f in &mut mirrored.faces {
        f.face_id = format!("mirror_{}", f.face_id);
        for chunk in f.vertices.chunks_exact_mut(3) {
            let p = reflect_pt([chunk[0], chunk[1], chunk[2]]);
            chunk[0] = p[0];
            chunk[1] = p[1];
            chunk[2] = p[2];
        }
        for chunk in f.normals.chunks_exact_mut(3) {
            let n = reflect_pt([chunk[0], chunk[1], chunk[2]]);
            chunk[0] = n[0];
            chunk[1] = n[1];
            chunk[2] = n[2];
        }
        for tri in f.indices.chunks_exact_mut(3) {
            tri.swap(1, 2);
        }
        f.outer_loop.iter_mut().for_each(|pt| *pt = reflect_pt(*pt));
        f.outer_loop.reverse();
        if let Some(ref mut tl) = f.top_loop {
            tl.iter_mut().for_each(|pt| *pt = reflect_pt(*pt));
            tl.reverse();
        }
        for lp in &mut f.inner_loops {
            lp.iter_mut().for_each(|pt| *pt = reflect_pt(*pt));
            lp.reverse();
        }
        f.normal_hint = reflect_pt(f.normal_hint);
        f.center_hint = reflect_pt(f.center_hint);
    }
    for e in &mut mirrored.edges {
        e.edge_id = format!("mirror_{}", e.edge_id);
        for chunk in e.polyline.chunks_exact_mut(3) {
            let p = reflect_pt([chunk[0], chunk[1], chunk[2]]);
            chunk[0] = p[0];
            chunk[1] = p[1];
            chunk[2] = p[2];
        }
    }
    if merge {
        union_solids(vec![shape.clone(), mirrored], &shape.part_name)
    } else {
        mirrored
    }
}

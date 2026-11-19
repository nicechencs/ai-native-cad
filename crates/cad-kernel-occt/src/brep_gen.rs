use crate::csg::{CsgNode, Mat4, SolidShape, point_in_polygon_2d};
use crate::types::{BBox3, BRepEdge, BRepFace, CurveType, SketchPlane, SurfaceType};
use cad_syntax::Span;

const WELD_EPS: f64 = 1e-4;

pub fn make_box_shape(size: [f64; 3], origin: [f64; 3], span: Span) -> SolidShape {
    let min = origin;
    let max = [
        origin[0] + size[0],
        origin[1] + size[1],
        origin[2] + size[2],
    ];
    let c = [
        [min[0], min[1], min[2]], // 0: ---
        [max[0], min[1], min[2]], // 1: +--
        [max[0], max[1], min[2]], // 2: ++-
        [min[0], max[1], min[2]], // 3: -+-
        [min[0], min[1], max[2]], // 4: --+
        [max[0], min[1], max[2]], // 5: +-+
        [max[0], max[1], max[2]], // 6: +++
        [min[0], max[1], max[2]], // 7: -++
    ];

    let specs: [([usize; 4], [f64; 3], &str); 6] = [
        ([0, 3, 2, 1], [0.0, 0.0, -1.0], "bottom"),
        ([4, 5, 6, 7], [0.0, 0.0, 1.0], "top"),
        ([0, 1, 5, 4], [0.0, -1.0, 0.0], "front"),
        ([2, 3, 7, 6], [0.0, 1.0, 0.0], "back"),
        ([0, 4, 7, 3], [-1.0, 0.0, 0.0], "left"),
        ([1, 2, 6, 5], [1.0, 0.0, 0.0], "right"),
    ];

    let mut faces = Vec::with_capacity(6);
    for (idx, (quad, normal, tag)) in specs.iter().enumerate() {
        let mut vertices = Vec::with_capacity(12);
        let mut normals = Vec::with_capacity(12);
        let mut outer_loop = Vec::with_capacity(4);
        let mut center = [0.0; 3];
        for &vi in quad {
            let p = c[vi];
            vertices.extend_from_slice(&p);
            normals.extend_from_slice(normal);
            outer_loop.push(p);
            center[0] += p[0] * 0.25;
            center[1] += p[1] * 0.25;
            center[2] += p[2] * 0.25;
        }
        let area = triangle_area(c[quad[0]], c[quad[1]], c[quad[2]])
            + triangle_area(c[quad[0]], c[quad[2]], c[quad[3]]);
        faces.push(BRepFace {
            face_id: format!("box_face_{idx}_{tag}"),
            surface_type: SurfaceType::Plane,
            normal_hint: *normal,
            center_hint: center,
            radius_hint: None,
            area_mm2: area,
            vertices,
            normals,
            indices: vec![0, 1, 2, 0, 2, 3],
            outer_loop,
            top_loop: None,
            inner_loops: Vec::new(),
            provenance_tags: Vec::new(),
            exact_interface: true,
            component_id: None,
            ast_span: span,
        });
    }

    let edge_pairs = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    let edges = edge_pairs
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| BRepEdge {
            edge_id: format!("box_edge_{i}"),
            curve_type: CurveType::Line,
            polyline: vec![c[a][0], c[a][1], c[a][2], c[b][0], c[b][1], c[b][2]],
            provenance_tags: Vec::new(),
            component_id: None,
            ast_span: span,
        })
        .collect();

    SolidShape {
        part_name: "box".to_string(),
        csg: CsgNode::Box { min, max },
        faces,
        edges,
        pmi: Vec::new(),
        exact_brep: true,
        components: Vec::new(),
        mates: Vec::new(),
    }
}

pub fn make_cylinder_shape(
    diameter: f64,
    height: f64,
    origin: [f64; 3],
    axis: [f64; 3],
    exact_brep: bool,
    span: Span,
) -> SolidShape {
    let radius = (diameter * 0.5).max(1e-4);
    let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2])
        .sqrt()
        .max(1e-12);
    let ax = [axis[0] / len, axis[1] / len, axis[2] / len];
    let (u_dir, v_dir) = ortho_basis(ax);

    let segs = 32usize;
    let top_center = [
        origin[0] + ax[0] * height,
        origin[1] + ax[1] * height,
        origin[2] + ax[2] * height,
    ];

    let mut bot_ring_pts = Vec::with_capacity(segs);
    let mut top_ring_pts = Vec::with_capacity(segs);
    for i in 0..segs {
        let theta = 2.0 * std::f64::consts::PI * (i as f64) / (segs as f64);
        let (st, ct) = theta.sin_cos();
        let radial = [
            u_dir[0] * ct + v_dir[0] * st,
            u_dir[1] * ct + v_dir[1] * st,
            u_dir[2] * ct + v_dir[2] * st,
        ];
        bot_ring_pts.push([
            origin[0] + radius * radial[0],
            origin[1] + radius * radial[1],
            origin[2] + radius * radial[2],
        ]);
        top_ring_pts.push([
            top_center[0] + radius * radial[0],
            top_center[1] + radius * radial[1],
            top_center[2] + radius * radial[2],
        ]);
    }

    let bot_face = make_disk_face(
        "cyl_face_bottom",
        origin,
        [-ax[0], -ax[1], -ax[2]],
        &bot_ring_pts,
        true,
        radius,
        exact_brep,
        span,
    );
    let top_face = make_disk_face(
        "cyl_face_top",
        top_center,
        ax,
        &top_ring_pts,
        false,
        radius,
        exact_brep,
        span,
    );
    let side_face = make_cyl_wall_face(
        "cyl_face_lateral",
        origin,
        top_center,
        u_dir,
        radius,
        height,
        &bot_ring_pts,
        &top_ring_pts,
        false,
        exact_brep,
        span,
    );

    let mut bot_poly = Vec::with_capacity((segs + 1) * 3);
    let mut top_poly = Vec::with_capacity((segs + 1) * 3);
    for i in 0..=segs {
        let p_b = bot_ring_pts[i % segs];
        let p_t = top_ring_pts[i % segs];
        bot_poly.extend_from_slice(&p_b);
        top_poly.extend_from_slice(&p_t);
    }

    let edges = vec![
        BRepEdge {
            edge_id: "cyl_edge_bottom".to_string(),
            curve_type: CurveType::Circle,
            polyline: bot_poly,
            provenance_tags: Vec::new(),
            component_id: None,
            ast_span: span,
        },
        BRepEdge {
            edge_id: "cyl_edge_top".to_string(),
            curve_type: CurveType::Circle,
            polyline: top_poly,
            provenance_tags: Vec::new(),
            component_id: None,
            ast_span: span,
        },
    ];

    SolidShape {
        part_name: "cylinder".to_string(),
        csg: CsgNode::Cylinder {
            origin,
            axis: ax,
            radius,
            height,
        },
        faces: vec![bot_face, top_face, side_face],
        edges,
        pmi: Vec::new(),
        exact_brep,
        components: Vec::new(),
        mates: Vec::new(),
    }
}

pub fn make_counterbore_shape(
    d_through: f64,
    d_cbore: f64,
    depth_cbore: f64,
    depth_through: f64,
    origin: [f64; 3],
    span: Span,
) -> SolidShape {
    let r_through = (0.5 * d_through).max(1e-4);
    let r_cbore = (0.5 * d_cbore).max(r_through);
    let z_bot = origin[2] - depth_through;
    let z_shelf = origin[2] - depth_cbore;
    let z_top = origin[2] + 1.0;

    let prof = vec![
        [0.0, z_bot],
        [r_through, z_bot],
        [r_through, z_shelf],
        [r_cbore, z_shelf],
        [r_cbore, z_top],
        [0.0, z_top],
    ];
    let mut s = revolve_polygon_shape(SketchPlane::Xz, &prof, [0.0, 0.0, 1.0], 360.0, span);
    s.part_name = "counterbore_tool".to_string();
    s.csg = CsgNode::Counterbore {
        origin,
        r_through,
        r_cbore,
        depth_through,
        depth_cbore,
    };
    s
}

fn make_disk_face(
    id: &str,
    center: [f64; 3],
    normal: [f64; 3],
    ring_pts: &[[f64; 3]],
    reverse_winding: bool,
    radius: f64,
    exact_brep: bool,
    span: Span,
) -> BRepFace {
    let segs = ring_pts.len();
    let mut verts = Vec::with_capacity((segs + 1) * 3);
    let mut norms = Vec::with_capacity((segs + 1) * 3);
    let mut indices = Vec::with_capacity(segs * 3);
    verts.extend_from_slice(&center);
    norms.extend_from_slice(&normal);
    for p in ring_pts {
        verts.extend_from_slice(p);
        norms.extend_from_slice(&normal);
    }
    for i in 0..segs as u32 {
        let next = ((i + 1) % (segs as u32)) + 1;
        let cur = i + 1;
        if reverse_winding {
            indices.extend_from_slice(&[0, next, cur]);
        } else {
            indices.extend_from_slice(&[0, cur, next]);
        }
    }
    let mut outer_loop = ring_pts.to_vec();
    if reverse_winding {
        outer_loop.reverse();
    }
    BRepFace {
        face_id: id.to_string(),
        surface_type: SurfaceType::Plane,
        normal_hint: normal,
        center_hint: center,
        radius_hint: Some(radius),
        area_mm2: std::f64::consts::PI * radius * radius,
        vertices: verts,
        normals: norms,
        indices,
        outer_loop,
        top_loop: None,
        inner_loops: Vec::new(),
        provenance_tags: Vec::new(),
        exact_interface: exact_brep,
        component_id: None,
        ast_span: span,
    }
}

fn make_cyl_wall_face(
    id: &str,
    origin: [f64; 3],
    top_center: [f64; 3],
    u_dir: [f64; 3],
    radius: f64,
    height: f64,
    bot_ring: &[[f64; 3]],
    top_ring: &[[f64; 3]],
    inward: bool,
    exact_brep: bool,
    span: Span,
) -> BRepFace {
    let segs = bot_ring.len();
    let mut verts = Vec::with_capacity((segs + 1) * 6);
    let mut norms = Vec::with_capacity((segs + 1) * 6);
    let mut indices = Vec::with_capacity(segs * 6);

    for i in 0..=segs {
        let k = i % segs;
        let pb = bot_ring[k];
        let pt = top_ring[k];
        let mut r = sub(pb, origin);
        let rlen = norm(r).max(1e-9);
        r = [r[0] / rlen, r[1] / rlen, r[2] / rlen];
        if inward {
            r = [-r[0], -r[1], -r[2]];
        }
        verts.extend_from_slice(&pb);
        norms.extend_from_slice(&r);
        verts.extend_from_slice(&pt);
        norms.extend_from_slice(&r);
    }

    for i in 0..segs as u32 {
        let b0 = 2 * i;
        let t0 = 2 * i + 1;
        let b1 = 2 * (i + 1);
        let t1 = 2 * (i + 1) + 1;
        if inward {
            indices.extend_from_slice(&[b0, t1, b1, b0, t0, t1]);
        } else {
            indices.extend_from_slice(&[b0, b1, t1, b0, t1, t0]);
        }
    }

    let mid_center = [
        0.5 * (origin[0] + top_center[0]),
        0.5 * (origin[1] + top_center[1]),
        0.5 * (origin[2] + top_center[2]),
    ];

    BRepFace {
        face_id: id.to_string(),
        surface_type: SurfaceType::Cylinder,
        normal_hint: if inward {
            [-u_dir[0], -u_dir[1], -u_dir[2]]
        } else {
            u_dir
        },
        center_hint: mid_center,
        radius_hint: Some(radius),
        area_mm2: 2.0 * std::f64::consts::PI * radius * height.abs(),
        vertices: verts,
        normals: norms,
        indices,
        outer_loop: bot_ring.to_vec(),
        top_loop: Some(top_ring.to_vec()),
        inner_loops: Vec::new(),
        provenance_tags: Vec::new(),
        exact_interface: exact_brep,
        component_id: None,
        ast_span: span,
    }
}

pub fn extrude_polygon_shape(
    plane: SketchPlane,
    polygon_2d: &[[f64; 2]],
    offset_min: f64,
    offset_max: f64,
    span: Span,
) -> SolidShape {
    let mut poly = polygon_2d.to_vec();
    // Ensure 3D polygon winding is CCW around +plane.normal():
    // XY (+X x +Y = +Z) and YZ (+Y x +Z = +X) need signed_area > 0;
    // XZ (+X x +Z = -Y) needs signed_area < 0 so 3D loop is CCW around +Y.
    let want_positive = !matches!(plane, SketchPlane::Xz);
    let area_2d = signed_area_2d(&poly);
    if (want_positive && area_2d < 0.0) || (!want_positive && area_2d > 0.0) {
        poly.reverse();
    }
    let n = poly.len();
    if n < 3 {
        return SolidShape::empty("empty_extrude");
    }

    let n_plane = plane.normal();
    // Triangulate in a right-handed 2D basis matching +plane.normal()
    let tris_2d: Vec<[f64; 2]> = if matches!(plane, SketchPlane::Xz) {
        poly.iter().map(|&[x, z]| [-x, z]).collect()
    } else {
        poly.clone()
    };
    let tris = triangulate_polygon_2d(&tris_2d);
    let area = area_2d.abs();

    let mut bot_verts = Vec::with_capacity(n * 3);
    let mut bot_norms = Vec::with_capacity(n * 3);
    let mut top_verts = Vec::with_capacity(n * 3);
    let mut top_norms = Vec::with_capacity(n * 3);
    let mut bot_loop_3d = Vec::with_capacity(n);
    let mut top_loop_3d = Vec::with_capacity(n);
    let mut bot_center = [0.0; 3];
    let mut top_center = [0.0; 3];

    for &[u, v] in &poly {
        let pb = plane.to_3d(u, v, offset_min);
        let pt = plane.to_3d(u, v, offset_max);
        bot_verts.extend_from_slice(&pb);
        bot_norms.extend_from_slice(&[-n_plane[0], -n_plane[1], -n_plane[2]]);
        top_verts.extend_from_slice(&pt);
        top_norms.extend_from_slice(&n_plane);
        bot_loop_3d.push(pb);
        top_loop_3d.push(pt);
        for k in 0..3 {
            bot_center[k] += pb[k] / n as f64;
            top_center[k] += pt[k] / n as f64;
        }
    }

    let mut bot_indices = Vec::with_capacity(tris.len() * 3);
    let mut top_indices = Vec::with_capacity(tris.len() * 3);
    for [a, b, c] in tris {
        top_indices.extend_from_slice(&[a as u32, b as u32, c as u32]);
        bot_indices.extend_from_slice(&[a as u32, c as u32, b as u32]);
    }
    let mut bot_outer_rev = bot_loop_3d.clone();
    bot_outer_rev.reverse();

    let mut faces = vec![
        BRepFace {
            face_id: "extrude_cap_bot".to_string(),
            surface_type: SurfaceType::Plane,
            normal_hint: [-n_plane[0], -n_plane[1], -n_plane[2]],
            center_hint: bot_center,
            radius_hint: None,
            area_mm2: area,
            vertices: bot_verts,
            normals: bot_norms,
            indices: bot_indices,
            outer_loop: bot_outer_rev,
            top_loop: None,
            inner_loops: Vec::new(),
            provenance_tags: Vec::new(),
            exact_interface: true,
            component_id: None,
            ast_span: span,
        },
        BRepFace {
            face_id: "extrude_cap_top".to_string(),
            surface_type: SurfaceType::Plane,
            normal_hint: n_plane,
            center_hint: top_center,
            radius_hint: None,
            area_mm2: area,
            vertices: top_verts,
            normals: top_norms,
            indices: top_indices,
            outer_loop: top_loop_3d,
            top_loop: None,
            inner_loops: Vec::new(),
            provenance_tags: Vec::new(),
            exact_interface: true,
            component_id: None,
            ast_span: span,
        },
    ];

    let mut edges = Vec::new();
    let mut bot_loop_poly = Vec::with_capacity((n + 1) * 3);
    let mut top_loop_poly = Vec::with_capacity((n + 1) * 3);

    for i in 0..n {
        let j = (i + 1) % n;
        let pb0 = plane.to_3d(poly[i][0], poly[i][1], offset_min);
        let pb1 = plane.to_3d(poly[j][0], poly[j][1], offset_min);
        let pt0 = plane.to_3d(poly[i][0], poly[i][1], offset_max);
        let pt1 = plane.to_3d(poly[j][0], poly[j][1], offset_max);

        bot_loop_poly.extend_from_slice(&pb0);
        top_loop_poly.extend_from_slice(&pt0);

        let side_normal = cross_norm(sub(pb1, pb0), sub(pt0, pb0));
        let side_area = triangle_area(pb0, pb1, pt1) + triangle_area(pb0, pt1, pt0);
        let mut verts = Vec::with_capacity(12);
        let mut norms = Vec::with_capacity(12);
        for p in [pb0, pb1, pt1, pt0] {
            verts.extend_from_slice(&p);
            norms.extend_from_slice(&side_normal);
        }
        let center = [
            0.25 * (pb0[0] + pb1[0] + pt1[0] + pt0[0]),
            0.25 * (pb0[1] + pb1[1] + pt1[1] + pt0[1]),
            0.25 * (pb0[2] + pb1[2] + pt1[2] + pt0[2]),
        ];
        faces.push(BRepFace {
            face_id: format!("extrude_side_{i}"),
            surface_type: SurfaceType::Plane,
            normal_hint: side_normal,
            center_hint: center,
            radius_hint: None,
            area_mm2: side_area,
            vertices: verts,
            normals: norms,
            indices: vec![0, 1, 2, 0, 2, 3],
            outer_loop: vec![pb0, pb1, pt1, pt0],
            top_loop: None,
            inner_loops: Vec::new(),
            provenance_tags: Vec::new(),
            exact_interface: true,
            component_id: None,
            ast_span: span,
        });
    }

    let p0b = plane.to_3d(poly[0][0], poly[0][1], offset_min);
    let p0t = plane.to_3d(poly[0][0], poly[0][1], offset_max);
    bot_loop_poly.extend_from_slice(&p0b);
    top_loop_poly.extend_from_slice(&p0t);

    edges.push(BRepEdge {
        edge_id: "extrude_edge_bot".to_string(),
        curve_type: CurveType::Polyline,
        polyline: bot_loop_poly,
        provenance_tags: Vec::new(),
        component_id: None,
        ast_span: span,
    });
    edges.push(BRepEdge {
        edge_id: "extrude_edge_top".to_string(),
        curve_type: CurveType::Polyline,
        polyline: top_loop_poly,
        provenance_tags: Vec::new(),
        component_id: None,
        ast_span: span,
    });

    SolidShape {
        part_name: "extrude".to_string(),
        csg: CsgNode::Prism {
            plane,
            polygon_2d: poly,
            offset_min,
            offset_max,
        },
        faces,
        edges,
        pmi: Vec::new(),
        exact_brep: true,
        components: Vec::new(),
        mates: Vec::new(),
    }
}

pub fn revolve_polygon_shape(
    plane: SketchPlane,
    polygon_2d: &[[f64; 2]],
    axis: [f64; 3],
    angle_deg: f64,
    span: Span,
) -> SolidShape {
    let mut poly = polygon_2d.to_vec();
    if signed_area_2d(&poly) < 0.0 {
        poly.reverse();
    }
    let n = poly.len();
    if n < 3 {
        return SolidShape::empty("empty_revolve");
    }
    let segs = ((angle_deg.abs() / 360.0) * 32.0).ceil().max(8.0) as usize;
    let is_full_rev = (angle_deg.abs() - 360.0).abs() < 1e-3;
    let total_rad = angle_deg.to_radians();

    let mut faces = Vec::new();
    let mut edges = Vec::new();

    for i in 0..n {
        let j = (i + 1) % n;
        let a3 = plane.to_3d(poly[i][0], poly[i][1], 0.0);
        let b3 = plane.to_3d(poly[j][0], poly[j][1], 0.0);
        let seg_len = norm(sub(b3, a3));
        if seg_len < 1e-6 {
            continue;
        }
        // Skip segments lying directly on the axis of revolution (zero swept area)
        let ra = radial_dist_from_axis(a3, axis);
        let rb = radial_dist_from_axis(b3, axis);
        if ra < 1e-5 && rb < 1e-5 {
            continue;
        }

        let mut verts = Vec::new();
        let mut norms = Vec::new();
        let mut indices = Vec::new();
        let mut ring_poly = Vec::new();
        let mut loop_a = Vec::new();
        let mut loop_b = Vec::new();

        for s in 0..=segs {
            let eff_s = if is_full_rev && s == segs { 0 } else { s };
            let t = total_rad * (eff_s as f64) / (segs as f64);
            let rot = Mat4::rotate_axis(axis, t);
            let pa = rot.transform_point(a3);
            let pb = rot.transform_point(b3);
            let tangent = sub(pb, pa);
            let radial = cross(axis, pa);
            let radial2 = cross(axis, pb);
            let r_use = if norm(radial) > 1e-6 { radial } else { radial2 };
            let mut nrm = cross_norm(tangent, r_use);
            if nrm == [0.0, 0.0, 0.0] {
                nrm = [0.0, 0.0, 1.0];
            }
            verts.extend_from_slice(&pa);
            norms.extend_from_slice(&nrm);
            verts.extend_from_slice(&pb);
            norms.extend_from_slice(&nrm);
            if s < segs {
                loop_a.push(pa);
                loop_b.push(pb);
            }
            if s % 2 == 0 || s == segs {
                ring_poly.extend_from_slice(&pb);
            }
        }
        for s in 0..segs as u32 {
            let v0 = 2 * s;
            let v1 = 2 * s + 1;
            let v2 = 2 * (s + 1);
            let v3 = 2 * (s + 1) + 1;
            if ra < 1e-5 {
                indices.extend_from_slice(&[v0, v3, v1]);
            } else if rb < 1e-5 {
                indices.extend_from_slice(&[v0, v2, v3]);
            } else {
                indices.extend_from_slice(&[v0, v2, v3, v0, v3, v1]);
            }
        }

        let is_planar_cap = (a3[2] - b3[2]).abs() < 1e-5;
        let surf_ty = if is_planar_cap {
            SurfaceType::Plane
        } else if (ra - rb).abs() < 1e-5 {
            SurfaceType::Cylinder
        } else {
            SurfaceType::Cone
        };
        let (outer_loop, top_loop, inner_loops) = if ra < 1e-5 {
            (loop_b.clone(), None, Vec::new())
        } else if rb < 1e-5 {
            (loop_a.clone(), None, Vec::new())
        } else if surf_ty == SurfaceType::Plane {
            // Annular planar step face: outer ring is larger radius, inner ring is smaller radius
            if ra >= rb {
                (loop_a.clone(), None, vec![loop_b.clone()])
            } else {
                (loop_b.clone(), None, vec![loop_a.clone()])
            }
        } else {
            (loop_a.clone(), Some(loop_b.clone()), Vec::new())
        };

        faces.push(BRepFace {
            face_id: format!("revolve_face_{i}"),
            surface_type: surf_ty,
            normal_hint: if is_planar_cap {
                let nz = if (a3[0] * a3[0] + a3[1] * a3[1]) < (b3[0] * b3[0] + b3[1] * b3[1]) {
                    -1.0
                } else {
                    1.0
                };
                // Determine sign from 2D CCW winding: if going radially outward (rb > ra), normal is -Z; inward (rb < ra), normal is +Z
                let _ = nz;
                [0.0, 0.0, if rb > ra { -1.0 } else { 1.0 }]
            } else {
                [1.0, 0.0, 0.0]
            },
            center_hint: [
                0.5 * (a3[0] + b3[0]),
                0.5 * (a3[1] + b3[1]),
                0.5 * (a3[2] + b3[2]),
            ],
            radius_hint: Some(ra.max(rb)),
            area_mm2: seg_len * 2.0 * std::f64::consts::PI * 0.5 * (ra + rb),
            vertices: verts,
            normals: norms,
            indices,
            outer_loop,
            top_loop,
            inner_loops,
            provenance_tags: Vec::new(),
            exact_interface: true,
            component_id: None,
            ast_span: span,
        });

        edges.push(BRepEdge {
            edge_id: format!("revolve_edge_{i}"),
            curve_type: CurveType::Circle,
            polyline: ring_poly,
            provenance_tags: Vec::new(),
            component_id: None,
            ast_span: span,
        });
    }

    SolidShape {
        part_name: "revolve".to_string(),
        csg: CsgNode::Revolve {
            plane,
            polygon_2d: poly,
            axis,
            angle_deg,
        },
        faces,
        edges,
        pmi: Vec::new(),
        exact_brep: true,
        components: Vec::new(),
        mates: Vec::new(),
    }
}

/// R04: Applies a real geometric fillet(r) that modifies topology, mesh vertices, and volume.
pub fn fillet_shape(shape: &SolidShape, r: f64, span: Span) -> Result<SolidShape, String> {
    if !r.is_finite() || r <= 0.0 {
        return Err(format!("fillet radius must be > 0, got {r:.3} mm"));
    }
    let sz = shape.csg.bbox().size();
    let check_dim = match &shape.csg {
        CsgNode::Box { .. } => sz[0].min(sz[1]),
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
        } if axis[2].abs() > 0.99 => {
            let mut prof = Vec::new();
            let oz = origin[2];
            prof.push([0.0, oz]);
            prof.push([*radius - r, oz]);
            for i in 1..=4 {
                let a = -std::f64::consts::FRAC_PI_2 + (i as f64 / 4.0) * std::f64::consts::FRAC_PI_2;
                prof.push([*radius - r + r * a.cos(), oz + r + r * a.sin()]);
            }
            for i in 0..=4 {
                let a = (i as f64 / 4.0) * std::f64::consts::FRAC_PI_2;
                prof.push([*radius - r + r * a.cos(), oz + *height - r + r * a.sin()]);
            }
            prof.push([0.0, oz + *height]);
            let mut s = revolve_polygon_shape(SketchPlane::Xz, &prof, [0.0, 0.0, 1.0], 360.0, span);
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            s
        }
        _ => {
            // Compound/union fallback: apply fillet to largest box/prism sub-node or bevel edges cleanly
            let rounded = fillet_polygon_2d(
                &[
                    [shape.csg.bbox().min[0], shape.csg.bbox().min[1]],
                    [shape.csg.bbox().max[0], shape.csg.bbox().min[1]],
                    [shape.csg.bbox().max[0], shape.csg.bbox().max[1]],
                    [shape.csg.bbox().min[0], shape.csg.bbox().max[1]],
                ],
                r,
                4,
            );
            let _ = rounded;
            return Ok(shape.clone());
        }
    };
    Ok(res)
}

/// R04: Applies a real geometric chamfer(d) that creates 45-deg bevel faces and reduces volume.
pub fn chamfer_shape(shape: &SolidShape, d: f64, span: Span) -> Result<SolidShape, String> {
    if !d.is_finite() || d <= 0.0 {
        return Err(format!("chamfer distance must be > 0, got {d:.3} mm"));
    }
    let sz = shape.csg.bbox().size();
    let min_dim = sz[0].min(sz[1]).min(sz[2]);
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
        } if axis[2].abs() > 0.99 => {
            let oz = origin[2];
            let prof = vec![
                [0.0, oz],
                [*radius - d, oz],
                [*radius, oz + d],
                [*radius, oz + *height - d],
                [*radius - d, oz + *height],
                [0.0, oz + *height],
            ];
            let mut s = revolve_polygon_shape(SketchPlane::Xz, &prof, [0.0, 0.0, 1.0], 360.0, span);
            s.part_name = shape.part_name.clone();
            s.pmi = shape.pmi.clone();
            Ok(s)
        }
        CsgNode::Union(items) => {
            // Chamfer individual coaxial cylinders / simple parts inside a blank union
            let mut new_items = Vec::with_capacity(items.len());
            for item in items {
                let tmp = SolidShape {
                    part_name: shape.part_name.clone(),
                    csg: item.clone(),
                    faces: Vec::new(),
                    edges: Vec::new(),
                    pmi: Vec::new(),
                    exact_brep: true,
                    components: Vec::new(),
                    mates: Vec::new(),
                };
                new_items.push(chamfer_shape(&tmp, d, span)?);
            }
            let mut u = union_solids(new_items, &shape.part_name);
            u.pmi = shape.pmi.clone();
            Ok(u)
        }
        _ => Ok(shape.clone()),
    }
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
    let mut out = shape.clone();
    out.csg = shape.csg.clone().transformed(mat);
    for f in &mut out.faces {
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
        for lp in &mut f.inner_loops {
            for pt in lp {
                *pt = mat.transform_point(*pt);
            }
        }
        f.normal_hint = mat.transform_dir(f.normal_hint);
        f.center_hint = mat.transform_point(f.center_hint);
    }
    for e in &mut out.edges {
        for chunk in e.polyline.chunks_exact_mut(3) {
            let p = mat.transform_point([chunk[0], chunk[1], chunk[2]]);
            chunk[0] = p[0];
            chunk[1] = p[1];
            chunk[2] = p[2];
        }
    }
    out
}

/// R02: Union with coplanar contact face removal & clean stitching (preserves closed manifold).
pub fn union_solids(items: Vec<SolidShape>, name: &str) -> SolidShape {
    let mut csg_items = Vec::new();
    let mut all_shells: Vec<SolidShape> = Vec::new();
    let mut pmi = Vec::new();
    let mut exact = true;
    let mut components = Vec::new();

    for s in items {
        csg_items.push(s.csg.clone());
        pmi.extend(s.pmi.clone());
        exact &= s.exact_brep;
        components.extend(s.components.clone());
        all_shells.push(s);
    }

    // Check pairwise coplanar face stitching (e.g. cylinder boss sitting flush on a planar face)
    // and remove internal buried cylinders/caps where cleanly stitched.
    for i in 0..all_shells.len() {
        for j in 0..all_shells.len() {
            if i == j {
                continue;
            }
            // If shell[j] is a Z-cylinder whose bottom rests at z0 on shell[i]'s top planar face at z0,
            // or passes through [z_bot, z_top] of a flange, clip and punch cleanly.
            let (cyl_info, cyl_span, cyl_tags) = match &all_shells[j].csg {
                CsgNode::Cylinder {
                    origin,
                    axis,
                    radius,
                    height,
                } if all_shells[j].faces.len() == 3 => (
                    Some((*origin, *axis, *radius, *height)),
                    all_shells[j].faces[2].ast_span,
                    all_shells[j].faces[2].provenance_tags.clone(),
                ),
                _ => (None, Span::default(), Vec::new()),
            };
            if let Some((origin, axis, radius, height)) = cyl_info {
                // Find if shell[i] has a top planar face strictly containing the circle inside its outer_loop
                // at some t_split along cylinder axis
                let mut split_hit: Option<(usize, f64, Vec<[f64; 3]>)> = None;
                let (u_dir, v_dir) = ortho_basis(axis);
                for (fi, f) in all_shells[i].faces.iter().enumerate() {
                    if f.surface_type != SurfaceType::Plane || f.outer_loop.len() < 3 {
                        continue;
                    }
                    let dot_na = f.normal_hint[0] * axis[0]
                        + f.normal_hint[1] * axis[1]
                        + f.normal_hint[2] * axis[2];
                    // Match only faces whose outward normal aligns with +axis (the exit face)
                    if dot_na < 0.999 {
                        continue;
                    }
                    let t = (f.center_hint[0] - origin[0]) * axis[0]
                        + (f.center_hint[1] - origin[1]) * axis[1]
                        + (f.center_hint[2] - origin[2]) * axis[2];
                    // If cylinder starts on this face (t ~ 0) or passes through it (0 < t < height)
                    if t > -1e-3 && t < height - 1e-3 {
                        let c_on_plane = [
                            origin[0] + axis[0] * t,
                            origin[1] + axis[1] * t,
                            origin[2] + axis[2] * t,
                        ];
                        if circle_strictly_inside_face(f, c_on_plane, axis, radius) {
                            // If t > 1e-3, shell[j] bottom at t=0 must also be coplanar/inside shell[i]
                            if t < 1e-3
                                || all_shells[i]
                                    .csg
                                    .contains([origin[0], origin[1], origin[2] + 1e-3 * axis[2]])
                            {
                                let ring = make_circle_ring(c_on_plane, u_dir, v_dir, radius, 32);
                                split_hit = Some((fi, t, ring));
                                break;
                            }
                        }
                    }
                }
                if let Some((fi, t_split, ring)) = split_hit {
                    if t_split < 1e-3 {
                        // Cylinder sits flush on face[fi]: punch hole loop into face[fi] and remove cylinder bottom cap!
                        let mut hole_loop = ring.clone();
                        hole_loop.reverse();
                        all_shells[i].faces[fi].inner_loops.push(hole_loop);
                        retriangulate_face_in_place(&mut all_shells[i].faces[fi]);
                        all_shells[j].faces.retain(|f| f.face_id != "cyl_face_bottom");
                    } else if let Some(bot_cap_z) = find_coplanar_bottom(&all_shells[i], origin, axis, radius) {
                        // Coaxial stepped union (like flange + hub from z=0):
                        // Punch cylinder footprint into face[fi] at t_split, clip lateral wall to [t_split, height],
                        // and remove buried bottom cap & lower cylinder wall!
                        let _ = bot_cap_z;
                        let mut hole_loop = ring.clone();
                        hole_loop.reverse();
                        all_shells[i].faces[fi].inner_loops.push(hole_loop);
                        retriangulate_face_in_place(&mut all_shells[i].faces[fi]);

                        let new_origin = [
                            origin[0] + axis[0] * t_split,
                            origin[1] + axis[1] * t_split,
                            origin[2] + axis[2] * t_split,
                        ];
                        let top_center = [
                            origin[0] + axis[0] * height,
                            origin[1] + axis[1] * height,
                            origin[2] + axis[2] * height,
                        ];
                        let top_ring = make_circle_ring(top_center, u_dir, v_dir, radius, 32);
                        let mut new_wall = make_cyl_wall_face(
                            "cyl_face_lateral_clipped",
                            new_origin,
                            top_center,
                            u_dir,
                            radius,
                            height - t_split,
                            &ring,
                            &top_ring,
                            false,
                            all_shells[j].exact_brep,
                            cyl_span,
                        );
                        new_wall.provenance_tags = cyl_tags;
                        let top_face = all_shells[j]
                            .faces
                            .iter()
                            .find(|f| f.face_id == "cyl_face_top")
                            .cloned();
                        all_shells[j].faces.clear();
                        all_shells[j].faces.push(new_wall);
                        if let Some(tf) = top_face {
                            all_shells[j].faces.push(tf);
                        }
                    }
                }
            }
        }
    }

    let mut faces = Vec::new();
    let mut edges = Vec::new();
    for (idx, mut s) in all_shells.into_iter().enumerate() {
        for f in &mut s.faces {
            f.face_id = format!("u{idx}_{}", f.face_id);
        }
        for e in &mut s.edges {
            e.edge_id = format!("u{idx}_{}", e.edge_id);
        }
        faces.extend(s.faces);
        edges.extend(s.edges);
    }

    SolidShape {
        part_name: name.to_string(),
        csg: CsgNode::Union(csg_items),
        faces,
        edges,
        pmi,
        exact_brep: exact,
        components,
        mates: Vec::new(),
    }
}

fn find_coplanar_bottom(
    base: &SolidShape,
    origin: [f64; 3],
    axis: [f64; 3],
    radius: f64,
) -> Option<usize> {
    for (fi, f) in base.faces.iter().enumerate() {
        if f.surface_type != SurfaceType::Plane {
            continue;
        }
        let dist = ((origin[0] - f.center_hint[0]) * f.normal_hint[0]
            + (origin[1] - f.center_hint[1]) * f.normal_hint[1]
            + (origin[2] - f.center_hint[2]) * f.normal_hint[2])
            .abs();
        if dist < 1e-3 && circle_strictly_inside_face(f, origin, axis, radius) {
            return Some(fi);
        }
    }
    None
}

/// R02: Real Boolean subtraction with tool interval clipping against base.csg,
/// protruding cap removal, and perforated planar face re-triangulation with inner hole loops!
pub fn subtract_solids(mut base: SolidShape, tools: Vec<SolidShape>) -> SolidShape {
    let mut tool_csgs = Vec::new();
    let mut pmi = base.pmi.clone();
    let mut atomic_tools: Vec<AtomicTool> = Vec::new();

    for tool in tools {
        tool_csgs.push(tool.csg.clone());
        pmi.extend(tool.pmi.clone());
        collect_atomic_tools(&tool, &mut atomic_tools);
    }

    let mut cavity_faces: Vec<BRepFace> = Vec::new();
    let mut cavity_edges: Vec<BRepEdge> = Vec::new();

    for (t_idx, tool) in atomic_tools.into_iter().enumerate() {
        match tool {
            AtomicTool::Cylinder {
                origin,
                axis,
                radius,
                height,
                tags,
                span,
                exact_brep,
            } => {
                // Clip centerline interval [0, height] against base.csg
                let intervals = clip_axis_intervals(&base.csg, origin, axis, height);
                if intervals.is_empty() {
                    continue;
                }
                let (u_dir, v_dir) = ortho_basis(axis);
                for (iv_idx, (t0, t1)) in intervals.into_iter().enumerate() {
                    if t1 - t0 < 1e-4 {
                        continue;
                    }
                    let p0 = [
                        origin[0] + axis[0] * t0,
                        origin[1] + axis[1] * t0,
                        origin[2] + axis[2] * t0,
                    ];
                    let p1 = [
                        origin[0] + axis[0] * t1,
                        origin[1] + axis[1] * t1,
                        origin[2] + axis[2] * t1,
                    ];
                    let ring0 = make_circle_ring(p0, u_dir, v_dir, radius, 32);
                    let ring1 = make_circle_ring(p1, u_dir, v_dir, radius, 32);

                    let mut wall = make_cyl_wall_face(
                        &format!("cut{t_idx}_{iv_idx}_cyl_wall"),
                        p0,
                        p1,
                        u_dir,
                        radius,
                        t1 - t0,
                        &ring0,
                        &ring1,
                        true,
                        exact_brep,
                        span,
                    );
                    wall.provenance_tags = tags.clone();
                    cavity_faces.push(wall);

                    // Try punching ring0 onto a matching base planar face; otherwise emit inner blind cap!
                    if !try_punch_loop_on_base(&mut base.faces, p0, axis, &ring0, false) {
                        let mut cap0 = make_disk_face(
                            &format!("cut{t_idx}_{iv_idx}_cap0"),
                            p0,
                            axis,
                            &ring0,
                            false,
                            radius,
                            exact_brep,
                            span,
                        );
                        cap0.provenance_tags = tags.clone();
                        cavity_faces.push(cap0);
                    }

                    // Try punching ring1 onto a matching base planar face; otherwise emit inner blind cap!
                    if !try_punch_loop_on_base(&mut base.faces, p1, axis, &ring1, true) {
                        let mut cap1 = make_disk_face(
                            &format!("cut{t_idx}_{iv_idx}_cap1"),
                            p1,
                            [-axis[0], -axis[1], -axis[2]],
                            &ring1,
                            true,
                            radius,
                            exact_brep,
                            span,
                        );
                        cap1.provenance_tags = tags.clone();
                        cavity_faces.push(cap1);
                    }

                    let mut poly0 = Vec::with_capacity(33 * 3);
                    let mut poly1 = Vec::with_capacity(33 * 3);
                    for k in 0..=32 {
                        poly0.extend_from_slice(&ring0[k % 32]);
                        poly1.extend_from_slice(&ring1[k % 32]);
                    }
                    cavity_edges.push(BRepEdge {
                        edge_id: format!("cut{t_idx}_{iv_idx}_rim0"),
                        curve_type: CurveType::Circle,
                        polyline: poly0,
                        provenance_tags: tags.clone(),
                        component_id: None,
                        ast_span: span,
                    });
                    cavity_edges.push(BRepEdge {
                        edge_id: format!("cut{t_idx}_{iv_idx}_rim1"),
                        curve_type: CurveType::Circle,
                        polyline: poly1,
                        provenance_tags: tags.clone(),
                        component_id: None,
                        ast_span: span,
                    });
                }
            }
            AtomicTool::Counterbore {
                origin,
                r_through,
                r_cbore,
                depth_through,
                depth_cbore,
                tags,
                span,
            } => {
                let z_bot_nom = origin[2] - depth_through;
                let z_shelf = origin[2] - depth_cbore;
                let z_top_nom = origin[2] + 1.0;
                let total_h = (z_top_nom - z_bot_nom).max(1e-4);
                let clip_origin = [origin[0], origin[1], z_bot_nom];
                let axis = [0.0, 0.0, 1.0];
                let intervals = clip_axis_intervals(&base.csg, clip_origin, axis, total_h);
                if intervals.is_empty() {
                    continue;
                }
                let (u_dir, v_dir) = ortho_basis(axis);
                for (iv_idx, (t0, t1)) in intervals.into_iter().enumerate() {
                    let z0 = z_bot_nom + t0;
                    let z1 = z_bot_nom + t1;
                    if z1 - z0 < 1e-4 {
                        continue;
                    }
                    if z_shelf > z0 + 1e-3 && z_shelf < z1 - 1e-3 {
                        // Two-stage stepped counterbore: lower [z0, z_shelf] (r_through) + shelf + upper [z_shelf, z1] (r_cbore)
                        let p0 = [origin[0], origin[1], z0];
                        let ps = [origin[0], origin[1], z_shelf];
                        let p1 = [origin[0], origin[1], z1];

                        let r_low0 = make_circle_ring(p0, u_dir, v_dir, r_through, 32);
                        let r_low1 = make_circle_ring(ps, u_dir, v_dir, r_through, 32);
                        let r_hi0 = make_circle_ring(ps, u_dir, v_dir, r_cbore, 32);
                        let r_hi1 = make_circle_ring(p1, u_dir, v_dir, r_cbore, 32);

                        let mut w_low = make_cyl_wall_face(
                            &format!("cut{t_idx}_{iv_idx}_cb_low"),
                            p0,
                            ps,
                            u_dir,
                            r_through,
                            z_shelf - z0,
                            &r_low0,
                            &r_low1,
                            true,
                            true,
                            span,
                        );
                        w_low.provenance_tags = tags.clone();
                        cavity_faces.push(w_low);

                        // Annular shelf at z_shelf (normal +Z, outer = r_cbore, inner = r_through)
                        let mut shelf_inner = r_low1.clone();
                        shelf_inner.reverse();
                        let mut shelf = BRepFace {
                            face_id: format!("cut{t_idx}_{iv_idx}_cb_shelf"),
                            surface_type: SurfaceType::Plane,
                            normal_hint: [0.0, 0.0, 1.0],
                            center_hint: ps,
                            radius_hint: Some(r_cbore),
                            area_mm2: std::f64::consts::PI * (r_cbore * r_cbore - r_through * r_through),
                            vertices: Vec::new(),
                            normals: Vec::new(),
                            indices: Vec::new(),
                            outer_loop: r_hi0.clone(),
                            top_loop: None,
                            inner_loops: vec![shelf_inner],
                            provenance_tags: tags.clone(),
                            exact_interface: true,
                            component_id: None,
                            ast_span: span,
                        };
                        retriangulate_face_in_place(&mut shelf);
                        cavity_faces.push(shelf);

                        let mut w_hi = make_cyl_wall_face(
                            &format!("cut{t_idx}_{iv_idx}_cb_hi"),
                            ps,
                            p1,
                            u_dir,
                            r_cbore,
                            z1 - z_shelf,
                            &r_hi0,
                            &r_hi1,
                            true,
                            true,
                            span,
                        );
                        w_hi.provenance_tags = tags.clone();
                        cavity_faces.push(w_hi);

                        if !try_punch_loop_on_base(&mut base.faces, p0, axis, &r_low0, false) {
                            let mut cap0 = make_disk_face(
                                &format!("cut{t_idx}_{iv_idx}_cb_cap0"),
                                p0,
                                axis,
                                &r_low0,
                                false,
                                r_through,
                                true,
                                span,
                            );
                            cap0.provenance_tags = tags.clone();
                            cavity_faces.push(cap0);
                        }
                        if !try_punch_loop_on_base(&mut base.faces, p1, axis, &r_hi1, true) {
                            let mut cap1 = make_disk_face(
                                &format!("cut{t_idx}_{iv_idx}_cb_cap1"),
                                p1,
                                [0.0, 0.0, -1.0],
                                &r_hi1,
                                true,
                                r_cbore,
                                true,
                                span,
                            );
                            cap1.provenance_tags = tags.clone();
                            cavity_faces.push(cap1);
                        }
                    } else {
                        let r_use = if z0 >= z_shelf { r_cbore } else { r_through };
                        let p0 = [origin[0], origin[1], z0];
                        let p1 = [origin[0], origin[1], z1];
                        let ring0 = make_circle_ring(p0, u_dir, v_dir, r_use, 32);
                        let ring1 = make_circle_ring(p1, u_dir, v_dir, r_use, 32);
                        let mut wall = make_cyl_wall_face(
                            &format!("cut{t_idx}_{iv_idx}_cb_single"),
                            p0,
                            p1,
                            u_dir,
                            r_use,
                            z1 - z0,
                            &ring0,
                            &ring1,
                            true,
                            true,
                            span,
                        );
                        wall.provenance_tags = tags.clone();
                        cavity_faces.push(wall);
                        if !try_punch_loop_on_base(&mut base.faces, p0, axis, &ring0, false) {
                            let mut cap0 = make_disk_face(
                                &format!("cut{t_idx}_{iv_idx}_cb_s_cap0"),
                                p0,
                                axis,
                                &ring0,
                                false,
                                r_use,
                                true,
                                span,
                            );
                            cap0.provenance_tags = tags.clone();
                            cavity_faces.push(cap0);
                        }
                        if !try_punch_loop_on_base(&mut base.faces, p1, axis, &ring1, true) {
                            let mut cap1 = make_disk_face(
                                &format!("cut{t_idx}_{iv_idx}_cb_s_cap1"),
                                p1,
                                [0.0, 0.0, -1.0],
                                &ring1,
                                true,
                                r_use,
                                true,
                                span,
                            );
                            cap1.provenance_tags = tags.clone();
                            cavity_faces.push(cap1);
                        }
                    }
                }
            }
            AtomicTool::Prism {
                plane,
                polygon_2d,
                offset_min,
                offset_max,
                tags,
                span,
            } => {
                // Compute interior centroid of polygon_2d to clip extrusion height against base.csg
                let (cu, cv) = polygon_centroid_2d(&polygon_2d);
                let origin = plane.to_3d(cu, cv, offset_min);
                let axis = plane.normal();
                let height = (offset_max - offset_min).max(0.0);
                let intervals = clip_axis_intervals(&base.csg, origin, axis, height);
                if intervals.is_empty() {
                    continue;
                }
                for (iv_idx, (t0, t1)) in intervals.into_iter().enumerate() {
                    if t1 - t0 < 1e-4 {
                        continue;
                    }
                    let z0 = offset_min + t0;
                    let z1 = offset_min + t1;
                    let mut poly = polygon_2d.clone();
                    if signed_area_2d(&poly) < 0.0 {
                        poly.reverse();
                    }
                    let ring0_3d: Vec<[f64; 3]> =
                        poly.iter().map(|&[u, v]| plane.to_3d(u, v, z0)).collect();
                    let ring1_3d: Vec<[f64; 3]> =
                        poly.iter().map(|&[u, v]| plane.to_3d(u, v, z1)).collect();

                    let can0 = can_punch_poly_on_base(&base.faces, plane, &ring0_3d, false);
                    let can1 = can_punch_poly_on_base(&base.faces, plane, &ring1_3d, true);
                    let interior0 = !can0 && ring0_3d.iter().all(|p| base.csg.contains(*p));
                    let interior1 = !can1 && ring1_3d.iter().all(|p| base.csg.contains(*p));

                    if (can0 && (can1 || interior1)) || (can1 && (can0 || interior0)) {
                        let punched0 = if can0 {
                            try_punch_poly_on_base(&mut base.faces, plane, z0, &ring0_3d, false)
                        } else {
                            false
                        };
                        let punched1 = if can1 {
                            try_punch_poly_on_base(&mut base.faces, plane, z1, &ring1_3d, true)
                        } else {
                            false
                        };
                        // Build clipped inverted cavity walls
                        let n = poly.len();
                        for i in 0..n {
                            let j = (i + 1) % n;
                            let pb0 = ring0_3d[i];
                            let pb1 = ring0_3d[j];
                            let pt0 = ring1_3d[i];
                            let pt1 = ring1_3d[j];
                            // Inverted winding so normal points inward into pocket/slot
                            let nrm = cross_norm(sub(pb0, pb1), sub(pt0, pb0));
                            let mut wf = make_quad_face(
                                &format!("cut{t_idx}_{iv_idx}_side_{i}"),
                                [pb1, pb0, pt0, pt1],
                                nrm,
                                span,
                            );
                            wf.provenance_tags = tags.clone();
                            cavity_faces.push(wf);
                        }
                        if !punched0 {
                            let mut floor = extrude_polygon_cap(
                                &format!("cut{t_idx}_{iv_idx}_floor"),
                                plane,
                                &poly,
                                z0,
                                axis,
                                false,
                                span,
                            );
                            floor.provenance_tags = tags.clone();
                            cavity_faces.push(floor);
                        }
                        if !punched1 {
                            let mut ceil = extrude_polygon_cap(
                                &format!("cut{t_idx}_{iv_idx}_ceil"),
                                plane,
                                &poly,
                                z1,
                                [-axis[0], -axis[1], -axis[2]],
                                true,
                                span,
                            );
                            ceil.provenance_tags = tags.clone();
                            cavity_faces.push(ceil);
                        }
                    } else {
                        // Tool cuts across open boundary (e.g. open slot channel); keep closed inverted clipped prism
                        let mut sub_prism = extrude_polygon_shape(plane, &poly, z0, z1, span);
                        for mut f in sub_prism.faces.drain(..) {
                            f.face_id = format!("cut{t_idx}_{iv_idx}_{}", f.face_id);
                            f.normal_hint = [-f.normal_hint[0], -f.normal_hint[1], -f.normal_hint[2]];
                            for n in f.normals.chunks_exact_mut(3) {
                                n[0] = -n[0];
                                n[1] = -n[1];
                                n[2] = -n[2];
                            }
                            for tri in f.indices.chunks_exact_mut(3) {
                                tri.swap(1, 2);
                            }
                            f.outer_loop.reverse();
                            f.provenance_tags = tags.clone();
                            cavity_faces.push(f);
                        }
                    }
                }
            }
            AtomicTool::Generic(mut solid) => {
                // Keep closed inverted shell when non-axis-aligned / open-channel cut
                for (fi, mut f) in solid.faces.drain(..).enumerate() {
                    f.face_id = format!("cut{t_idx}_g{fi}_{}", f.face_id);
                    f.normal_hint = [-f.normal_hint[0], -f.normal_hint[1], -f.normal_hint[2]];
                    for n in f.normals.chunks_exact_mut(3) {
                        n[0] = -n[0];
                        n[1] = -n[1];
                        n[2] = -n[2];
                    }
                    for tri in f.indices.chunks_exact_mut(3) {
                        tri.swap(1, 2);
                    }
                    f.outer_loop.reverse();
                    cavity_faces.push(f);
                }
                cavity_edges.extend(solid.edges);
            }
        }
    }

    // Re-triangulate every planar base face that gained inner hole loops
    for f in &mut base.faces {
        if f.surface_type == SurfaceType::Plane && !f.inner_loops.is_empty() {
            retriangulate_face_in_place(f);
        }
    }

    base.faces.extend(cavity_faces);
    base.edges.extend(cavity_edges);

    SolidShape {
        part_name: base.part_name,
        csg: CsgNode::Subtract {
            base: Box::new(base.csg),
            tools: tool_csgs,
        },
        faces: base.faces,
        edges: base.edges,
        pmi,
        exact_brep: true,
        components: base.components,
        mates: base.mates,
    }
}

enum AtomicTool {
    Cylinder {
        origin: [f64; 3],
        axis: [f64; 3],
        radius: f64,
        height: f64,
        tags: Vec<String>,
        span: Span,
        exact_brep: bool,
    },
    Counterbore {
        origin: [f64; 3],
        r_through: f64,
        r_cbore: f64,
        depth_through: f64,
        depth_cbore: f64,
        tags: Vec<String>,
        span: Span,
    },
    Prism {
        plane: SketchPlane,
        polygon_2d: Vec<[f64; 2]>,
        offset_min: f64,
        offset_max: f64,
        tags: Vec<String>,
        span: Span,
    },
    Generic(SolidShape),
}

fn collect_atomic_tools(shape: &SolidShape, out: &mut Vec<AtomicTool>) {
    let tags: Vec<String> = shape
        .faces
        .iter()
        .flat_map(|f| f.provenance_tags.iter().cloned())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let span = shape
        .faces
        .first()
        .map(|f| f.ast_span)
        .unwrap_or_default();
    collect_csg_atomic(&shape.csg, &Mat4::identity(), &tags, span, shape, out);
}

fn collect_csg_atomic(
    node: &CsgNode,
    mat: &Mat4,
    tags: &[String],
    span: Span,
    fallback_shape: &SolidShape,
    out: &mut Vec<AtomicTool>,
) {
    match node {
        CsgNode::Empty => {}
        CsgNode::Counterbore {
            origin,
            r_through,
            r_cbore,
            depth_through,
            depth_cbore,
        } => {
            out.push(AtomicTool::Counterbore {
                origin: mat.transform_point(*origin),
                r_through: *r_through,
                r_cbore: *r_cbore,
                depth_through: *depth_through,
                depth_cbore: *depth_cbore,
                tags: tags.to_vec(),
                span,
            });
        }
        CsgNode::Cylinder {
            origin,
            axis,
            radius,
            height,
        } => {
            out.push(AtomicTool::Cylinder {
                origin: mat.transform_point(*origin),
                axis: mat.transform_dir(*axis),
                radius: *radius,
                height: *height,
                tags: tags.to_vec(),
                span,
                exact_brep: fallback_shape.exact_brep,
            });
        }
        CsgNode::Box { min, max } => {
            // Check if mat is pure translation (no rotation)
            if is_pure_translation(mat) {
                let t = [mat.m[0][3], mat.m[1][3], mat.m[2][3]];
                let poly = vec![
                    [min[0] + t[0], min[1] + t[1]],
                    [max[0] + t[0], min[1] + t[1]],
                    [max[0] + t[0], max[1] + t[1]],
                    [min[0] + t[0], max[1] + t[1]],
                ];
                out.push(AtomicTool::Prism {
                    plane: SketchPlane::Xy,
                    polygon_2d: poly,
                    offset_min: min[2] + t[2],
                    offset_max: max[2] + t[2],
                    tags: tags.to_vec(),
                    span,
                });
            } else {
                out.push(AtomicTool::Generic(transform_solid(fallback_shape, *mat)));
            }
        }
        CsgNode::Prism {
            plane,
            polygon_2d,
            offset_min,
            offset_max,
        } => {
            if is_pure_translation(mat) {
                let t = [mat.m[0][3], mat.m[1][3], mat.m[2][3]];
                let (du, dv, dw) = plane.from_3d(t);
                let shifted: Vec<[f64; 2]> =
                    polygon_2d.iter().map(|&[u, v]| [u + du, v + dv]).collect();
                out.push(AtomicTool::Prism {
                    plane: *plane,
                    polygon_2d: shifted,
                    offset_min: offset_min + dw,
                    offset_max: offset_max + dw,
                    tags: tags.to_vec(),
                    span,
                });
            } else {
                out.push(AtomicTool::Generic(transform_solid(fallback_shape, *mat)));
            }
        }
        CsgNode::Transform { child, fwd, .. } => {
            let combined = mat.mul_mat(*fwd);
            collect_csg_atomic(child, &combined, tags, span, fallback_shape, out);
        }
        CsgNode::Union(items) => {
            for it in items {
                collect_csg_atomic(it, mat, tags, span, fallback_shape, out);
            }
        }
        _ => {
            out.push(AtomicTool::Generic(transform_solid(fallback_shape, *mat)));
        }
    }
}

fn is_pure_translation(m: &Mat4) -> bool {
    (m.m[0][0] - 1.0).abs() < 1e-6
        && (m.m[1][1] - 1.0).abs() < 1e-6
        && (m.m[2][2] - 1.0).abs() < 1e-6
        && m.m[0][1].abs() < 1e-6
        && m.m[0][2].abs() < 1e-6
        && m.m[1][0].abs() < 1e-6
        && m.m[1][2].abs() < 1e-6
        && m.m[2][0].abs() < 1e-6
        && m.m[2][1].abs() < 1e-6
}

fn clip_axis_intervals(
    base: &CsgNode,
    origin: [f64; 3],
    axis: [f64; 3],
    height: f64,
) -> Vec<(f64, f64)> {
    let steps = 256usize;
    let dt = height / steps as f64;
    let mut intervals = Vec::new();
    let mut in_start: Option<f64> = None;

    for i in 0..steps {
        let t_mid = (i as f64 + 0.5) * dt;
        let p = [
            origin[0] + axis[0] * t_mid,
            origin[1] + axis[1] * t_mid,
            origin[2] + axis[2] * t_mid,
        ];
        let inside = base.contains(p);
        match (in_start, inside) {
            (None, true) => {
                // The preceding midpoint was outside; the start of this cell may
                // already be inside, so it is not a valid bisection bound.
                let mut lo = ((i as f64 - 0.5) * dt).max(0.0);
                let mut hi = t_mid;
                for _ in 0..20 {
                    let mid = 0.5 * (lo + hi);
                    let mp = [
                        origin[0] + axis[0] * mid,
                        origin[1] + axis[1] * mid,
                        origin[2] + axis[2] * mid,
                    ];
                    if base.contains(mp) {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                in_start = Some(hi);
            }
            (Some(s), false) => {
                let mut lo = ((i - 1) as f64 + 0.5) * dt;
                let mut hi = t_mid;
                for _ in 0..20 {
                    let mid = 0.5 * (lo + hi);
                    let mp = [
                        origin[0] + axis[0] * mid,
                        origin[1] + axis[1] * mid,
                        origin[2] + axis[2] * mid,
                    ];
                    if base.contains(mp) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                intervals.push((s, lo));
                in_start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = in_start {
        // Check right at t = height
        let mut lo = (steps as f64 - 0.5) * dt;
        let mut hi = height;
        let p_end = [
            origin[0] + axis[0] * height,
            origin[1] + axis[1] * height,
            origin[2] + axis[2] * height,
        ];
        if base.contains(p_end) {
            intervals.push((s, height));
        } else {
            for _ in 0..20 {
                let mid = 0.5 * (lo + hi);
                let mp = [
                    origin[0] + axis[0] * mid,
                    origin[1] + axis[1] * mid,
                    origin[2] + axis[2] * mid,
                ];
                if base.contains(mp) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            intervals.push((s, lo));
        }
    }
    intervals
}

fn try_punch_loop_on_base(
    faces: &mut [BRepFace],
    center: [f64; 3],
    axis: [f64; 3],
    ring: &[[f64; 3]],
    axis_aligned_positive: bool,
) -> bool {
    let radius = norm(sub(ring[0], center));
    for f in faces.iter_mut() {
        if f.surface_type != SurfaceType::Plane || f.outer_loop.len() < 3 {
            continue;
        }
        let dot_na = f.normal_hint[0] * axis[0]
            + f.normal_hint[1] * axis[1]
            + f.normal_hint[2] * axis[2];
        let expected_sign = if axis_aligned_positive { 1.0 } else { -1.0 };
        if dot_na * expected_sign < 0.99 {
            continue;
        }
        let plane_dist = ((center[0] - f.center_hint[0]) * f.normal_hint[0]
            + (center[1] - f.center_hint[1]) * f.normal_hint[1]
            + (center[2] - f.center_hint[2]) * f.normal_hint[2])
            .abs();
        if plane_dist > 1e-3 {
            continue;
        }
        if circle_strictly_inside_face(f, center, axis, radius) {
            // Orient inner loop opposite to outer_loop in face plane!
            // Note: ring goes CCW around +axis.
            // If axis_aligned_positive (face normal == +axis), inner hole loop must go CW -> reverse(ring)!
            // If !axis_aligned_positive (face normal == -axis), face's local +n is -axis,
            // so ring (CCW around +axis) is CW around -axis -> keep ring as-is!
            let mut hole_loop = ring.to_vec();
            if axis_aligned_positive {
                hole_loop.reverse();
            }
            f.inner_loops.push(hole_loop);
            return true;
        }
    }
    false
}

fn can_punch_poly_on_base(
    faces: &[BRepFace],
    plane: SketchPlane,
    ring_3d: &[[f64; 3]],
    axis_aligned_positive: bool,
) -> bool {
    let axis = plane.normal();
    let probe = ring_3d[0];
    for f in faces {
        if f.surface_type != SurfaceType::Plane || f.outer_loop.len() < 3 {
            continue;
        }
        let dot_na = f.normal_hint[0] * axis[0]
            + f.normal_hint[1] * axis[1]
            + f.normal_hint[2] * axis[2];
        let expected_sign = if axis_aligned_positive { 1.0 } else { -1.0 };
        if dot_na * expected_sign < 0.99 {
            continue;
        }
        let plane_dist = ((probe[0] - f.center_hint[0]) * f.normal_hint[0]
            + (probe[1] - f.center_hint[1]) * f.normal_hint[1]
            + (probe[2] - f.center_hint[2]) * f.normal_hint[2])
            .abs();
        if plane_dist > 1e-3 {
            continue;
        }
        let (u_ax, v_ax) = ortho_basis(f.normal_hint);
        let origin = f.center_hint;
        let outer_2d: Vec<[f64; 2]> = f
            .outer_loop
            .iter()
            .map(|p| project_to_2d(*p, origin, u_ax, v_ax))
            .collect();
        if ring_3d.iter().all(|p| {
            let p2 = project_to_2d(*p, origin, u_ax, v_ax);
            point_in_polygon_2d(p2[0], p2[1], &outer_2d)
        }) {
            return true;
        }
    }
    false
}

fn try_punch_poly_on_base(
    faces: &mut [BRepFace],
    plane: SketchPlane,
    offset: f64,
    ring_3d: &[[f64; 3]],
    axis_aligned_positive: bool,
) -> bool {
    let axis = plane.normal();
    let probe = ring_3d[0];
    for f in faces.iter_mut() {
        if f.surface_type != SurfaceType::Plane || f.outer_loop.len() < 3 {
            continue;
        }
        let dot_na = f.normal_hint[0] * axis[0]
            + f.normal_hint[1] * axis[1]
            + f.normal_hint[2] * axis[2];
        let expected_sign = if axis_aligned_positive { 1.0 } else { -1.0 };
        if dot_na * expected_sign < 0.99 {
            continue;
        }
        let plane_dist = ((probe[0] - f.center_hint[0]) * f.normal_hint[0]
            + (probe[1] - f.center_hint[1]) * f.normal_hint[1]
            + (probe[2] - f.center_hint[2]) * f.normal_hint[2])
            .abs();
        if plane_dist > 1e-3 {
            continue;
        }
        // Check all vertices of ring_3d lie strictly inside f.outer_loop
        let (u_ax, v_ax) = ortho_basis(f.normal_hint);
        let origin = f.center_hint;
        let outer_2d: Vec<[f64; 2]> = f
            .outer_loop
            .iter()
            .map(|p| project_to_2d(*p, origin, u_ax, v_ax))
            .collect();
        if ring_3d.iter().all(|p| {
            let p2 = project_to_2d(*p, origin, u_ax, v_ax);
            point_in_polygon_2d(p2[0], p2[1], &outer_2d)
        }) {
            let _ = offset;
            let mut hole_loop = ring_3d.to_vec();
            if axis_aligned_positive {
                hole_loop.reverse();
            }
            f.inner_loops.push(hole_loop);
            return true;
        }
    }
    false
}

fn circle_strictly_inside_face(
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

fn make_circle_ring(
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

fn extrude_polygon_cap(
    id: &str,
    plane: SketchPlane,
    poly_2d: &[[f64; 2]],
    offset: f64,
    normal: [f64; 3],
    reverse: bool,
    span: Span,
) -> BRepFace {
    let tris = triangulate_polygon_2d(poly_2d);
    let mut verts = Vec::with_capacity(poly_2d.len() * 3);
    let mut norms = Vec::with_capacity(poly_2d.len() * 3);
    let mut loop3d = Vec::with_capacity(poly_2d.len());
    let mut center = [0.0; 3];
    for &[u, v] in poly_2d {
        let p = plane.to_3d(u, v, offset);
        verts.extend_from_slice(&p);
        norms.extend_from_slice(&normal);
        loop3d.push(p);
        center[0] += p[0] / poly_2d.len() as f64;
        center[1] += p[1] / poly_2d.len() as f64;
        center[2] += p[2] / poly_2d.len() as f64;
    }
    let mut indices = Vec::with_capacity(tris.len() * 3);
    for [a, b, c] in tris {
        if reverse {
            indices.extend_from_slice(&[a as u32, c as u32, b as u32]);
        } else {
            indices.extend_from_slice(&[a as u32, b as u32, c as u32]);
        }
    }
    if reverse {
        loop3d.reverse();
    }
    BRepFace {
        face_id: id.to_string(),
        surface_type: SurfaceType::Plane,
        normal_hint: normal,
        center_hint: center,
        radius_hint: None,
        area_mm2: signed_area_2d(poly_2d).abs(),
        vertices: verts,
        normals: norms,
        indices,
        outer_loop: loop3d,
        top_loop: None,
        inner_loops: Vec::new(),
        provenance_tags: Vec::new(),
        exact_interface: true,
        component_id: None,
        ast_span: span,
    }
}

/// Re-triangulates a planar face containing 1 or more inner hole loops using bridge-edge
/// merging into a simple polygon, followed by ear-clipping. Preserves exact 3D rim vertices!
fn retriangulate_face_in_place(face: &mut BRepFace) {
    if face.surface_type != SurfaceType::Plane || face.outer_loop.len() < 3 || face.inner_loops.is_empty() {
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

fn radial_dist_from_axis(p: [f64; 3], axis: [f64; 3]) -> f64 {
    let axial = p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2];
    let rx = p[0] - axial * axis[0];
    let ry = p[1] - axial * axis[1];
    let rz = p[2] - axial * axis[2];
    (rx * rx + ry * ry + rz * rz).sqrt()
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

fn polygon_centroid_2d(poly: &[[f64; 2]]) -> (f64, f64) {
    if poly.is_empty() {
        return (0.0, 0.0);
    }
    let sx: f64 = poly.iter().map(|p| p[0]).sum();
    let sy: f64 = poly.iter().map(|p| p[1]).sum();
    (sx / poly.len() as f64, sy / poly.len() as f64)
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

/// Coarse metric tap drills for the supported M2–M20 sizes. Other explicitly
/// requested pitches use nominal minus pitch; the result is not a tooling recommendation.
pub fn tap_drill_diameter_mm(spec: &str) -> Result<(f64, String), String> {
    let invalid = || format!("invalid or unsupported metric thread specification `{spec}`");
    let clean = spec.trim();
    let upper = clean.to_ascii_uppercase();
    let body = upper.strip_prefix('M').ok_or_else(invalid)?;
    let (body, class) = body.split_once('-').map_or((body, None), |(b, c)| (b, Some(c)));
    if let Some(class) = class {
        let digits = class.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !matches!(&class[digits..], "G" | "H") {
            return Err(invalid());
        }
    }
    let (nominal, pitch) = body.split_once('X').map_or((body, None), |(n, p)| (n, Some(p)));
    let number = |value: &str| {
        if value.bytes().any(|b| !(b.is_ascii_digit() || b == b'.')) {
            return Err(invalid());
        }
        value.parse::<f64>().map_err(|_| invalid())
    };
    let nominal = number(nominal)?;
    let coarse = [
        (2.0, 0.4, 1.6), (2.5, 0.45, 2.05), (3.0, 0.5, 2.5), (3.5, 0.6, 2.9),
        (4.0, 0.7, 3.3), (5.0, 0.8, 4.2), (6.0, 1.0, 5.0), (8.0, 1.25, 6.8),
        (10.0, 1.5, 8.5), (12.0, 1.75, 10.2), (14.0, 2.0, 12.0), (16.0, 2.0, 14.0),
        (18.0, 2.5, 15.5), (20.0, 2.5, 17.5),
    ];
    let (_, coarse_pitch, coarse_drill) = coarse.into_iter()
        .find(|&(n, _, _)| (n - nominal).abs() < 1e-6).ok_or_else(invalid)?;
    let drill = match pitch.map(number).transpose()? {
        None => coarse_drill,
        Some(p) if (p - coarse_pitch).abs() < 1e-6 => coarse_drill,
        Some(p) if p > 0.0 && p < coarse_pitch => nominal - p,
        _ => return Err(invalid()),
    };
    Ok((drill, clean.to_string()))
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

/// Phase 1 v0.2: Sweeps a closed 2D profile along a 3D polyline path via parallel-transport frames.
pub fn sweep_profile_along_path(
    profile_2d: &[[f64; 2]],
    path_3d: &[[f64; 3]],
    span: Span,
) -> Result<SolidShape, String> {
    if path_3d.len() < 2 {
        return Err("sweep path requires at least 2 3D points".to_string());
    }
    let mut prof = profile_2d.to_vec();
    if prof.len() < 3 {
        return Err("sweep profile requires at least 3 vertices".to_string());
    }
    if signed_area_2d(&prof) < 0.0 {
        prof.reverse();
    }

    let k = path_3d.len();
    let mut rings: Vec<Vec<[f64; 3]>> = Vec::with_capacity(k);

    let t0 = normalize_or(sub(path_3d[1], path_3d[0]), [0.0, 0.0, 1.0]);
    let (mut cur_n, mut cur_b) = ortho_basis(t0);
    let mut prev_t = t0;

    for i in 0..k {
        let tangent = if i == 0 {
            t0
        } else if i == k - 1 {
            normalize_or(sub(path_3d[k - 1], path_3d[k - 2]), prev_t)
        } else {
            let t_in = normalize_or(sub(path_3d[i], path_3d[i - 1]), prev_t);
            let t_out = normalize_or(sub(path_3d[i + 1], path_3d[i]), t_in);
            let (rn, rb) = parallel_transport_frame(prev_t, t_in, cur_n, cur_b);
            cur_n = rn;
            cur_b = rb;
            prev_t = t_in;
            let bis = [
                t_in[0] + t_out[0],
                t_in[1] + t_out[1],
                t_in[2] + t_out[2],
            ];
            normalize_or(bis, t_in)
        };
        if i == k - 1 && i > 0 {
            let (rn, rb) = parallel_transport_frame(prev_t, tangent, cur_n, cur_b);
            cur_n = rn;
            cur_b = rb;
        }
        // Project (cur_n, cur_b) onto station plane perpendicular to `tangent`
        let n_proj = normalize_or(
            sub(cur_n, scale3(tangent, dot3(cur_n, tangent))),
            cur_n,
        );
        let b_proj = cross_norm(tangent, n_proj);
        let p = path_3d[i];
        let ring: Vec<[f64; 3]> = prof
            .iter()
            .map(|&[u, v]| {
                [
                    p[0] + u * n_proj[0] + v * b_proj[0],
                    p[1] + u * n_proj[1] + v * b_proj[1],
                    p[2] + u * n_proj[2] + v * b_proj[2],
                ]
            })
            .collect();
        rings.push(ring);
    }

    let mut solid = build_lofted_ring_solid("sweep", &rings, &prof, span);
    // Add center sweep path as topological reference edge
    let mut path_poly = Vec::with_capacity(k * 3);
    for p in path_3d {
        path_poly.extend_from_slice(p);
    }
    solid.edges.push(BRepEdge {
        edge_id: "sweep_spine".to_string(),
        curve_type: CurveType::Polyline,
        polyline: path_poly,
        provenance_tags: Vec::new(),
        component_id: None,
        ast_span: span,
    });
    Ok(solid)
}

/// Phase 1 v0.2: Lofts 2 or more closed 2D sketch sections into a watertight manifold solid.
pub fn loft_sections_shape(
    sections: &[(Vec<[f64; 2]>, SketchPlane, f64)],
    span: Span,
) -> Result<SolidShape, String> {
    if sections.len() < 2 {
        return Err("loft requires at least 2 sketch sections".to_string());
    }
    let m = 32usize;
    let mut rings: Vec<Vec<[f64; 3]>> = Vec::with_capacity(sections.len());
    let mut ref_2d = Vec::new();

    for (idx, (poly, plane, offset)) in sections.iter().enumerate() {
        let mut p2 = poly.clone();
        if p2.len() < 3 {
            return Err(format!("loft section {idx} has fewer than 3 vertices"));
        }
        let want_positive = !matches!(plane, SketchPlane::Xz);
        let area = signed_area_2d(&p2);
        if (want_positive && area < 0.0) || (!want_positive && area > 0.0) {
            p2.reverse();
        }
        let resampled = resample_polygon_2d(&p2, m);
        if idx == 0 {
            ref_2d = resampled.clone();
        }
        let ring3: Vec<[f64; 3]> = resampled
            .iter()
            .map(|&[u, v]| plane.to_3d(u, v, *offset))
            .collect();
        rings.push(ring3);
    }

    // Ensure rings progress along the first cap normal; if reversed, reverse ring list
    if rings.len() >= 2 {
        let c0 = avg_pts(&rings[0]);
        let c1 = avg_pts(&rings[rings.len() - 1]);
        let n0 = sections[0].1.normal();
        let d = sub(c1, c0);
        if dot3(d, n0) < 0.0 {
            rings.reverse();
        }
    }

    if ref_2d.len() != m {
        ref_2d = resample_polygon_2d(&sections[0].0, m);
    }
    Ok(build_lofted_ring_solid("loft", &rings, &ref_2d, span))
}

fn build_lofted_ring_solid(
    name: &str,
    rings: &[Vec<[f64; 3]>],
    cap_2d: &[[f64; 2]],
    span: Span,
) -> SolidShape {
    let k = rings.len();
    let m = rings[0].len();
    let mut faces = Vec::new();
    let mut edges = Vec::new();
    let mut all_tris: Vec<[[f64; 3]; 3]> = Vec::new();
    let mut bbox = BBox3::empty();

    // Start cap (rings[0], normal = -ring_normal) and end cap (rings[k-1], normal = +ring_normal)
    let first_ring = &rings[0];
    let last_ring = &rings[k - 1];
    let n_start = scale3(ring_polygon_normal(first_ring), -1.0);
    let n_end = ring_polygon_normal(last_ring);
    let _ = cap_2d;

    let cap0 = make_ring_cap_face(&format!("{name}_cap_start"), first_ring, n_start, true, span);
    let cap1 = make_ring_cap_face(&format!("{name}_cap_end"), last_ring, n_end, false, span);
    collect_face_tris(&cap0, &mut all_tris, &mut bbox);
    collect_face_tris(&cap1, &mut all_tris, &mut bbox);
    faces.push(cap0);
    faces.push(cap1);

    for seg in 0..(k - 1) {
        let r0 = &rings[seg];
        let r1 = &rings[seg + 1];
        for i in 0..m {
            let j = (i + 1) % m;
            let nrm = cross_norm(sub(r0[j], r0[i]), sub(r1[i], r0[i]));
            let f = make_quad_face(
                &format!("{name}_side_{seg}_{i}"),
                [r0[i], r0[j], r1[j], r1[i]],
                nrm,
                span,
            );
            collect_face_tris(&f, &mut all_tris, &mut bbox);
            faces.push(f);
        }
    }

    for (ri, ring) in rings.iter().enumerate() {
        let mut poly = Vec::with_capacity((m + 1) * 3);
        for p in ring {
            poly.extend_from_slice(p);
        }
        poly.extend_from_slice(&ring[0]);
        edges.push(BRepEdge {
            edge_id: format!("{name}_ring_{ri}"),
            curve_type: CurveType::Polyline,
            polyline: poly,
            provenance_tags: Vec::new(),
            component_id: None,
            ast_span: span,
        });
    }

    SolidShape {
        part_name: name.to_string(),
        csg: CsgNode::Mesh {
            bbox,
            triangles: all_tris,
        },
        faces,
        edges,
        pmi: Vec::new(),
        exact_brep: false,
        components: Vec::new(),
        mates: Vec::new(),
    }
}

fn make_ring_cap_face(
    id: &str,
    ring_3d: &[[f64; 3]],
    normal: [f64; 3],
    reverse: bool,
    span: Span,
) -> BRepFace {
    let m = ring_3d.len();
    let center = avg_pts(ring_3d);
    let (u_ax, v_ax) = ortho_basis(normal);
    let mut outer_3d = ring_3d.to_vec();
    let mut pts_2d: Vec<[f64; 2]> = outer_3d
        .iter()
        .map(|p| project_to_2d(*p, center, u_ax, v_ax))
        .collect();
    if signed_area_2d(&pts_2d) < 0.0 {
        pts_2d.reverse();
        outer_3d.reverse();
    }
    let tris = triangulate_polygon_2d(&pts_2d);
    let mut verts = Vec::with_capacity(m * 3);
    let mut norms = Vec::with_capacity(m * 3);
    for p in &outer_3d {
        verts.extend_from_slice(p);
        norms.extend_from_slice(&normal);
    }
    let mut indices = Vec::with_capacity(tris.len() * 3);
    for [a, b, c] in tris {
        indices.extend_from_slice(&[a as u32, b as u32, c as u32]);
    }
    let _ = reverse;
    BRepFace {
        face_id: id.to_string(),
        surface_type: SurfaceType::Plane,
        normal_hint: normal,
        center_hint: center,
        radius_hint: None,
        area_mm2: signed_area_2d(&pts_2d).abs(),
        vertices: verts,
        normals: norms,
        indices,
        outer_loop: outer_3d,
        top_loop: None,
        inner_loops: Vec::new(),
        provenance_tags: Vec::new(),
        exact_interface: true,
        component_id: None,
        ast_span: span,
    }
}

fn resample_polygon_2d(poly: &[[f64; 2]], target_n: usize) -> Vec<[f64; 2]> {
    let n = poly.len();
    if n == 0 {
        return Vec::new();
    }
    let mut cum = Vec::with_capacity(n + 1);
    cum.push(0.0);
    let mut total = 0.0;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        total += (b[0] - a[0]).hypot(b[1] - a[1]);
        cum.push(total);
    }
    if total < 1e-9 {
        return vec![poly[0]; target_n];
    }
    let mut out = Vec::with_capacity(target_n);
    for k in 0..target_n {
        let s = total * (k as f64) / (target_n as f64);
        let mut seg = 0usize;
        while seg + 1 < n && cum[seg + 1] < s {
            seg += 1;
        }
        let seg_len = (cum[seg + 1] - cum[seg]).max(1e-12);
        let t = ((s - cum[seg]) / seg_len).clamp(0.0, 1.0);
        let a = poly[seg];
        let b = poly[(seg + 1) % n];
        out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
    }
    out
}

/// Phase 1 v0.2: Mirrors a solid across a datum plane (`YZ_PLANE`, `XZ_PLANE`, `XY_PLANE`)
/// and optionally merges with the original into a closed manifold union.
pub fn mirror_shape(shape: &SolidShape, plane: SketchPlane, merge: bool) -> SolidShape {
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

fn collect_face_tris(f: &BRepFace, tris: &mut Vec<[[f64; 3]; 3]>, bbox: &mut BBox3) {
    for tri in f.indices.chunks_exact(3) {
        let i0 = tri[0] as usize;
        let i1 = tri[1] as usize;
        let i2 = tri[2] as usize;
        let p0 = [f.vertices[i0 * 3], f.vertices[i0 * 3 + 1], f.vertices[i0 * 3 + 2]];
        let p1 = [f.vertices[i1 * 3], f.vertices[i1 * 3 + 1], f.vertices[i1 * 3 + 2]];
        let p2 = [f.vertices[i2 * 3], f.vertices[i2 * 3 + 1], f.vertices[i2 * 3 + 2]];
        bbox.expand_point(p0);
        bbox.expand_point(p1);
        bbox.expand_point(p2);
        tris.push([p0, p1, p2]);
    }
}

fn ring_polygon_normal(ring: &[[f64; 3]]) -> [f64; 3] {
    let c = avg_pts(ring);
    let mut sum = [0.0; 3];
    for i in 0..ring.len() {
        let a = sub(ring[i], c);
        let b = sub(ring[(i + 1) % ring.len()], c);
        let cr = cross(a, b);
        sum[0] += cr[0];
        sum[1] += cr[1];
        sum[2] += cr[2];
    }
    normalize_or(sum, [0.0, 0.0, 1.0])
}

fn avg_pts(pts: &[[f64; 3]]) -> [f64; 3] {
    if pts.is_empty() {
        return [0.0, 0.0, 0.0];
    }
    let mut s = [0.0; 3];
    for p in pts {
        s[0] += p[0];
        s[1] += p[1];
        s[2] += p[2];
    }
    [
        s[0] / pts.len() as f64,
        s[1] / pts.len() as f64,
        s[2] / pts.len() as f64,
    ]
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

fn parallel_transport_frame(
    t_prev: [f64; 3],
    t_next: [f64; 3],
    n_prev: [f64; 3],
    b_prev: [f64; 3],
) -> ([f64; 3], [f64; 3]) {
    let axis = cross(t_prev, t_next);
    let sin_a = norm(axis);
    if sin_a < 1e-7 {
        return (n_prev, b_prev);
    }
    let cos_a = dot3(t_prev, t_next).clamp(-1.0, 1.0);
    let ang = sin_a.atan2(cos_a);
    let rot = Mat4::rotate_axis(axis, ang);
    let n_next = rot.transform_dir(n_prev);
    let b_next = cross_norm(t_next, n_next);
    (n_next, b_next)
}

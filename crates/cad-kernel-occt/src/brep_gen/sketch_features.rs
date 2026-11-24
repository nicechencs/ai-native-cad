use super::{BRepEdge, BRepFace, CsgNode, CurveType, KernelPayload, Mat4, SketchPlane, SolidShape, Span, SurfaceType};
use super::{cross, cross_norm, norm, signed_area_2d, sub, triangle_area, triangulate_polygon_2d};

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
        payload: KernelPayload::Analytic,
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
        payload: KernelPayload::Analytic,
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

fn radial_dist_from_axis(p: [f64; 3], axis: [f64; 3]) -> f64 {
    let axial = p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2];
    let rx = p[0] - axial * axis[0];
    let ry = p[1] - axial * axis[1];
    let rz = p[2] - axial * axis[2];
    (rx * rx + ry * ry + rz * rz).sqrt()
}

use super::{BRepEdge, BRepFace, CsgNode, CurveType, KernelPayload, SolidShape, Span, SurfaceType};
use super::{norm, ortho_basis, sub, triangle_area};

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
        payload: KernelPayload::Analytic,
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
        payload: KernelPayload::Analytic,
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

pub(super) fn make_disk_face(
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

pub(super) fn make_cyl_wall_face(
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

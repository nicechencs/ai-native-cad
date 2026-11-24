use super::{
    BBox3, BRepEdge, BRepFace, CsgNode, CurveType, KernelPayload, Mat4, SketchPlane, SolidShape,
    Span, SurfaceType, cross, cross_norm, dot3, make_quad_face, norm, normalize_or, ortho_basis,
    project_to_2d, scale3, signed_area_2d, sub, triangulate_polygon_2d,
};

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
        payload: KernelPayload::Analytic,
        part_name: name.to_string(),
        csg: CsgNode::Mesh {
            bbox,
            triangles: all_tris,
            closed: true,
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

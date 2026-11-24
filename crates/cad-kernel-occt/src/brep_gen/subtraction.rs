use super::face_loops::{circle_strictly_inside_face, make_circle_ring, retriangulate_face_in_place};
use super::primitives::{make_cyl_wall_face, make_disk_face};
use super::{
    BRepEdge, BRepFace, CsgNode, CurveType, KernelPayload, Mat4, SketchPlane, SolidShape, Span,
    SurfaceType, cross_norm, extrude_polygon_shape, make_quad_face, norm, ortho_basis, planar_cut,
    point_in_polygon_2d, project_to_2d, signed_area_2d, sub, transform_solid, triangulate_polygon_2d,
};

/// R02: Real Boolean subtraction with tool interval clipping against base.csg,
/// protruding cap removal, and perforated planar face re-triangulation with inner hole loops!
pub fn subtract_solids(mut base: SolidShape, tools: Vec<SolidShape>) -> SolidShape {
    if std::iter::once(&base)
        .chain(&tools)
        .any(|shape| !matches!(shape.payload, KernelPayload::Analytic))
    {
        let name = base.part_name.clone();
        return crate::try_boolean_subtract(base, tools)
            .unwrap_or_else(|_| SolidShape::empty(name));
    }
    let mut tool_csgs = Vec::new();
    let mut pmi = base.pmi.clone();
    let mut atomic_tools: Vec<AtomicTool> = Vec::new();

    for tool in tools {
        base.exact_brep &= tool.exact_brep;
        tool_csgs.push(tool.csg.clone());
        pmi.extend(tool.pmi.clone());
        collect_atomic_tools(&tool, &mut atomic_tools);
    }

    let mut cavity_faces: Vec<BRepFace> = Vec::new();
    let mut cavity_edges: Vec<BRepEdge> = Vec::new();
    let mut original_csg = None;
    let mut unsupported_edge_cut = false;

    let mut atomic_tools = atomic_tools.into_iter().enumerate().peekable();
    while let Some((t_idx, tool)) = atomic_tools.next() {
        let stepped = atomic_tools.peek().and_then(|(_, next)| {
            stepped_bore_tools(&tool, next).or_else(|| stepped_bore_tools(next, &tool))
        });
        let tool = if let Some(stepped) = stepped {
            atomic_tools.next();
            stepped
        } else {
            tool
        };
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
                    if cylinder_intersects_csg(&base.csg, origin, axis, radius, height) {
                        unsupported_edge_cut = true;
                        break;
                    }
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
                origin: clip_origin,
                height: total_h,
                z_shelf,
                r_through,
                r_cbore,
                tags,
                span,
                upper,
            } => {
                let origin = clip_origin;
                let z_bot_nom = origin[2];
                let (upper_tags, upper_span) = upper.as_ref().map_or((&tags, span), |(tags, span)| (tags, *span));
                let axis = [0.0, 0.0, 1.0];
                let intervals = clip_axis_intervals(&base.csg, clip_origin, axis, total_h);
                if intervals.is_empty() {
                    let lower_h = (z_shelf - clip_origin[2]).clamp(0.0, total_h);
                    let upper_h = (total_h - lower_h).max(0.0);
                    let lower_hit = lower_h > 1e-6
                        && cylinder_intersects_csg(&base.csg, clip_origin, axis, r_through, lower_h);
                    let upper_hit = upper_h > 1e-6
                        && cylinder_intersects_csg(
                            &base.csg,
                            [clip_origin[0], clip_origin[1], clip_origin[2] + lower_h],
                            axis,
                            r_cbore,
                            upper_h,
                        );
                    if lower_hit || upper_hit {
                        unsupported_edge_cut = true;
                        break;
                    }
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
                            provenance_tags: upper_tags.clone(),
                            exact_interface: true,
                            component_id: None,
                            ast_span: upper_span,
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
                            upper_span,
                        );
                        w_hi.provenance_tags = upper_tags.clone();
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
                                upper_span,
                            );
                            cap1.provenance_tags = upper_tags.clone();
                            cavity_faces.push(cap1);
                        }
                    } else {
                        let (r_use, tags, span) = if z0 >= z_shelf - 1e-3 {
                            (r_cbore, upper_tags, upper_span)
                        } else {
                            (r_through, &tags, span)
                        };
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
                if cavity_faces.is_empty()
                    && base.components.is_empty()
                    && matches!(original_csg.as_ref().unwrap_or(&base.csg), CsgNode::Union(_))
                {
                    let mut cutter =
                        extrude_polygon_shape(plane, &polygon_2d, offset_min, offset_max, span);
                    for face in &mut cutter.faces {
                        face.face_id = format!("cut{t_idx}_{}", face.face_id);
                        face.provenance_tags = tags.clone();
                    }
                    let clipped = CsgNode::Subtract {
                        base: Box::new(base.csg.clone()),
                        tools: vec![cutter.csg],
                    };
                    if let Some((faces, edges)) = planar_cut::cut(&base.faces, &cutter.faces, &clipped) {
                        base.faces = faces;
                        base.edges = edges;
                        // Later drills must clip against the slotted stock. Keep the
                        // input CSG for the final subtraction, which already includes this tool.
                        let previous = std::mem::replace(&mut base.csg, clipped);
                        original_csg.get_or_insert(previous);
                        continue;
                    }
                }

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

    if unsupported_edge_cut {
        return SolidShape {
            payload: KernelPayload::Analytic,
            part_name: base.part_name,
            csg: CsgNode::Subtract {
                base: Box::new(original_csg.unwrap_or(base.csg)),
                tools: tool_csgs,
            },
            faces: Vec::new(),
            edges: Vec::new(),
            pmi,
            exact_brep: false,
            components: base.components,
            mates: base.mates,
        };
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
        payload: KernelPayload::Analytic,
        part_name: base.part_name,
        csg: CsgNode::Subtract {
            base: Box::new(original_csg.unwrap_or(base.csg)),
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
        height: f64,
        z_shelf: f64,
        r_through: f64,
        r_cbore: f64,
        tags: Vec<String>,
        span: Span,
        upper: Option<(Vec<String>, Span)>,
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

// Two overlapping coaxial tools that widen toward +Z form a stepped bore.
fn stepped_bore_tools(first: &AtomicTool, second: &AtomicTool) -> Option<AtomicTool> {
    let (
        AtomicTool::Cylinder { origin, axis, radius, height, tags, span, .. },
        AtomicTool::Cylinder {
            origin: upper_origin,
            axis: upper_axis,
            radius: upper_radius,
            height: upper_height,
            tags: upper_tags,
            span: upper_span,
            ..
        },
    ) = (first, second) else {
        return None;
    };
    let top = upper_origin[2] + upper_height;
    if *axis != [0., 0., 1.] || *upper_axis != [0., 0., 1.]
        || (origin[0] - upper_origin[0]).hypot(origin[1] - upper_origin[1]) > 1e-8
        || *radius >= *upper_radius
        || origin[2] >= upper_origin[2]
        || upper_origin[2] >= origin[2] + height
        || top < origin[2] + height
    {
        return None;
    }
    Some(AtomicTool::Counterbore {
        origin: *origin,
        height: top - origin[2],
        z_shelf: upper_origin[2],
        r_through: *radius,
        r_cbore: *upper_radius,
        tags: tags.clone(),
        span: *span,
        upper: Some((upper_tags.clone(), *upper_span)),
    })
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
            let [x, y, z] = mat.transform_point(*origin);
            out.push(AtomicTool::Counterbore {
                origin: [x, y, z - depth_through],
                height: (depth_through + 1.0).max(1e-4),
                z_shelf: z - depth_cbore,
                r_through: *r_through,
                r_cbore: *r_cbore,
                tags: tags.to_vec(),
                span,
                upper: None,
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

fn polygon_centroid_2d(poly: &[[f64; 2]]) -> (f64, f64) {
    if poly.is_empty() {
        return (0.0, 0.0);
    }
    let sx: f64 = poly.iter().map(|p| p[0]).sum();
    let sy: f64 = poly.iter().map(|p| p[1]).sum();
    (sx / poly.len() as f64, sy / poly.len() as f64)
}

/// Detect whether a cylinder whose centerline lies outside `base` still overlaps `base`
/// with positive volume (an edge-grazing cut unsupported by the analytic fallback).
fn cylinder_intersects_csg(
    base: &CsgNode,
    origin: [f64; 3],
    axis: [f64; 3],
    radius: f64,
    height: f64,
) -> bool {
    if radius <= 1e-6 || height <= 1e-6 {
        return false;
    }
    let base_bb = base.bbox();
    let cyl_bb = CsgNode::Cylinder {
        origin,
        axis,
        radius,
        height,
    }
    .bbox();
    const OVERLAP_EPS: f64 = 1e-5;
    for k in 0..3 {
        if base_bb.min[k].max(cyl_bb.min[k]) + OVERLAP_EPS >= base_bb.max[k].min(cyl_bb.max[k]) {
            return false;
        }
    }

    // Project base_bb's 8 corners onto the cylinder axis to restrict sampling to the
    // overlapping axial span [t_lo, t_hi] (prevents long cutters from stepping over thin plates).
    let mut proj_min = f64::INFINITY;
    let mut proj_max = f64::NEG_INFINITY;
    for &x in &[base_bb.min[0], base_bb.max[0]] {
        for &y in &[base_bb.min[1], base_bb.max[1]] {
            for &z in &[base_bb.min[2], base_bb.max[2]] {
                let t = (x - origin[0]) * axis[0]
                    + (y - origin[1]) * axis[1]
                    + (z - origin[2]) * axis[2];
                proj_min = proj_min.min(t);
                proj_max = proj_max.max(t);
            }
        }
    }
    let t_lo = proj_min.max(0.0);
    let t_hi = proj_max.min(height);
    if t_hi <= t_lo + OVERLAP_EPS {
        return false;
    }

    let (u_dir, v_dir) = ortho_basis(axis);
    let axial_steps = 16usize;
    let radial_angles = 16usize;
    let radial_fracs = [0.5, 0.98];

    for i in 0..axial_steps {
        let t = t_lo + ((i as f64) + 0.5) * ((t_hi - t_lo) / axial_steps as f64);
        let c = [
            origin[0] + axis[0] * t,
            origin[1] + axis[1] * t,
            origin[2] + axis[2] * t,
        ];

        let closest = [
            c[0].clamp(base_bb.min[0], base_bb.max[0]),
            c[1].clamp(base_bb.min[1], base_bb.max[1]),
            c[2].clamp(base_bb.min[2], base_bb.max[2]),
        ];
        let diff = sub(closest, c);
        let du = diff[0] * u_dir[0] + diff[1] * u_dir[1] + diff[2] * u_dir[2];
        let dv = diff[0] * v_dir[0] + diff[1] * v_dir[1] + diff[2] * v_dir[2];
        let d_rad = du.hypot(dv);
        if d_rad > 1e-9 && d_rad < radius - OVERLAP_EPS {
            let dir_u = du / d_rad;
            let dir_v = dv / d_rad;
            for r_try in [
                d_rad + 0.5 * (radius - d_rad),
                (radius - 1e-4).max(0.5 * radius),
            ] {
                let p = [
                    c[0] + (dir_u * u_dir[0] + dir_v * v_dir[0]) * r_try,
                    c[1] + (dir_u * u_dir[1] + dir_v * v_dir[1]) * r_try,
                    c[2] + (dir_u * u_dir[2] + dir_v * v_dir[2]) * r_try,
                ];
                if base.contains(p) {
                    return true;
                }
            }
        }

        for &frac in &radial_fracs {
            let r_try = radius * frac;
            for a_idx in 0..radial_angles {
                let ang = std::f64::consts::TAU * (a_idx as f64) / (radial_angles as f64);
                let ca = ang.cos();
                let sa = ang.sin();
                let p = [
                    c[0] + (ca * u_dir[0] + sa * v_dir[0]) * r_try,
                    c[1] + (ca * u_dir[1] + sa * v_dir[1]) * r_try,
                    c[2] + (ca * u_dir[2] + sa * v_dir[2]) * r_try,
                ];
                if base.contains(p) {
                    return true;
                }
            }
        }
    }
    false
}


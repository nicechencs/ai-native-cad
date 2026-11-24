use super::face_loops::{circle_strictly_inside_face, make_circle_ring, retriangulate_face_in_place};
use super::primitives::make_cyl_wall_face;
use super::{BBox3, CsgNode, KernelPayload, SolidShape, Span, SurfaceType, ortho_basis, planar_cut};

/// R02: Union with coplanar contact face removal & clean stitching (preserves closed manifold).
pub fn union_solids(items: Vec<SolidShape>, name: &str) -> SolidShape {
    if items
        .iter()
        .any(|shape| !matches!(shape.payload, KernelPayload::Analytic))
    {
        return crate::try_boolean_union(items, name).unwrap_or_else(|_| SolidShape::empty(name));
    }
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

    let union_csg = CsgNode::Union(csg_items);
    if components.is_empty()
        && let CsgNode::Union(items) = &union_csg
        && items.len() >= 2
        && items.iter().all(|node| matches!(node, CsgNode::Box { .. }))
        && has_positive_bbox_overlap(items)
        && let Some((merged_faces, merged_edges)) = planar_cut::cut(&faces, &[], &union_csg)
    {
        faces = merged_faces;
        edges = merged_edges;
    }

    SolidShape {
        payload: KernelPayload::Analytic,
        part_name: name.to_string(),
        csg: union_csg,
        faces,
        edges,
        pmi,
        exact_brep: exact,
        components,
        mates: Vec::new(),
    }
}

fn has_positive_bbox_overlap(items: &[CsgNode]) -> bool {
    let bboxes: Vec<BBox3> = items.iter().map(CsgNode::bbox).collect();
    (0..bboxes.len()).any(|i| {
        ((i + 1)..bboxes.len()).any(|j| {
            (0..3).all(|k| {
                bboxes[i].min[k].max(bboxes[j].min[k]) + 1e-4
                    < bboxes[i].max[k].min(bboxes[j].max[k])
            })
        })
    })
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

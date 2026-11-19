use crate::csg::SolidShape;
use crate::types::{BRepFace, SurfaceType};
use std::collections::HashMap;
use std::fmt::Write;

type QuantKey = (i64, i64, i64);

fn quant_pt(p: [f64; 3]) -> QuantKey {
    (
        (p[0] * 10_000.0).round() as i64,
        (p[1] * 10_000.0).round() as i64,
        (p[2] * 10_000.0).round() as i64,
    )
}

/// Exports a `SolidShape` into ISO-10303-21 STEP AP242 Part 21 ASCII with complete
/// topological boundary loops (`VERTEX_POINT`, `EDGE_CURVE`, `ORIENTED_EDGE`, `EDGE_LOOP`,
/// `FACE_OUTER_BOUND`, `FACE_BOUND`), manufacturing PMI metadata, and Assembly BOM links (R01).
pub fn export_step_ap242(shape: &SolidShape) -> String {
    let mut out = String::with_capacity(32768);
    let part = sanitize_step_str(&shape.part_name);

    let _ = writeln!(out, "ISO-10303-21;");
    let _ = writeln!(out, "HEADER;");
    let _ = writeln!(
        out,
        "FILE_DESCRIPTION(('AI-Native CAD MVP v0.1 STEP AP242 Export'),'2;1');"
    );
    let _ = writeln!(
        out,
        "FILE_NAME('{part}.step','2026-11-18T00:00:00',('AI-Native CAD'),('AI-Native CAD Engine'),'cad-kernel-occt 0.1','cad-cli','');"
    );
    let _ = writeln!(
        out,
        "FILE_SCHEMA(('AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF'));"
    );
    let _ = writeln!(out, "ENDSEC;");
    let _ = writeln!(out, "DATA;");

    let mut next_id = 1usize;
    let mut alloc = || {
        let cur = next_id;
        next_id += 1;
        cur
    };

    let app_ctx = alloc();
    let _ = writeln!(
        out,
        "#{app_ctx}=APPLICATION_CONTEXT('managed model based 3d engineering');"
    );
    let app_proto = alloc();
    let _ = writeln!(
        out,
        "#{app_proto}=APPLICATION_PROTOCOL_DEFINITION('international standard','ap242_managed_model_based_3d_engineering',2014,#{app_ctx});"
    );
    let prod_ctx = alloc();
    let _ = writeln!(
        out,
        "#{prod_ctx}=PRODUCT_CONTEXT('',# {app_ctx},'mechanical');"
    );
    let prod_id = alloc();
    let _ = writeln!(
        out,
        "#{prod_id}=PRODUCT('{part}','{part}','',(#{prod_ctx}));"
    );
    let pdf_id = alloc();
    let _ = writeln!(
        out,
        "#{pdf_id}=PRODUCT_DEFINITION_FORMATION('1','',#{prod_id});"
    );
    let pdef_ctx = alloc();
    let _ = writeln!(
        out,
        "#{pdef_ctx}=PRODUCT_DEFINITION_CONTEXT('part definition',#{app_ctx},'design');"
    );
    let pd_id = alloc();
    let _ = writeln!(
        out,
        "#{pd_id}=PRODUCT_DEFINITION('design','',#{pdf_id},#{pdef_ctx});"
    );
    let pds_id = alloc();
    let _ = writeln!(
        out,
        "#{pds_id}=PRODUCT_DEFINITION_SHAPE('','',#{pd_id});"
    );

    let uncert = alloc();
    let len_unit = alloc();
    let ang_unit = alloc();
    let solid_ang_unit = alloc();
    let shape_rep_ctx = alloc();
    let _ = writeln!(
        out,
        "#{len_unit}=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));"
    );
    let _ = writeln!(
        out,
        "#{ang_unit}=(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.));"
    );
    let _ = writeln!(
        out,
        "#{solid_ang_unit}=(NAMED_UNIT(*)SOLID_ANGLE_UNIT()SI_UNIT($,.STERADIAN.));"
    );
    let _ = writeln!(
        out,
        "#{uncert}=UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-04),#{len_unit},'distance_accuracy_value','confusion accuracy');"
    );
    let _ = writeln!(
        out,
        "#{shape_rep_ctx}=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{uncert}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{len_unit},#{ang_unit},#{solid_ang_unit}))REPRESENTATION_CONTEXT('Context3D','3D'));"
    );

    // Deduplicated vertex points and edge curves across all faces
    let mut vertex_map: HashMap<QuantKey, usize> = HashMap::new();
    let mut edge_map: HashMap<(QuantKey, QuantKey), usize> = HashMap::new();

    let mut face_ids = Vec::with_capacity(shape.faces.len());
    for face in &shape.faces {
        let adv_face_id = emit_advanced_face(
            face,
            &mut out,
            &mut alloc,
            &mut vertex_map,
            &mut edge_map,
        );
        face_ids.push(adv_face_id);
    }

    let shell_id = alloc();
    let face_refs: Vec<String> = face_ids.iter().map(|i| format!("#{i}")).collect();
    let _ = writeln!(
        out,
        "#{shell_id}=CLOSED_SHELL('',({}));",
        face_refs.join(",")
    );
    let brep_id = alloc();
    let _ = writeln!(
        out,
        "#{brep_id}=MANIFOLD_SOLID_BREP('{part}',#{shell_id});"
    );
    let rep_id = alloc();
    let _ = writeln!(
        out,
        "#{rep_id}=ADVANCED_BREP_SHAPE_REPRESENTATION('{part}',(#{brep_id}),#{shape_rep_ctx});"
    );
    let sdr_id = alloc();
    let _ = writeln!(
        out,
        "#{sdr_id}=SHAPE_DEFINITION_REPRESENTATION(#{pds_id},#{rep_id});"
    );

    // Assembly hierarchy & NEXT_ASSEMBLY_USAGE_OCCURRENCE when multiple components exist
    if !shape.components.is_empty() {
        for comp in &shape.components {
            let cid = sanitize_step_str(&comp.component_id);
            let cname = sanitize_step_str(&comp.part_name);
            let c_prod = alloc();
            let _ = writeln!(
                out,
                "#{c_prod}=PRODUCT('{cname}','{cname}','Component {cid}',(#{prod_ctx}));"
            );
            let c_pdf = alloc();
            let _ = writeln!(
                out,
                "#{c_pdf}=PRODUCT_DEFINITION_FORMATION('1','',#{c_prod});"
            );
            let c_pd = alloc();
            let _ = writeln!(
                out,
                "#{c_pd}=PRODUCT_DEFINITION('design','',#{c_pdf},#{pdef_ctx});"
            );
            let nauo = alloc();
            let _ = writeln!(
                out,
                "#{nauo}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('{cid}','{cname}','',#{pd_id},#{c_pd},$);"
            );
            let pt_o = alloc();
            let _ = writeln!(
                out,
                "#{pt_o}=CARTESIAN_POINT('',({:.4},{:.4},{:.4}));",
                comp.origin_mm[0], comp.origin_mm[1], comp.origin_mm[2]
            );
            let ax_o = alloc();
            let dz = alloc();
            let dx = alloc();
            let _ = writeln!(out, "#{dz}=DIRECTION('',(0.,0.,1.));");
            let _ = writeln!(out, "#{dx}=DIRECTION('',(1.,0.,0.));");
            let _ = writeln!(
                out,
                "#{ax_o}=AXIS2_PLACEMENT_3D('{cid}_placement',#{pt_o},#{dz},#{dx});"
            );
        }
    }

    // Semantic PMI and CAM feature metadata
    for pmi in &shape.pmi {
        let pmi_id = alloc();
        let kind = sanitize_step_str(&pmi.kind);
        let label = sanitize_step_str(&pmi.label);
        let tol = pmi.tolerance_mm.unwrap_or(0.0);
        let _ = writeln!(
            out,
            "#{pmi_id}=DESCRIPTIVE_REPRESENTATION_ITEM('PMI_{kind}','{label} tol={tol:.4}mm');"
        );
    }

    let _ = writeln!(out, "ENDSEC;");
    let _ = writeln!(out, "END-ISO-10303-21;");
    out
}

fn emit_advanced_face(
    face: &BRepFace,
    out: &mut String,
    alloc: &mut impl FnMut() -> usize,
    vertex_map: &mut HashMap<QuantKey, usize>,
    edge_map: &mut HashMap<(QuantKey, QuantKey), usize>,
) -> usize {
    let mut bound_ids = Vec::new();

    if face.outer_loop.len() >= 3 {
        let loop_id = emit_edge_loop(&face.outer_loop, out, alloc, vertex_map, edge_map);
        let b_id = alloc();
        let _ = writeln!(out, "#{b_id}=FACE_OUTER_BOUND('',#{loop_id},.T.);");
        bound_ids.push(b_id);
    }

    for inner in &face.inner_loops {
        if inner.len() >= 3 {
            let loop_id = emit_edge_loop(inner, out, alloc, vertex_map, edge_map);
            let b_id = alloc();
            let _ = writeln!(out, "#{b_id}=FACE_BOUND('',#{loop_id},.T.);");
            bound_ids.push(b_id);
        }
    }
    if let Some(top) = &face.top_loop {
        if top.len() >= 3 {
            let loop_id = emit_edge_loop(top, out, alloc, vertex_map, edge_map);
            let b_id = alloc();
            let _ = writeln!(out, "#{b_id}=FACE_BOUND('',#{loop_id},.T.);");
            bound_ids.push(b_id);
        }
    }

    let pt_id = alloc();
    let _ = writeln!(
        out,
        "#{pt_id}=CARTESIAN_POINT('',({:.4},{:.4},{:.4}));",
        face.center_hint[0], face.center_hint[1], face.center_hint[2]
    );
    let dir_n = alloc();
    let _ = writeln!(
        out,
        "#{dir_n}=DIRECTION('',({:.6},{:.6},{:.6}));",
        face.normal_hint[0], face.normal_hint[1], face.normal_hint[2]
    );
    let ref_d = if face.normal_hint[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let dir_ref = alloc();
    let _ = writeln!(
        out,
        "#{dir_ref}=DIRECTION('',({:.6},{:.6},{:.6}));",
        ref_d[0], ref_d[1], ref_d[2]
    );
    let axis_id = alloc();
    let _ = writeln!(
        out,
        "#{axis_id}=AXIS2_PLACEMENT_3D('',#{pt_id},#{dir_n},#{dir_ref});"
    );

    let surf_id = alloc();
    let tag_label = sanitize_step_str(&face.provenance_tags.join(","));
    match face.surface_type {
        SurfaceType::Cylinder => {
            let r = face.radius_hint.unwrap_or(10.0);
            let _ = writeln!(
                out,
                "#{surf_id}=CYLINDRICAL_SURFACE('{tag_label}',#{axis_id},{r:.4});"
            );
        }
        SurfaceType::Cone => {
            let r = face.radius_hint.unwrap_or(10.0);
            let _ = writeln!(
                out,
                "#{surf_id}=CONICAL_SURFACE('{tag_label}',#{axis_id},{r:.4},0.785398);"
            );
        }
        _ => {
            let _ = writeln!(out, "#{surf_id}=PLANE('{tag_label}',#{axis_id});");
        }
    }

    let bound_refs: Vec<String> = bound_ids.iter().map(|i| format!("#{i}")).collect();
    let adv_face_id = alloc();
    let _ = writeln!(
        out,
        "#{adv_face_id}=ADVANCED_FACE('{tag_label}',({}),#{surf_id},.T.);",
        bound_refs.join(",")
    );
    adv_face_id
}

fn emit_edge_loop(
    pts: &[[f64; 3]],
    out: &mut String,
    alloc: &mut impl FnMut() -> usize,
    vertex_map: &mut HashMap<QuantKey, usize>,
    edge_map: &mut HashMap<(QuantKey, QuantKey), usize>,
) -> usize {
    let mut oriented_edges = Vec::with_capacity(pts.len());
    let n = pts.len();
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let ka = quant_pt(a);
        let kb = quant_pt(b);
        if ka == kb {
            continue;
        }
        let va = get_or_insert_vertex(ka, a, out, alloc, vertex_map);
        let vb = get_or_insert_vertex(kb, b, out, alloc, vertex_map);

        let (ec_id, same_sense) = if let Some(&existing) = edge_map.get(&(ka, kb)) {
            (existing, ".T.")
        } else if let Some(&existing_rev) = edge_map.get(&(kb, ka)) {
            (existing_rev, ".F.")
        } else {
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            let dz = b[2] - a[2];
            let len = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-9);
            let p_id = alloc();
            let _ = writeln!(
                out,
                "#{p_id}=CARTESIAN_POINT('',({:.4},{:.4},{:.4}));",
                a[0], a[1], a[2]
            );
            let d_id = alloc();
            let _ = writeln!(
                out,
                "#{d_id}=DIRECTION('',({:.6},{:.6},{:.6}));",
                dx / len,
                dy / len,
                dz / len
            );
            let v_id = alloc();
            let _ = writeln!(out, "#{v_id}=VECTOR('',#{d_id},{len:.4});");
            let l_id = alloc();
            let _ = writeln!(out, "#{l_id}=LINE('',#{p_id},#{v_id});");
            let ec = alloc();
            let _ = writeln!(out, "#{ec}=EDGE_CURVE('',#{va},#{vb},#{l_id},.T.);");
            edge_map.insert((ka, kb), ec);
            (ec, ".T.")
        };

        let oe = alloc();
        let _ = writeln!(out, "#{oe}=ORIENTED_EDGE('',*,*,#{ec_id},{same_sense});");
        oriented_edges.push(format!("#{oe}"));
    }
    let loop_id = alloc();
    let _ = writeln!(
        out,
        "#{loop_id}=EDGE_LOOP('',({}));",
        oriented_edges.join(",")
    );
    loop_id
}

fn get_or_insert_vertex(
    key: QuantKey,
    p: [f64; 3],
    out: &mut String,
    alloc: &mut impl FnMut() -> usize,
    vertex_map: &mut HashMap<QuantKey, usize>,
) -> usize {
    if let Some(&vid) = vertex_map.get(&key) {
        return vid;
    }
    let cp_id = alloc();
    let _ = writeln!(
        out,
        "#{cp_id}=CARTESIAN_POINT('',({:.4},{:.4},{:.4}));",
        p[0], p[1], p[2]
    );
    let vp_id = alloc();
    let _ = writeln!(out, "#{vp_id}=VERTEX_POINT('',#{cp_id});");
    vertex_map.insert(key, vp_id);
    vp_id
}

fn sanitize_step_str(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\'' { '_' } else { c })
        .collect()
}

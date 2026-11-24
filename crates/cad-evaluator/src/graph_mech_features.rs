//! Typed evaluation of the remaining V2 mechanical features.
//!
//! Holes are cut out of the target solid. Patterns and mirrors copy that solid
//! and union the copies back when they should stay. Draft and direct face edits
//! call the kernel and fail with a rollback when the kernel cannot do the work.

use std::collections::BTreeSet;

use cad_ir::{
    BooleanOpKind, DimInput, DirectFaceEditOp, FastenerHoleStandard, FeatureSpec, HoleDepthSpec,
    HoleProfileSpec, IntInput, IrType, IrValue, NodeErrorDetail, NodeId, NodeKind, SketchDimValue,
    SketchPlaneSpec,
};
use cad_kernel_occt::{
    CsgNode, DraftFacesSpec, GeometryKernel, KernelOperationError, Mat4, PmiRecord, SketchPlane,
    SolidShape,
};
use cad_stdlib::{
    ClearanceClass, lookup_iso10642_csink, lookup_iso4762_cbore, lookup_metric_tap_drill,
};
use cad_syntax::Span;
use cad_topo::{
    CandidateEntity, MatchTolerances, RecoveryHint, ResolveResult, TopoEntityId, TopoError,
    TopoRef, TopoSnapshot, build_ambiguous_topo_hints, resolve_topo_ref,
};

use super::{Evaluated, FeatureError, NodeInputs, check_watertight, positive, valid_bbox};

const ENTRY_MARGIN_MM: f64 = 0.5;
const MAX_PATTERN_COPIES: usize = 256;
const HINT_CANDIDATES: usize = 8;

pub(super) fn eval_hole(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::Hole {
        solid,
        target_face,
        origin_mm,
        axis,
        diameter,
        depth,
        profile,
    } = spec
    else {
        return Err("hole node is missing a Hole feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    if let Some(face) = target_face.as_ref() {
        let _ = face_indices_for(topo_of(inputs, &solid)?, &shape, std::slice::from_ref(face), true)?;
    }
    let axis = unit_dir(axis, "hole axis")?;
    let diameter = positive(resolve_dim(inputs, &diameter, IrType::Length)?, "hole diameter")?;
    let depth = resolve_depth(inputs, &depth)?;
    let cut = match profile {
        HoleProfileSpec::Simple => HoleCut::Simple { diameter },
        HoleProfileSpec::Counterbore {
            bore_diameter,
            bore_depth,
        } => {
            let bore = positive(
                resolve_dim(inputs, &bore_diameter, IrType::Length)?,
                "counterbore diameter",
            )?;
            let bore_depth = positive(
                resolve_dim(inputs, &bore_depth, IrType::Length)?,
                "counterbore depth",
            )?;
            HoleCut::Counterbore {
                through: diameter,
                bore,
                bore_depth,
            }
        }
        HoleProfileSpec::Countersink {
            sink_diameter,
            sink_angle,
        } => {
            let sink = positive(
                resolve_dim(inputs, &sink_diameter, IrType::Length)?,
                "countersink diameter",
            )?;
            let angle = positive(
                resolve_dim(inputs, &sink_angle, IrType::Angle)?,
                "countersink angle",
            )?;
            HoleCut::Countersink {
                through: diameter,
                sink,
                angle_deg: angle,
            }
        }
    };
    cut_hole(kernel, &shape, origin_mm, axis, depth, cut)
}

pub(super) fn eval_std_fastener_hole(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::StdFastenerHole {
        solid,
        target_face,
        origin_mm,
        axis,
        standard,
        designation,
        clearance_class,
        depth,
        thread_depth,
    } = spec
    else {
        return Err("standard-library node is missing a StdFastenerHole feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    if let Some(face) = target_face.as_ref() {
        let _ = face_indices_for(topo_of(inputs, &solid)?, &shape, std::slice::from_ref(face), true)?;
    }
    let axis = unit_dir(axis, "hole axis")?;
    let depth = resolve_depth(inputs, &depth)?;
    if let Some(thread_depth) = thread_depth.as_ref() {
        let thread_depth = positive(
            resolve_dim(inputs, thread_depth, IrType::Length)?,
            "thread depth",
        )?;
        if let HoleDepth::Blind(hole_depth) = depth
            && thread_depth >= hole_depth
        {
            return Err(bound_exceeded(
                "thread_depth",
                Some(hole_depth),
                format!(
                    "thread depth {thread_depth:.3} mm exceeds the hole depth of {hole_depth:.3} mm"
                ),
                "E207_INVALID_FEATURE",
            ));
        }
    }
    let cut = fastener_cut(&standard, &designation, clearance_class.as_deref())?;
    let mut evaluated = cut_hole(kernel, &shape, origin_mm, axis, depth, cut)?;
    if let Evaluated::Solid(solid, _) = &mut evaluated {
        if solid.pmi.is_empty() && !shape.pmi.is_empty() {
            solid.pmi.extend(shape.pmi.iter().cloned());
        }
        if matches!(standard, FastenerHoleStandard::MetricTapDrill) {
            let label = designation.trim();
            let target_tag = format!("thread:{label}");
            solid.pmi.push(PmiRecord {
                kind: "pmi::thread".to_owned(),
                label: label.to_owned(),
                tolerance_mm: None,
                datum: None,
                target_tags: vec![target_tag],
                span: Span::default(),
            });
        }
    }
    Ok(evaluated)
}

pub(super) fn eval_linear_pattern(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::LinearPattern {
        solid,
        direction,
        count,
        spacing,
        second_direction,
    } = spec
    else {
        return Err("linear pattern node is missing a LinearPattern feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    let direction = unit_dir(direction, "pattern direction")?;
    let count = pattern_count(resolve_int(inputs, &count)?, "pattern count")?;
    let spacing = resolve_dim(inputs, &spacing, IrType::Length)?;
    require_spacing(spacing, count, "pattern spacing")?;
    let (second_dir, second_count, second_spacing) = match second_direction {
        Some(axis) => {
            let direction = unit_dir(axis.direction, "second pattern direction")?;
            let count = pattern_count(resolve_int(inputs, &axis.count)?, "second pattern count")?;
            let spacing = resolve_dim(inputs, &axis.spacing, IrType::Length)?;
            require_spacing(spacing, count, "second pattern spacing")?;
            (direction, count, spacing)
        }
        None => ([0.0, 0.0, 0.0], 1, 0.0),
    };
    let copies = count.saturating_mul(second_count);
    if copies > MAX_PATTERN_COPIES {
        return Err(format!("pattern copy count exceeds {MAX_PATTERN_COPIES}").into());
    }
    let mut placed = Vec::new();
    for i in 0..count {
        for j in 0..second_count {
            if i == 0 && j == 0 {
                continue;
            }
            let offset = add(
                scale(direction, i as f64 * spacing),
                scale(second_dir, j as f64 * second_spacing),
            );
            placed.push(translate_solid(kernel, &shape, offset)?);
        }
    }
    accept(kernel, unite(kernel, shape, placed, &inputs.node.name)?)
}

pub(super) fn eval_polar_pattern(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::PolarPattern {
        solid,
        axis_origin_mm,
        axis_direction,
        count,
        total_angle,
        radius,
        start_angle,
    } = spec
    else {
        return Err("polar pattern node is missing a PolarPattern feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    let axis = unit_dir(axis_direction, "pattern axis")?;
    let count = pattern_count(resolve_int(inputs, &count)?, "pattern count")?;
    let angle = resolve_dim(inputs, &total_angle, IrType::Angle)?;
    if !angle.is_finite() {
        return Err("pattern angle must be finite".into());
    }
    if count > 1 && angle.abs() <= 1e-9 {
        return Err("polar pattern angle must be non-zero when count is greater than 1".into());
    }
    if count > MAX_PATTERN_COPIES {
        return Err(format!("pattern copy count exceeds {MAX_PATTERN_COPIES}").into());
    }
    let radius_mm = match radius {
        Some(dim) => {
            let r = resolve_dim(inputs, &dim, IrType::Length)?;
            if !r.is_finite() || r < 0.0 {
                return Err("polar pattern radius must be finite and nonnegative".into());
            }
            Some(r)
        }
        None => None,
    };
    let start_angle_deg = match start_angle {
        Some(dim) => {
            let a = resolve_dim(inputs, &dim, IrType::Angle)?;
            if !a.is_finite() {
                return Err("polar pattern start_angle must be finite".into());
            }
            a
        }
        None => 0.0,
    };
    let step = polar_step(angle, count);
    let bbox = shape.csg.bbox();
    let cx = 0.5 * (bbox.min[0] + bbox.max[0]);
    let cy = 0.5 * (bbox.min[1] + bbox.max[1]);
    let radial = radius_mm.filter(|_| cx.hypot(cy) < 1e-3);

    if radial.is_none() && start_angle_deg.abs() <= 1e-9 {
        let mut placed = Vec::new();
        for index in 1..count {
            let matrix = rotate_about(axis_origin_mm, axis, step * index as f64);
            placed.push(transform_solid(kernel, &shape, matrix)?);
        }
        return accept(kernel, unite(kernel, shape, placed, &inputs.node.name)?);
    }

    let mut instances = Vec::with_capacity(count);
    for index in 0..count {
        let angle_deg = start_angle_deg + step * index as f64;
        let rot = rotate_about(axis_origin_mm, axis, angle_deg);
        let matrix = match radial {
            Some(r) => rot.mul_mat(Mat4::translate(r, 0.0, 0.0)),
            None => rot,
        };
        instances.push(transform_solid(kernel, &shape, matrix)?);
    }
    let mut iter = instances.into_iter();
    let first = iter.next().ok_or("polar pattern produced no instances")?;
    accept(kernel, unite(kernel, first, iter.collect(), &inputs.node.name)?)
}

pub(super) fn eval_mirror(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::Mirror {
        solid,
        plane,
        keep_original,
    } = spec
    else {
        return Err("mirror node is missing a Mirror feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    let frame = plane_frame(inputs, &solid, &shape, &plane)?;
    let mirrored = match frame {
        PlaneFrame::Datum { plane, offset } if offset.abs() <= 1e-9 => {
            kernel.mirror(&shape, plane, keep_original)
        }
        PlaneFrame::Datum { plane, offset } => {
            let (origin, normal) = datum_origin_normal(plane, offset);
            reflect_solid(kernel, shape, origin, normal, keep_original, &inputs.node.name)?
        }
        PlaneFrame::General { origin, normal } => {
            reflect_solid(kernel, shape, origin, normal, keep_original, &inputs.node.name)?
        }
    };
    accept(kernel, mirrored)
}

pub(super) fn eval_draft(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::Draft {
        solid,
        faces,
        pull_direction,
        neutral_plane,
        angle,
    } = spec
    else {
        return Err("draft node is missing a Draft feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    if !faces.is_empty() {
        let _ = face_indices_for(topo_of(inputs, &solid)?, &shape, &faces, false)?;
    }
    let pull = unit_dir(pull_direction, "draft pull direction")?;
    let angle = resolve_dim(inputs, &angle, IrType::Angle)?;
    let frame = plane_frame(inputs, &solid, &shape, &neutral_plane)?;
    let (origin, normal) = match frame {
        PlaneFrame::Datum { plane, offset } => datum_origin_normal(plane, offset),
        PlaneFrame::General { origin, normal } => (origin, normal),
    };
    let draft = DraftFacesSpec {
        draft_angle_deg: angle,
        pull_dir: pull,
        neutral_origin_mm: origin,
        neutral_normal: normal,
    };
    match cad_kernel_occt::draft_solid_faces(&shape, &draft, Span::default()) {
        Ok(solid) => accept(kernel, solid),
        Err(error) => Err(error.to_string().into()),
    }
}

pub(super) fn eval_direct_face_edit(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::DirectFaceEdit { solid, face, edit } = spec else {
        return Err("direct face edit node is missing a DirectFaceEdit feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    let index = *face_indices_for(topo_of(inputs, &solid)?, &shape, &[face], true)?
        .iter()
        .next()
        .ok_or("direct face edit requires one face")?;
    let span = Span::default();
    let edited = match edit {
        DirectFaceEditOp::Offset { distance } => {
            let distance = resolve_dim(inputs, &distance, IrType::Length)?;
            kernel.offset_through_hole(&shape, index, distance, span)
        }
        DirectFaceEditOp::Move { delta_mm } => {
            kernel.move_through_hole(&shape, index, delta_mm, span)
        }
        DirectFaceEditOp::DeleteAndHeal => kernel.delete_through_hole(&shape, index, span),
    };
    match edited {
        Ok(solid) => accept(kernel, solid),
        Err(error) => Err(error.to_string().into()),
    }
}

pub(super) fn eval_boolean(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::Boolean { op, target, tools } = spec else {
        return Err("boolean node is missing a Boolean feature spec".into());
    };
    if tools.is_empty() {
        return Err("boolean requires at least one tool solid".into());
    }
    let base = solid_of(inputs, &target)?;
    let mut tool_solids = Vec::with_capacity(tools.len());
    for tool in &tools {
        tool_solids.push(solid_of(inputs, tool)?);
    }
    if matches!(op, BooleanOpKind::Union)
        && inputs.node.metadata.get("dsl.hole_tool").map(String::as_str) == Some("counterbore")
        && tool_solids.len() == 1
        && let (
            CsgNode::Cylinder {
                origin: o_through,
                axis: low_axis,
                radius: r_through,
                ..
            },
            CsgNode::Cylinder {
                origin: o_cbore,
                axis: hi_axis,
                radius: r_cbore,
                height: h_cbore,
            },
        ) = (&base.csg, &tool_solids[0].csg)
        && (low_axis[0].abs() <= 1e-6
            && low_axis[1].abs() <= 1e-6
            && (low_axis[2] - 1.0).abs() <= 1e-6)
        && (hi_axis[0].abs() <= 1e-6
            && hi_axis[1].abs() <= 1e-6
            && (hi_axis[2] - 1.0).abs() <= 1e-6)
        && (o_through[0] - o_cbore[0]).abs() <= 1e-4
        && (o_through[1] - o_cbore[1]).abs() <= 1e-4
        && *r_cbore >= *r_through
        && *h_cbore > 1.0
    {
        let depth_cbore = (h_cbore - 1.0).max(1e-4);
        let z_entry = o_cbore[2] + depth_cbore;
        let depth_through = (z_entry - o_through[2]).max(depth_cbore + 1e-4);
        let tool = kernel.hole_counterbore(
            positive(2.0 * r_through, "counterbore through diameter")?,
            positive(2.0 * r_cbore, "counterbore diameter")?,
            positive(depth_cbore, "counterbore depth")?,
            positive(depth_through, "counterbore through depth")?,
            [0.0, 0.0, z_entry],
            inputs.node_span(),
        );
        let solid = if o_through[0] == 0.0 && o_through[1] == 0.0 {
            tool
        } else {
            kernel
                .transform(&tool, Mat4::translate(o_through[0], o_through[1], 0.0))
                .map_err(|error| error.to_string())?
        };
        return accept(kernel, solid);
    }
    if crate::is_imported_preview(&base) || tool_solids.iter().any(crate::is_imported_preview) {
        let message =
            "imported open-shell preview cannot participate in solid boolean operations".to_owned();
        return Err(FeatureError {
            reason: message.clone(),
            detail: Some(NodeErrorDetail {
                code: "E505_IMPORT_NON_MANIFOLD_OR_OPEN".to_owned(),
                operation: Some("boolean".to_owned()),
                native_status: None,
                message,
            }),
            sketch: None,
            recovery_hint: None,
        });
    }
    let solid = match op {
        BooleanOpKind::Union => {
            let mut items = Vec::with_capacity(tool_solids.len() + 1);
            items.push(base);
            items.extend(tool_solids);
            kernel.try_boolean_union_detailed(items, &inputs.node.name)?
        }
        BooleanOpKind::Subtract => {
            kernel.try_boolean_subtract_detailed(base, tool_solids)?
        }
        BooleanOpKind::Intersect => {
            let mut acc = base;
            for tool in tool_solids {
                acc = intersect_solids(kernel, acc, tool)?;
            }
            acc
        }
    };
    if solid.faces.is_empty() || !valid_bbox(solid.csg.bbox()) {
        return Err("kernel returned an empty solid or invalid bounding box".into());
    }
    Ok(Evaluated::Solid(solid, None))
}

pub(super) fn eval_fillet(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::Fillet {
        solid,
        edges,
        radius,
    } = spec
    else {
        return Err("fillet node is missing a Fillet feature spec".into());
    };
    let shape = solid_of(inputs, &solid)?;
    let edge_indices = if edges.is_empty() {
        Vec::new()
    } else {
        edge_indices_for(topo_of(inputs, &solid)?, &shape, &edges)?
    };
    let radius = positive(resolve_dim(inputs, &radius, IrType::Length)?, "fillet radius")?;
    let solid = apply_fillet_edges(kernel, &shape, radius, &edge_indices, Span::default())?;
    accept(kernel, solid)
}

pub(super) fn eval_chamfer(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::Chamfer {
        solid,
        edges,
        distance,
        distance2,
        angle,
    } = spec
    else {
        return Err("chamfer node is missing a Chamfer feature spec".into());
    };
    if distance2.is_some() || angle.is_some() {
        return Err("asymmetric chamfer is not supported by the active kernel".into());
    }
    let shape = solid_of(inputs, &solid)?;
    let edge_indices = if edges.is_empty() {
        Vec::new()
    } else {
        edge_indices_for(topo_of(inputs, &solid)?, &shape, &edges)?
    };
    let distance = positive(
        resolve_dim(inputs, &distance, IrType::Length)?,
        "chamfer distance",
    )?;
    let solid = apply_chamfer_edges(kernel, &shape, distance, &edge_indices, Span::default())?;
    accept(kernel, solid)
}

pub(super) fn eval_shell(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = read_spec(inputs)?;
    let FeatureSpec::Shell {
        solid,
        open_faces,
        thickness,
        inward,
    } = spec
    else {
        return Err("shell node is missing a Shell feature spec".into());
    };
    if !inward {
        return Err("outward shell is not supported by the active kernel".into());
    }
    let shape = solid_of(inputs, &solid)?;
    let normals = face_indices_for(topo_of(inputs, &solid)?, &shape, &open_faces, false)?
        .into_iter()
        .map(|index| shape.faces[index].normal_hint)
        .collect::<Vec<_>>();
    let thickness = positive(
        resolve_dim(inputs, &thickness, IrType::Length)?,
        "shell thickness",
    )?;
    let solid = apply_shell(kernel, &shape, thickness, &normals, Span::default())?;
    if solid.faces.is_empty() || !valid_bbox(solid.csg.bbox()) {
        return Err("kernel returned an empty solid or invalid bounding box".into());
    }
    Ok(Evaluated::Solid(solid, None))
}

pub(super) fn apply_fillet(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    radius: f64,
    span: Span,
) -> Result<SolidShape, FeatureError> {
    apply_dressup(
        shape,
        "radius",
        radius,
        "E205_INVALID_FILLET_CHAMFER",
        kernel.try_fillet(shape, radius, span),
    )
}

pub(super) fn apply_fillet_edges(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    radius: f64,
    edge_indices: &[usize],
    span: Span,
) -> Result<SolidShape, FeatureError> {
    if edge_indices.is_empty() {
        return apply_fillet(kernel, shape, radius, span);
    }
    apply_dressup(
        shape,
        "radius",
        radius,
        "E205_INVALID_FILLET_CHAMFER",
        kernel.try_fillet_edges(shape, radius, edge_indices, span),
    )
}

pub(super) fn apply_chamfer(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    distance: f64,
    span: Span,
) -> Result<SolidShape, FeatureError> {
    apply_dressup(
        shape,
        "distance",
        distance,
        "E205_INVALID_FILLET_CHAMFER",
        kernel.try_chamfer(shape, distance, span),
    )
}

pub(super) fn apply_chamfer_edges(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    distance: f64,
    edge_indices: &[usize],
    span: Span,
) -> Result<SolidShape, FeatureError> {
    if edge_indices.is_empty() {
        return apply_chamfer(kernel, shape, distance, span);
    }
    apply_dressup(
        shape,
        "distance",
        distance,
        "E205_INVALID_FILLET_CHAMFER",
        kernel.try_chamfer_edges(shape, distance, edge_indices, span),
    )
}

pub(super) fn apply_shell(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    thickness: f64,
    normals: &[[f64; 3]],
    span: Span,
) -> Result<SolidShape, FeatureError> {
    apply_dressup(
        shape,
        "thickness",
        thickness,
        "E206_INVALID_SHELL_THICKNESS",
        kernel.try_shell(shape, thickness, normals, span),
    )
}

pub(super) fn face_indices_for(
    snapshot: &TopoSnapshot,
    solid: &SolidShape,
    exprs: &[String],
    require_one: bool,
) -> Result<BTreeSet<usize>, FeatureError> {
    let mut indices = BTreeSet::new();
    for expr in exprs {
        for index in resolve_face_expr(snapshot, solid, expr, require_one)? {
            indices.insert(index);
        }
    }
    if require_one && indices.len() != 1 {
        if indices.len() > 1 {
            let ids = indices
                .iter()
                .filter_map(|index| snapshot.candidates.get(*index).map(|c| c.entity_id.clone()))
                .collect::<Vec<_>>();
            return Err(ambiguous(snapshot, &ids, "selection matched more than one face"));
        }
        return Err("topology selection did not match a face".into());
    }
    Ok(indices)
}

fn edge_indices_for(
    snapshot: &TopoSnapshot,
    solid: &SolidShape,
    exprs: &[String],
) -> Result<Vec<usize>, FeatureError> {
    let mut indices = BTreeSet::new();
    for expr in exprs {
        for index in resolve_edge_expr(snapshot, solid, expr)? {
            indices.insert(index);
        }
    }
    if indices.is_empty() {
        return Err("topology selection did not match an edge".into());
    }
    Ok(indices.into_iter().collect())
}

fn resolve_edge_expr(
    snapshot: &TopoSnapshot,
    solid: &SolidShape,
    expr: &str,
) -> Result<Vec<usize>, FeatureError> {
    if expr.trim().is_empty() {
        return Err("topology reference is empty".into());
    }
    let face_count = solid.faces.len().min(snapshot.candidates.len());
    let edge_end = (face_count + solid.edges.len()).min(snapshot.candidates.len());
    let edge_candidates = &snapshot.candidates[face_count..edge_end];
    if let Ok(reference) = serde_json::from_str::<TopoRef>(expr) {
        let ids = match resolve_topo_ref(&reference, snapshot, &MatchTolerances::default()) {
            Ok(ResolveResult::Unique(id)) => vec![id],
            Ok(ResolveResult::Multiple(ids)) => ids,
            Err(TopoError::AmbiguousMatch { candidates, reason }) => {
                return Err(ambiguous(snapshot, &candidates, &reason));
            }
            Err(TopoError::MissingMatch { reason }) => {
                return Err(missing(snapshot, expr, &reason));
            }
            Err(error) => return Err(error.to_string().into()),
        };
        let mut indices = Vec::with_capacity(ids.len());
        for id in ids {
            let index = edge_candidates
                .iter()
                .position(|candidate| candidate.entity_id == id)
                .ok_or_else(|| format!("topology reference `{expr}` must select edges"))?;
            indices.push(index);
        }
        return Ok(indices);
    }
    let matches = solid
        .edges
        .iter()
        .enumerate()
        .filter_map(|(index, edge)| {
            let candidate_match = edge_candidates
                .get(index)
                .is_some_and(|candidate| candidate.entity_id.0 == expr);
            (edge.edge_id == expr
                || edge.provenance_tags.iter().any(|tag| tag == expr)
                || candidate_match)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(format!("no edge matches topology reference `{expr}`").into());
    }
    Ok(matches)
}

fn resolve_face_expr(
    snapshot: &TopoSnapshot,
    solid: &SolidShape,
    expr: &str,
    require_one: bool,
) -> Result<Vec<usize>, FeatureError> {
    if let Ok(reference) = serde_json::from_str::<TopoRef>(expr) {
        let ids = match resolve_topo_ref(&reference, snapshot, &MatchTolerances::default()) {
            Ok(ResolveResult::Unique(id)) => vec![id],
            Ok(ResolveResult::Multiple(ids)) if !require_one => ids,
            Ok(ResolveResult::Multiple(ids)) => {
                return Err(ambiguous(
                    snapshot,
                    &ids,
                    "query matched more than one entity",
                ));
            }
            Err(TopoError::AmbiguousMatch { candidates, reason }) => {
                return Err(ambiguous(snapshot, &candidates, &reason));
            }
            Err(TopoError::MissingMatch { reason }) => {
                return Err(missing(snapshot, expr, &reason));
            }
            Err(error) => return Err(error.to_string().into()),
        };
        let face_count = solid.faces.len().min(snapshot.candidates.len());
        let mut indices = Vec::new();
        for id in ids {
            let index = snapshot.candidates[..face_count]
                .iter()
                .position(|candidate| candidate.entity_id == id)
                .ok_or_else(|| format!("topology reference `{expr}` must select faces"))?;
            indices.push(index);
        }
        return Ok(indices);
    }
    let matches = solid
        .faces
        .iter()
        .enumerate()
        .filter_map(|(index, face)| {
            (face.face_id == expr || face.provenance_tags.iter().any(|tag| tag == expr))
                .then_some(index)
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(format!("no face matches topology reference `{expr}`").into());
    }
    if require_one && matches.len() > 1 {
        let ids = matches
            .iter()
            .filter_map(|index| snapshot.candidates.get(*index).map(|c| c.entity_id.clone()))
            .collect::<Vec<_>>();
        return Err(ambiguous(snapshot, &ids, "face id matched more than one face"));
    }
    Ok(matches)
}

fn ambiguous(snapshot: &TopoSnapshot, ids: &[TopoEntityId], reason: &str) -> FeatureError {
    let matched = lookup_ids(snapshot, ids);
    let hint = build_ambiguous_topo_hints(&matched, HINT_CANDIDATES);
    hinted(
        format!("topology reference is ambiguous: {reason}"),
        hint,
        "E201_AMBIGUOUS_TOPOLOGY",
    )
}

fn missing(snapshot: &TopoSnapshot, requested: &str, reason: &str) -> FeatureError {
    hinted(
        format!("topology reference has no match: {reason}"),
        RecoveryHint::missing_topology(requested, &snapshot.candidates, HINT_CANDIDATES),
        "E201_AMBIGUOUS_TOPOLOGY",
    )
}

fn lookup_ids(snapshot: &TopoSnapshot, ids: &[TopoEntityId]) -> Vec<CandidateEntity> {
    ids.iter()
        .filter_map(|id| {
            snapshot
                .candidates
                .iter()
                .find(|candidate| &candidate.entity_id == id)
                .cloned()
        })
        .collect()
}

fn apply_dressup(
    shape: &SolidShape,
    parameter: &str,
    requested: f64,
    code: &str,
    result: Result<SolidShape, KernelOperationError>,
) -> Result<SolidShape, FeatureError> {
    let error = match result {
        Ok(solid) => {
            if dressup_did_not_modify(shape, &solid) {
                let mut feature = FeatureError::from(format!(
                    "{parameter} {requested} mm did not modify geometry (volume and topology unchanged)"
                ));
                feature.detail = Some(NodeErrorDetail {
                    code: code.to_owned(),
                    operation: Some(parameter.to_owned()),
                    native_status: None,
                    message: feature.reason.clone(),
                });
                return Err(feature);
            }
            return Ok(solid);
        }
        Err(error) => error,
    };
    let mut feature = FeatureError::from(error);
    let limit = dressup_limit(shape, parameter);
    if is_geometry_bound(&feature.reason, limit, requested) {
        feature.recovery_hint = Some(RecoveryHint::geometry_bound_exceeded(
            parameter,
            limit,
            feature.reason.clone(),
        ));
    }
    if feature.detail.is_none() {
        feature.detail = Some(NodeErrorDetail {
            code: code.to_owned(),
            operation: Some(parameter.to_owned()),
            native_status: None,
            message: feature.reason.clone(),
        });
    }
    Err(feature)
}

fn dressup_did_not_modify(before: &SolidShape, after: &SolidShape) -> bool {
    let volume_delta = (after.csg.compute_volume() - before.csg.compute_volume()).abs();
    volume_delta < 0.001
        && after.faces.len() == before.faces.len()
        && after.edges.len() == before.edges.len()
}

fn dressup_limit(shape: &SolidShape, parameter: &str) -> Option<f64> {
    match parameter {
        "radius" => fillet_limit(shape),
        "distance" | "thickness" => half_min_dimension(shape),
        _ => None,
    }
}

fn fillet_limit(shape: &SolidShape) -> Option<f64> {
    let size = shape.csg.bbox().size();
    let check = match &shape.csg {
        CsgNode::Box { .. } => size[0].min(size[1]),
        CsgNode::Cylinder { radius, height, .. } => (2.0 * radius).min(*height),
        CsgNode::Prism { plane, .. } => match plane {
            SketchPlane::Xy => size[0].min(size[1]),
            SketchPlane::Xz => size[0].min(size[2]),
            SketchPlane::Yz => size[1].min(size[2]),
        },
        _ => size[0].min(size[1]).min(size[2]),
    };
    (check > 0.0).then_some(0.5 * check)
}

fn half_min_dimension(shape: &SolidShape) -> Option<f64> {
    let size = shape.csg.bbox().size();
    let min_dim = match &shape.csg {
        CsgNode::Cylinder { radius, height, .. } => (2.0 * radius).min(*height),
        _ => size[0].min(size[1]).min(size[2]),
    };
    (min_dim > 0.0).then_some(0.5 * min_dim)
}

fn is_geometry_bound(reason: &str, limit: Option<f64>, requested: f64) -> bool {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("unavailable")
        || lower.contains("not supported")
        || lower.contains("unsupported")
    {
        return false;
    }
    if lower.contains("exceed") {
        return true;
    }
    limit.is_some_and(|limit| requested >= limit - 1e-9)
}

#[derive(Clone, Copy)]
enum HoleDepth {
    Through,
    Blind(f64),
}

#[derive(Clone, Copy)]
enum HoleCut {
    Simple { diameter: f64 },
    Counterbore { through: f64, bore: f64, bore_depth: f64 },
    Countersink { through: f64, sink: f64, angle_deg: f64 },
}

#[derive(Clone, Copy)]
struct AxialCyl {
    diameter: f64,
    start: f64,
    length: f64,
}

fn fastener_cut(
    standard: &FastenerHoleStandard,
    designation: &str,
    clearance_class: Option<&str>,
) -> Result<HoleCut, FeatureError> {
    match standard {
        FastenerHoleStandard::Iso4762Counterbore => {
            let row = lookup_iso4762_cbore(designation).map_err(|error| error.to_string())?;
            let class = parse_clearance(clearance_class)?;
            Ok(HoleCut::Counterbore {
                through: row.clearance_mm(class),
                bore: row.cbore_diameter_mm,
                bore_depth: row.cbore_depth_mm,
            })
        }
        FastenerHoleStandard::Iso10642Countersink => {
            let row = lookup_iso10642_csink(designation).map_err(|error| error.to_string())?;
            let through = if clearance_class.is_some() {
                let cbore = lookup_iso4762_cbore(designation).map_err(|error| error.to_string())?;
                cbore.clearance_mm(parse_clearance(clearance_class)?)
            } else {
                row.clearance_normal_mm
            };
            Ok(HoleCut::Countersink {
                through,
                sink: row.csink_diameter_mm,
                angle_deg: row.csink_angle_deg,
            })
        }
        FastenerHoleStandard::MetricTapDrill => {
            if clearance_class.is_some() {
                parse_clearance(clearance_class)?;
            }
            let diameter = lookup_metric_tap_drill(designation).map_err(|error| {
                let message = error.to_string();
                FeatureError {
                    reason: message.clone(),
                    detail: Some(NodeErrorDetail {
                        code: "E209_INVALID_THREAD_SPEC".to_owned(),
                        operation: Some("hole::tapped".to_owned()),
                        native_status: None,
                        message,
                    }),
                    sketch: None,
                    recovery_hint: None,
                }
            })?;
            Ok(HoleCut::Simple { diameter })
        }
        FastenerHoleStandard::MetricClearance => {
            let row = lookup_iso4762_cbore(designation).map_err(|error| error.to_string())?;
            Ok(HoleCut::Simple {
                diameter: row.clearance_mm(parse_clearance(clearance_class)?),
            })
        }
    }
}

fn parse_clearance(raw: Option<&str>) -> Result<ClearanceClass, FeatureError> {
    match raw.unwrap_or("normal").trim().to_ascii_lowercase().as_str() {
        "" | "normal" | "medium" | "h13" => Ok(ClearanceClass::Normal),
        "close" | "fine" | "h12" => Ok(ClearanceClass::Close),
        "loose" | "coarse" | "h14" => Ok(ClearanceClass::Loose),
        other => Err(format!("unknown clearance class `{other}`").into()),
    }
}

fn cut_hole(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    origin: [f64; 3],
    axis: [f64; 3],
    depth: HoleDepth,
    cut: HoleCut,
) -> Result<Evaluated, FeatureError> {
    let (hit_min, hit_max) = axis_hit_span(shape, origin, axis).ok_or_else(|| {
        FeatureError::from("hole axis does not intersect the target solid")
    })?;
    let cross = cross_section_min(shape, axis);
    let (bbox_min, bbox_max) = bbox_span(shape, origin, axis);
    let cut_end = match depth {
        HoleDepth::Through => bbox_max + ENTRY_MARGIN_MM,
        HoleDepth::Blind(depth) => depth,
    };
    let cut_start = match depth {
        HoleDepth::Through => bbox_min - ENTRY_MARGIN_MM,
        HoleDepth::Blind(_) => -ENTRY_MARGIN_MM,
    };
    if cut_end <= cut_start {
        return Err("hole depth must be positive".into());
    }
    let material = (hit_max - hit_min).max(0.0);
    let depth_limit = match depth {
        HoleDepth::Blind(depth) => depth,
        HoleDepth::Through => material,
    };
    let tools = hole_tools(cut, cross, depth_limit, cut_start, cut_end)?;
    let probe_lo = hit_min.max(cut_start).max(1e-3);
    let probe_hi = hit_max.min(cut_end);
    let probe = interior_probe(shape, origin, axis, probe_lo, probe_hi).ok_or_else(|| {
        FeatureError::from("hole does not remove material from the target solid")
    })?;
    let mut tool_shapes = Vec::with_capacity(tools.len());
    for tool in &tools {
        tool_shapes.push(place_tool(kernel, origin, axis, tool)?);
    }
    let cut_solid = subtract_tools(kernel, shape.clone(), &tool_shapes)?;
    if cut_solid.csg.contains(probe) {
        return Err("hole subtraction did not remove material".into());
    }
    accept(kernel, cut_solid)
}

fn hole_tools(
    cut: HoleCut,
    cross: f64,
    depth_limit: f64,
    cut_start: f64,
    cut_end: f64,
) -> Result<Vec<AxialCyl>, FeatureError> {
    let through_length = cut_end - cut_start;
    match cut {
        HoleCut::Simple { diameter } => {
            reject_diameter("diameter", diameter, cross)?;
            Ok(vec![AxialCyl {
                diameter,
                start: cut_start,
                length: through_length,
            }])
        }
        HoleCut::Counterbore {
            through,
            bore,
            bore_depth,
        } => {
            reject_diameter("diameter", through, cross)?;
            reject_diameter("bore_diameter", bore, cross)?;
            if bore <= through {
                return Err("counterbore diameter must exceed the through diameter".into());
            }
            if bore_depth >= depth_limit - 1e-9 {
                return Err(bound_exceeded(
                    "bore_depth",
                    Some(depth_limit),
                    format!(
                        "counterbore depth {bore_depth:.3} mm exceeds the hole depth of {depth_limit:.3} mm"
                    ),
                    "E207_INVALID_FEATURE",
                ));
            }
            Ok(vec![
                AxialCyl {
                    diameter: through,
                    start: cut_start,
                    length: through_length,
                },
                AxialCyl {
                    diameter: bore,
                    start: cut_start,
                    length: bore_depth - cut_start,
                },
            ])
        }
        HoleCut::Countersink {
            through,
            sink,
            angle_deg,
        } => {
            reject_diameter("diameter", through, cross)?;
            reject_diameter("sink_diameter", sink, cross)?;
            if sink <= through {
                return Err("countersink diameter must exceed the through diameter".into());
            }
            let cone_h = cone_height(through, sink, angle_deg)?;
            if cone_h >= depth_limit - 1e-9 {
                let half = angle_deg.to_radians() * 0.5;
                let limit = through + 2.0 * depth_limit * half.tan();
                return Err(bound_exceeded(
                    "sink_diameter",
                    Some(limit),
                    format!(
                        "countersink depth {cone_h:.3} mm exceeds the hole depth of {depth_limit:.3} mm"
                    ),
                    "E207_INVALID_FEATURE",
                ));
            }
            let mut tools = vec![
                AxialCyl {
                    diameter: through,
                    start: cut_start,
                    length: through_length,
                },
                AxialCyl {
                    diameter: sink,
                    start: cut_start,
                    length: cone_h - cut_start,
                },
            ];
            // Stair-step the cone inside the bounding seat. The analytic kernel
            // closes one coaxial step, so a non-watertight stack falls back to
            // the through hole plus that seat.
            const STEPS: usize = 4;
            let sink_r = sink * 0.5;
            let through_r = through * 0.5;
            for step in 1..STEPS {
                let frac = step as f64 / STEPS as f64;
                let radius = sink_r - (sink_r - through_r) * frac;
                let end = cone_h * (frac + 1.0 / STEPS as f64);
                tools.push(AxialCyl {
                    diameter: radius * 2.0,
                    start: cone_h * frac,
                    length: end - cone_h * frac,
                });
            }
            Ok(tools)
        }
    }
}

fn cone_height(through: f64, sink: f64, angle_deg: f64) -> Result<f64, FeatureError> {
    let half = angle_deg.to_radians() * 0.5;
    let tan_half = half.tan();
    if !(tan_half.is_finite() && tan_half > 1e-6) {
        return Err("countersink angle must be between 0 and 180 degrees".into());
    }
    Ok(((sink - through) * 0.5) / tan_half)
}

fn reject_diameter(parameter: &str, diameter: f64, cross: f64) -> Result<(), FeatureError> {
    if cross > 0.0 && diameter >= cross - 1e-9 {
        return Err(bound_exceeded(
            parameter,
            Some(cross),
            format!(
                "hole {parameter} {diameter:.3} mm exceeds the {cross:.3} mm cross-section perpendicular to the axis"
            ),
            "E207_INVALID_FEATURE",
        ));
    }
    Ok(())
}

fn subtract_tools(
    kernel: &dyn GeometryKernel,
    base: SolidShape,
    tools: &[SolidShape],
) -> Result<SolidShape, FeatureError> {
    let combined = kernel.try_boolean_subtract_detailed(base.clone(), tools.to_vec())?;
    if is_closed(kernel, &combined) {
        return Ok(combined);
    }
    if tools.len() > 2 {
        let reduced = kernel.try_boolean_subtract_detailed(base, vec![tools[0].clone(), tools[1].clone()])?;
        if is_closed(kernel, &reduced) {
            return Ok(reduced);
        }
    }
    Err("hole subtraction did not produce a watertight solid".into())
}

fn place_tool(
    kernel: &dyn GeometryKernel,
    origin: [f64; 3],
    axis: [f64; 3],
    tool: &AxialCyl,
) -> Result<SolidShape, FeatureError> {
    if tool.length <= 1e-9 || tool.diameter <= 0.0 {
        return Err("hole tool is degenerate".into());
    }
    // A downward vertical bore is rebuilt pointing +Z. The subtractor then
    // stitches the narrow and wide cylinders into one stepped bore.
    if drills_down_z(axis) {
        let z_far = origin[2] - (tool.start + tool.length);
        return kernel
            .try_make_cylinder(
                tool.diameter,
                tool.length,
                [origin[0], origin[1], z_far],
                [0.0, 0.0, 1.0],
                Span::default(),
            )
            .map_err(FeatureError::from);
    }
    let start = [
        origin[0] + axis[0] * tool.start,
        origin[1] + axis[1] * tool.start,
        origin[2] + axis[2] * tool.start,
    ];
    kernel
        .try_make_cylinder(tool.diameter, tool.length, start, axis, Span::default())
        .map_err(FeatureError::from)
}

fn drills_down_z(axis: [f64; 3]) -> bool {
    axis[0].abs() <= 1e-8 && axis[1].abs() <= 1e-8 && axis[2] < 0.0
}

fn axis_hit_span(shape: &SolidShape, origin: [f64; 3], axis: [f64; 3]) -> Option<(f64, f64)> {
    let (t_min, t_max) = bbox_span(shape, origin, axis);
    if !t_min.is_finite() || t_max - t_min < 1e-9 {
        return None;
    }
    let mut hit_min = f64::INFINITY;
    let mut hit_max = f64::NEG_INFINITY;
    const STEPS: usize = 48;
    for step in 0..=STEPS {
        let t = t_min + (t_max - t_min) * (step as f64) / STEPS as f64;
        if shape.csg.contains(point_at(origin, axis, t)) {
            hit_min = hit_min.min(t);
            hit_max = hit_max.max(t);
        }
    }
    (hit_max > hit_min).then_some((hit_min, hit_max))
}

fn interior_probe(
    shape: &SolidShape,
    origin: [f64; 3],
    axis: [f64; 3],
    t_lo: f64,
    t_hi: f64,
) -> Option<[f64; 3]> {
    if t_hi - t_lo < 1e-4 {
        return None;
    }
    const STEPS: usize = 24;
    for step in 0..=STEPS {
        let t = t_lo + (t_hi - t_lo) * (step as f64) / STEPS as f64;
        let point = point_at(origin, axis, t);
        if shape.csg.contains(point) {
            return Some(point);
        }
    }
    None
}

fn bbox_span(shape: &SolidShape, origin: [f64; 3], axis: [f64; 3]) -> (f64, f64) {
    let bounds = shape.csg.bbox();
    let mut t_min = f64::INFINITY;
    let mut t_max = f64::NEG_INFINITY;
    for i in 0..2 {
        for j in 0..2 {
            for k in 0..2 {
                let point = [
                    if i == 0 { bounds.min[0] } else { bounds.max[0] },
                    if j == 0 { bounds.min[1] } else { bounds.max[1] },
                    if k == 0 { bounds.min[2] } else { bounds.max[2] },
                ];
                let t = dot(sub(point, origin), axis);
                t_min = t_min.min(t);
                t_max = t_max.max(t);
            }
        }
    }
    (t_min, t_max)
}

fn cross_section_min(shape: &SolidShape, axis: [f64; 3]) -> f64 {
    let (u_axis, v_axis) = ortho(axis);
    let size = shape.csg.bbox().size();
    let extent = |direction: [f64; 3]| {
        direction[0].abs() * size[0]
            + direction[1].abs() * size[1]
            + direction[2].abs() * size[2]
    };
    extent(u_axis).min(extent(v_axis))
}

fn intersect_solids(
    kernel: &dyn GeometryKernel,
    left: SolidShape,
    right: SolidShape,
) -> Result<SolidShape, FeatureError> {
    if let (Some((left_min, left_max)), Some((right_min, right_max))) =
        (box_bounds(&left), box_bounds(&right))
    {
        let min = [
            left_min[0].max(right_min[0]),
            left_min[1].max(right_min[1]),
            left_min[2].max(right_min[2]),
        ];
        let max = [
            left_max[0].min(right_max[0]),
            left_max[1].min(right_max[1]),
            left_max[2].min(right_max[2]),
        ];
        let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        if size.iter().any(|edge| *edge <= 1e-9) {
            return Err("boolean intersect is empty".into());
        }
        return kernel
            .try_make_box(size, min, Span::default())
            .map_err(FeatureError::from);
    }
    cad_kernel_occt::try_boolean_intersect_detailed(left, right).map_err(|error| {
        format!("boolean intersect is unavailable for these solids: {error}").into()
    })
}

fn box_bounds(shape: &SolidShape) -> Option<([f64; 3], [f64; 3])> {
    match shape.csg {
        CsgNode::Box { min, max } => Some((min, max)),
        _ => None,
    }
}

fn translate_solid(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    offset: [f64; 3],
) -> Result<SolidShape, FeatureError> {
    transform_solid(
        kernel,
        shape,
        Mat4::translate(offset[0], offset[1], offset[2]),
    )
}

fn transform_solid(
    kernel: &dyn GeometryKernel,
    shape: &SolidShape,
    matrix: Mat4,
) -> Result<SolidShape, FeatureError> {
    kernel
        .transform(shape, matrix)
        .map_err(|error| FeatureError::from(error.to_string()))
}

fn reflect_solid(
    kernel: &dyn GeometryKernel,
    shape: SolidShape,
    origin: [f64; 3],
    normal: [f64; 3],
    keep_original: bool,
    name: &str,
) -> Result<SolidShape, FeatureError> {
    let mirrored = transform_solid(kernel, &shape, reflection(origin, normal))?;
    if keep_original {
        unite(kernel, shape, vec![mirrored], name)
    } else {
        Ok(mirrored)
    }
}

fn unite(
    kernel: &dyn GeometryKernel,
    base: SolidShape,
    copies: Vec<SolidShape>,
    name: &str,
) -> Result<SolidShape, FeatureError> {
    let mut acc = base;
    for copy in copies {
        acc = kernel.try_boolean_union_detailed(vec![acc, copy], name)?;
    }
    Ok(acc)
}

fn accept(kernel: &dyn GeometryKernel, solid: SolidShape) -> Result<Evaluated, FeatureError> {
    if solid.faces.is_empty() || !valid_bbox(solid.csg.bbox()) {
        return Err("kernel returned an empty solid or invalid bounding box".into());
    }
    check_watertight(kernel, &solid)?;
    Ok(Evaluated::Solid(solid, None))
}

fn is_closed(kernel: &dyn GeometryKernel, solid: &SolidShape) -> bool {
    !solid.faces.is_empty() && valid_bbox(solid.csg.bbox()) && kernel.check_solid(solid).watertight
}

enum PlaneFrame {
    Datum { plane: SketchPlane, offset: f64 },
    General { origin: [f64; 3], normal: [f64; 3] },
}

fn plane_frame(
    inputs: &NodeInputs<'_>,
    solid_id: &NodeId,
    shape: &SolidShape,
    plane: &SketchPlaneSpec,
) -> Result<PlaneFrame, FeatureError> {
    match plane {
        SketchPlaneSpec::Datum { plane, offset } => {
            let offset = match offset {
                Some(dim) => resolve_sketch_dim(inputs, dim)?,
                None => 0.0,
            };
            let plane = match plane {
                cad_ir::DatumPlane::Xy => SketchPlane::Xy,
                cad_ir::DatumPlane::Xz => SketchPlane::Xz,
                cad_ir::DatumPlane::Yz => SketchPlane::Yz,
            };
            Ok(PlaneFrame::Datum { plane, offset })
        }
        SketchPlaneSpec::Custom {
            origin_mm, normal, ..
        } => Ok(PlaneFrame::General {
            origin: *origin_mm,
            normal: unit_dir(*normal, "mirror plane normal")?,
        }),
        SketchPlaneSpec::FaceRef { face, offset } => {
            let index = *face_indices_for(
                topo_of(inputs, solid_id)?,
                shape,
                std::slice::from_ref(face),
                true,
            )?
            .iter()
            .next()
            .ok_or("mirror plane face was not found")?;
            let face = &shape.faces[index];
            let normal = unit_dir(face.normal_hint, "mirror face normal")?;
            let shift = match offset {
                Some(dim) => resolve_sketch_dim(inputs, dim)?,
                None => 0.0,
            };
            Ok(PlaneFrame::General {
                origin: add(face.center_hint, scale(normal, shift)),
                normal,
            })
        }
    }
}

fn datum_origin_normal(plane: SketchPlane, offset: f64) -> ([f64; 3], [f64; 3]) {
    match plane {
        SketchPlane::Xy => ([0.0, 0.0, offset], [0.0, 0.0, 1.0]),
        SketchPlane::Xz => ([0.0, offset, 0.0], [0.0, 1.0, 0.0]),
        SketchPlane::Yz => ([offset, 0.0, 0.0], [1.0, 0.0, 0.0]),
    }
}

fn reflection(origin: [f64; 3], normal: [f64; 3]) -> Mat4 {
    let along = 2.0 * dot(origin, normal);
    let mut matrix = Mat4::identity();
    for row in 0..3 {
        for col in 0..3 {
            let identity = if row == col { 1.0 } else { 0.0 };
            matrix.m[row][col] = identity - 2.0 * normal[row] * normal[col];
        }
        matrix.m[row][3] = along * normal[row];
    }
    matrix
}

fn rotate_about(origin: [f64; 3], axis: [f64; 3], angle_deg: f64) -> Mat4 {
    let to_origin = Mat4::translate(-origin[0], -origin[1], -origin[2]);
    let rotation = Mat4::rotate_axis(axis, angle_deg.to_radians());
    let back = Mat4::translate(origin[0], origin[1], origin[2]);
    back.mul_mat(rotation).mul_mat(to_origin)
}

fn polar_step(total_deg: f64, count: usize) -> f64 {
    if count <= 1 {
        return 0.0;
    }
    let turns = (total_deg / 360.0).abs();
    let nearest = turns.round();
    if nearest >= 1.0 && (turns - nearest).abs() < 1e-4 {
        total_deg.signum() * 360.0 / count as f64
    } else {
        total_deg / (count - 1) as f64
    }
}

fn resolve_depth(inputs: &NodeInputs<'_>, depth: &HoleDepthSpec) -> Result<HoleDepth, FeatureError> {
    match depth {
        HoleDepthSpec::ThroughAll => Ok(HoleDepth::Through),
        HoleDepthSpec::Blind { depth } => {
            let depth = positive(resolve_dim(inputs, depth, IrType::Length)?, "hole depth")?;
            Ok(HoleDepth::Blind(depth))
        }
    }
}

fn pattern_count(count: i64, label: &str) -> Result<usize, FeatureError> {
    if count < 1 {
        return Err(format!("{label} must be at least 1").into());
    }
    let count = usize::try_from(count).map_err(|_| format!("{label} is too large"))?;
    if count > MAX_PATTERN_COPIES {
        return Err(format!("{label} exceeds {MAX_PATTERN_COPIES}").into());
    }
    Ok(count)
}

fn require_spacing(spacing: f64, count: usize, label: &str) -> Result<(), FeatureError> {
    if count > 1 && !(spacing.is_finite() && spacing > 0.0) {
        return Err(format!("{label} must be finite and positive").into());
    }
    if count <= 1 && !spacing.is_finite() {
        return Err(format!("{label} must be finite").into());
    }
    Ok(())
}

fn read_spec(inputs: &NodeInputs<'_>) -> Result<FeatureSpec, FeatureError> {
    inputs
        .node
        .try_feature_spec()
        .map_err(|error| error.to_string().into())
}

fn solid_of(inputs: &NodeInputs<'_>, id: &NodeId) -> Result<SolidShape, FeatureError> {
    inputs
        .cache
        .get(id)
        .and_then(|snapshot| snapshot.solid.clone())
        .ok_or_else(|| format!("node `{id}` has no solid").into())
}

fn topo_of<'a>(inputs: &'a NodeInputs<'a>, id: &NodeId) -> Result<&'a TopoSnapshot, FeatureError> {
    inputs
        .cache
        .get(id)
        .and_then(|snapshot| snapshot.topo_snapshot.as_ref())
        .ok_or_else(|| format!("node `{id}` has no topology snapshot").into())
}

fn resolve_dim(inputs: &NodeInputs<'_>, dim: &DimInput, expected: IrType) -> Result<f64, FeatureError> {
    match dim {
        DimInput::Literal(quantity) => quantity_value(quantity, expected),
        DimInput::Param { param_id } => {
            let def = inputs
                .graph
                .parameters
                .get(&param_id.0)
                .ok_or_else(|| format!("unknown parameter `{param_id}`"))?;
            match &def.value {
                IrValue::Quantity(quantity) => quantity_value(quantity, expected),
                _ => Err(format!("parameter `{param_id}` is not a {expected:?}").into()),
            }
        }
        DimInput::NodeOutput { node_id, port } => {
            let node = inputs
                .graph
                .nodes
                .get(&node_id.0)
                .ok_or_else(|| format!("unknown node `{node_id}`"))?;
            if let NodeKind::ParameterRef { param_id } = &node.kind {
                return resolve_dim(inputs, &DimInput::param(param_id.clone()), expected);
            }
            Err(format!("node output `{node_id}.{port}` is not a resolved {expected:?}").into())
        }
    }
}

fn resolve_int(inputs: &NodeInputs<'_>, value: &IntInput) -> Result<i64, FeatureError> {
    match value {
        IntInput::Literal { value } => Ok(*value),
        IntInput::Param { param_id } => int_param(inputs, param_id),
        IntInput::NodeOutput { node_id, port } => {
            let node = inputs
                .graph
                .nodes
                .get(&node_id.0)
                .ok_or_else(|| format!("unknown node `{node_id}`"))?;
            if let NodeKind::ParameterRef { param_id } = &node.kind {
                return int_param(inputs, param_id);
            }
            Err(format!("node output `{node_id}.{port}` is not an integer").into())
        }
    }
}

fn int_param(inputs: &NodeInputs<'_>, param_id: &cad_ir::ParamId) -> Result<i64, FeatureError> {
    let def = inputs
        .graph
        .parameters
        .get(&param_id.0)
        .ok_or_else(|| format!("unknown parameter `{param_id}`"))?;
    match &def.value {
        IrValue::Int(value) => Ok(*value),
        _ => Err(format!("parameter `{param_id}` is not an integer").into()),
    }
}

fn resolve_sketch_dim(inputs: &NodeInputs<'_>, dim: &SketchDimValue) -> Result<f64, FeatureError> {
    match dim {
        SketchDimValue::Literal(quantity) => quantity_value(quantity, IrType::Length),
        SketchDimValue::Param { param_id } => {
            let def = inputs
                .graph
                .parameters
                .get(&param_id.0)
                .ok_or_else(|| format!("unknown parameter `{param_id}`"))?;
            match &def.value {
                IrValue::Quantity(quantity) => quantity_value(quantity, IrType::Length),
                _ => Err(format!("parameter `{param_id}` is not a length").into()),
            }
        }
    }
}

fn quantity_value(quantity: &cad_ir::Quantity, expected: IrType) -> Result<f64, FeatureError> {
    if quantity.ir_type() != expected {
        return Err(format!("expected {expected:?}, got {:?}", quantity.ir_type()).into());
    }
    let value = quantity.canonical_value();
    if value.is_finite() {
        Ok(value)
    } else {
        Err("dimension is not finite".into())
    }
}

fn bound_exceeded(
    parameter: &str,
    limit_mm: Option<f64>,
    reason: impl Into<String>,
    code: &str,
) -> FeatureError {
    let reason = reason.into();
    hinted(
        reason.clone(),
        RecoveryHint::geometry_bound_exceeded(parameter, limit_mm, reason),
        code,
    )
}

fn hinted(reason: impl Into<String>, hint: RecoveryHint, code: &str) -> FeatureError {
    let reason = reason.into();
    FeatureError {
        reason: reason.clone(),
        detail: Some(NodeErrorDetail {
            code: code.to_owned(),
            operation: None,
            native_status: None,
            message: reason,
        }),
        sketch: None,
        recovery_hint: Some(hint),
    }
}

fn unit_dir(value: [f64; 3], label: &str) -> Result<[f64; 3], FeatureError> {
    if !value.iter().all(|component| component.is_finite()) {
        return Err(format!("{label} must be finite").into());
    }
    let length = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt();
    if length < 1e-12 {
        return Err(format!("{label} must be non-zero").into());
    }
    Ok([value[0] / length, value[1] / length, value[2] / length])
}

fn point_at(origin: [f64; 3], axis: [f64; 3], t: f64) -> [f64; 3] {
    add(origin, scale(axis, t))
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn sub(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(value: [f64; 3], factor: f64) -> [f64; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn ortho(axis: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let helper = if axis[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let u_axis = unit_or_x(cross(helper, axis));
    let v_axis = cross(axis, u_axis);
    (u_axis, v_axis)
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn unit_or_x(value: [f64; 3]) -> [f64; 3] {
    let length = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt();
    if length < 1e-12 {
        [1.0, 0.0, 0.0]
    } else {
        [value[0] / length, value[1] / length, value[2] / length]
    }
}

//! Sketch and form-feature evaluation for [`super::GraphEvaluator`].
//!
//! A sketch is solved with `cad_gcs`, then extrude, revolve, sweep, and loft
//! turn the solved loops into solids. Parameter formulas are already applied
//! to the graph before this module runs.

use std::collections::{BTreeMap, BTreeSet};

use cad_gcs::{
    Constraint, DofStatus, EdgeId, PointId, SketchGeom, SketchSystem, SolveStatus, analyze_dof,
};
use cad_ir::{
    BlobHandle, DimInput, FeatureBodyOp, FeatureSpec, IrType, IrValue, NodeId, NodeKind, ParamId,
    SketchAnchor, SketchConstraint2D, SketchDimValue, SketchEntity2D, SketchPlaneSpec,
    SketchPointRef, StructuredSketch2D, SweepPathSpec,
};
use cad_kernel_occt::{GeometryKernel, Mat4, SketchPlane, SolidShape};
use cad_syntax::Span;

use super::{
    CachedOutput, Evaluated, FeatureError, NodeInputs, graph_mech_features, positive, valid_bbox,
};
use crate::sketch_sample::{
    DEFAULT_SPLINE_SEGMENTS_PER_SPAN, sample_closed_spline_polygon, sample_ellipse_polygon,
    sample_sketch_path,
};

const SOLVE_ITERS: usize = 100;
const SOLVE_TOL: f64 = 1e-6;
const ARC_SEGMENTS: usize = 16;

/// Solve status and profile counts for one sketch node.
#[derive(Debug, Clone, PartialEq)]
pub struct SketchEvalSummary {
    pub status: SolveStatus,
    pub remaining_dof: i64,
    pub iterations: usize,
    pub rms_residual: f64,
    pub closed_loop_count: usize,
    pub open_path_count: usize,
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SolvedLoop {
    polygon_2d: Vec<[f64; 2]>,
    is_outer: bool,
}

#[derive(Debug, Clone)]
pub(super) struct SketchFrame {
    origin: [f64; 3],
    x_axis: [f64; 3],
    y_axis: [f64; 3],
    normal: [f64; 3],
    /// Datum plane and normal offset, when the sketch lies on XY, XZ, or YZ.
    datum: Option<(SketchPlane, f64)>,
}

impl SketchFrame {
    fn map(&self, u: f64, v: f64) -> [f64; 3] {
        [
            self.origin[0] + self.x_axis[0] * u + self.y_axis[0] * v,
            self.origin[1] + self.x_axis[1] * u + self.y_axis[1] * v,
            self.origin[2] + self.x_axis[2] * u + self.y_axis[2] * v,
        ]
    }

    fn placement(&self) -> Mat4 {
        Mat4 {
            m: [
                [self.x_axis[0], self.y_axis[0], self.normal[0], self.origin[0]],
                [self.x_axis[1], self.y_axis[1], self.normal[1], self.origin[1]],
                [self.x_axis[2], self.y_axis[2], self.normal[2], self.origin[2]],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SolvedSketch {
    pub(super) summary: SketchEvalSummary,
    frame: SketchFrame,
    loops: Vec<SolvedLoop>,
    open_paths: Vec<Vec<[f64; 2]>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecKind {
    Point,
    Line,
    Arc,
    Circle,
    Rectangle,
    Spline,
}

struct EntityRec {
    kind: RecKind,
    edge: Option<EdgeId>,
    point: Option<PointId>,
    start: Option<PointId>,
    end: Option<PointId>,
    center: Option<PointId>,
    construction: bool,
    closed_polygon: Option<Vec<[f64; 2]>>,
    open_points: Option<Vec<[f64; 2]>>,
}

struct BuiltSketch {
    sys: SketchSystem,
    entities: BTreeMap<String, EntityRec>,
}

pub(super) fn eval_sketch(inputs: &NodeInputs<'_>) -> Result<Evaluated, FeatureError> {
    let spec = feature_spec(inputs)?;
    let FeatureSpec::Sketch2D(sketch) = spec else {
        return Err("sketch node is missing a Sketch2D feature spec".into());
    };
    let frame = match build_frame(inputs, &sketch.plane) {
        Ok(frame) => frame,
        Err(error) => {
            return Err(sketch_failure(
                error.reason,
                SolveStatus::FailedToConverge,
                0,
                0,
                0.0,
                xy_frame(),
            ));
        }
    };
    let built = match build_system(inputs, &sketch, &frame) {
        Ok(built) => built,
        Err(reason) => {
            return Err(sketch_failure(
                reason,
                SolveStatus::FailedToConverge,
                0,
                0,
                0.0,
                frame,
            ));
        }
    };
    let mut sys = built.sys;
    let report = sys.solve(SOLVE_ITERS, SOLVE_TOL);
    let remaining = remaining_dof(&analyze_dof(&sys, 1e-8));
    if report.status != SolveStatus::Converged {
        let reason = format!(
            "sketch solve {:?}: rms {:.3e}, remaining dof {remaining}",
            report.status, report.rms_residual
        );
        return Err(sketch_failure(
            reason,
            report.status,
            remaining,
            report.iterations,
            report.rms_residual,
            frame,
        ));
    }
    let (loops, open_paths) = match extract_profiles(&sys, &built.entities, &sketch) {
        Ok(profiles) => profiles,
        Err(reason) => {
            let mut err = sketch_failure(
                reason.clone(),
                SolveStatus::FailedToConverge,
                remaining,
                report.iterations,
                report.rms_residual,
                frame,
            );
            let lower = reason.to_ascii_lowercase();
            if lower.contains("open sketch")
                || lower.contains("not closed")
                || lower.contains("disconnected")
            {
                err.detail = Some(cad_ir::NodeErrorDetail {
                    code: "E204_OPEN_SKETCH_REGION".to_owned(),
                    operation: Some("sketch".to_owned()),
                    native_status: None,
                    message: reason,
                });
            }
            return Err(err);
        }
    };
    let summary = SketchEvalSummary {
        status: report.status,
        remaining_dof: remaining,
        iterations: report.iterations,
        rms_residual: report.rms_residual,
        closed_loop_count: loops.len(),
        open_path_count: open_paths.len(),
        diagnostic: None,
    };
    Ok(Evaluated::Track(CachedOutput::Sketch(Box::new(SolvedSketch {
        summary,
        frame,
        loops,
        open_paths,
    }))))
}

pub(super) fn eval_extrude(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = feature_spec(inputs)?;
    let FeatureSpec::Extrude {
        sketch,
        distance,
        direction,
        symmetric,
        operation,
        target,
    } = spec
    else {
        return Err("extrude node is missing an Extrude feature spec".into());
    };
    let solved = require_sketch(inputs, &sketch)?;
    let distance = resolve_dim(inputs, &distance, IrType::Length)?;
    let height = extrusion_height(&solved.frame, direction, distance, symmetric)?;
    let tool = extrude_solved(kernel, solved, height, symmetric)?;
    let solid = apply_body_op(inputs, kernel, operation, target.as_ref(), tool)?;
    accept_solid(solid)
}

pub(super) fn eval_revolve(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = feature_spec(inputs)?;
    let FeatureSpec::Revolve {
        sketch,
        axis_origin_mm,
        axis_direction,
        angle,
        operation,
        target,
    } = spec
    else {
        return Err("revolve node is missing a Revolve feature spec".into());
    };
    let solved = require_sketch(inputs, &sketch)?;
    let angle = resolve_dim(inputs, &angle, IrType::Angle)?;
    let (material, holes) = split_loops(solved);
    if material.is_empty() {
        return Err("revolve sketch has no closed profile".into());
    }
    let mut solids = Vec::new();
    for polygon in &material {
        solids.push(revolve_polygon(
            kernel,
            &solved.frame,
            polygon,
            axis_origin_mm,
            axis_direction,
            angle,
        )?);
    }
    let mut body = union_solids(kernel, solids, &inputs.node.name)?;
    if !holes.is_empty() {
        let mut tools = Vec::new();
        for polygon in &holes {
            tools.push(revolve_polygon(
                kernel,
                &solved.frame,
                polygon,
                axis_origin_mm,
                axis_direction,
                angle,
            )?);
        }
        body = kernel.try_boolean_subtract_detailed(body, tools)?;
    }
    let solid = apply_body_op(inputs, kernel, operation, target.as_ref(), body)?;
    accept_solid(solid)
}

pub(super) fn eval_sweep(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = feature_spec(inputs)?;
    let FeatureSpec::Sweep {
        profile,
        path,
        keep_normal: _,
    } = spec
    else {
        return Err("sweep node is missing a Sweep feature spec".into());
    };
    // The kernel carries the profile in path-local coordinates (parallel transport).
    let solved = require_sketch(inputs, &profile)?;
    let (material, _) = split_loops(solved);
    let polygon = material
        .into_iter()
        .next()
        .ok_or("sweep profile sketch has no closed profile")?;
    let path_3d = sweep_path(inputs, &path)?;
    let solid = kernel
        .sweep(&polygon, &path_3d, Span::default())
        .map_err(FeatureError::from)?;
    accept_solid(solid)
}

pub(super) fn eval_loft(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = feature_spec(inputs)?;
    let FeatureSpec::Loft {
        sections,
        guides,
        closed,
        as_surface: _,
    } = spec
    else {
        return Err("loft node is missing a Loft feature spec".into());
    };
    if !guides.is_empty() {
        return Err("loft guides are not supported by the geometry kernel".into());
    }
    if closed {
        return Err("closed loft is not supported by the geometry kernel".into());
    }
    if sections.len() < 2 {
        return Err("loft requires at least 2 section sketches".into());
    }
    let mut loft_sections = Vec::with_capacity(sections.len());
    for section_id in &sections {
        loft_sections.push(loft_section(inputs, section_id)?);
    }
    let solid = kernel
        .loft(&loft_sections, Span::default())
        .map_err(FeatureError::from)?;
    accept_solid(solid)
}

pub(super) fn eval_spec_box(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let spec = feature_spec(inputs)?;
    let FeatureSpec::PrimitiveBox {
        width,
        depth,
        height,
        origin_mm,
    } = spec
    else {
        return Err("box node is missing a PrimitiveBox feature spec".into());
    };
    let origin = if matches!(inputs.raw(&["origin"]), Some(IrValue::NodeRef { .. })) {
        inputs.origin()?
    } else {
        origin_mm
    };
    let size = [
        positive(resolve_dim(inputs, &width, IrType::Length)?, "box width")?,
        positive(resolve_dim(inputs, &depth, IrType::Length)?, "box depth")?,
        positive(resolve_dim(inputs, &height, IrType::Length)?, "box height")?,
    ];
    let solid = kernel.try_make_box(size, origin, inputs.node_span())?;
    accept_solid(solid)
}

pub(super) fn eval_spec_cylinder(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
) -> Result<Evaluated, FeatureError> {
    let (diameter, height, origin, axis) = match feature_spec(inputs) {
        Ok(FeatureSpec::PrimitiveCylinder {
            radius,
            height,
            origin_mm,
            axis,
        }) => {
            let origin = if matches!(inputs.raw(&["origin"]), Some(IrValue::NodeRef { .. })) {
                inputs.origin()?
            } else {
                origin_mm
            };
            let radius = positive(
                resolve_dim(inputs, &radius, IrType::Length)?,
                "cylinder radius",
            )?;
            let height = positive(
                resolve_dim(inputs, &height, IrType::Length)?,
                "cylinder height",
            )?;
            (radius * 2.0, height, origin, axis)
        }
        Ok(_) => return Err("cylinder node is missing a PrimitiveCylinder feature spec".into()),
        Err(_)
            if inputs.raw(&[cad_ir::FEATURE_SPEC_PORT]).is_none()
                && inputs.raw(&["diameter", "d"]).is_some() =>
        {
            let diameter = positive(inputs.length(&["diameter", "d"])?, "cylinder diameter")?;
            let height = positive(inputs.length(&["height", "h"])?, "cylinder height")?;
            let origin = inputs.origin()?;
            let axis = inputs
                .input(&["axis"])
                .map(super::vector)
                .transpose()?
                .unwrap_or(super::Z_AXIS);
            (diameter, height, origin, axis)
        }
        Err(err) => return Err(err),
    };
    let solid = kernel.try_make_cylinder(diameter, height, origin, axis, inputs.node_span())?;
    accept_solid(solid)
}

fn feature_spec(inputs: &NodeInputs<'_>) -> Result<FeatureSpec, FeatureError> {
    inputs
        .node
        .try_feature_spec()
        .map_err(|err| err.to_string().into())
}

fn require_sketch<'a>(
    inputs: &'a NodeInputs<'a>,
    id: &NodeId,
) -> Result<&'a SolvedSketch, FeatureError> {
    match inputs.cache.outputs.get(id) {
        Some(CachedOutput::Sketch(solved)) => {
            if solved.summary.status != SolveStatus::Converged {
                let why = solved
                    .summary
                    .diagnostic
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", solved.summary.status));
                return Err(format!("sketch `{id}` did not solve: {why}").into());
            }
            Ok(solved)
        }
        _ => Err(format!("sketch `{id}` has no solved profile").into()),
    }
}

fn resolve_dim(inputs: &NodeInputs<'_>, dim: &DimInput, expected: IrType) -> Result<f64, FeatureError> {
    match dim {
        DimInput::Literal(quantity) => quantity_value(quantity, expected),
        DimInput::Param { param_id } => resolve_param(inputs, param_id, expected),
        DimInput::NodeOutput { node_id, port } => resolve_node_output(inputs, node_id, port, expected),
    }
}

fn resolve_sketch_dim(
    inputs: &NodeInputs<'_>,
    dim: &SketchDimValue,
    expected: IrType,
) -> Result<f64, String> {
    match dim {
        SketchDimValue::Literal(quantity) => {
            quantity_value(quantity, expected).map_err(|error| error.reason)
        }
        SketchDimValue::Param { param_id } => {
            resolve_param(inputs, param_id, expected).map_err(|error| error.reason)
        }
    }
}

fn resolve_param(
    inputs: &NodeInputs<'_>,
    param_id: &ParamId,
    expected: IrType,
) -> Result<f64, FeatureError> {
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

fn resolve_node_output(
    inputs: &NodeInputs<'_>,
    node_id: &NodeId,
    port: &str,
    expected: IrType,
) -> Result<f64, FeatureError> {
    let node = inputs
        .graph
        .nodes
        .get(&node_id.0)
        .ok_or_else(|| format!("unknown node `{node_id}`"))?;
    if let NodeKind::ParameterRef { param_id } = &node.kind {
        return resolve_param(inputs, param_id, expected);
    }
    Err(format!("node output `{node_id}.{port}` is not a resolved {expected:?}").into())
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

fn accept_solid(solid: SolidShape) -> Result<Evaluated, FeatureError> {
    if solid.faces.is_empty() || !valid_bbox(solid.csg.bbox()) {
        return Err("kernel returned an empty solid or invalid bounding box".into());
    }
    Ok(Evaluated::Solid(solid, None))
}

fn apply_body_op(
    inputs: &NodeInputs<'_>,
    kernel: &dyn GeometryKernel,
    operation: FeatureBodyOp,
    target: Option<&NodeId>,
    tool: SolidShape,
) -> Result<SolidShape, FeatureError> {
    let target_id = match operation {
        FeatureBodyOp::NewBody => return Ok(tool),
        FeatureBodyOp::Join => target.ok_or("join requires a target body")?,
        FeatureBodyOp::Cut => target.ok_or("cut requires a target body")?,
        FeatureBodyOp::Intersect => target.ok_or("intersect requires a target body")?,
    };
    let base = inputs
        .cache
        .get(target_id)
        .and_then(|snap| snap.solid.clone())
        .ok_or_else(|| format!("target body `{target_id}` has no solid"))?;
    match operation {
        FeatureBodyOp::NewBody => Ok(tool),
        FeatureBodyOp::Join => {
            Ok(kernel.try_boolean_union_detailed(vec![base, tool], &inputs.node.name)?)
        }
        FeatureBodyOp::Cut => Ok(kernel.try_boolean_subtract_detailed(base, vec![tool])?),
        FeatureBodyOp::Intersect => {
            Ok(cad_kernel_occt::try_boolean_intersect_detailed(base, tool)?)
        }
    }
}

fn extrusion_height(
    frame: &SketchFrame,
    direction: Option<[f64; 3]>,
    distance: f64,
    symmetric: bool,
) -> Result<f64, FeatureError> {
    if !distance.is_finite() || distance.abs() <= 1e-9 {
        return Err("extrude distance must be finite and non-zero".into());
    }
    let sign = match direction {
        None => distance.signum(),
        Some(dir) => parallel_sign(&frame.normal, dir)?,
    };
    if symmetric {
        Ok(distance.abs())
    } else {
        Ok(sign * distance.abs())
    }
}

fn parallel_sign(normal: &[f64; 3], direction: [f64; 3]) -> Result<f64, FeatureError> {
    let len = direction[0].hypot(direction[1]).hypot(direction[2]);
    if !len.is_finite() || len <= 1e-12 {
        return Err("extrude direction must be a finite nonzero vector".into());
    }
    let dot = (direction[0] * normal[0] + direction[1] * normal[1] + direction[2] * normal[2]) / len;
    if dot > 0.999 {
        Ok(1.0)
    } else if dot < -0.999 {
        Ok(-1.0)
    } else {
        Err("extrude direction is not parallel to the sketch normal".into())
    }
}

fn extrude_solved(
    kernel: &dyn GeometryKernel,
    solved: &SolvedSketch,
    height: f64,
    symmetric: bool,
) -> Result<SolidShape, FeatureError> {
    let (material, holes) = split_loops(solved);
    if material.is_empty() {
        return Err("extrude sketch has no closed profile".into());
    }
    let mut solids = Vec::new();
    for polygon in &material {
        solids.push(extrude_polygon(kernel, &solved.frame, polygon, height, symmetric)?);
    }
    let mut body = union_solids(kernel, solids, "extrude")?;
    if !holes.is_empty() {
        let mut tools = Vec::new();
        for polygon in &holes {
            tools.push(extrude_polygon(kernel, &solved.frame, polygon, height, symmetric)?);
        }
        body = kernel.try_boolean_subtract_detailed(body, tools)?;
    }
    Ok(body)
}

fn extrude_polygon(
    kernel: &dyn GeometryKernel,
    frame: &SketchFrame,
    polygon: &[[f64; 2]],
    height: f64,
    symmetric: bool,
) -> Result<SolidShape, FeatureError> {
    let (plane, offset) = match frame.datum {
        Some((plane, offset)) => (plane, offset),
        None => (SketchPlane::Xy, 0.0),
    };
    let solid = kernel.try_extrude(plane, polygon, height, symmetric, Span::default())?;
    if frame.datum.is_some() {
        if offset.abs() <= 1e-9 {
            return Ok(solid);
        }
        let normal = plane.normal();
        place_solid(
            kernel,
            solid,
            Mat4::translate(normal[0] * offset, normal[1] * offset, normal[2] * offset),
        )
    } else {
        place_solid(kernel, solid, frame.placement())
    }
}

fn revolve_polygon(
    kernel: &dyn GeometryKernel,
    frame: &SketchFrame,
    polygon: &[[f64; 2]],
    axis_origin: [f64; 3],
    axis_direction: [f64; 3],
    angle: f64,
) -> Result<SolidShape, FeatureError> {
    let Some((plane, _)) = frame.datum else {
        return Err("revolve requires a datum sketch plane".into());
    };
    let mut flat = Vec::with_capacity(polygon.len());
    for &[u, v] in polygon {
        let point = frame.map(u, v);
        let shifted = [
            point[0] - axis_origin[0],
            point[1] - axis_origin[1],
            point[2] - axis_origin[2],
        ];
        let (su, sv, sw) = plane.from_3d(shifted);
        if sw.abs() > 1e-4 {
            return Err(
                "revolve profile must lie in a datum plane that contains the axis origin".into(),
            );
        }
        flat.push([su, sv]);
    }
    let solid = kernel.try_revolve(plane, &flat, axis_direction, angle, Span::default())?;
    place_solid(
        kernel,
        solid,
        Mat4::translate(axis_origin[0], axis_origin[1], axis_origin[2]),
    )
}

fn sweep_path(inputs: &NodeInputs<'_>, path: &SweepPathSpec) -> Result<Vec<[f64; 3]>, FeatureError> {
    match path {
        SweepPathSpec::Polyline { points_mm } => {
            if points_mm.len() < 2 {
                return Err("sweep polyline path requires at least 2 points".into());
            }
            Ok(points_mm.clone())
        }
        SweepPathSpec::Sketch { node_id } => {
            let solved = require_sketch(inputs, node_id)?;
            let uv = if let Some(path) = solved.open_paths.first() {
                path.clone()
            } else if let Some(loop_) = solved.loops.first() {
                let mut points = loop_.polygon_2d.clone();
                if let Some(first) = points.first().copied() {
                    points.push(first);
                }
                points
            } else {
                return Err(format!("sweep path sketch `{node_id}` has no curve").into());
            };
            if uv.len() < 2 {
                return Err(format!("sweep path sketch `{node_id}` has no curve").into());
            }
            Ok(uv
                .into_iter()
                .map(|[u, v]| solved.frame.map(u, v))
                .collect())
        }
    }
}

fn loft_section(
    inputs: &NodeInputs<'_>,
    section_id: &NodeId,
) -> Result<(Vec<[f64; 2]>, SketchPlane, f64), FeatureError> {
    let solved = require_sketch(inputs, section_id)?;
    let (material, _) = split_loops(solved);
    let polygon = material.into_iter().next().ok_or_else(|| {
        format!("loft section sketch `{section_id}` has no closed profile")
    })?;
    let (plane, offset) = solved
        .frame
        .datum
        .ok_or("loft sections must lie on a datum plane")?;
    Ok((polygon, plane, offset))
}

fn split_loops(solved: &SolvedSketch) -> (Vec<Vec<[f64; 2]>>, Vec<Vec<[f64; 2]>>) {
    let any_outer = solved.loops.iter().any(|loop_| loop_.is_outer);
    let mut material = Vec::new();
    let mut holes = Vec::new();
    for loop_ in &solved.loops {
        if !any_outer || loop_.is_outer {
            material.push(loop_.polygon_2d.clone());
        } else {
            holes.push(loop_.polygon_2d.clone());
        }
    }
    (material, holes)
}

fn union_solids(
    kernel: &dyn GeometryKernel,
    mut solids: Vec<SolidShape>,
    name: &str,
) -> Result<SolidShape, FeatureError> {
    if solids.is_empty() {
        return Err("sketch has no closed profile".into());
    }
    if solids.len() == 1 {
        return Ok(solids.remove(0));
    }
    Ok(kernel.try_boolean_union_detailed(solids, name)?)
}

fn place_solid(
    kernel: &dyn GeometryKernel,
    solid: SolidShape,
    matrix: Mat4,
) -> Result<SolidShape, FeatureError> {
    if is_near_identity(&matrix) {
        return Ok(solid);
    }
    kernel
        .transform(&solid, matrix)
        .map_err(|err| FeatureError::from(err.to_string()))
}

fn is_near_identity(matrix: &Mat4) -> bool {
    let identity = Mat4::identity();
    matrix
        .m
        .iter()
        .zip(identity.m)
        .all(|(row, id_row)| row.iter().zip(id_row).all(|(a, b)| (a - b).abs() <= 1e-9))
}

fn xy_frame() -> SketchFrame {
    SketchFrame {
        origin: [0.0, 0.0, 0.0],
        x_axis: [1.0, 0.0, 0.0],
        y_axis: [0.0, 1.0, 0.0],
        normal: [0.0, 0.0, 1.0],
        datum: Some((SketchPlane::Xy, 0.0)),
    }
}

fn sketch_failure(
    reason: impl Into<String>,
    status: SolveStatus,
    remaining_dof: i64,
    iterations: usize,
    rms_residual: f64,
    frame: SketchFrame,
) -> FeatureError {
    let reason = reason.into();
    let summary = SketchEvalSummary {
        status,
        remaining_dof,
        iterations,
        rms_residual,
        closed_loop_count: 0,
        open_path_count: 0,
        diagnostic: Some(reason.clone()),
    };
    FeatureError {
        reason,
        detail: None,
        sketch: Some(Box::new(SolvedSketch {
            summary,
            frame,
            loops: Vec::new(),
            open_paths: Vec::new(),
        })),
        recovery_hint: None,
    }
}

fn remaining_dof(analysis: &cad_gcs::DofAnalysis) -> i64 {
    match analysis.status {
        DofStatus::WellConstrained => 0,
        DofStatus::UnderConstrained { free_dof } => free_dof as i64,
        DofStatus::OverConstrained {
            redundant_or_conflicting,
        } => -(redundant_or_conflicting as i64),
    }
}

fn build_frame(inputs: &NodeInputs<'_>, plane: &SketchPlaneSpec) -> Result<SketchFrame, FeatureError> {
    match plane {
        SketchPlaneSpec::Datum { plane, offset } => {
            let offset = match offset {
                Some(dim) => resolve_sketch_dim(inputs, dim, IrType::Length)?,
                None => 0.0,
            };
            let (x_axis, y_axis, normal, datum) = match plane {
                cad_ir::DatumPlane::Xy => (
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                    SketchPlane::Xy,
                ),
                cad_ir::DatumPlane::Xz => (
                    [1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0],
                    SketchPlane::Xz,
                ),
                cad_ir::DatumPlane::Yz => (
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                    [1.0, 0.0, 0.0],
                    SketchPlane::Yz,
                ),
            };
            Ok(SketchFrame {
                origin: [normal[0] * offset, normal[1] * offset, normal[2] * offset],
                x_axis,
                y_axis,
                normal,
                datum: Some((datum, offset)),
            })
        }
        SketchPlaneSpec::FaceRef { face, offset } => {
            let offset = match offset {
                Some(dim) => resolve_sketch_dim(inputs, dim, IrType::Length)?,
                None => 0.0,
            };
            let anchor_solid = resolve_anchor_face(inputs, None, Some(face))?;
            let normal = unit(anchor_solid.normal_hint, "sketch face normal")?;
            let origin = [
                anchor_solid.center_hint[0] + normal[0] * offset,
                anchor_solid.center_hint[1] + normal[1] * offset,
                anchor_solid.center_hint[2] + normal[2] * offset,
            ];
            let datum = if normal[0].abs() <= 1e-6 && normal[1].abs() <= 1e-6 && normal[2].abs() > 0.999 {
                Some((SketchPlane::Xy, origin[2]))
            } else if normal[0].abs() <= 1e-6 && normal[2].abs() <= 1e-6 && normal[1].abs() > 0.999 {
                Some((SketchPlane::Xz, origin[1]))
            } else if normal[1].abs() <= 1e-6 && normal[2].abs() <= 1e-6 && normal[0].abs() > 0.999 {
                Some((SketchPlane::Yz, origin[0]))
            } else {
                None
            };
            let (x_axis, y_axis) = match datum {
                Some((SketchPlane::Xy, _)) => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                Some((SketchPlane::Xz, _)) => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
                Some((SketchPlane::Yz, _)) => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
                None => {
                    let x = perpendicular(normal);
                    let y = cross(normal, x);
                    (x, y)
                }
            };
            Ok(SketchFrame {
                origin,
                x_axis,
                y_axis,
                normal,
                datum,
            })
        }
        SketchPlaneSpec::Custom {
            origin_mm,
            normal,
            x_axis,
        } => {
            let normal = unit(*normal, "sketch normal")?;
            let x_axis = match x_axis {
                Some(axis) => unit(reject(*axis, normal), "sketch x axis")?,
                None => perpendicular(normal),
            };
            let y_axis = cross(normal, x_axis);
            Ok(SketchFrame {
                origin: *origin_mm,
                x_axis,
                y_axis,
                normal,
                datum: None,
            })
        }
    }
}

fn build_system(
    inputs: &NodeInputs<'_>,
    sketch: &StructuredSketch2D,
    frame: &SketchFrame,
) -> Result<BuiltSketch, String> {
    let mut sys = SketchSystem::new();
    let mut entities = BTreeMap::new();
    for entity in &sketch.entities {
        let id = entity.id().to_owned();
        if entities.contains_key(&id) {
            return Err(format!("duplicate sketch entity `{id}`"));
        }
        entities.insert(id, build_entity(inputs, &mut sys, sketch, entity)?);
    }
    for (index, constraint) in sketch.constraints.iter().enumerate() {
        lower_constraint(inputs, &mut sys, &entities, constraint, index)?;
    }
    if let Some(raw_tangents) = inputs.node.metadata.get("dsl.sketch_anchor_tangents")
        && let Ok(tangents) = serde_json::from_str::<Vec<AnchorTangentMeta>>(raw_tangents)
    {
        for (index, tangent) in tangents.into_iter().enumerate() {
            let arc_rec = entity(&entities, &tangent.arc_entity)?;
            let Some(arc_id) = arc_rec.edge else {
                continue;
            };
            let at_point = tangent
                .at_point
                .as_deref()
                .and_then(|pt_id| entities.get(pt_id).and_then(|rec| rec.point));
            let (line_dir, line_point) =
                match resolve_anchor_face(inputs, Some(tangent.anchor_index), tangent.face_query.as_deref()) {
                    Ok(face) => {
                        let (n2, c2) = match frame.datum.map(|(p, _)| p) {
                            Some(SketchPlane::Xz) => (
                                [face.normal_hint[0], face.normal_hint[2]],
                                [face.center_hint[0], face.center_hint[2]],
                            ),
                            Some(SketchPlane::Yz) => (
                                [face.normal_hint[1], face.normal_hint[2]],
                                [face.center_hint[1], face.center_hint[2]],
                            ),
                            _ => (
                                [face.normal_hint[0], face.normal_hint[1]],
                                [face.center_hint[0], face.center_hint[1]],
                            ),
                        };
                        ([-n2[1], n2[0]], c2)
                    }
                    Err(_) => ([1.0, 0.0], [0.0, 0.0]),
                };
            sys.add_constraint(Constraint::TangentLineArc {
                name: format!("anchor_tangent_{index}"),
                arc_id,
                line_dir,
                line_point,
                at_point,
            });
        }
    }
    Ok(BuiltSketch { sys, entities })
}

#[derive(serde::Deserialize)]
struct AnchorTangentMeta {
    arc_entity: String,
    anchor_index: usize,
    face_query: Option<String>,
    at_point: Option<String>,
}

fn resolve_anchor_face(
    inputs: &NodeInputs<'_>,
    anchor_index: Option<usize>,
    face_query: Option<&str>,
) -> Result<cad_kernel_occt::BRepFace, FeatureError> {
    let anchors = match inputs.raw(&["anchors"]) {
        Some(IrValue::List(items)) => items.as_slice(),
        _ => &[],
    };
    let anchor_val = match anchor_index {
        Some(idx) => anchors.get(idx),
        None => anchors.first(),
    };
    let snap = anchor_val
        .and_then(|v| inputs.upstream(v))
        .or_else(|| inputs.primary())
        .ok_or_else(|| FeatureError::from("sketch face reference has no upstream anchor solid"))?;
    let solid = snap
        .solid
        .as_ref()
        .ok_or_else(|| FeatureError::from("sketch anchor node has no solid"))?;
    if let Some(expr) = face_query {
        let topo = snap
            .topo_snapshot
            .as_ref()
            .ok_or_else(|| FeatureError::from("sketch anchor solid has no topology snapshot"))?;
        let indices = graph_mech_features::face_indices_for(topo, solid, &[expr.to_owned()], true)?;
        let idx = *indices
            .iter()
            .next()
            .ok_or_else(|| FeatureError::from("sketch face query matched no face"))?;
        return Ok(solid.faces[idx].clone());
    }
    solid
        .faces
        .first()
        .cloned()
        .ok_or_else(|| FeatureError::from("sketch anchor solid has no faces"))
}

fn build_entity(
    inputs: &NodeInputs<'_>,
    sys: &mut SketchSystem,
    sketch: &StructuredSketch2D,
    entity: &SketchEntity2D,
) -> Result<EntityRec, String> {
    let mentioned = |id: &str| sketch.constraints.iter().any(|c| constraint_mentions(c, id));
    match entity {
        SketchEntity2D::Point {
            position_mm,
            fixed,
            construction,
            ..
        } => {
            let point = sys.add_point(position_mm[0], position_mm[1], *fixed);
            Ok(record(
                RecKind::Point,
                None,
                Some(point),
                None,
                None,
                None,
                *construction,
                None,
                None,
            ))
        }
        SketchEntity2D::Line {
            start_mm,
            end_mm,
            construction,
            ..
        } => {
            let start = sys.add_point(start_mm[0], start_mm[1], false);
            let end = sys.add_point(end_mm[0], end_mm[1], false);
            let edge = sys.add_line(start, end);
            Ok(record(
                RecKind::Line,
                Some(edge),
                None,
                Some(start),
                Some(end),
                None,
                *construction,
                None,
                None,
            ))
        }
        SketchEntity2D::Arc {
            center_mm,
            start_mm,
            end_mm,
            construction,
            ..
        } => {
            let start = sys.add_point(start_mm[0], start_mm[1], false);
            let end = sys.add_point(end_mm[0], end_mm[1], false);
            let edge = sys.add_arc(start, end, Some((center_mm[0], center_mm[1])));
            let center = geom_center(sys, edge)?;
            Ok(record(
                RecKind::Arc,
                Some(edge),
                None,
                Some(start),
                Some(end),
                Some(center),
                *construction,
                None,
                None,
            ))
        }
        SketchEntity2D::Circle {
            id,
            center_mm,
            radius_mm,
            construction,
        } => {
            let edge = sys.add_circle(center_mm[0], center_mm[1], *radius_mm, !mentioned(id));
            let center = geom_center(sys, edge)?;
            Ok(record(
                RecKind::Circle,
                Some(edge),
                None,
                None,
                None,
                Some(center),
                *construction,
                None,
                None,
            ))
        }
        SketchEntity2D::Rectangle {
            id,
            origin_mm,
            width_mm,
            height_mm,
            centered,
            construction,
        } => {
            let (x, y) = if *centered {
                (origin_mm[0] - width_mm / 2.0, origin_mm[1] - height_mm / 2.0)
            } else {
                (origin_mm[0], origin_mm[1])
            };
            let edge = sys.add_rectangle(x, y, *width_mm, *height_mm, !mentioned(id));
            let corners = match sys.geoms.get(edge) {
                Some(SketchGeom::Rectangle { corners, .. }) => *corners,
                _ => return Err("internal error: rectangle was not created".into()),
            };
            Ok(record(
                RecKind::Rectangle,
                Some(edge),
                None,
                Some(corners[0]),
                Some(corners[2]),
                None,
                *construction,
                None,
                None,
            ))
        }
        SketchEntity2D::SplineBlob {
            blob,
            closed,
            construction,
            ..
        } => {
            let points = parse_spline_points(blob_bytes(inputs, blob)?)?;
            if *closed {
                let polygon = sample_closed_spline_polygon(&points, 12)?;
                Ok(record(
                    RecKind::Spline,
                    None,
                    None,
                    None,
                    None,
                    None,
                    *construction,
                    Some(polygon),
                    None,
                ))
            } else {
                if points.len() < 2 {
                    return Err("open spline requires at least 2 points".into());
                }
                Ok(record(
                    RecKind::Spline,
                    None,
                    None,
                    None,
                    None,
                    None,
                    *construction,
                    None,
                    Some(points),
                ))
            }
        }
        SketchEntity2D::Ellipse {
            center_mm,
            rx_mm,
            ry_mm,
            segments,
            construction,
            ..
        } => {
            let polygon = sample_ellipse_polygon(*center_mm, *rx_mm, *ry_mm, segments.map(f64::from))?;
            Ok(record(
                RecKind::Spline,
                None,
                None,
                None,
                None,
                None,
                *construction,
                Some(polygon),
                None,
            ))
        }
        SketchEntity2D::Spline {
            control_points_mm,
            closed,
            segments_per_span,
            construction,
            ..
        } => {
            if *closed {
                let segs = segments_per_span
                    .map(|n| n as usize)
                    .unwrap_or(DEFAULT_SPLINE_SEGMENTS_PER_SPAN);
                let polygon = sample_closed_spline_polygon(control_points_mm, segs)?;
                Ok(record(
                    RecKind::Spline,
                    None,
                    None,
                    None,
                    None,
                    None,
                    *construction,
                    Some(polygon),
                    None,
                ))
            } else {
                if control_points_mm.len() < 2 {
                    return Err("open spline requires at least 2 points".into());
                }
                Ok(record(
                    RecKind::Spline,
                    None,
                    None,
                    None,
                    None,
                    None,
                    *construction,
                    None,
                    Some(control_points_mm.clone()),
                ))
            }
        }
    }
}

fn record(
    kind: RecKind,
    edge: Option<EdgeId>,
    point: Option<PointId>,
    start: Option<PointId>,
    end: Option<PointId>,
    center: Option<PointId>,
    construction: bool,
    closed_polygon: Option<Vec<[f64; 2]>>,
    open_points: Option<Vec<[f64; 2]>>,
) -> EntityRec {
    EntityRec {
        kind,
        edge,
        point,
        start,
        end,
        center,
        construction,
        closed_polygon,
        open_points,
    }
}

fn lower_constraint(
    inputs: &NodeInputs<'_>,
    sys: &mut SketchSystem,
    entities: &BTreeMap<String, EntityRec>,
    constraint: &SketchConstraint2D,
    index: usize,
) -> Result<(), String> {
    let name_of = |name: &Option<String>, fallback: &str| {
        name.clone()
            .unwrap_or_else(|| format!("{fallback}_{index}"))
    };
    match constraint {
        SketchConstraint2D::Coincident { name, a, b } => {
            sys.add_constraint(Constraint::Coincident {
                name: name_of(name, "coincident"),
                p1: anchor_point(entities, a)?,
                p2: anchor_point(entities, b)?,
            });
        }
        SketchConstraint2D::PointOnLine { name, point, line } => {
            let (p1, p2) = line_ends(entities, line)?;
            sys.add_constraint(Constraint::PointOnLine {
                name: name_of(name, "point_on_line"),
                point: anchor_point(entities, point)?,
                line_p1: p1,
                line_p2: p2,
            });
        }
        SketchConstraint2D::Horizontal { name, entity } => {
            let (p1, p2) = line_ends(entities, entity)?;
            sys.add_constraint(Constraint::Horizontal {
                name: name_of(name, "horizontal"),
                p1,
                p2,
            });
        }
        SketchConstraint2D::Vertical { name, entity } => {
            let (p1, p2) = line_ends(entities, entity)?;
            sys.add_constraint(Constraint::Vertical {
                name: name_of(name, "vertical"),
                p1,
                p2,
            });
        }
        SketchConstraint2D::Parallel { name, a, b } => {
            let (a1, a2) = line_ends(entities, a)?;
            let (b1, b2) = line_ends(entities, b)?;
            sys.add_constraint(Constraint::Parallel {
                name: name_of(name, "parallel"),
                a1,
                a2,
                b1,
                b2,
            });
        }
        SketchConstraint2D::Perpendicular { name, a, b } => {
            let (a1, a2) = line_ends(entities, a)?;
            let (b1, b2) = line_ends(entities, b)?;
            sys.add_constraint(Constraint::Perpendicular {
                name: name_of(name, "perpendicular"),
                a1,
                a2,
                b1,
                b2,
            });
        }
        SketchConstraint2D::Tangent { name, a, b } => {
            lower_tangent(sys, entities, &name_of(name, "tangent"), a, b)?;
        }
        SketchConstraint2D::Concentric { name, a, b } => {
            sys.add_constraint(Constraint::Coincident {
                name: name_of(name, "concentric"),
                p1: center_of(entities, a)?,
                p2: center_of(entities, b)?,
            });
        }
        SketchConstraint2D::Equal { name, a, b } => {
            let left = entity(entities, a)?;
            let right = entity(entities, b)?;
            match (left.kind, right.kind) {
                (RecKind::Line, RecKind::Line) => {
                    let (a1, a2) = line_ends(entities, a)?;
                    let (b1, b2) = line_ends(entities, b)?;
                    let pa1 = sys.points[a1];
                    let pa2 = sys.points[a2];
                    let len = (pa2.x - pa1.x).hypot(pa2.y - pa1.y);
                    if len.is_finite() && len > 1e-6 {
                        sys.add_constraint(Constraint::Distance {
                            name: name_of(name, "equal"),
                            p1: b1,
                            p2: b2,
                            target: len,
                        });
                    }
                }
                (RecKind::Circle | RecKind::Arc, RecKind::Circle | RecKind::Arc) => {
                    let edge_a = curve_edge(entities, a)?;
                    let edge_b = curve_edge(entities, b)?;
                    let radius_a = match sys.geoms.get(edge_a) {
                        Some(SketchGeom::Circle { radius, .. }) => Some(*radius),
                        Some(SketchGeom::Arc { p1, center, .. }) => {
                            let pa = sys.points[*p1];
                            let pc = sys.points[*center];
                            Some((pa.x - pc.x).hypot(pa.y - pc.y))
                        }
                        _ => None,
                    };
                    if let Some(r) = radius_a
                        && r.is_finite()
                        && r > 1e-6
                    {
                        unlock_circle_radius(sys, edge_b);
                        sys.add_constraint(Constraint::Radius {
                            name: name_of(name, "equal"),
                            edge_id: edge_b,
                            target: r,
                        });
                    }
                }
                _ => {}
            }
        }
        SketchConstraint2D::Symmetric { .. } => {
            return Err("symmetric constraints are not supported by the 2D solver yet".into());
        }
        SketchConstraint2D::Distance { name, a, b, value } => {
            let target = resolve_sketch_dim(inputs, value, IrType::Length)?;
            sys.add_constraint(Constraint::Distance {
                name: name_of(name, "distance"),
                p1: anchor_point(entities, a)?,
                p2: anchor_point(entities, b)?,
                target,
            });
        }
        SketchConstraint2D::Radius { name, entity, value } => {
            let target = resolve_sketch_dim(inputs, value, IrType::Length)?;
            let edge = curve_edge(entities, entity)?;
            unlock_circle_radius(sys, edge);
            sys.add_constraint(Constraint::Radius {
                name: name_of(name, "radius"),
                edge_id: edge,
                target,
            });
        }
        SketchConstraint2D::Diameter { name, entity, value } => {
            let target = resolve_sketch_dim(inputs, value, IrType::Length)? / 2.0;
            let edge = curve_edge(entities, entity)?;
            unlock_circle_radius(sys, edge);
            sys.add_constraint(Constraint::Radius {
                name: name_of(name, "diameter"),
                edge_id: edge,
                target,
            });
        }
        SketchConstraint2D::Angle { .. } => {
            return Err("angle constraints are not supported by the 2D solver yet".into());
        }
    }
    Ok(())
}

fn lower_tangent(
    sys: &mut SketchSystem,
    entities: &BTreeMap<String, EntityRec>,
    name: &str,
    a: &str,
    b: &str,
) -> Result<(), String> {
    let (left, right) = (entity(entities, a)?, entity(entities, b)?);
    let (line, arc) = match (left.kind, right.kind) {
        (RecKind::Line, RecKind::Arc) => (left, right),
        (RecKind::Arc, RecKind::Line) => (right, left),
        (RecKind::Arc, RecKind::Arc) => {
            let arc1 = left.edge.ok_or("tangent arc is missing")?;
            let arc2 = right.edge.ok_or("tangent arc is missing")?;
            sys.add_constraint(Constraint::TangentArcs {
                name: name.to_owned(),
                arc1,
                arc2,
            });
            return Ok(());
        }
        _ => {
            return Err(
                "tangent requires a line and an arc, or two arcs; circles are not supported yet"
                    .into(),
            );
        }
    };
    let (p1, p2) = (line.start.ok_or("tangent line is missing")?, line.end.ok_or("tangent line is missing")?);
    let start = sys.points[p1];
    let end = sys.points[p2];
    let arc_id = arc.edge.ok_or("tangent arc is missing")?;
    sys.add_constraint(Constraint::TangentLineArc {
        name: name.to_owned(),
        arc_id,
        line_dir: [end.x - start.x, end.y - start.y],
        line_point: [start.x, start.y],
        at_point: None,
    });
    Ok(())
}

fn extract_profiles(
    sys: &SketchSystem,
    entities: &BTreeMap<String, EntityRec>,
    sketch: &StructuredSketch2D,
) -> Result<(Vec<SolvedLoop>, Vec<Vec<[f64; 2]>>), String> {
    let mut used = BTreeSet::new();
    let mut loops = Vec::new();
    if sketch.profiles.is_empty() {
        for entity in &sketch.entities {
            let rec = entity_rec(entities, entity.id())?;
            if rec.construction {
                continue;
            }
            if let Some(polygon) = &rec.closed_polygon {
                loops.push(SolvedLoop {
                    polygon_2d: polygon.clone(),
                    is_outer: false,
                });
                used.insert(entity.id().to_owned());
            } else if matches!(rec.kind, RecKind::Circle | RecKind::Rectangle)
                && let Some(edge) = rec.edge
            {
                loops.push(SolvedLoop {
                    polygon_2d: sys.sample_closed_region(&[edge], ARC_SEGMENTS)?,
                    is_outer: false,
                });
                used.insert(entity.id().to_owned());
            }
        }
    } else {
        for profile in &sketch.profiles {
            loops.push(SolvedLoop {
                polygon_2d: sample_profile(sys, entities, profile)?,
                is_outer: profile.is_outer,
            });
            used.extend(profile.entity_ids.iter().cloned());
        }
    }

    let mut open_paths = Vec::new();
    let mut chain = Vec::new();
    for entity in &sketch.entities {
        let rec = entity_rec(entities, entity.id())?;
        if rec.construction || used.contains(entity.id()) {
            push_chain(sys, &mut chain, &mut open_paths)?;
            continue;
        }
        if matches!(rec.kind, RecKind::Line | RecKind::Arc)
            && let Some(edge) = rec.edge
        {
            chain.push(edge);
        } else if let Some(points) = &rec.open_points {
            push_chain(sys, &mut chain, &mut open_paths)?;
            open_paths.push(points.clone());
        } else {
            push_chain(sys, &mut chain, &mut open_paths)?;
        }
    }
    push_chain(sys, &mut chain, &mut open_paths)?;
    Ok((loops, open_paths))
}

fn sample_profile(
    sys: &SketchSystem,
    entities: &BTreeMap<String, EntityRec>,
    profile: &cad_ir::SketchProfileLoop,
) -> Result<Vec<[f64; 2]>, String> {
    if profile.entity_ids.len() == 1 {
        let rec = entity_rec(entities, &profile.entity_ids[0])?;
        if rec.construction {
            return Err(format!(
                "profile `{}` references construction entity `{}`",
                profile.id, profile.entity_ids[0]
            ));
        }
        if let Some(polygon) = &rec.closed_polygon {
            return Ok(polygon.clone());
        }
    }
    let mut edges = Vec::new();
    for id in &profile.entity_ids {
        let rec = entity_rec(entities, id)?;
        if rec.construction {
            return Err(format!(
                "profile `{}` references construction entity `{id}`",
                profile.id
            ));
        }
        if rec.closed_polygon.is_some() {
            return Err(format!(
                "spline `{id}` must be the only entity in profile `{}`",
                profile.id
            ));
        }
        let edge = rec
            .edge
            .ok_or_else(|| format!("entity `{id}` cannot form a closed profile"))?;
        edges.push(edge);
    }
    sys.sample_closed_region(&edges, ARC_SEGMENTS)
        .map_err(|err| format!("profile `{}`: {err}", profile.id))
}

fn push_chain(
    sys: &SketchSystem,
    chain: &mut Vec<EdgeId>,
    open_paths: &mut Vec<Vec<[f64; 2]>>,
) -> Result<(), String> {
    if chain.is_empty() {
        return Ok(());
    }
    let points = sample_sketch_path(sys, chain, SketchPlane::Xy)?;
    open_paths.push(points.into_iter().map(|point| [point[0], point[1]]).collect());
    chain.clear();
    Ok(())
}

fn constraint_mentions(constraint: &SketchConstraint2D, id: &str) -> bool {
    match constraint {
        SketchConstraint2D::Coincident { a, b, .. } => a.entity_id == id || b.entity_id == id,
        SketchConstraint2D::PointOnLine { point, line, .. } => point.entity_id == id || line == id,
        SketchConstraint2D::Horizontal { entity, .. }
        | SketchConstraint2D::Vertical { entity, .. }
        | SketchConstraint2D::Radius { entity, .. }
        | SketchConstraint2D::Diameter { entity, .. } => entity == id,
        SketchConstraint2D::Parallel { a, b, .. }
        | SketchConstraint2D::Perpendicular { a, b, .. }
        | SketchConstraint2D::Tangent { a, b, .. }
        | SketchConstraint2D::Concentric { a, b, .. }
        | SketchConstraint2D::Equal { a, b, .. }
        | SketchConstraint2D::Angle { a, b, .. } => a == id || b == id,
        SketchConstraint2D::Symmetric { a, b, axis_line, .. } => {
            a.entity_id == id || b.entity_id == id || axis_line == id
        }
        SketchConstraint2D::Distance { a, b, .. } => a.entity_id == id || b.entity_id == id,
    }
}

fn anchor_point(entities: &BTreeMap<String, EntityRec>, point_ref: &SketchPointRef) -> Result<PointId, String> {
    let rec = entity(entities, &point_ref.entity_id)?;
    let id = match point_ref.anchor {
        SketchAnchor::Midpoint => {
            return Err(format!(
                "midpoint anchors are not supported yet (entity `{}`)",
                point_ref.entity_id
            ));
        }
        SketchAnchor::Point => rec.point.or(rec.start).or(rec.center),
        SketchAnchor::Start => rec.start.or(rec.point),
        SketchAnchor::End => rec.end,
        SketchAnchor::Center => rec.center.or(rec.point),
    };
    id.ok_or_else(|| {
        format!(
            "entity `{}` has no {:?} anchor",
            point_ref.entity_id, point_ref.anchor
        )
    })
}

fn line_ends(entities: &BTreeMap<String, EntityRec>, id: &str) -> Result<(PointId, PointId), String> {
    let rec = entity(entities, id)?;
    if rec.kind != RecKind::Line {
        return Err(format!("entity `{id}` is not a line"));
    }
    match (rec.start, rec.end) {
        (Some(start), Some(end)) => Ok((start, end)),
        _ => Err(format!("line `{id}` is missing endpoints")),
    }
}

fn center_of(entities: &BTreeMap<String, EntityRec>, id: &str) -> Result<PointId, String> {
    entity(entities, id)?
        .center
        .ok_or_else(|| format!("entity `{id}` has no center"))
}

fn curve_edge(entities: &BTreeMap<String, EntityRec>, id: &str) -> Result<EdgeId, String> {
    let rec = entity(entities, id)?;
    if !matches!(rec.kind, RecKind::Circle | RecKind::Arc) {
        return Err(format!("entity `{id}` is not a circle or arc"));
    }
    rec.edge.ok_or_else(|| format!("entity `{id}` has no curve"))
}

fn entity<'a>(entities: &'a BTreeMap<String, EntityRec>, id: &str) -> Result<&'a EntityRec, String> {
    entities
        .get(id)
        .ok_or_else(|| format!("unknown sketch entity `{id}`"))
}

fn entity_rec<'a>(
    entities: &'a BTreeMap<String, EntityRec>,
    id: &str,
) -> Result<&'a EntityRec, String> {
    entity(entities, id)
}

fn geom_center(sys: &SketchSystem, edge: EdgeId) -> Result<PointId, String> {
    match sys.geoms.get(edge) {
        Some(SketchGeom::Circle { center, .. } | SketchGeom::Arc { center, .. }) => Ok(*center),
        _ => Err("internal error: curve center was not created".into()),
    }
}

fn unlock_circle_radius(sys: &mut SketchSystem, edge: EdgeId) {
    if let Some(SketchGeom::Circle { radius_fixed, .. }) = sys.geoms.get_mut(edge) {
        *radius_fixed = false;
    }
}

fn blob_bytes<'a>(inputs: &'a NodeInputs<'a>, handle: &BlobHandle) -> Result<&'a [u8], String> {
    inputs
        .blob_payload(handle)
        .ok_or_else(|| format!("blob `{handle}` payload is not loaded"))
}

fn parse_spline_points(bytes: &[u8]) -> Result<Vec<[f64; 2]>, String> {
    if let Ok(points) = serde_json::from_slice::<Vec<[f64; 2]>>(bytes) {
        return Ok(points);
    }
    #[derive(serde::Deserialize)]
    struct Points {
        points: Vec<[f64; 2]>,
    }
    if let Ok(points) = serde_json::from_slice::<Points>(bytes) {
        return Ok(points.points);
    }
    Err("spline blob must be a JSON array of [x, y] points".into())
}

fn unit(value: [f64; 3], label: &str) -> Result<[f64; 3], FeatureError> {
    let len = value[0].hypot(value[1]).hypot(value[2]);
    if !len.is_finite() || len <= 1e-12 {
        return Err(format!("{label} must be a finite nonzero vector").into());
    }
    Ok([value[0] / len, value[1] / len, value[2] / len])
}

fn reject(value: [f64; 3], normal: [f64; 3]) -> [f64; 3] {
    let dot = value[0] * normal[0] + value[1] * normal[1] + value[2] * normal[2];
    [
        value[0] - dot * normal[0],
        value[1] - dot * normal[1],
        value[2] - dot * normal[2],
    ]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn perpendicular(normal: [f64; 3]) -> [f64; 3] {
    let hint = if normal[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let axis = cross(normal, hint);
    let len = axis[0].hypot(axis[1]).hypot(axis[2]).max(1e-12);
    [axis[0] / len, axis[1] / len, axis[2] / len]
}

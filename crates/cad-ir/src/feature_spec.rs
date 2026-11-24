//! Strongly-typed V2.0 mechanical feature specifications (`FeatureSpec`) and
//! deterministic lossless conversion to/from [`IrNode`].

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    AngleUnit, BlobHandle, ExternalRefId, IrError, IrNode, IrType, IrValue, LengthUnit, NodeId,
    NodeKind, NodeState, OUTPUT_PORT, ParamId, Quantity, SketchPlaneSpec, StructuredSketch2D,
    TopoRefExpr, wire,
};

/// Canonical input key carrying the deterministic serialized [`FeatureSpec`] payload
/// on nodes constructed via [`IrNode::from_feature_spec`].
pub const FEATURE_SPEC_PORT: &str = "feature_spec";

fn default_output_port() -> String {
    OUTPUT_PORT.to_owned()
}

/// A dimensional feature input bound either to a literal physical [`Quantity`],
/// a graph parameter [`ParamId`], or an upstream node output port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum DimInput {
    Literal(Quantity),
    Param {
        param_id: ParamId,
    },
    NodeOutput {
        node_id: NodeId,
        #[serde(default = "default_output_port")]
        port: String,
    },
}

impl DimInput {
    pub fn mm(value: f64) -> Result<Self, IrError> {
        Ok(Self::Literal(Quantity::length(value, LengthUnit::Mm)?))
    }

    pub fn deg(value: f64) -> Result<Self, IrError> {
        Ok(Self::Literal(Quantity::angle(value, AngleUnit::Deg)?))
    }

    pub const fn quantity(q: Quantity) -> Self {
        Self::Literal(q)
    }

    pub fn param(param_id: impl Into<ParamId>) -> Self {
        Self::Param {
            param_id: param_id.into(),
        }
    }

    pub fn node(node_id: impl Into<NodeId>) -> Self {
        Self::NodeOutput {
            node_id: node_id.into(),
            port: OUTPUT_PORT.to_owned(),
        }
    }

    fn to_port_value(&self) -> Option<IrValue> {
        match self {
            Self::Literal(q) => Some(IrValue::Quantity(*q)),
            Self::Param { .. } => None,
            Self::NodeOutput { node_id, port } => Some(IrValue::NodeRef {
                node_id: node_id.clone(),
                port: port.clone(),
            }),
        }
    }

    fn from_port_value(value: &IrValue) -> Option<Self> {
        match value {
            IrValue::Quantity(q) => Some(Self::Literal(*q)),
            IrValue::NodeRef { node_id, port } => Some(Self::NodeOutput {
                node_id: node_id.clone(),
                port: port.clone(),
            }),
            _ => None,
        }
    }

    fn collect_param(&self, out: &mut BTreeSet<ParamId>) {
        if let Self::Param { param_id } = self {
            out.insert(param_id.clone());
        }
    }

    fn validate_expected(&self, node_id: &NodeId, expected: IrType, field: &str) -> Result<(), IrError> {
        match self {
            Self::Literal(q) => {
                if !q.canonical_value().is_finite() {
                    return Err(IrError::NonFiniteNumber);
                }
                if q.ir_type() != expected {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: format!("field `{field}` expects {:?}, got {:?}", expected, q.ir_type()),
                    });
                }
                Ok(())
            }
            Self::Param { param_id } => {
                if param_id.0.trim().is_empty() {
                    return Err(IrError::EmptyId { entity: "parameter" });
                }
                Ok(())
            }
            Self::NodeOutput { node_id: target, .. } => {
                if target.0.trim().is_empty() {
                    return Err(IrError::EmptyId { entity: "node" });
                }
                Ok(())
            }
        }
    }
}

/// An integer count feature input bound to a literal integer, a [`ParamId`],
/// or an upstream node output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntInput {
    Literal {
        value: i64,
    },
    Param {
        param_id: ParamId,
    },
    NodeOutput {
        node_id: NodeId,
        #[serde(default = "default_output_port")]
        port: String,
    },
}

impl IntInput {
    pub const fn literal(value: i64) -> Self {
        Self::Literal { value }
    }

    pub fn param(param_id: impl Into<ParamId>) -> Self {
        Self::Param {
            param_id: param_id.into(),
        }
    }

    pub fn node(node_id: impl Into<NodeId>) -> Self {
        Self::NodeOutput {
            node_id: node_id.into(),
            port: OUTPUT_PORT.to_owned(),
        }
    }

    fn to_port_value(&self) -> Option<IrValue> {
        match self {
            Self::Literal { value } => Some(IrValue::Int(*value)),
            Self::Param { .. } => None,
            Self::NodeOutput { node_id, port } => Some(IrValue::NodeRef {
                node_id: node_id.clone(),
                port: port.clone(),
            }),
        }
    }

    fn from_port_value(value: &IrValue) -> Option<Self> {
        match value {
            IrValue::Int(v) => Some(Self::Literal { value: *v }),
            IrValue::NodeRef { node_id, port } => Some(Self::NodeOutput {
                node_id: node_id.clone(),
                port: port.clone(),
            }),
            _ => None,
        }
    }

    fn collect_param(&self, out: &mut BTreeSet<ParamId>) {
        if let Self::Param { param_id } = self {
            out.insert(param_id.clone());
        }
    }
}

/// Boolean combination behavior for sketch-based solid features (`Extrude` / `Revolve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureBodyOp {
    #[default]
    NewBody,
    Join,
    Cut,
    Intersect,
}

/// Path source for a `Sweep` feature.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SweepPathSpec {
    Sketch { node_id: NodeId },
    Polyline { points_mm: Vec<[f64; 3]> },
}

/// Depth specification for `Hole` and `StdFastenerHole`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HoleDepthSpec {
    ThroughAll,
    Blind { depth: DimInput },
}

/// Hole cross-section geometry for generic `Hole` features.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HoleProfileSpec {
    Simple,
    Counterbore {
        bore_diameter: DimInput,
        bore_depth: DimInput,
    },
    Countersink {
        sink_diameter: DimInput,
        sink_angle: DimInput,
    },
}

/// Standardized fastener hole families backed by `cad-stdlib`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FastenerHoleStandard {
    Iso4762Counterbore,
    Iso10642Countersink,
    MetricTapDrill,
    MetricClearance,
}

/// Secondary linear pattern axis definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinearPatternAxisSpec {
    #[serde(serialize_with = "wire::vec3")]
    pub direction: [f64; 3],
    pub count: IntInput,
    pub spacing: DimInput,
}

/// Boolean operation mode for `FeatureSpec::Boolean`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BooleanOpKind {
    Union,
    Subtract,
    Intersect,
}

/// Direct B-Rep face modification operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum DirectFaceEditOp {
    Offset {
        distance: DimInput,
    },
    Move {
        #[serde(serialize_with = "wire::vec3")]
        delta_mm: [f64; 3],
    },
    DeleteAndHeal,
}

/// Geometry source for an assembly `ComponentInstance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComponentSourceSpec {
    LocalNode { node_id: NodeId },
    ExternalRef { ref_id: ExternalRefId },
}

/// Rigid-body 3D assembly mate constraint kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MateKindSpec {
    Coincident,
    Concentric,
    Distance {
        value: DimInput,
    },
    Angle {
        value: DimInput,
    },
    Parallel,
    Perpendicular,
    Gear {
        #[serde(serialize_with = "wire::number")]
        ratio: f64,
    },
}

/// Declarative engineering assertion rule checked during verification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesignAssertionKind {
    VolumeBounds {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_mm3: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_mm3: Option<f64>,
    },
    MinWallThickness {
        min: DimInput,
    },
    BooleanCondition {
        condition: NodeId,
    },
    Custom {
        expression: String,
        #[serde(default)]
        params: BTreeMap<String, String>,
    },
}

/// Complete V2.0 strongly-typed mechanical feature & assembly specification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "feature", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeatureSpec {
    PrimitiveBox {
        width: DimInput,
        depth: DimInput,
        height: DimInput,
        #[serde(default, serialize_with = "wire::vec3")]
        origin_mm: [f64; 3],
    },
    PrimitiveCylinder {
        radius: DimInput,
        height: DimInput,
        #[serde(default, serialize_with = "wire::vec3")]
        origin_mm: [f64; 3],
        #[serde(default = "default_z_axis", serialize_with = "wire::vec3")]
        axis: [f64; 3],
    },
    #[serde(rename = "sketch_2d")]
    Sketch2D(StructuredSketch2D),
    Extrude {
        sketch: NodeId,
        distance: DimInput,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_opt_vec3"
        )]
        direction: Option<[f64; 3]>,
        #[serde(default)]
        symmetric: bool,
        #[serde(default)]
        operation: FeatureBodyOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<NodeId>,
    },
    Revolve {
        sketch: NodeId,
        #[serde(default, serialize_with = "wire::vec3")]
        axis_origin_mm: [f64; 3],
        #[serde(default = "default_z_axis", serialize_with = "wire::vec3")]
        axis_direction: [f64; 3],
        angle: DimInput,
        #[serde(default)]
        operation: FeatureBodyOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<NodeId>,
    },
    Sweep {
        profile: NodeId,
        path: SweepPathSpec,
        #[serde(default)]
        keep_normal: bool,
    },
    Loft {
        sections: Vec<NodeId>,
        #[serde(default)]
        guides: Vec<NodeId>,
        #[serde(default)]
        closed: bool,
        #[serde(default)]
        as_surface: bool,
    },
    Hole {
        solid: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_face: Option<TopoRefExpr>,
        #[serde(default, serialize_with = "wire::vec3")]
        origin_mm: [f64; 3],
        #[serde(default = "default_neg_z_axis", serialize_with = "wire::vec3")]
        axis: [f64; 3],
        diameter: DimInput,
        depth: HoleDepthSpec,
        profile: HoleProfileSpec,
    },
    StdFastenerHole {
        solid: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_face: Option<TopoRefExpr>,
        #[serde(default, serialize_with = "wire::vec3")]
        origin_mm: [f64; 3],
        #[serde(default = "default_neg_z_axis", serialize_with = "wire::vec3")]
        axis: [f64; 3],
        standard: FastenerHoleStandard,
        designation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clearance_class: Option<String>,
        depth: HoleDepthSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_depth: Option<DimInput>,
    },
    Fillet {
        solid: NodeId,
        edges: Vec<TopoRefExpr>,
        radius: DimInput,
    },
    Chamfer {
        solid: NodeId,
        edges: Vec<TopoRefExpr>,
        distance: DimInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        distance2: Option<DimInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        angle: Option<DimInput>,
    },
    Shell {
        solid: NodeId,
        open_faces: Vec<TopoRefExpr>,
        thickness: DimInput,
        #[serde(default = "default_true")]
        inward: bool,
    },
    Draft {
        solid: NodeId,
        faces: Vec<TopoRefExpr>,
        #[serde(serialize_with = "wire::vec3")]
        pull_direction: [f64; 3],
        neutral_plane: SketchPlaneSpec,
        angle: DimInput,
    },
    LinearPattern {
        solid: NodeId,
        #[serde(serialize_with = "wire::vec3")]
        direction: [f64; 3],
        count: IntInput,
        spacing: DimInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        second_direction: Option<LinearPatternAxisSpec>,
    },
    PolarPattern {
        solid: NodeId,
        #[serde(default, serialize_with = "wire::vec3")]
        axis_origin_mm: [f64; 3],
        #[serde(default = "default_z_axis", serialize_with = "wire::vec3")]
        axis_direction: [f64; 3],
        count: IntInput,
        total_angle: DimInput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<DimInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_angle: Option<DimInput>,
    },
    Mirror {
        solid: NodeId,
        plane: SketchPlaneSpec,
        #[serde(default = "default_true")]
        keep_original: bool,
    },
    Boolean {
        op: BooleanOpKind,
        target: NodeId,
        tools: Vec<NodeId>,
    },
    DirectFaceEdit {
        solid: NodeId,
        face: TopoRefExpr,
        edit: DirectFaceEditOp,
    },
    ImportStep {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blob: Option<BlobHandle>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default = "default_one", serialize_with = "wire::number")]
        scale: f64,
        #[serde(default)]
        as_reference_only: bool,
    },
    ComponentInstance {
        source: ComponentSourceSpec,
        #[serde(default, serialize_with = "wire::vec3")]
        translation_mm: [f64; 3],
        #[serde(default, serialize_with = "wire::vec3")]
        rotation_deg: [f64; 3],
        #[serde(default)]
        grounded: bool,
    },
    MateConstraint {
        left: NodeId,
        right: NodeId,
        left_ref: TopoRefExpr,
        right_ref: TopoRefExpr,
        mate_kind: MateKindSpec,
        #[serde(default)]
        flip: bool,
    },
    TagTopology {
        solid: NodeId,
        tag: String,
        targets: Vec<TopoRefExpr>,
    },
    DesignAssertion {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<NodeId>,
        assertion_id: String,
        kind: DesignAssertionKind,
    },
}

const fn default_z_axis() -> [f64; 3] {
    [0.0, 0.0, 1.0]
}

const fn default_neg_z_axis() -> [f64; 3] {
    [0.0, 0.0, -1.0]
}

const fn default_true() -> bool {
    true
}

const fn default_one() -> f64 {
    1.0
}

fn serialize_opt_vec3<S: serde::Serializer>(
    v: &Option<[f64; 3]>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match v {
        Some(arr) => wire::vec3(arr, serializer),
        None => serializer.serialize_none(),
    }
}

fn node_ref(node_id: &NodeId) -> IrValue {
    IrValue::NodeRef {
        node_id: node_id.clone(),
        port: OUTPUT_PORT.to_owned(),
    }
}

fn check_finite_vec3(v: &[f64; 3]) -> Result<(), IrError> {
    if v.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(IrError::NonFiniteNumber)
    }
}

fn check_nonzero_dir(node_id: &NodeId, dir: &[f64; 3], field: &str) -> Result<(), IrError> {
    check_finite_vec3(dir)?;
    if dir.iter().all(|x| *x == 0.0) {
        return Err(IrError::InvalidFeatureSpec {
            node_id: node_id.clone(),
            message: format!("direction `{field}` cannot be the zero vector"),
        });
    }
    Ok(())
}

impl FeatureSpec {
    /// Return the corresponding legacy-compatible [`NodeKind`] for this feature.
    pub const fn node_kind(&self) -> NodeKind {
        match self {
            Self::PrimitiveBox { .. } => NodeKind::PrimitiveBox,
            Self::PrimitiveCylinder { .. } => NodeKind::PrimitiveCylinder,
            Self::Sketch2D(_) => NodeKind::Sketch2D,
            Self::Extrude { .. } => NodeKind::Extrude,
            Self::Revolve { .. } => NodeKind::Revolve,
            Self::Sweep { .. } => NodeKind::Sweep,
            Self::Loft { .. } => NodeKind::LoftGuided,
            Self::Hole { .. } => NodeKind::Hole,
            Self::StdFastenerHole { .. } => NodeKind::StdLibCall,
            Self::Fillet { .. } => NodeKind::Fillet,
            Self::Chamfer { .. } => NodeKind::Chamfer,
            Self::Shell { .. } => NodeKind::Shell,
            Self::Draft { .. } => NodeKind::Draft,
            Self::LinearPattern { .. } => NodeKind::PatternLinear,
            Self::PolarPattern { .. } => NodeKind::PatternPolar,
            Self::Mirror { .. } => NodeKind::Mirror,
            Self::Boolean { op, .. } => match op {
                BooleanOpKind::Union => NodeKind::BooleanUnion,
                BooleanOpKind::Subtract => NodeKind::BooleanSubtract,
                BooleanOpKind::Intersect => NodeKind::BooleanIntersect,
            },
            Self::DirectFaceEdit { .. } => NodeKind::DirectFaceEdit,
            Self::ImportStep { .. } => NodeKind::ImportAsset,
            Self::ComponentInstance { .. } => NodeKind::AssemblyComponent,
            Self::MateConstraint { .. } => NodeKind::AssemblyMate,
            Self::TagTopology { .. } => NodeKind::TagFaces,
            Self::DesignAssertion { .. } => NodeKind::AssertGate,
        }
    }

    /// Return the static [`IrType`] produced by this feature.
    pub const fn output_type(&self) -> IrType {
        match self {
            Self::Sketch2D(_) => IrType::Sketch,
            Self::Loft { as_surface: true, .. } => IrType::Surface,
            Self::ComponentInstance { .. } => IrType::Component,
            Self::MateConstraint { .. } => IrType::Assembly,
            Self::DesignAssertion { .. } => IrType::Bool,
            _ => IrType::Solid,
        }
    }

    /// Collect all [`ParamId`]s directly bound inside this feature specification.
    pub fn referenced_params(&self) -> BTreeSet<ParamId> {
        let mut params = BTreeSet::new();
        match self {
            Self::PrimitiveBox {
                width,
                depth,
                height,
                ..
            } => {
                width.collect_param(&mut params);
                depth.collect_param(&mut params);
                height.collect_param(&mut params);
            }
            Self::PrimitiveCylinder { radius, height, .. } => {
                radius.collect_param(&mut params);
                height.collect_param(&mut params);
            }
            Self::Sketch2D(sketch) => {
                params.extend(sketch.referenced_params());
            }
            Self::Extrude { distance, .. } => distance.collect_param(&mut params),
            Self::Revolve { angle, .. } => angle.collect_param(&mut params),
            Self::Hole {
                diameter,
                depth,
                profile,
                ..
            } => {
                diameter.collect_param(&mut params);
                if let HoleDepthSpec::Blind { depth } = depth {
                    depth.collect_param(&mut params);
                }
                match profile {
                    HoleProfileSpec::Simple => {}
                    HoleProfileSpec::Counterbore {
                        bore_diameter,
                        bore_depth,
                    } => {
                        bore_diameter.collect_param(&mut params);
                        bore_depth.collect_param(&mut params);
                    }
                    HoleProfileSpec::Countersink {
                        sink_diameter,
                        sink_angle,
                    } => {
                        sink_diameter.collect_param(&mut params);
                        sink_angle.collect_param(&mut params);
                    }
                }
            }
            Self::StdFastenerHole {
                depth,
                thread_depth,
                ..
            } => {
                if let HoleDepthSpec::Blind { depth } = depth {
                    depth.collect_param(&mut params);
                }
                if let Some(td) = thread_depth {
                    td.collect_param(&mut params);
                }
            }
            Self::Fillet { radius, .. } => radius.collect_param(&mut params),
            Self::Chamfer {
                distance,
                distance2,
                angle,
                ..
            } => {
                distance.collect_param(&mut params);
                if let Some(d2) = distance2 {
                    d2.collect_param(&mut params);
                }
                if let Some(a) = angle {
                    a.collect_param(&mut params);
                }
            }
            Self::Shell { thickness, .. } => thickness.collect_param(&mut params),
            Self::Draft {
                neutral_plane,
                angle,
                ..
            } => {
                collect_plane_params(neutral_plane, &mut params);
                angle.collect_param(&mut params);
            }
            Self::LinearPattern {
                count,
                spacing,
                second_direction,
                ..
            } => {
                count.collect_param(&mut params);
                spacing.collect_param(&mut params);
                if let Some(axis2) = second_direction {
                    axis2.count.collect_param(&mut params);
                    axis2.spacing.collect_param(&mut params);
                }
            }
            Self::PolarPattern {
                count,
                total_angle,
                radius,
                start_angle,
                ..
            } => {
                count.collect_param(&mut params);
                total_angle.collect_param(&mut params);
                if let Some(r) = radius {
                    r.collect_param(&mut params);
                }
                if let Some(a) = start_angle {
                    a.collect_param(&mut params);
                }
            }
            Self::Mirror { plane, .. } => collect_plane_params(plane, &mut params),
            Self::DirectFaceEdit {
                edit: DirectFaceEditOp::Offset { distance },
                ..
            } => distance.collect_param(&mut params),
            Self::MateConstraint {
                mate_kind: MateKindSpec::Distance { value } | MateKindSpec::Angle { value },
                ..
            } => value.collect_param(&mut params),
            Self::DesignAssertion {
                kind: DesignAssertionKind::MinWallThickness { min },
                ..
            } => min.collect_param(&mut params),
            _ => {}
        }
        params
    }

    /// Validate local invariants of this feature specification (finite numbers,
    /// non-zero directions, matching dimensional kinds, and valid sketch structure).
    pub fn validate(&self, node_id: &NodeId) -> Result<(), IrError> {
        match self {
            Self::PrimitiveBox {
                width,
                depth,
                height,
                origin_mm,
            } => {
                width.validate_expected(node_id, IrType::Length, "width")?;
                depth.validate_expected(node_id, IrType::Length, "depth")?;
                height.validate_expected(node_id, IrType::Length, "height")?;
                check_finite_vec3(origin_mm)?;
            }
            Self::PrimitiveCylinder {
                radius,
                height,
                origin_mm,
                axis,
            } => {
                radius.validate_expected(node_id, IrType::Length, "radius")?;
                height.validate_expected(node_id, IrType::Length, "height")?;
                check_finite_vec3(origin_mm)?;
                check_nonzero_dir(node_id, axis, "axis")?;
            }
            Self::Sketch2D(sketch) => sketch.validate(node_id)?,
            Self::Extrude {
                distance,
                direction,
                ..
            } => {
                distance.validate_expected(node_id, IrType::Length, "distance")?;
                if let Some(dir) = direction {
                    check_nonzero_dir(node_id, dir, "direction")?;
                }
            }
            Self::Revolve {
                axis_origin_mm,
                axis_direction,
                angle,
                ..
            } => {
                check_finite_vec3(axis_origin_mm)?;
                check_nonzero_dir(node_id, axis_direction, "axis_direction")?;
                angle.validate_expected(node_id, IrType::Angle, "angle")?;
            }
            Self::Sweep { path, .. } => {
                if let SweepPathSpec::Polyline { points_mm } = path {
                    if points_mm.len() < 2 {
                        return Err(IrError::InvalidFeatureSpec {
                            node_id: node_id.clone(),
                            message: "sweep polyline path requires at least 2 points".to_owned(),
                        });
                    }
                    for pt in points_mm {
                        check_finite_vec3(pt)?;
                    }
                }
            }
            Self::Loft { sections, .. } => {
                if sections.len() < 2 {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "loft requires at least 2 section sketches".to_owned(),
                    });
                }
            }
            Self::Hole {
                origin_mm,
                axis,
                diameter,
                depth,
                profile,
                ..
            } => {
                check_finite_vec3(origin_mm)?;
                check_nonzero_dir(node_id, axis, "axis")?;
                diameter.validate_expected(node_id, IrType::Length, "diameter")?;
                if let HoleDepthSpec::Blind { depth } = depth {
                    depth.validate_expected(node_id, IrType::Length, "depth")?;
                }
                match profile {
                    HoleProfileSpec::Simple => {}
                    HoleProfileSpec::Counterbore {
                        bore_diameter,
                        bore_depth,
                    } => {
                        bore_diameter.validate_expected(node_id, IrType::Length, "bore_diameter")?;
                        bore_depth.validate_expected(node_id, IrType::Length, "bore_depth")?;
                    }
                    HoleProfileSpec::Countersink {
                        sink_diameter,
                        sink_angle,
                    } => {
                        sink_diameter.validate_expected(node_id, IrType::Length, "sink_diameter")?;
                        sink_angle.validate_expected(node_id, IrType::Angle, "sink_angle")?;
                    }
                }
            }
            Self::StdFastenerHole {
                origin_mm,
                axis,
                designation,
                depth,
                thread_depth,
                ..
            } => {
                check_finite_vec3(origin_mm)?;
                check_nonzero_dir(node_id, axis, "axis")?;
                if designation.trim().is_empty() {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "fastener designation cannot be empty".to_owned(),
                    });
                }
                if let HoleDepthSpec::Blind { depth } = depth {
                    depth.validate_expected(node_id, IrType::Length, "depth")?;
                }
                if let Some(td) = thread_depth {
                    td.validate_expected(node_id, IrType::Length, "thread_depth")?;
                }
            }
            Self::Fillet { radius, .. } => {
                radius.validate_expected(node_id, IrType::Length, "radius")?;
            }
            Self::Chamfer {
                distance,
                distance2,
                angle,
                ..
            } => {
                distance.validate_expected(node_id, IrType::Length, "distance")?;
                if let Some(d2) = distance2 {
                    d2.validate_expected(node_id, IrType::Length, "distance2")?;
                }
                if let Some(a) = angle {
                    a.validate_expected(node_id, IrType::Angle, "angle")?;
                }
            }
            Self::Shell { thickness, .. } => {
                thickness.validate_expected(node_id, IrType::Length, "thickness")?;
            }
            Self::Draft {
                pull_direction,
                angle,
                ..
            } => {
                check_nonzero_dir(node_id, pull_direction, "pull_direction")?;
                angle.validate_expected(node_id, IrType::Angle, "angle")?;
            }
            Self::LinearPattern {
                direction,
                spacing,
                second_direction,
                ..
            } => {
                check_nonzero_dir(node_id, direction, "direction")?;
                spacing.validate_expected(node_id, IrType::Length, "spacing")?;
                if let Some(axis2) = second_direction {
                    check_nonzero_dir(node_id, &axis2.direction, "second_direction.direction")?;
                    axis2
                        .spacing
                        .validate_expected(node_id, IrType::Length, "second_direction.spacing")?;
                }
            }
            Self::PolarPattern {
                axis_origin_mm,
                axis_direction,
                total_angle,
                radius,
                start_angle,
                ..
            } => {
                check_finite_vec3(axis_origin_mm)?;
                check_nonzero_dir(node_id, axis_direction, "axis_direction")?;
                total_angle.validate_expected(node_id, IrType::Angle, "total_angle")?;
                if let Some(r) = radius {
                    r.validate_expected(node_id, IrType::Length, "radius")?;
                }
                if let Some(a) = start_angle {
                    a.validate_expected(node_id, IrType::Angle, "start_angle")?;
                }
            }
            Self::Mirror { .. } => {}
            Self::Boolean { tools, .. } => {
                if tools.is_empty() {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "boolean feature requires at least one tool solid".to_owned(),
                    });
                }
            }
            Self::DirectFaceEdit { face, edit, .. } => {
                if face.trim().is_empty() {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "direct_face_edit face reference cannot be empty".to_owned(),
                    });
                }
                match edit {
                    DirectFaceEditOp::Offset { distance } => {
                        distance.validate_expected(node_id, IrType::Length, "distance")?;
                    }
                    DirectFaceEditOp::Move { delta_mm } => check_finite_vec3(delta_mm)?,
                    DirectFaceEditOp::DeleteAndHeal => {}
                }
            }
            Self::ImportStep {
                blob, path, scale, ..
            } => {
                if blob.is_none() && path.as_deref().is_none_or(|p| p.trim().is_empty()) {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "import_step requires either `blob` or non-empty `path`".to_owned(),
                    });
                }
                if !scale.is_finite() {
                    return Err(IrError::NonFiniteNumber);
                }
                if *scale <= 0.0 {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "import_step scale must be positive".to_owned(),
                    });
                }
            }
            Self::ComponentInstance {
                translation_mm,
                rotation_deg,
                ..
            } => {
                check_finite_vec3(translation_mm)?;
                check_finite_vec3(rotation_deg)?;
            }
            Self::MateConstraint { mate_kind, .. } => match mate_kind {
                MateKindSpec::Distance { value } => {
                    value.validate_expected(node_id, IrType::Length, "mate distance")?;
                }
                MateKindSpec::Angle { value } => {
                    value.validate_expected(node_id, IrType::Angle, "mate angle")?;
                }
                MateKindSpec::Gear { ratio } => {
                    if !ratio.is_finite() {
                        return Err(IrError::NonFiniteNumber);
                    }
                    if *ratio == 0.0 {
                        return Err(IrError::InvalidFeatureSpec {
                            node_id: node_id.clone(),
                            message: "gear mate ratio cannot be zero".to_owned(),
                        });
                    }
                }
                _ => {}
            },
            Self::TagTopology { tag, .. } => {
                if tag.trim().is_empty() {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "tag_topology tag cannot be empty".to_owned(),
                    });
                }
            }
            Self::DesignAssertion {
                assertion_id, kind, ..
            } => {
                if assertion_id.trim().is_empty() {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "design_assertion assertion_id cannot be empty".to_owned(),
                    });
                }
                match kind {
                    DesignAssertionKind::VolumeBounds { min_mm3, max_mm3 } => {
                        if let Some(min) = min_mm3
                            && !min.is_finite()
                        {
                            return Err(IrError::NonFiniteNumber);
                        }
                        if let Some(max) = max_mm3
                            && !max.is_finite()
                        {
                            return Err(IrError::NonFiniteNumber);
                        }
                    }
                    DesignAssertionKind::MinWallThickness { min } => {
                        min.validate_expected(node_id, IrType::Length, "min_wall_thickness")?;
                    }
                    DesignAssertionKind::BooleanCondition { .. }
                    | DesignAssertionKind::Custom { .. } => {}
                }
            }
        }
        Ok(())
    }

    fn populate_canonical_inputs(&self, inputs: &mut BTreeMap<String, IrValue>) {
        match self {
            Self::PrimitiveBox {
                width,
                depth,
                height,
                origin_mm,
            } => {
                insert_dim(inputs, "width", width);
                insert_dim(inputs, "depth", depth);
                insert_dim(inputs, "height", height);
                inputs.insert("origin".to_owned(), IrValue::Vec3(*origin_mm));
            }
            Self::PrimitiveCylinder {
                radius,
                height,
                origin_mm,
                axis,
            } => {
                insert_dim(inputs, "radius", radius);
                insert_dim(inputs, "height", height);
                inputs.insert("origin".to_owned(), IrValue::Vec3(*origin_mm));
                inputs.insert("axis".to_owned(), IrValue::Vec3(*axis));
            }
            Self::Sketch2D(sketch) => {
                let blobs: Vec<IrValue> = sketch
                    .referenced_blobs()
                    .into_iter()
                    .map(IrValue::BlobRef)
                    .collect();
                if !blobs.is_empty() {
                    inputs.insert("blobs".to_owned(), IrValue::List(blobs));
                }
            }
            Self::Extrude {
                sketch,
                distance,
                direction,
                target,
                ..
            } => {
                inputs.insert("sketch".to_owned(), node_ref(sketch));
                insert_dim(inputs, "distance", distance);
                if let Some(dir) = direction {
                    inputs.insert("direction".to_owned(), IrValue::Vec3(*dir));
                }
                if let Some(t) = target {
                    inputs.insert("target".to_owned(), node_ref(t));
                }
            }
            Self::Revolve {
                sketch,
                axis_origin_mm,
                axis_direction,
                angle,
                target,
                ..
            } => {
                inputs.insert("sketch".to_owned(), node_ref(sketch));
                inputs.insert("origin".to_owned(), IrValue::Vec3(*axis_origin_mm));
                inputs.insert("axis".to_owned(), IrValue::Vec3(*axis_direction));
                insert_dim(inputs, "angle", angle);
                if let Some(t) = target {
                    inputs.insert("target".to_owned(), node_ref(t));
                }
            }
            Self::Sweep { profile, path, .. } => {
                inputs.insert("profile".to_owned(), node_ref(profile));
                match path {
                    SweepPathSpec::Sketch { node_id } => {
                        inputs.insert("path".to_owned(), node_ref(node_id));
                    }
                    SweepPathSpec::Polyline { points_mm } => {
                        inputs.insert(
                            "path".to_owned(),
                            IrValue::List(points_mm.iter().copied().map(IrValue::Vec3).collect()),
                        );
                    }
                }
            }
            Self::Loft {
                sections, guides, ..
            } => {
                inputs.insert(
                    "sections".to_owned(),
                    IrValue::List(sections.iter().map(node_ref).collect()),
                );
                if !guides.is_empty() {
                    inputs.insert(
                        "guides".to_owned(),
                        IrValue::List(guides.iter().map(node_ref).collect()),
                    );
                }
            }
            Self::Hole {
                solid,
                origin_mm,
                axis,
                diameter,
                depth,
                profile,
                ..
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert("origin".to_owned(), IrValue::Vec3(*origin_mm));
                inputs.insert("axis".to_owned(), IrValue::Vec3(*axis));
                insert_dim(inputs, "diameter", diameter);
                if let HoleDepthSpec::Blind { depth } = depth {
                    insert_dim(inputs, "depth", depth);
                }
                match profile {
                    HoleProfileSpec::Simple => {}
                    HoleProfileSpec::Counterbore {
                        bore_diameter,
                        bore_depth,
                    } => {
                        insert_dim(inputs, "bore_diameter", bore_diameter);
                        insert_dim(inputs, "bore_depth", bore_depth);
                    }
                    HoleProfileSpec::Countersink {
                        sink_diameter,
                        sink_angle,
                    } => {
                        insert_dim(inputs, "sink_diameter", sink_diameter);
                        insert_dim(inputs, "sink_angle", sink_angle);
                    }
                }
            }
            Self::StdFastenerHole {
                solid,
                origin_mm,
                axis,
                depth,
                thread_depth,
                ..
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert("origin".to_owned(), IrValue::Vec3(*origin_mm));
                inputs.insert("axis".to_owned(), IrValue::Vec3(*axis));
                if let HoleDepthSpec::Blind { depth } = depth {
                    insert_dim(inputs, "depth", depth);
                }
                if let Some(td) = thread_depth {
                    insert_dim(inputs, "thread_depth", td);
                }
            }
            Self::Fillet {
                solid,
                edges,
                radius,
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert(
                    "edges".to_owned(),
                    IrValue::List(edges.iter().cloned().map(IrValue::TopoRefExpr).collect()),
                );
                insert_dim(inputs, "radius", radius);
            }
            Self::Chamfer {
                solid,
                edges,
                distance,
                distance2,
                angle,
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert(
                    "edges".to_owned(),
                    IrValue::List(edges.iter().cloned().map(IrValue::TopoRefExpr).collect()),
                );
                insert_dim(inputs, "distance", distance);
                if let Some(d2) = distance2 {
                    insert_dim(inputs, "distance2", d2);
                }
                if let Some(a) = angle {
                    insert_dim(inputs, "angle", a);
                }
            }
            Self::Shell {
                solid,
                open_faces,
                thickness,
                ..
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert(
                    "open_faces".to_owned(),
                    IrValue::List(
                        open_faces
                            .iter()
                            .cloned()
                            .map(IrValue::TopoRefExpr)
                            .collect(),
                    ),
                );
                insert_dim(inputs, "thickness", thickness);
            }
            Self::Draft {
                solid,
                faces,
                pull_direction,
                angle,
                ..
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert(
                    "faces".to_owned(),
                    IrValue::List(faces.iter().cloned().map(IrValue::TopoRefExpr).collect()),
                );
                inputs.insert("pull_direction".to_owned(), IrValue::Vec3(*pull_direction));
                insert_dim(inputs, "angle", angle);
            }
            Self::LinearPattern {
                solid,
                direction,
                count,
                spacing,
                second_direction,
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert("direction".to_owned(), IrValue::Vec3(*direction));
                insert_int(inputs, "count", count);
                insert_dim(inputs, "spacing", spacing);
                if let Some(axis2) = second_direction {
                    inputs.insert("second_direction".to_owned(), IrValue::Vec3(axis2.direction));
                    insert_int(inputs, "second_count", &axis2.count);
                    insert_dim(inputs, "second_spacing", &axis2.spacing);
                }
            }
            Self::PolarPattern {
                solid,
                axis_origin_mm,
                axis_direction,
                count,
                total_angle,
                radius,
                start_angle,
            } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert("axis_origin".to_owned(), IrValue::Vec3(*axis_origin_mm));
                inputs.insert("axis".to_owned(), IrValue::Vec3(*axis_direction));
                insert_int(inputs, "count", count);
                insert_dim(inputs, "angle", total_angle);
                if let Some(r) = radius {
                    insert_dim(inputs, "radius", r);
                }
                if let Some(a) = start_angle {
                    insert_dim(inputs, "start_angle", a);
                }
            }
            Self::Mirror { solid, .. } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
            }
            Self::Boolean { target, tools, .. } => {
                inputs.insert("target".to_owned(), node_ref(target));
                inputs.insert(
                    "tools".to_owned(),
                    IrValue::List(tools.iter().map(node_ref).collect()),
                );
            }
            Self::DirectFaceEdit { solid, face, edit } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert("face".to_owned(), IrValue::TopoRefExpr(face.clone()));
                if let DirectFaceEditOp::Offset { distance } = edit {
                    insert_dim(inputs, "offset", distance);
                }
            }
            Self::ImportStep { blob, .. } => {
                if let Some(handle) = blob {
                    inputs.insert("blob".to_owned(), IrValue::BlobRef(handle.clone()));
                }
            }
            Self::ComponentInstance { source, .. } => match source {
                ComponentSourceSpec::LocalNode { node_id } => {
                    inputs.insert("geometry".to_owned(), node_ref(node_id));
                }
                ComponentSourceSpec::ExternalRef { ref_id } => {
                    inputs.insert("external_ref".to_owned(), IrValue::ExternalRef(ref_id.clone()));
                }
            },
            Self::MateConstraint {
                left,
                right,
                mate_kind,
                ..
            } => {
                inputs.insert("left".to_owned(), node_ref(left));
                inputs.insert("right".to_owned(), node_ref(right));
                match mate_kind {
                    MateKindSpec::Distance { value } => insert_dim(inputs, "distance", value),
                    MateKindSpec::Angle { value } => insert_dim(inputs, "angle", value),
                    _ => {}
                }
            }
            Self::TagTopology { solid, targets, .. } => {
                inputs.insert("solid".to_owned(), node_ref(solid));
                inputs.insert(
                    "faces".to_owned(),
                    IrValue::List(targets.iter().cloned().map(IrValue::TopoRefExpr).collect()),
                );
            }
            Self::DesignAssertion { target, kind, .. } => {
                if let Some(t) = target {
                    inputs.insert("target".to_owned(), node_ref(t));
                }
                match kind {
                    DesignAssertionKind::BooleanCondition { condition } => {
                        inputs.insert("condition".to_owned(), node_ref(condition));
                    }
                    DesignAssertionKind::MinWallThickness { min } => {
                        insert_dim(inputs, "min_wall_thickness", min);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn insert_dim(inputs: &mut BTreeMap<String, IrValue>, key: &str, dim: &DimInput) {
    if let Some(val) = dim.to_port_value() {
        inputs.insert(key.to_owned(), val);
    }
}

fn insert_int(inputs: &mut BTreeMap<String, IrValue>, key: &str, count: &IntInput) {
    if let Some(val) = count.to_port_value() {
        inputs.insert(key.to_owned(), val);
    }
}

fn collect_plane_params(plane: &SketchPlaneSpec, out: &mut BTreeSet<ParamId>) {
    match plane {
        SketchPlaneSpec::Datum {
            offset: Some(dim), ..
        }
        | SketchPlaneSpec::FaceRef {
            offset: Some(dim), ..
        } => {
            if let Some(id) = dim.referenced_param() {
                out.insert(id.clone());
            }
        }
        _ => {}
    }
}

impl IrNode {
    /// Construct a validated [`IrNode`] deterministically from a strongly-typed
    /// [`FeatureSpec`], populating both canonical typed ports and the lossless
    /// `feature_spec` payload.
    pub fn from_feature_spec(
        id: impl Into<NodeId>,
        name: impl Into<String>,
        spec: FeatureSpec,
    ) -> Result<Self, IrError> {
        let id = id.into();
        spec.validate(&id)?;
        let kind = spec.node_kind();
        let output_type = spec.output_type();
        let mut inputs = BTreeMap::new();
        spec.populate_canonical_inputs(&mut inputs);
        let serialized_spec = serde_json::to_string(&spec)?;
        inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(serialized_spec));
        Ok(Self {
            id,
            name: name.into(),
            kind,
            inputs,
            output_type,
            state: NodeState::Valid,
            kernel_op_version: None,
            metadata: BTreeMap::new(),
        })
    }

    /// Decode a strongly-typed [`FeatureSpec`] from this [`IrNode`].
    ///
    /// When the node carries an explicit `feature_spec` payload, decodes it
    /// losslessly and verifies consistency with `self.kind` and `self.output_type`.
    /// Otherwise, attempts deterministic decoding from legacy canonical ports.
    pub fn try_feature_spec(&self) -> Result<FeatureSpec, IrError> {
        if let Some(raw) = self.inputs.get(FEATURE_SPEC_PORT) {
            let IrValue::String(spec_json) = raw else {
                return Err(IrError::InvalidFeatureSpec {
                    node_id: self.id.clone(),
                    message: format!("input `{FEATURE_SPEC_PORT}` must be a JSON string"),
                });
            };
            let spec: FeatureSpec = serde_json::from_str(spec_json).map_err(|err| {
                IrError::InvalidFeatureSpec {
                    node_id: self.id.clone(),
                    message: format!("failed to deserialize `{FEATURE_SPEC_PORT}`: {err}"),
                }
            })?;
            if spec.node_kind() != self.kind {
                return Err(IrError::InvalidFeatureSpec {
                    node_id: self.id.clone(),
                    message: format!(
                        "node kind {:?} does not match feature_spec kind {:?}",
                        self.kind,
                        spec.node_kind()
                    ),
                });
            }
            if spec.output_type() != self.output_type {
                return Err(IrError::InvalidFeatureSpec {
                    node_id: self.id.clone(),
                    message: format!(
                        "node output_type {:?} does not match feature_spec output_type {:?}",
                        self.output_type,
                        spec.output_type()
                    ),
                });
            }
            spec.validate(&self.id)?;
            return Ok(spec);
        }

        decode_legacy_ports(self)
    }
}

fn decode_legacy_ports(node: &IrNode) -> Result<FeatureSpec, IrError> {
    let spec = match &node.kind {
        NodeKind::PrimitiveBox => FeatureSpec::PrimitiveBox {
            width: required_dim(node, &["width", "length"])?,
            depth: required_dim(node, &["depth"])?,
            height: required_dim(node, &["height"])?,
            origin_mm: optional_vec3(node, "origin")?.unwrap_or([0.0, 0.0, 0.0]),
        },
        NodeKind::PrimitiveCylinder => FeatureSpec::PrimitiveCylinder {
            radius: required_dim(node, &["radius"])?,
            height: required_dim(node, &["height"])?,
            origin_mm: optional_vec3(node, "origin")?.unwrap_or([0.0, 0.0, 0.0]),
            axis: optional_vec3(node, "axis")?.unwrap_or([0.0, 0.0, 1.0]),
        },
        NodeKind::Extrude => FeatureSpec::Extrude {
            sketch: required_node_ref(node, &["sketch", "profile"])?,
            distance: required_dim(node, &["distance", "height", "depth", "thickness"])?,
            direction: optional_vec3(node, "direction")?,
            symmetric: optional_bool(node, "symmetric").unwrap_or(false),
            operation: FeatureBodyOp::NewBody,
            target: optional_node_ref(node, &["target", "solid", "base"]),
        },
        NodeKind::Revolve => FeatureSpec::Revolve {
            sketch: required_node_ref(node, &["sketch", "profile"])?,
            axis_origin_mm: optional_vec3(node, "origin")?.unwrap_or([0.0, 0.0, 0.0]),
            axis_direction: optional_vec3(node, "axis")?.unwrap_or([0.0, 0.0, 1.0]),
            angle: required_dim(node, &["angle"])?,
            operation: FeatureBodyOp::NewBody,
            target: optional_node_ref(node, &["target", "solid", "base"]),
        },
        NodeKind::Fillet => FeatureSpec::Fillet {
            solid: required_node_ref(node, &["solid", "base", "target", "body"])?,
            edges: optional_topo_list(node, "edges")?,
            radius: required_dim(node, &["radius", "r"])?,
        },
        NodeKind::Chamfer => FeatureSpec::Chamfer {
            solid: required_node_ref(node, &["solid", "base", "target", "body"])?,
            edges: optional_topo_list(node, "edges")?,
            distance: required_dim(node, &["distance", "d"])?,
            distance2: optional_dim(node, &["distance2"]),
            angle: optional_dim(node, &["angle"]),
        },
        NodeKind::Shell => FeatureSpec::Shell {
            solid: required_node_ref(node, &["solid", "base", "target", "body"])?,
            open_faces: optional_topo_list(node, "open_faces")?,
            thickness: required_dim(node, &["thickness"])?,
            inward: optional_bool(node, "inward").unwrap_or(true),
        },
        NodeKind::Hole => FeatureSpec::Hole {
            solid: required_node_ref(node, &["solid", "base", "target", "body"])?,
            target_face: None,
            origin_mm: optional_vec3(node, "origin")?.unwrap_or([0.0, 0.0, 0.0]),
            axis: optional_vec3(node, "axis")?.unwrap_or([0.0, 0.0, -1.0]),
            diameter: required_dim(node, &["diameter"])?,
            depth: match optional_dim(node, &["depth"]) {
                Some(depth) => HoleDepthSpec::Blind { depth },
                None => HoleDepthSpec::ThroughAll,
            },
            profile: HoleProfileSpec::Simple,
        },
        NodeKind::PatternLinear => FeatureSpec::LinearPattern {
            solid: required_node_ref(node, &["solid", "base", "target", "body"])?,
            direction: optional_vec3(node, "direction")?.unwrap_or([1.0, 0.0, 0.0]),
            count: required_int(node, &["count"])?,
            spacing: required_dim(node, &["spacing", "step"])?,
            second_direction: None,
        },
        NodeKind::PatternPolar => FeatureSpec::PolarPattern {
            solid: required_node_ref(node, &["solid", "base", "target", "body"])?,
            axis_origin_mm: optional_vec3(node, "axis_origin")?.unwrap_or([0.0, 0.0, 0.0]),
            axis_direction: optional_vec3(node, "axis")?.unwrap_or([0.0, 0.0, 1.0]),
            count: required_int(node, &["count"])?,
            total_angle: required_dim(node, &["angle"])?,
            radius: optional_dim(node, &["radius"]),
            start_angle: optional_dim(node, &["start_angle"]),
        },
        NodeKind::BooleanUnion | NodeKind::BooleanSubtract | NodeKind::BooleanIntersect => {
            let op = match node.kind {
                NodeKind::BooleanUnion => BooleanOpKind::Union,
                NodeKind::BooleanSubtract => BooleanOpKind::Subtract,
                NodeKind::BooleanIntersect => BooleanOpKind::Intersect,
                _ => unreachable!(),
            };
            let target = required_node_ref(node, &["target", "base", "left", "a"])?;
            let tools = if let Some(IrValue::List(items)) =
                node.inputs.get("tools").or_else(|| node.inputs.get("solids"))
            {
                let mut ids = Vec::with_capacity(items.len());
                for item in items {
                    if let IrValue::NodeRef { node_id, .. } = item {
                        ids.push(node_id.clone());
                    }
                }
                ids
            } else {
                vec![required_node_ref(node, &["tool", "right", "b"])?]
            };
            FeatureSpec::Boolean { op, target, tools }
        }
        NodeKind::AssemblyComponent => {
            let source = if let Some(IrValue::ExternalRef(ref_id)) = node.inputs.get("external_ref") {
                ComponentSourceSpec::ExternalRef {
                    ref_id: ref_id.clone(),
                }
            } else {
                ComponentSourceSpec::LocalNode {
                    node_id: required_node_ref(node, &["geometry"])?,
                }
            };
            FeatureSpec::ComponentInstance {
                source,
                translation_mm: optional_vec3(node, "translation")?.unwrap_or([0.0, 0.0, 0.0]),
                rotation_deg: optional_vec3(node, "rotation")?.unwrap_or([0.0, 0.0, 0.0]),
                grounded: optional_bool(node, "grounded").unwrap_or(false),
            }
        }
        NodeKind::TagFaces => FeatureSpec::TagTopology {
            solid: required_node_ref(node, &["solid", "surface", "geometry"])?,
            tag: match node.inputs.get("tag") {
                Some(IrValue::String(s)) => s.clone(),
                _ => node.name.clone(),
            },
            targets: optional_topo_list(node, "faces")?,
        },
        _ => {
            return Err(IrError::InvalidFeatureSpec {
                node_id: node.id.clone(),
                message: format!(
                    "node kind {:?} does not carry `{FEATURE_SPEC_PORT}` and cannot be decoded from legacy ports",
                    node.kind
                ),
            });
        }
    };
    spec.validate(&node.id)?;
    Ok(spec)
}

fn required_dim(node: &IrNode, names: &[&str]) -> Result<DimInput, IrError> {
    optional_dim(node, names).ok_or_else(|| IrError::InvalidFeatureSpec {
        node_id: node.id.clone(),
        message: format!("missing dimensional input `{}`", names[0]),
    })
}

fn optional_dim(node: &IrNode, names: &[&str]) -> Option<DimInput> {
    for name in names {
        if let Some(val) = node.inputs.get(*name)
            && let Some(dim) = DimInput::from_port_value(val)
        {
            return Some(dim);
        }
    }
    None
}

fn required_int(node: &IrNode, names: &[&str]) -> Result<IntInput, IrError> {
    for name in names {
        if let Some(val) = node.inputs.get(*name)
            && let Some(int_in) = IntInput::from_port_value(val)
        {
            return Ok(int_in);
        }
    }
    Err(IrError::InvalidFeatureSpec {
        node_id: node.id.clone(),
        message: format!("missing integer input `{}`", names[0]),
    })
}

fn required_node_ref(node: &IrNode, names: &[&str]) -> Result<NodeId, IrError> {
    optional_node_ref(node, names).ok_or_else(|| IrError::InvalidFeatureSpec {
        node_id: node.id.clone(),
        message: format!("missing node reference input `{}`", names[0]),
    })
}

fn optional_node_ref(node: &IrNode, names: &[&str]) -> Option<NodeId> {
    for name in names {
        if let Some(IrValue::NodeRef { node_id, .. }) = node.inputs.get(*name) {
            return Some(node_id.clone());
        }
    }
    None
}

fn optional_vec3(node: &IrNode, name: &str) -> Result<Option<[f64; 3]>, IrError> {
    match node.inputs.get(name) {
        None => Ok(None),
        Some(IrValue::Vec3(v)) => {
            check_finite_vec3(v)?;
            Ok(Some(*v))
        }
        Some(_) => Err(IrError::InvalidFeatureSpec {
            node_id: node.id.clone(),
            message: format!("input `{name}` must be a Vec3"),
        }),
    }
}

fn optional_bool(node: &IrNode, name: &str) -> Option<bool> {
    match node.inputs.get(name) {
        Some(IrValue::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn optional_topo_list(node: &IrNode, name: &str) -> Result<Vec<TopoRefExpr>, IrError> {
    let Some(val) = node.inputs.get(name) else {
        return Ok(Vec::new());
    };
    let IrValue::List(items) = val else {
        return Err(IrError::InvalidFeatureSpec {
            node_id: node.id.clone(),
            message: format!("input `{name}` must be a list of TopoRefExpr"),
        });
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let IrValue::TopoRefExpr(expr) = item else {
            return Err(IrError::InvalidFeatureSpec {
                node_id: node.id.clone(),
                message: format!("input `{name}` items must be TopoRefExpr"),
            });
        };
        out.push(expr.clone());
    }
    Ok(out)
}

//! First-class strongly-typed 2D parametric sketch model (`StructuredSketch2D`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{BlobHandle, IrError, IrType, NodeId, ParamId, Quantity, TopoRefExpr, wire};

/// Stable entity identifier inside a [`StructuredSketch2D`].
pub type SketchEntityId = String;

/// Standard orthogonal datum planes for sketching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatumPlane {
    Xy,
    Xz,
    Yz,
}

/// A dimensional value inside a sketch (plane offset or dimensional constraint)
/// expressed either as an inline physical [`Quantity`] or as a [`ParamId`] binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "binding", rename_all = "snake_case", deny_unknown_fields)]
pub enum SketchDimValue {
    Literal(Quantity),
    Param { param_id: ParamId },
}

impl SketchDimValue {
    pub fn mm(value: f64) -> Result<Self, IrError> {
        Ok(Self::Literal(Quantity::length(value, crate::LengthUnit::Mm)?))
    }

    pub fn deg(value: f64) -> Result<Self, IrError> {
        Ok(Self::Literal(Quantity::angle(value, crate::AngleUnit::Deg)?))
    }

    pub const fn quantity(quantity: Quantity) -> Self {
        Self::Literal(quantity)
    }

    pub fn param(param_id: impl Into<ParamId>) -> Self {
        Self::Param {
            param_id: param_id.into(),
        }
    }

    pub fn referenced_param(&self) -> Option<&ParamId> {
        match self {
            Self::Literal(_) => None,
            Self::Param { param_id } => Some(param_id),
        }
    }

    fn validate_expected_type(
        &self,
        node_id: &NodeId,
        expected: IrType,
        context: &str,
    ) -> Result<(), IrError> {
        match self {
            Self::Literal(q) => {
                if !q.canonical_value().is_finite() {
                    return Err(IrError::NonFiniteNumber);
                }
                if q.ir_type() != expected {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: format!(
                            "{context} expects {:?}, got {:?}",
                            expected,
                            q.ir_type()
                        ),
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
        }
    }
}

/// Sketch plane specification: a datum plane with optional normal offset,
/// a persistent topological face reference, or a custom 3D frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SketchPlaneSpec {
    Datum {
        plane: DatumPlane,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<SketchDimValue>,
    },
    FaceRef {
        face: TopoRefExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<SketchDimValue>,
    },
    Custom {
        #[serde(serialize_with = "wire::vec3")]
        origin_mm: [f64; 3],
        #[serde(serialize_with = "wire::vec3")]
        normal: [f64; 3],
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_opt_vec3"
        )]
        x_axis: Option<[f64; 3]>,
    },
}

impl SketchPlaneSpec {
    pub const fn datum(plane: DatumPlane) -> Self {
        Self::Datum {
            plane,
            offset: None,
        }
    }

    pub const fn datum_with_offset(plane: DatumPlane, offset: SketchDimValue) -> Self {
        Self::Datum {
            plane,
            offset: Some(offset),
        }
    }

    pub fn face_ref(face: impl Into<TopoRefExpr>) -> Self {
        Self::FaceRef {
            face: face.into(),
            offset: None,
        }
    }
}

/// Ordered 2D sketch geometry entities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SketchEntity2D {
    Point {
        id: SketchEntityId,
        #[serde(serialize_with = "serialize_vec2")]
        position_mm: [f64; 2],
        #[serde(default)]
        fixed: bool,
        #[serde(default)]
        construction: bool,
    },
    Line {
        id: SketchEntityId,
        #[serde(serialize_with = "serialize_vec2")]
        start_mm: [f64; 2],
        #[serde(serialize_with = "serialize_vec2")]
        end_mm: [f64; 2],
        #[serde(default)]
        construction: bool,
    },
    Arc {
        id: SketchEntityId,
        #[serde(serialize_with = "serialize_vec2")]
        center_mm: [f64; 2],
        #[serde(serialize_with = "serialize_vec2")]
        start_mm: [f64; 2],
        #[serde(serialize_with = "serialize_vec2")]
        end_mm: [f64; 2],
        #[serde(default)]
        ccw: bool,
        #[serde(default)]
        construction: bool,
    },
    Circle {
        id: SketchEntityId,
        #[serde(serialize_with = "serialize_vec2")]
        center_mm: [f64; 2],
        #[serde(serialize_with = "wire::number")]
        radius_mm: f64,
        #[serde(default)]
        construction: bool,
    },
    Rectangle {
        id: SketchEntityId,
        #[serde(serialize_with = "serialize_vec2")]
        origin_mm: [f64; 2],
        #[serde(serialize_with = "wire::number")]
        width_mm: f64,
        #[serde(serialize_with = "wire::number")]
        height_mm: f64,
        #[serde(default)]
        centered: bool,
        #[serde(default)]
        construction: bool,
    },
    SplineBlob {
        id: SketchEntityId,
        blob: BlobHandle,
        #[serde(default)]
        closed: bool,
        #[serde(default)]
        construction: bool,
    },
    Ellipse {
        id: SketchEntityId,
        #[serde(serialize_with = "serialize_vec2")]
        center_mm: [f64; 2],
        #[serde(serialize_with = "wire::number")]
        rx_mm: f64,
        #[serde(serialize_with = "wire::number")]
        ry_mm: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<u32>,
        #[serde(default)]
        construction: bool,
    },
    Spline {
        id: SketchEntityId,
        #[serde(serialize_with = "serialize_vec2_list")]
        control_points_mm: Vec<[f64; 2]>,
        #[serde(default)]
        closed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments_per_span: Option<u32>,
        #[serde(default)]
        construction: bool,
    },
}

impl SketchEntity2D {
    pub fn id(&self) -> &str {
        match self {
            Self::Point { id, .. }
            | Self::Line { id, .. }
            | Self::Arc { id, .. }
            | Self::Circle { id, .. }
            | Self::Rectangle { id, .. }
            | Self::SplineBlob { id, .. }
            | Self::Ellipse { id, .. }
            | Self::Spline { id, .. } => id,
        }
    }
}

/// A closed 2D profile loop formed by an ordered sequence of sketch entity IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SketchProfileLoop {
    pub id: String,
    pub entity_ids: Vec<SketchEntityId>,
    #[serde(default)]
    pub is_outer: bool,
}

/// Specific anchor point on a 2D sketch entity referenced by a constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SketchAnchor {
    #[default]
    Point,
    Start,
    End,
    Center,
    Midpoint,
}

/// Reference to a point or entity anchor inside a [`StructuredSketch2D`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SketchPointRef {
    pub entity_id: SketchEntityId,
    #[serde(default)]
    pub anchor: SketchAnchor,
}

impl SketchPointRef {
    pub fn point(entity_id: impl Into<SketchEntityId>) -> Self {
        Self {
            entity_id: entity_id.into(),
            anchor: SketchAnchor::Point,
        }
    }

    pub fn anchor(entity_id: impl Into<SketchEntityId>, anchor: SketchAnchor) -> Self {
        Self {
            entity_id: entity_id.into(),
            anchor,
        }
    }
}

/// Strongly-typed geometric and dimensional 2D sketch constraints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SketchConstraint2D {
    Coincident {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchPointRef,
        b: SketchPointRef,
    },
    PointOnLine {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        point: SketchPointRef,
        line: SketchEntityId,
    },
    Horizontal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        entity: SketchEntityId,
    },
    Vertical {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        entity: SketchEntityId,
    },
    Parallel {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchEntityId,
        b: SketchEntityId,
    },
    Perpendicular {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchEntityId,
        b: SketchEntityId,
    },
    Tangent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchEntityId,
        b: SketchEntityId,
    },
    Concentric {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchEntityId,
        b: SketchEntityId,
    },
    Equal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchEntityId,
        b: SketchEntityId,
    },
    Symmetric {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchPointRef,
        b: SketchPointRef,
        axis_line: SketchEntityId,
    },
    Distance {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchPointRef,
        b: SketchPointRef,
        value: SketchDimValue,
    },
    Radius {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        entity: SketchEntityId,
        value: SketchDimValue,
    },
    Diameter {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        entity: SketchEntityId,
        value: SketchDimValue,
    },
    Angle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        a: SketchEntityId,
        b: SketchEntityId,
        value: SketchDimValue,
    },
}

/// First-class 2D parametric sketch document payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredSketch2D {
    pub plane: SketchPlaneSpec,
    #[serde(default)]
    pub entities: Vec<SketchEntity2D>,
    #[serde(default)]
    pub profiles: Vec<SketchProfileLoop>,
    #[serde(default)]
    pub constraints: Vec<SketchConstraint2D>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_dof: Option<u32>,
}

impl StructuredSketch2D {
    /// Create an empty sketch on `plane`.
    pub const fn new(plane: SketchPlaneSpec) -> Self {
        Self {
            plane,
            entities: Vec::new(),
            profiles: Vec::new(),
            constraints: Vec::new(),
            remaining_dof: None,
        }
    }

    /// Collect all [`ParamId`]s bound in the plane offset and dimensional constraints.
    pub fn referenced_params(&self) -> BTreeSet<ParamId> {
        let mut params = BTreeSet::new();
        match &self.plane {
            SketchPlaneSpec::Datum {
                offset: Some(dim), ..
            }
            | SketchPlaneSpec::FaceRef {
                offset: Some(dim), ..
            } => {
                if let Some(id) = dim.referenced_param() {
                    params.insert(id.clone());
                }
            }
            _ => {}
        }
        for constraint in &self.constraints {
            let dim = match constraint {
                SketchConstraint2D::Distance { value, .. }
                | SketchConstraint2D::Radius { value, .. }
                | SketchConstraint2D::Diameter { value, .. }
                | SketchConstraint2D::Angle { value, .. } => Some(value),
                _ => None,
            };
            if let Some(id) = dim.and_then(SketchDimValue::referenced_param) {
                params.insert(id.clone());
            }
        }
        params
    }

    /// Collect all [`BlobHandle`]s referenced by `SplineBlob` entities.
    pub fn referenced_blobs(&self) -> BTreeSet<BlobHandle> {
        let mut blobs = BTreeSet::new();
        for entity in &self.entities {
            if let SketchEntity2D::SplineBlob { blob, .. } = entity {
                blobs.insert(blob.clone());
            }
        }
        blobs
    }

    /// Validate structural integrity, finite coordinates, unique IDs, and
    /// constraint/profile references inside this sketch.
    pub fn validate(&self, node_id: &NodeId) -> Result<(), IrError> {
        match &self.plane {
            SketchPlaneSpec::Datum { offset, .. } => {
                if let Some(dim) = offset {
                    dim.validate_expected_type(node_id, IrType::Length, "sketch plane offset")?;
                }
            }
            SketchPlaneSpec::FaceRef { face, offset } => {
                if face.trim().is_empty() {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "sketch face_ref cannot be empty".to_owned(),
                    });
                }
                if let Some(dim) = offset {
                    dim.validate_expected_type(node_id, IrType::Length, "sketch plane offset")?;
                }
            }
            SketchPlaneSpec::Custom {
                origin_mm,
                normal,
                x_axis,
            } => {
                check_finite_vec3(origin_mm)?;
                check_finite_vec3(normal)?;
                if normal.iter().all(|v| *v == 0.0) {
                    return Err(IrError::InvalidFeatureSpec {
                        node_id: node_id.clone(),
                        message: "sketch custom plane normal cannot be zero".to_owned(),
                    });
                }
                if let Some(x) = x_axis {
                    check_finite_vec3(x)?;
                }
            }
        }

        let mut entity_ids = BTreeSet::new();
        for entity in &self.entities {
            let id = entity.id();
            if id.trim().is_empty() {
                return Err(IrError::EmptyId {
                    entity: "sketch_entity",
                });
            }
            if !entity_ids.insert(id) {
                return Err(IrError::DuplicateId {
                    entity: "sketch_entity",
                    id: id.to_owned(),
                });
            }
            match entity {
                SketchEntity2D::Point { position_mm, .. } => check_finite_vec2(position_mm)?,
                SketchEntity2D::Line {
                    start_mm, end_mm, ..
                } => {
                    check_finite_vec2(start_mm)?;
                    check_finite_vec2(end_mm)?;
                }
                SketchEntity2D::Arc {
                    center_mm,
                    start_mm,
                    end_mm,
                    ..
                } => {
                    check_finite_vec2(center_mm)?;
                    check_finite_vec2(start_mm)?;
                    check_finite_vec2(end_mm)?;
                }
                SketchEntity2D::Circle {
                    center_mm,
                    radius_mm,
                    ..
                } => {
                    check_finite_vec2(center_mm)?;
                    if !radius_mm.is_finite() {
                        return Err(IrError::NonFiniteNumber);
                    }
                    if *radius_mm <= 0.0 {
                        return Err(IrError::InvalidFeatureSpec {
                            node_id: node_id.clone(),
                            message: format!("circle `{id}` radius must be positive"),
                        });
                    }
                }
                SketchEntity2D::Rectangle {
                    origin_mm,
                    width_mm,
                    height_mm,
                    ..
                } => {
                    check_finite_vec2(origin_mm)?;
                    if !width_mm.is_finite() || !height_mm.is_finite() {
                        return Err(IrError::NonFiniteNumber);
                    }
                    if *width_mm <= 0.0 || *height_mm <= 0.0 {
                        return Err(IrError::InvalidFeatureSpec {
                            node_id: node_id.clone(),
                            message: format!("rectangle `{id}` width and height must be positive"),
                        });
                    }
                }
                SketchEntity2D::SplineBlob { blob, .. } => {
                    if blob.0.trim().is_empty() {
                        return Err(IrError::EmptyId { entity: "blob" });
                    }
                }
                SketchEntity2D::Ellipse {
                    center_mm,
                    rx_mm,
                    ry_mm,
                    ..
                } => {
                    check_finite_vec2(center_mm)?;
                    if !rx_mm.is_finite() || !ry_mm.is_finite() {
                        return Err(IrError::NonFiniteNumber);
                    }
                    if *rx_mm <= 0.0 || *ry_mm <= 0.0 {
                        return Err(IrError::InvalidFeatureSpec {
                            node_id: node_id.clone(),
                            message: format!("ellipse `{id}` semi-axes must be positive"),
                        });
                    }
                }
                SketchEntity2D::Spline {
                    control_points_mm,
                    closed,
                    ..
                } => {
                    let min_pts = if *closed { 3 } else { 2 };
                    if control_points_mm.len() < min_pts {
                        return Err(IrError::InvalidFeatureSpec {
                            node_id: node_id.clone(),
                            message: format!(
                                "spline `{id}` requires at least {min_pts} control points"
                            ),
                        });
                    }
                    for pt in control_points_mm {
                        check_finite_vec2(pt)?;
                    }
                }
            }
        }

        let mut profile_ids = BTreeSet::new();
        for profile in &self.profiles {
            if profile.id.trim().is_empty() {
                return Err(IrError::EmptyId {
                    entity: "sketch_profile",
                });
            }
            if !profile_ids.insert(profile.id.as_str()) {
                return Err(IrError::DuplicateId {
                    entity: "sketch_profile",
                    id: profile.id.clone(),
                });
            }
            if profile.entity_ids.is_empty() {
                return Err(IrError::InvalidFeatureSpec {
                    node_id: node_id.clone(),
                    message: format!("sketch profile `{}` cannot be empty", profile.id),
                });
            }
            for eid in &profile.entity_ids {
                check_entity_exists(node_id, &entity_ids, eid)?;
            }
        }

        for constraint in &self.constraints {
            match constraint {
                SketchConstraint2D::Coincident { a, b, .. } => {
                    check_entity_exists(node_id, &entity_ids, &a.entity_id)?;
                    check_entity_exists(node_id, &entity_ids, &b.entity_id)?;
                }
                SketchConstraint2D::PointOnLine { point, line, .. } => {
                    check_entity_exists(node_id, &entity_ids, &point.entity_id)?;
                    check_entity_exists(node_id, &entity_ids, line)?;
                }
                SketchConstraint2D::Horizontal { entity, .. }
                | SketchConstraint2D::Vertical { entity, .. } => {
                    check_entity_exists(node_id, &entity_ids, entity)?;
                }
                SketchConstraint2D::Parallel { a, b, .. }
                | SketchConstraint2D::Perpendicular { a, b, .. }
                | SketchConstraint2D::Tangent { a, b, .. }
                | SketchConstraint2D::Concentric { a, b, .. }
                | SketchConstraint2D::Equal { a, b, .. } => {
                    check_entity_exists(node_id, &entity_ids, a)?;
                    check_entity_exists(node_id, &entity_ids, b)?;
                }
                SketchConstraint2D::Symmetric {
                    a, b, axis_line, ..
                } => {
                    check_entity_exists(node_id, &entity_ids, &a.entity_id)?;
                    check_entity_exists(node_id, &entity_ids, &b.entity_id)?;
                    check_entity_exists(node_id, &entity_ids, axis_line)?;
                }
                SketchConstraint2D::Distance { a, b, value, .. } => {
                    check_entity_exists(node_id, &entity_ids, &a.entity_id)?;
                    check_entity_exists(node_id, &entity_ids, &b.entity_id)?;
                    value.validate_expected_type(node_id, IrType::Length, "distance constraint")?;
                }
                SketchConstraint2D::Radius { entity, value, .. } => {
                    check_entity_exists(node_id, &entity_ids, entity)?;
                    value.validate_expected_type(node_id, IrType::Length, "radius constraint")?;
                }
                SketchConstraint2D::Diameter { entity, value, .. } => {
                    check_entity_exists(node_id, &entity_ids, entity)?;
                    value.validate_expected_type(node_id, IrType::Length, "diameter constraint")?;
                }
                SketchConstraint2D::Angle { a, b, value, .. } => {
                    check_entity_exists(node_id, &entity_ids, a)?;
                    check_entity_exists(node_id, &entity_ids, b)?;
                    value.validate_expected_type(node_id, IrType::Angle, "angle constraint")?;
                }
            }
        }

        Ok(())
    }
}

fn check_entity_exists(
    node_id: &NodeId,
    entity_ids: &BTreeSet<&str>,
    eid: &str,
) -> Result<(), IrError> {
    if !entity_ids.contains(eid) {
        return Err(IrError::InvalidFeatureSpec {
            node_id: node_id.clone(),
            message: format!("sketch references unknown entity `{eid}`"),
        });
    }
    Ok(())
}

fn check_finite_vec2(v: &[f64; 2]) -> Result<(), IrError> {
    if v.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(IrError::NonFiniteNumber)
    }
}

fn check_finite_vec3(v: &[f64; 3]) -> Result<(), IrError> {
    if v.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(IrError::NonFiniteNumber)
    }
}

fn serialize_vec2<S: serde::Serializer>(v: &[f64; 2], serializer: S) -> Result<S::Ok, S::Error> {
    if !v[0].is_finite() || !v[1].is_finite() {
        return Err(serde::ser::Error::custom("IR numbers must be finite"));
    }
    [wire::canonical_zero(v[0]), wire::canonical_zero(v[1])].serialize(serializer)
}

fn serialize_vec2_list<S: serde::Serializer>(
    points: &[[f64; 2]],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(points.len()))?;
    for v in points {
        if !v[0].is_finite() || !v[1].is_finite() {
            return Err(serde::ser::Error::custom("IR numbers must be finite"));
        }
        seq.serialize_element(&[wire::canonical_zero(v[0]), wire::canonical_zero(v[1])])?;
    }
    seq.end()
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

use cad_syntax::Span;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DimType {
    Length,
    Angle,
    Force,
    Pressure,
    Area,
    Volume,
    F64,
    U32,
    Bool,
    Str,
    Solid,
    Assembly,
    Component,
    Sketch2D,
    SketchPoint,
    SketchEntity,
    FaceSet,
    EdgeSet,
    Axis,
    Plane,
    Direction,
    PmiNote,
    ForceLoad,
    Array(Box<DimType>),
    Unknown,
}

impl fmt::Display for DimType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DimType::Length => write!(f, "Length"),
            DimType::Angle => write!(f, "Angle"),
            DimType::Force => write!(f, "Force"),
            DimType::Pressure => write!(f, "Pressure"),
            DimType::Area => write!(f, "Area"),
            DimType::Volume => write!(f, "Volume"),
            DimType::F64 => write!(f, "f64"),
            DimType::U32 => write!(f, "u32"),
            DimType::Bool => write!(f, "bool"),
            DimType::Str => write!(f, "str"),
            DimType::Solid => write!(f, "Solid"),
            DimType::Assembly => write!(f, "Assembly"),
            DimType::Component => write!(f, "Component"),
            DimType::Sketch2D => write!(f, "Sketch2D"),
            DimType::SketchPoint => write!(f, "Point2D"),
            DimType::SketchEntity => write!(f, "SketchEntity"),
            DimType::FaceSet => write!(f, "FaceSet"),
            DimType::EdgeSet => write!(f, "EdgeSet"),
            DimType::Axis => write!(f, "Axis"),
            DimType::Plane => write!(f, "Plane"),
            DimType::Direction => write!(f, "Direction"),
            DimType::PmiNote => write!(f, "PmiNote"),
            DimType::ForceLoad => write!(f, "ForceLoad"),
            DimType::Array(inner) => write!(f, "[{inner}]"),
            DimType::Unknown => write!(f, "Unknown"),
        }
    }
}

impl DimType {
    pub fn from_type_name(name: &str) -> Self {
        match name {
            "Length" => DimType::Length,
            "Angle" => DimType::Angle,
            "Force" => DimType::Force,
            "Pressure" => DimType::Pressure,
            "Area" => DimType::Area,
            "Volume" => DimType::Volume,
            "f64" | "Float" | "Scalar" => DimType::F64,
            "u32" | "i32" | "Int" => DimType::U32,
            "bool" | "Bool" => DimType::Bool,
            "str" | "String" => DimType::Str,
            "Solid" => DimType::Solid,
            "Assembly" => DimType::Assembly,
            "Component" => DimType::Component,
            "Sketch" | "Sketch2D" => DimType::Sketch2D,
            _ => DimType::Unknown,
        }
    }

    pub fn is_numeric_scalar(&self) -> bool {
        matches!(self, DimType::F64 | DimType::U32)
    }

    pub fn canonical_unit(&self) -> &'static str {
        match self {
            DimType::Length => "mm",
            DimType::Angle => "deg",
            DimType::Force => "N",
            DimType::Pressure => "MPa",
            DimType::Area => "mm2",
            DimType::Volume => "mm3",
            _ => "",
        }
    }
}

/// Converts a literal quantity with an optional unit suffix into canonical units (`mm`, `deg`, `N`, `MPa`).
pub fn convert_quantity(value: f64, unit: Option<&str>) -> Result<(f64, DimType, &'static str), String> {
    match unit {
        None => Ok((value, DimType::F64, "")),
        Some("mm") => Ok((value, DimType::Length, "mm")),
        Some("cm") => Ok((value * 10.0, DimType::Length, "mm")),
        Some("m") => Ok((value * 1000.0, DimType::Length, "mm")),
        Some("in") => Ok((value * 25.4, DimType::Length, "mm")),
        Some("deg") => Ok((value, DimType::Angle, "deg")),
        Some("rad") => Ok((value.to_degrees(), DimType::Angle, "deg")),
        Some("N") => Ok((value, DimType::Force, "N")),
        Some("kN") => Ok((value * 1000.0, DimType::Force, "N")),
        Some("Pa") => Ok((value * 1e-6, DimType::Pressure, "MPa")),
        Some("MPa") => Ok((value, DimType::Pressure, "MPa")),
        Some("GPa") => Ok((value * 1000.0, DimType::Pressure, "MPa")),
        Some(other) => Err(format!("unknown physical unit `{other}`")),
    }
}

/// Three-tier bidirectional AST synchronization editability classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SyncTier {
    Tier1 = 1,
    Tier2 = 2,
    Tier3 = 3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierClassification {
    pub tier: SyncTier,
    pub base_param: Option<String>,
    pub offset_value: Option<f64>,
    pub offset_span: Option<Span>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditHandleCandidate {
    pub handle_id: String,
    pub param_name: String,
    pub tier: u8,
    pub dim_type: DimType,
    pub current_value: Option<f64>,
    pub unit: String,
    pub ast_span: Span,
    pub classification: TierClassification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoverInfo {
    pub symbol: String,
    pub dim_type: DimType,
    pub sync_tier: Option<SyncTier>,
    pub canonical_unit: String,
    pub span: Span,
}

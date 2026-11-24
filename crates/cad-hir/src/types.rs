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

/// Import parameter metadata shared by the checker and editor tooling.
#[derive(Debug)]
pub struct ImportParameter {
    pub name: &'static str,
    pub ty: DimType,
    pub required: bool,
    pub documentation: &'static str,
}

pub fn import_parameters(callee: &str) -> Option<&'static [ImportParameter]> {
    const PATH: ImportParameter = ImportParameter {
        name: "path",
        ty: DimType::Str,
        required: true,
        documentation: "Asset path relative to the workspace root.",
    };
    const SHA256: ImportParameter = ImportParameter {
        name: "sha256",
        ty: DimType::Str,
        required: false,
        documentation: "Expected SHA-256 hex digest of the asset bytes.",
    };
    const OPEN_SHELL: ImportParameter = ImportParameter {
        name: "allow_open_shell",
        ty: DimType::Bool,
        required: false,
        documentation: "Allows open shells for preview, not for booleans. Defaults to false.",
    };
    const STEP: &[ImportParameter] = &[
        PATH,
        SHA256,
        ImportParameter {
            name: "deflection",
            ty: DimType::Length,
            required: false,
            documentation: "Chordal deflection for viewport tessellation. Defaults to 0.05mm.",
        },
        ImportParameter {
            name: "component",
            ty: DimType::Str,
            required: false,
            documentation: "Selects one assembly component by name or ID.",
        },
        OPEN_SHELL,
        ImportParameter {
            name: "heal",
            ty: DimType::Bool,
            required: false,
            documentation: "Repairs and sews exact geometry, then merges same-domain faces. Defaults to false.",
        },
        ImportParameter {
            name: "sewing_tol",
            ty: DimType::Length,
            required: false,
            documentation: "Positive sewing tolerance used when heal is true. Defaults to 0.001mm.",
        },
    ];
    const MESH: &[ImportParameter] = &[
        PATH,
        ImportParameter {
            name: "source_unit",
            ty: DimType::Str,
            required: false,
            documentation: "Source length unit. STL and OBJ default to mm, glTF and GLB to m, and 3MF to the file unit.",
        },
        SHA256,
        ImportParameter {
            name: "cluster_analytic_faces",
            ty: DimType::Bool,
            required: false,
            documentation: "Clusters approximate planar and cylindrical faces. Defaults to true.",
        },
        OPEN_SHELL,
    ];
    match callee {
        "import::step" => Some(STEP),
        "import::mesh" => Some(MESH),
        _ => None,
    }
}

#[derive(Debug)]
pub struct ImportArgumentHover {
    pub parameter: &'static ImportParameter,
    pub span: Span,
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

//! Engineering metadata kept orthogonal to the canonical modeling graph.

use std::collections::BTreeMap;

use cad_topo::TopoRef;
use serde::{Deserialize, Serialize};

/// A PMI annotation kind supported by the container schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PmiKind {
    Datum,
    Flatness,
    Perpendicularity,
    Position,
    ToleranceBand,
    SurfaceFinish,
    ThreadCallout,
}

/// Descriptive alias for callers that prefer the full annotation type name.
pub type PmiAnnotationKind = PmiKind;

/// A semantic PMI annotation associated with a durable topology reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PmiAnnotation {
    pub id: String,
    pub kind: PmiKind,
    pub label: String,
    pub value_mm: Option<f64>,
    pub datum_refs: Vec<String>,
    pub target: TopoRef,
}

/// A per-topology appearance override for rendering and review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppearanceOverride {
    pub target: TopoRef,
    pub rgba: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub finish_note: Option<String>,
}

/// A part or assembly item in the engineering BOM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BomItem {
    pub part_number: String,
    pub revision: String,
    pub name: String,
    pub material_id: Option<String>,
    pub quantity: u32,
    #[serde(default)]
    pub custom_attributes: BTreeMap<String, String>,
}

/// Engineering records that intentionally remain independent of graph nodes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringMetadata {
    #[serde(default)]
    pub pmi: Vec<PmiAnnotation>,
    #[serde(default)]
    pub appearances: Vec<AppearanceOverride>,
    #[serde(default)]
    pub bom: Vec<BomItem>,
}

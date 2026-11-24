//! Associative two-dimensional drawing records.

use cad_topo::TopoRef;
use serde::{Deserialize, Serialize};

/// A projected, sectioned, or detail drawing view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrawingViewKind {
    Front,
    Top,
    Right,
    Isometric,
    Section {
        label: String,
        plane_origin_mm: [f64; 3],
        plane_normal: [f64; 3],
    },
    Detail {
        scale: f64,
        center_mm: [f64; 2],
        radius_mm: f64,
    },
}

/// A view placement on a drawing sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrawingViewSpec {
    pub view_id: String,
    pub kind: DrawingViewKind,
    pub placement_mm: [f64; 2],
    pub scale: f64,
}

impl DrawingViewSpec {
    pub fn new(
        view_id: impl Into<String>,
        kind: DrawingViewKind,
        placement_mm: [f64; 2],
        scale: f64,
    ) -> Self {
        Self {
            view_id: view_id.into(),
            kind,
            placement_mm,
            scale,
        }
    }
}

/// A dimension whose geometry anchors remain associated with model topology.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrawingDimension {
    pub id: String,
    pub label: String,
    pub anchors: Vec<TopoRef>,
    pub override_text: Option<String>,
    pub placement_mm: [f64; 2],
}

/// One paper sheet and its associative views and dimensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrawingSheet {
    pub sheet_id: String,
    pub title: String,
    pub paper_size: String,
    #[serde(default)]
    pub views: Vec<DrawingViewSpec>,
    #[serde(default)]
    pub dimensions: Vec<DrawingDimension>,
}

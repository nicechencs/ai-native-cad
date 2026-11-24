//! Shared scene mesh DTOs and builder decoupled from rendering backends.

use cad_kernel_occt::{ComponentMeta, CurveType, SolidShape, SurfaceType};
use cad_syntax::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceMesh {
    pub face_id: String,
    pub surface_type: SurfaceType,
    pub vertices: Vec<f64>,
    pub normals: Vec<f64>,
    pub indices: Vec<u32>,
    pub provenance_tags: Vec<String>,
    pub exact_interface: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub ast_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopoEdge {
    pub edge_id: String,
    pub curve_type: CurveType,
    pub polyline: Vec<f64>,
    pub provenance_tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub ast_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditHandle {
    pub handle_id: String,
    pub param_name: String,
    pub tier: u8,
    pub current_value: f64,
    pub unit: String,
    pub ast_span: Span,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoleOffsetHandle {
    pub face_id: String,
    pub radius_mm: f64,
    pub selector: String,
    pub return_start: usize,
    pub expression_end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneMeshPayload {
    pub format_version: String,
    pub part_name: String,
    pub kind: String,
    pub units: String,
    pub faces: Vec<FaceMesh>,
    pub edges: Vec<TopoEdge>,
    pub edit_handles: Vec<EditHandle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hole_offsets: Vec<HoleOffsetHandle>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentMeta>,
}

pub fn build_scene_mesh(shape: &SolidShape, edit_handles: Vec<EditHandle>) -> SceneMeshPayload {
    let faces = shape
        .faces
        .iter()
        .map(|f| FaceMesh {
            face_id: f.face_id.clone(),
            surface_type: f.surface_type,
            vertices: f.vertices.clone(),
            normals: f.normals.clone(),
            indices: f.indices.clone(),
            provenance_tags: f.provenance_tags.clone(),
            exact_interface: f.exact_interface,
            component_id: f.component_id.clone(),
            ast_span: f.ast_span,
        })
        .collect();

    let edges = shape
        .edges
        .iter()
        .map(|e| TopoEdge {
            edge_id: e.edge_id.clone(),
            curve_type: e.curve_type,
            polyline: e.polyline.clone(),
            provenance_tags: e.provenance_tags.clone(),
            component_id: e.component_id.clone(),
            ast_span: e.ast_span,
        })
        .collect();

    let kind = if shape.components.is_empty() {
        "part".to_string()
    } else {
        "assembly".to_string()
    };

    SceneMeshPayload {
        format_version: "0.1".to_string(),
        part_name: shape.part_name.clone(),
        kind,
        units: "mm".to_string(),
        faces,
        edges,
        edit_handles,
        hole_offsets: Vec::new(),
        components: shape.components.clone(),
    }
}

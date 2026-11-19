use cad_syntax::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BBox3 {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Default for BBox3 {
    fn default() -> Self {
        Self {
            min: [0.0, 0.0, 0.0],
            max: [0.0, 0.0, 0.0],
        }
    }
}

impl BBox3 {
    pub fn empty() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }

    pub fn is_valid(&self) -> bool {
        self.min[0].is_finite()
            && self.max[0].is_finite()
            && self.min[0] <= self.max[0]
            && self.min[1] <= self.max[1]
            && self.min[2] <= self.max[2]
    }

    pub fn expand_point(&mut self, p: [f64; 3]) {
        for i in 0..3 {
            self.min[i] = self.min[i].min(p[i]);
            self.max[i] = self.max[i].max(p[i]);
        }
    }

    pub fn union(self, other: BBox3) -> BBox3 {
        if !self.is_valid() {
            return other;
        }
        if !other.is_valid() {
            return self;
        }
        BBox3 {
            min: [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            max: [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        }
    }

    pub fn size(&self) -> [f64; 3] {
        if !self.is_valid() {
            return [0.0, 0.0, 0.0];
        }
        [
            (self.max[0] - self.min[0]).max(0.0),
            (self.max[1] - self.min[1]).max(0.0),
            (self.max[2] - self.min[2]).max(0.0),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SurfaceType {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    Bspline,
    Mesh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CurveType {
    Line,
    Circle,
    Arc,
    Ellipse,
    Bspline,
    Polyline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BRepFace {
    pub face_id: String,
    pub surface_type: SurfaceType,
    pub normal_hint: [f64; 3],
    pub center_hint: [f64; 3],
    pub radius_hint: Option<f64>,
    pub area_mm2: f64,
    pub vertices: Vec<f64>,
    pub normals: Vec<f64>,
    pub indices: Vec<u32>,
    /// Ordered 3D outer boundary loop vertices for STEP FACE_OUTER_BOUND & topological validation.
    pub outer_loop: Vec<[f64; 3]>,
    /// Optional second rim loop for cylindrical/conical surface bounds (STEP FACE_BOUND).
    pub top_loop: Option<Vec<[f64; 3]>>,
    /// Ordered 3D inner hole boundary loops for STEP FACE_BOUND & perforated planar faces.
    pub inner_loops: Vec<Vec<[f64; 3]>>,
    pub provenance_tags: Vec<String>,
    pub exact_interface: bool,
    pub component_id: Option<String>,
    pub ast_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BRepEdge {
    pub edge_id: String,
    pub curve_type: CurveType,
    pub polyline: Vec<f64>,
    pub provenance_tags: Vec<String>,
    pub component_id: Option<String>,
    pub ast_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PmiRecord {
    pub kind: String,
    pub label: String,
    pub tolerance_mm: Option<f64>,
    pub datum: Option<String>,
    pub target_tags: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MateRecord {
    pub kind: String,
    pub component_a: String,
    pub component_b: String,
    pub satisfied: bool,
    pub message: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentMeta {
    pub component_id: String,
    pub part_name: String,
    pub origin_mm: [f64; 3],
    pub rotation_deg: [f64; 3],
    pub volume_mm3: f64,
    pub bbox_mm: BBox3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckReport {
    pub watertight: bool,
    pub volume_mm3: f64,
    pub surface_area_mm2: f64,
    pub bbox_mm: BBox3,
    pub face_count: usize,
    pub edge_count: usize,
    pub open_edge_count: usize,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SketchPlane {
    Xy,
    Xz,
    Yz,
}

impl SketchPlane {
    pub fn to_3d(self, u: f64, v: f64, offset: f64) -> [f64; 3] {
        match self {
            SketchPlane::Xy => [u, v, offset],
            SketchPlane::Xz => [u, offset, v],
            SketchPlane::Yz => [offset, u, v],
        }
    }

    pub fn from_3d(self, p: [f64; 3]) -> (f64, f64, f64) {
        match self {
            SketchPlane::Xy => (p[0], p[1], p[2]),
            SketchPlane::Xz => (p[0], p[2], p[1]),
            SketchPlane::Yz => (p[1], p[2], p[0]),
        }
    }

    pub fn normal(self) -> [f64; 3] {
        match self {
            SketchPlane::Xy => [0.0, 0.0, 1.0],
            SketchPlane::Xz => [0.0, 1.0, 0.0],
            SketchPlane::Yz => [1.0, 0.0, 0.0],
        }
    }
}

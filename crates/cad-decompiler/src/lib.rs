//! Face adjacency, kernel-agnostic Attributed Adjacency Graph (AAG), prismatic
//! mechanical feature recognition, and parametric `CanonicalGraph` decompilation
//! with zero-trust symmetric volume verification closure.

pub mod aag;
#[cfg(feature = "occt-ffi")]
mod occt;
pub mod recognize;
pub mod reconstruct;

pub use aag::{
    AagEdge, AagFace, AagSurfaceKind, AttributedAdjacencyGraph, CylinderAttributes, EdgeConvexity,
    PlaneAttributes,
};
pub use recognize::{
    BaseStock, BossFeature, DefaultFeatureRecognizer, DressUpHint, DressUpHintKind,
    FeatureRecognizer, HoleFeature, HoleKind, HolePatternFeature, PatternKind, PocketFeature,
    RecognitionOptions, RecognitionResult, RecognizedFeature,
};
pub use reconstruct::{
    DecompileError, DecompileOptions, DecompileOutput, DecompileReport, DecompileStatus,
    VerificationMethod, decompile_solid, decompile_solid_with_recognizer,
    reconstruct_canonical_graph, save_decompiled_project, verify_decompilation,
};

/// A zero-based face index in one graph. It is not a persistent reference
/// across topology-changing operations, export/import, or healing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct FaceId(pub usize);

/// A zero-based edge index in one graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct EdgeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    BSpline,
    Other,
}

/// The type of the supporting curve (e.g. a circular arc is `Circle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveKind {
    Line,
    Circle,
    Ellipse,
    BSpline,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceUse {
    pub face: FaceId,
    /// The edge has two parameter-space curves on this face's closed surface.
    pub is_seam: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub curve_kind: CurveKind,
    pub is_degenerate: bool,
    /// One record per distinct incident face. A seam on one face is not a
    /// self-adjacency. Non-manifold edges may have three or more incident faces.
    pub face_uses: Vec<FaceUse>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidIncidence {
    pub edge: EdgeId,
    pub face: FaceId,
}

impl std::fmt::Display for InvalidIncidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "edge {} references missing or repeated face {}",
            self.edge.0, self.face.0
        )
    }
}

impl std::error::Error for InvalidIncidence {}

/// Two distinct faces are adjacent if they share a topological edge. Face
/// and edge vectors preserve input ID order, including isolated faces, wire
/// edges, and multiple shared edges between the same pair of faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceAdjacencyGraph {
    faces: Vec<SurfaceKind>,
    edges: Vec<Edge>,
    incident_edges: Vec<Vec<EdgeId>>,
}

impl FaceAdjacencyGraph {
    /// Builds the graph from face and edge attributes supplied by a B-Rep
    /// backend. Face incidences are sorted and repeated or invalid IDs rejected.
    pub fn from_incidence(
        faces: Vec<SurfaceKind>,
        mut edges: Vec<Edge>,
    ) -> Result<Self, InvalidIncidence> {
        let mut incident_edges = vec![Vec::new(); faces.len()];
        for (index, edge) in edges.iter_mut().enumerate() {
            let edge_id = EdgeId(index);
            edge.face_uses.sort_unstable_by_key(|use_| use_.face);
            let mut previous = None;
            for use_ in &edge.face_uses {
                let face = use_.face;
                let error = InvalidIncidence {
                    edge: edge_id,
                    face,
                };
                let incident = incident_edges.get_mut(face.0).ok_or(error)?;
                if previous == Some(face) {
                    return Err(error);
                }
                incident.push(edge_id);
                previous = Some(face);
            }
        }
        Ok(Self {
            faces,
            edges,
            incident_edges,
        })
    }

    pub fn faces(&self) -> &[SurfaceKind] {
        &self.faces
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// Returns incident edge IDs in ascending order or `None` for an invalid face.
    pub fn incident_edges(&self, face: FaceId) -> Option<&[EdgeId]> {
        self.incident_edges.get(face.0).map(Vec::as_slice)
    }

    /// Returns each neighboring face once, in ascending ID order. A
    /// non-manifold edge links all its distinct incident faces to one another.
    pub fn neighbors(&self, face: FaceId) -> Option<Vec<FaceId>> {
        let mut neighbors: Vec<_> = self
            .incident_edges(face)?
            .iter()
            .flat_map(|id| self.edges[id.0].face_uses.iter().map(|use_| use_.face))
            .filter(|&id| id != face)
            .collect();
        neighbors.sort_unstable();
        neighbors.dedup();
        Some(neighbors)
    }
}

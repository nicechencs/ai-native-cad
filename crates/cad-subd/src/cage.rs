//! SubD control cage: vertices, n-gon faces, and undirected crease weights.

use std::collections::BTreeMap;

/// Smooth crease. The edge uses the ordinary Catmull-Clark rule.
pub const CREASE_SMOOTH: f32 = 0.0;

/// Infinitely sharp crease. Values at or above this do not decay when subdivided.
pub const CREASE_INFINITE: f32 = 10.0;

/// Why a cage or a `SUBD` blob was rejected.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SubDError {
    /// The cage has an empty vertex list.
    #[error("control cage has no vertices")]
    EmptyVertices,

    /// Subdivision was asked to refine a cage that has no faces.
    #[error("control cage has no faces")]
    EmptyFaces,

    /// A vertex coordinate is NaN or infinite.
    #[error("vertex {index} has a non-finite coordinate")]
    NonFiniteVertex { index: usize },

    /// A face has fewer than three corners.
    #[error("face {face} has {count} vertices; at least 3 are required")]
    DegenerateFace { face: usize, count: usize },

    /// The same vertex index appears twice in one face.
    #[error("face {face} repeats vertex {vertex}")]
    RepeatedFaceVertex { face: usize, vertex: u32 },

    /// A face corner does not name a vertex in this cage.
    #[error("face {face} references vertex {vertex} but the cage has {vertex_count} vertices")]
    VertexOutOfRange {
        face: usize,
        vertex: u32,
        vertex_count: usize,
    },

    /// Crease keys must be ordered undirected edges `(min, max)` with `min < max`.
    #[error("crease ({a}, {b}) must be stored as an ordered pair with a < b")]
    CreaseKeyNotNormalized { a: u32, b: u32 },

    /// A crease weight is NaN or infinite.
    #[error("crease sharpness is not finite")]
    NonFiniteCrease,

    /// A crease weight is outside the closed range `0.0..=10.0`.
    #[error("crease sharpness {sharpness} is outside 0.0..=10.0")]
    CreaseOutOfRange { sharpness: f32 },

    /// A crease endpoint is not a vertex of this cage.
    #[error("crease ({a}, {b}) references a vertex outside 0..{vertex_count}")]
    CreaseVertexOutOfRange { a: u32, b: u32, vertex_count: usize },

    /// A crease does not lie on any face edge.
    #[error("crease ({a}, {b}) is not an edge of any face")]
    UnknownCreaseEdge { a: u32, b: u32 },

    /// The same crease key was stored twice in one blob.
    #[error("duplicate crease ({a}, {b})")]
    DuplicateCrease { a: u32, b: u32 },

    /// More than two faces meet at one edge, so subdivision has no unique rule.
    #[error("edge ({a}, {b}) has {incident_faces} incident faces; a manifold edge has at most 2")]
    NonManifoldEdge {
        a: u32,
        b: u32,
        incident_faces: usize,
    },

    /// Two adjacent faces traverse a shared edge in the same direction (inconsistent winding).
    #[error("directed edge ({a}, {b}) is used twice with the same orientation")]
    NonOrientableEdge { a: u32, b: u32 },

    /// The byte slice ends before a complete field.
    #[error("SubD blob is truncated")]
    TruncatedBlob,

    /// The first four bytes are not `SUBD`.
    #[error("SubD blob magic is not SUBD")]
    BadMagic,

    /// The blob header names a format version this crate does not read.
    #[error("SubD blob version {version} is not supported")]
    UnsupportedVersion { version: u32 },

    /// Bytes remain after the last crease entry.
    #[error("SubD blob has {count} trailing bytes")]
    TrailingBytes { count: usize },

    /// A count overflows `u32` or the product of a count and a stride.
    #[error("SubD blob count does not fit the payload")]
    InvalidCount,
}

/// Edge-incidence summary from [`SubDCage::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CageReport {
    /// Every edge is shared by exactly two faces, and the cage has at least one edge.
    pub is_closed_manifold: bool,
    /// Edges that belong to exactly one face.
    pub boundary_edge_count: usize,
    /// Edges that belong to three or more faces.
    pub non_manifold_edge_count: usize,
    /// Number of control vertices.
    pub vertex_count: usize,
    /// Number of faces.
    pub face_count: usize,
    /// Number of unique undirected face edges.
    pub edge_count: usize,
}

/// Polygon control cage for Catmull-Clark subdivision.
///
/// `edge_creases` uses an undirected key `(min(u, v), max(u, v))`. Sharpness
/// `0.0` is smooth and sharpness `10.0` is infinitely sharp.
#[derive(Debug, Clone, PartialEq)]
pub struct SubDCage {
    /// Control points, in millimeters when produced by [`SubDCage::unit_cube`].
    pub vertices: Vec<[f64; 3]>,
    /// Face corners. Each face may be a quad or any n-gon with at least 3 corners.
    pub faces: Vec<Vec<u32>>,
    /// Undirected edge sharpness in `0.0..=10.0`.
    pub edge_creases: BTreeMap<(u32, u32), f32>,
}

impl SubDCage {
    /// Axis-aligned cube centered at the origin.
    ///
    /// `size_mm` is the edge length. Corners sit at `±size_mm / 2`. Faces wind
    /// so their normals point outward.
    pub fn unit_cube(size_mm: f64) -> Self {
        let h = size_mm * 0.5;
        Self {
            vertices: vec![
                [-h, -h, -h],
                [h, -h, -h],
                [h, h, -h],
                [-h, h, -h],
                [-h, -h, h],
                [h, -h, h],
                [h, h, h],
                [-h, h, h],
            ],
            faces: vec![
                vec![0, 3, 2, 1],
                vec![4, 5, 6, 7],
                vec![0, 1, 5, 4],
                vec![3, 7, 6, 2],
                vec![0, 4, 7, 3],
                vec![1, 2, 6, 5],
            ],
            edge_creases: BTreeMap::new(),
        }
    }

    /// Store a crease on the undirected edge `(a, b)`.
    ///
    /// The key is normalized to `(min, max)`. The edge must already exist on a face.
    pub fn set_crease(&mut self, a: u32, b: u32, sharpness: f32) -> Result<(), SubDError> {
        let key = normalized_crease_key(a, b)?;
        check_sharpness(sharpness)?;
        let vertex_count = self.vertices.len();
        if key.0 as usize >= vertex_count || key.1 as usize >= vertex_count {
            return Err(SubDError::CreaseVertexOutOfRange {
                a: key.0,
                b: key.1,
                vertex_count,
            });
        }
        if !self.edge_incidences().contains_key(&key) {
            return Err(SubDError::UnknownCreaseEdge { a: key.0, b: key.1 });
        }
        self.edge_creases.insert(key, sharpness);
        Ok(())
    }

    /// Sharpness of the undirected edge `(a, b)`, if one was stored.
    pub fn crease(&self, a: u32, b: u32) -> Option<f32> {
        if a == b {
            return None;
        }
        self.edge_creases.get(&(a.min(b), a.max(b))).copied()
    }

    /// Check vertex coordinates, face indices, crease weights, and edge incidence.
    ///
    /// A boundary edge or a non-manifold edge does not fail validation. Those
    /// counts are reported. Subdivision itself rejects non-manifold edges.
    pub fn validate(&self) -> Result<CageReport, SubDError> {
        if self.vertices.is_empty() {
            return Err(SubDError::EmptyVertices);
        }
        for (index, vertex) in self.vertices.iter().enumerate() {
            if !vertex.iter().all(|coordinate| coordinate.is_finite()) {
                return Err(SubDError::NonFiniteVertex { index });
            }
        }

        let vertex_count = self.vertices.len();
        for (face_index, face) in self.faces.iter().enumerate() {
            if face.len() < 3 {
                return Err(SubDError::DegenerateFace {
                    face: face_index,
                    count: face.len(),
                });
            }
            for &vertex in face {
                if vertex as usize >= vertex_count {
                    return Err(SubDError::VertexOutOfRange {
                        face: face_index,
                        vertex,
                        vertex_count,
                    });
                }
            }
            for left in 0..face.len() {
                for right in (left + 1)..face.len() {
                    if face[left] == face[right] {
                        return Err(SubDError::RepeatedFaceVertex {
                            face: face_index,
                            vertex: face[left],
                        });
                    }
                }
            }
        }

        let edges = self.edge_incidences();
        let mut boundary_edge_count = 0;
        let mut non_manifold_edge_count = 0;
        for incident in edges.values() {
            match incident.len() {
                1 => boundary_edge_count += 1,
                2 => {}
                _ => non_manifold_edge_count += 1,
            }
        }

        for (&(a, b), &sharpness) in &self.edge_creases {
            if a >= b {
                return Err(SubDError::CreaseKeyNotNormalized { a, b });
            }
            check_sharpness(sharpness)?;
            if a as usize >= vertex_count || b as usize >= vertex_count {
                return Err(SubDError::CreaseVertexOutOfRange { a, b, vertex_count });
            }
            if !edges.contains_key(&(a, b)) {
                return Err(SubDError::UnknownCreaseEdge { a, b });
            }
        }

        Ok(CageReport {
            is_closed_manifold: !edges.is_empty()
                && boundary_edge_count == 0
                && non_manifold_edge_count == 0,
            boundary_edge_count,
            non_manifold_edge_count,
            vertex_count,
            face_count: self.faces.len(),
            edge_count: edges.len(),
        })
    }

    /// Undirected face edges and the faces that use each one, in key order.
    pub(crate) fn edge_incidences(&self) -> BTreeMap<(u32, u32), Vec<usize>> {
        let mut edges: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
        for (face_index, face) in self.faces.iter().enumerate() {
            let corner_count = face.len();
            if corner_count < 2 {
                continue;
            }
            for corner in 0..corner_count {
                let a = face[corner];
                let b = face[(corner + 1) % corner_count];
                if a == b {
                    continue;
                }
                let key = (a.min(b), a.max(b));
                edges.entry(key).or_default().push(face_index);
            }
        }
        edges
    }
}

pub(crate) fn fit_u32(len: usize) -> Result<u32, SubDError> {
    u32::try_from(len).map_err(|_| SubDError::InvalidCount)
}

fn normalized_crease_key(a: u32, b: u32) -> Result<(u32, u32), SubDError> {
    if a == b {
        return Err(SubDError::CreaseKeyNotNormalized { a, b });
    }
    Ok((a.min(b), a.max(b)))
}

fn check_sharpness(sharpness: f32) -> Result<(), SubDError> {
    if !sharpness.is_finite() {
        return Err(SubDError::NonFiniteCrease);
    }
    if !(CREASE_SMOOTH..=CREASE_INFINITE).contains(&sharpness) {
        return Err(SubDError::CreaseOutOfRange { sharpness });
    }
    Ok(())
}

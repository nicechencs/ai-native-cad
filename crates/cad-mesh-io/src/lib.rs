//! Pure Rust STL, 3MF, OBJ and glTF triangle mesh import. All returned coordinates are millimeters.

mod clustering;
mod gltf;
mod obj;
mod stl;
mod threemf;
mod threemf_writer;

pub use clustering::{AnalyticPatch, AnalyticSurface, cluster_analytic_faces};
pub use gltf::{
    GltfDocument, GltfPrimitiveGroup, load_gltf, load_gltf_document, load_gltf_with_source_unit,
};
pub use obj::{ObjDocument, ObjMaterialGroup, load_obj, load_obj_document};
pub use stl::load_stl;
pub use threemf::{
    ThreeMfBuildItem, ThreeMfDocument, load_3mf, load_3mf_document, load_3mf_document_tolerant,
    load_3mf_with_metadata, load_3mf_with_source_unit,
};
pub use threemf_writer::{
    ThreeMfBeam, ThreeMfBeamLattice, write_3mf_mesh_bytes, write_3mf_mesh_file,
    write_3mf_with_beam_lattice_bytes,
};

use std::collections::HashMap;
use std::str::FromStr;
use thiserror::Error;

const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
const MAX_TRIANGLES: usize = 1_000_000;
const MAX_VERTICES: usize = 3 * MAX_TRIANGLES;
const NO_EDGE: u32 = u32::MAX;

type Vec3 = [f64; 3];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LengthUnit {
    Micron,
    #[default]
    Millimeter,
    Centimeter,
    Inch,
    Foot,
    Meter,
}

impl LengthUnit {
    pub const fn millimeters_per_unit(self) -> f64 {
        match self {
            Self::Micron => 0.001,
            Self::Millimeter => 1.0,
            Self::Centimeter => 10.0,
            Self::Inch => 25.4,
            Self::Foot => 304.8,
            Self::Meter => 1000.0,
        }
    }

    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Micron => "um",
            Self::Millimeter => "mm",
            Self::Centimeter => "cm",
            Self::Inch => "in",
            Self::Foot => "ft",
            Self::Meter => "m",
        }
    }
}

impl FromStr for LengthUnit {
    type Err = MeshError;

    fn from_str(unit: &str) -> Result<Self, Self::Err> {
        match unit {
            "um" | "µm" | "μm" | "micron" => Ok(Self::Micron),
            "mm" | "millimeter" => Ok(Self::Millimeter),
            "cm" | "centimeter" => Ok(Self::Centimeter),
            "in" | "inch" => Ok(Self::Inch),
            "ft" | "foot" => Ok(Self::Foot),
            "m" | "meter" => Ok(Self::Meter),
            _ => Err(MeshError::InvalidUnit(unit.to_owned())),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MeshError {
    #[error("invalid mesh format: {0}")]
    InvalidFormat(String),
    #[error("unsupported mesh length unit: {0}")]
    InvalidUnit(String),
    #[error("mesh resource limit exceeded: {0}")]
    ResourceLimit(&'static str),
    #[error("mesh has no triangles")]
    EmptyMesh,
    #[error("vertex {vertex} has non-finite coordinates")]
    NonFiniteVertex { vertex: usize },
    #[error("triangle {triangle} references vertex {vertex} outside the mesh")]
    InvalidIndex { triangle: usize, vertex: u32 },
    #[error("triangle {triangle} is degenerate")]
    DegenerateTriangle { triangle: usize },
    #[error("edge {vertices:?} does not have exactly two oppositely oriented incident triangles")]
    NonManifoldEdge { vertices: [u32; 2] },
    #[error("triangles {triangles:?} duplicate the same face")]
    DuplicateTriangle { triangles: [usize; 2] },
    #[error("vertex {vertex} has disconnected incident face fans")]
    NonManifoldVertex { vertex: u32 },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub vertices: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

impl Mesh {
    /// Remove triangles with repeated vertex indices or near-zero area in place.
    /// Out-of-bounds indices and non-finite vertices are left untouched so
    /// [`Mesh::validate_geometry`] continues to report them.
    pub fn strip_degenerate_triangles(&mut self) -> usize {
        let before = self.triangles.len();
        let vertices = &self.vertices;
        self.triangles
            .retain(|&tri| !is_degenerate_triangle(vertices, tri));
        before - self.triangles.len()
    }

    /// Deterministically merge vertices within `tol_mm` using a quantized spatial
    /// hash grid, remap triangle indices, strip triangles that become degenerate,
    /// and compact unreferenced vertices. Returns the number of vertices removed.
    pub fn weld_vertices(&mut self, tol_mm: f64) -> usize {
        if !tol_mm.is_finite() || tol_mm <= 0.0 || self.vertices.is_empty() {
            return 0;
        }
        let before_vertices = self.vertices.len();
        let inv_tol = 1.0 / tol_mm;
        let tol_sq = tol_mm * tol_mm;
        let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::with_capacity(self.vertices.len());
        let mut welded: Vec<Vec3> = Vec::with_capacity(self.vertices.len());
        let mut remap = Vec::with_capacity(self.vertices.len());

        for &p in &self.vertices {
            if !p.iter().all(|v| v.is_finite()) {
                let idx = welded.len() as u32;
                welded.push(p);
                remap.push(idx);
                continue;
            }
            let cell = quantize_point(p, inv_tol);
            let mut matched: Option<u32> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let neighbor = [
                            cell[0].saturating_add(dx),
                            cell[1].saturating_add(dy),
                            cell[2].saturating_add(dz),
                        ];
                        if let Some(bucket) = grid.get(&neighbor) {
                            for &cand in bucket {
                                if matched.is_some_and(|best| cand >= best) {
                                    continue;
                                }
                                let d = sub(p, welded[cand as usize]);
                                if dot(d, d) <= tol_sq {
                                    matched = Some(cand);
                                }
                            }
                        }
                    }
                }
            }
            let target = match matched {
                Some(idx) => idx,
                None => {
                    let idx = welded.len() as u32;
                    welded.push(p);
                    grid.entry(cell).or_default().push(idx);
                    idx
                }
            };
            remap.push(target);
        }

        for tri in &mut self.triangles {
            for idx in tri.iter_mut() {
                if let Some(&mapped) = remap.get(*idx as usize) {
                    *idx = mapped;
                }
            }
        }
        self.vertices = welded;
        self.strip_degenerate_triangles();

        // Compact unreferenced vertices while preserving deterministic order.
        let mut used = vec![false; self.vertices.len()];
        for &idx in self.triangles.iter().flatten() {
            if let Some(slot) = used.get_mut(idx as usize) {
                *slot = true;
            }
        }
        let mut compact_map = vec![u32::MAX; self.vertices.len()];
        let mut compacted = Vec::with_capacity(self.vertices.len());
        for (old_idx, (&p, &is_used)) in self.vertices.iter().zip(&used).enumerate() {
            if is_used {
                compact_map[old_idx] = compacted.len() as u32;
                compacted.push(p);
            }
        }
        for tri in &mut self.triangles {
            for idx in tri.iter_mut() {
                if let Some(&mapped) = compact_map.get(*idx as usize)
                    && mapped != u32::MAX
                {
                    *idx = mapped;
                }
            }
        }
        self.vertices = compacted;
        before_vertices.saturating_sub(self.vertices.len())
    }

    /// Validate coordinates and triangle geometry without requiring a closed shell.
    pub fn validate_geometry(&self) -> Result<(), MeshError> {
        check_size(self.vertices.len(), self.triangles.len())?;
        if self.triangles.is_empty() {
            return Err(MeshError::EmptyMesh);
        }
        for (vertex, p) in self.vertices.iter().enumerate() {
            if p.iter().any(|v| !v.is_finite()) {
                return Err(MeshError::NonFiniteVertex { vertex });
            }
        }
        for (triangle, &indices) in self.triangles.iter().enumerate() {
            for vertex in indices {
                if vertex as usize >= self.vertices.len() {
                    return Err(MeshError::InvalidIndex { triangle, vertex });
                }
            }
            if triangle_normal(indices.map(|v| self.vertices[v as usize])).is_none() {
                return Err(MeshError::DegenerateTriangle { triangle });
            }
        }
        Ok(())
    }

    /// Verify closed, oriented two-manifold topology; this does not test for geometric
    /// self-intersections, shell nesting, or globally outward orientation.
    pub fn validate_closed_manifold(&self) -> Result<(), MeshError> {
        self.validate_geometry()?;
        let twins = self.twins(true)?;
        let mut valence = vec![0_u32; self.vertices.len()];
        for &vertex in self.triangles.iter().flatten() {
            valence[vertex as usize] += 1;
        }
        let mut visited = vec![false; twins.len()];
        for start in 0..twins.len() {
            if visited[start] {
                continue;
            }
            let vertex = self.triangles[start / 3][start % 3];
            let mut corner = start;
            let mut count = 0;
            while !visited[corner] {
                visited[corner] = true;
                count += 1;
                let twin = twins[corner] as usize;
                corner = twin / 3 * 3 + (twin + 1) % 3;
            }
            if count != valence[vertex as usize] {
                return Err(MeshError::NonManifoldVertex { vertex });
            }
        }
        Ok(())
    }

    // Each half-edge ID is also the ID of its start corner. In preview mode,
    // unpaired or ambiguous edges are seams, not clustering adjacencies.
    fn twins(&self, strict: bool) -> Result<Vec<u32>, MeshError> {
        let mut edges = Vec::with_capacity(self.triangles.len() * 3);
        for (face, triangle) in self.triangles.iter().enumerate() {
            for (side, &a) in triangle.iter().enumerate() {
                let b = triangle[(side + 1) % 3];
                let id = (3 * face + side) as u32;
                edges.push(([a.min(b), a.max(b)], id, a < b));
            }
        }
        edges.sort_unstable_by_key(|edge| edge.0);
        let mut twins = vec![NO_EDGE; edges.len()];
        for group in edges.chunk_by(|a, b| a.0 == b.0) {
            if let [(_, a, ab), (_, b, ba)] = group
                && ab != ba
            {
                twins[*a as usize] = *b;
                twins[*b as usize] = *a;
            } else if strict {
                return Err(MeshError::NonManifoldEdge {
                    vertices: group[0].0,
                });
            }
        }
        if strict {
            for (face, edges) in twins.as_chunks::<3>().0.iter().enumerate() {
                let [a, b, c] = [edges[0] / 3, edges[1] / 3, edges[2] / 3];
                if a == b || a == c {
                    return Err(MeshError::DuplicateTriangle {
                        triangles: [face, a as usize],
                    });
                }
                if b == c {
                    return Err(MeshError::DuplicateTriangle {
                        triangles: [face, b as usize],
                    });
                }
            }
        }
        Ok(twins)
    }
}

fn check_size(vertices: usize, triangles: usize) -> Result<(), MeshError> {
    if vertices > MAX_VERTICES {
        return Err(MeshError::ResourceLimit("vertex count"));
    }
    if triangles > MAX_TRIANGLES {
        return Err(MeshError::ResourceLimit("triangle count"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> MeshError {
    MeshError::InvalidFormat(message.into())
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    std::array::from_fn(|i| a[i] - b[i])
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    std::array::from_fn(|i| a[i] + b[i])
}

fn mul(a: Vec3, b: f64) -> Vec3 {
    a.map(|x| x * b)
}

fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    std::array::from_fn(|i| {
        let j = (i + 1) % 3;
        let k = (i + 2) % 3;
        a[j] * b[k] - a[k] * b[j]
    })
}

fn norm(a: Vec3) -> f64 {
    a[0].hypot(a[1]).hypot(a[2])
}

fn normalized(a: Vec3) -> Option<Vec3> {
    let n = norm(a);
    (n > 0.0 && n.is_finite()).then(|| a.map(|x| x / n))
}

fn triangle_normal([a, b, c]: [Vec3; 3]) -> Option<Vec3> {
    normalized(cross(normalized(sub(b, a))?, normalized(sub(c, a))?))
}

pub(crate) fn is_degenerate_triangle(vertices: &[Vec3], [a, b, c]: [u32; 3]) -> bool {
    if a == b || b == c || a == c {
        return true;
    }
    let (Some(&pa), Some(&pb), Some(&pc)) = (
        vertices.get(a as usize),
        vertices.get(b as usize),
        vertices.get(c as usize),
    ) else {
        return false;
    };
    if pa.iter().chain(&pb).chain(&pc).any(|v| !v.is_finite()) {
        return false;
    }
    let u = sub(pb, pa);
    let v = sub(pc, pa);
    let Some(nu) = normalized(u) else {
        return true;
    };
    let Some(nv) = normalized(v) else {
        return true;
    };
    let unit_cross = cross(nu, nv);
    if normalized(unit_cross).is_none() || norm(unit_cross) <= 1e-12 {
        return true;
    }
    let raw_cross = cross(u, v);
    let area2_sq = dot(raw_cross, raw_cross);
    !area2_sq.is_finite() || area2_sq <= 1e-30
}

fn quantize_point(p: Vec3, inv_tol: f64) -> [i64; 3] {
    const LIMIT: f64 = (i64::MAX / 2) as f64;
    p.map(|v| (v * inv_tol).floor().clamp(-LIMIT, LIMIT) as i64)
}

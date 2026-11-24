//! Marching tetrahedra and sampled volume estimates for scalar fields.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::field::{ScalarField, normal, unit_vector};

/// An indexed triangle mesh with outward winding and per-vertex unit normals.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImplicitMesh {
    /// Vertex positions in millimeters.
    pub vertices: Vec<[f64; 3]>,
    pub normals: Vec<[f64; 3]>,
    /// Three zero-based vertex indices per triangle.
    pub indices: Vec<u32>,
}

const CORNERS: [[usize; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [1, 1, 0],
    [0, 1, 0],
    [0, 0, 1],
    [1, 0, 1],
    [1, 1, 1],
    [0, 1, 1],
];

// These positively oriented tetrahedra share the 0–6 body diagonal. Neighboring cells
// use the same diagonals on their shared faces.
const TETRAHEDRA: [[usize; 4]; 6] = [
    [0, 5, 1, 6],
    [0, 1, 2, 6],
    [0, 2, 3, 6],
    [0, 3, 7, 6],
    [0, 7, 4, 6],
    [0, 4, 5, 6],
];

/// Extracts the zero isosurface over `resolution` grid cells per axis.
///
/// Crossings on shared tetrahedral edges use the same vertex index. The mesh closes where
/// the sampled solid is enclosed by the box, including where the field is zero on its boundary.
/// Invalid grids, non-finite grid samples, and grid or index size overflow yield an empty mesh.
pub fn extract_isosurface(
    field: &dyn ScalarField,
    bbox_min: [f64; 3],
    bbox_max: [f64; 3],
    resolution: [usize; 3],
) -> ImplicitMesh {
    match extract_isosurface_checked(field, bbox_min, bbox_max, resolution) {
        Ok(mesh) => mesh,
        Err(reason) => {
            cad_observe::tracing::warn!(
                component = "cad-implicit",
                operation = "extract_isosurface",
                code = cad_observe::codes::R013_EMPTY_ISOSURFACE,
                error_type = reason.error_type(),
                "{}",
                reason.message()
            );
            ImplicitMesh::default()
        }
    }
}

fn extract_isosurface_checked(
    field: &dyn ScalarField,
    bbox_min: [f64; 3],
    bbox_max: [f64; 3],
    resolution: [usize; 3],
) -> Result<ImplicitMesh, EmptyIsosurfaceReason> {
    let grid =
        Grid::new(bbox_min, bbox_max, resolution).ok_or(EmptyIsosurfaceReason::InvalidGrid)?;
    grid.extract(field)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyIsosurfaceReason {
    InvalidGrid,
    GridOverflow,
    NonFiniteSample,
    IndexOverflow,
    NoZeroCrossings,
}

impl EmptyIsosurfaceReason {
    fn error_type(self) -> &'static str {
        match self {
            Self::InvalidGrid => "InvalidGrid",
            Self::GridOverflow => "GridOverflow",
            Self::NonFiniteSample => "NonFiniteSample",
            Self::IndexOverflow => "IndexOverflow",
            Self::NoZeroCrossings => "NoZeroCrossings",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::InvalidGrid => {
                "invalid grid bounds or zero resolution; returning empty isosurface mesh"
            }
            Self::GridOverflow => {
                "grid resolution overflows sample capacity; returning empty isosurface mesh"
            }
            Self::NonFiniteSample => {
                "non-finite scalar field sample encountered; returning empty isosurface mesh"
            }
            Self::IndexOverflow => {
                "isosurface vertex count overflows u32 indices; returning empty isosurface mesh"
            }
            Self::NoZeroCrossings => {
                "scalar field has no zero crossings in grid; returning empty isosurface mesh"
            }
        }
    }
}

/// Estimates the negative solid volume in the bounding box in cubic millimeters.
///
/// Each cell contributes 27 equally spaced subcell midpoint samples. Invalid grids yield zero.
pub fn estimate_volume_mm3(
    field: &dyn ScalarField,
    bbox_min: [f64; 3],
    bbox_max: [f64; 3],
    resolution: [usize; 3],
) -> f64 {
    let Some(grid) = Grid::new(bbox_min, bbox_max, resolution) else {
        return 0.0;
    };
    let fraction = grid.solid_fraction(field);
    if fraction == 0.0 {
        return 0.0;
    }
    let [x, y, z] = grid.extents_mm;
    fraction * x * y * z
}

/// Estimates the fraction of the bounding box occupied by solid, in `[0, 1]`.
///
/// Invalid grids yield zero. Non-finite samples contribute no solid volume.
pub fn relative_density(
    field: &dyn ScalarField,
    bbox_min: [f64; 3],
    bbox_max: [f64; 3],
    resolution: [usize; 3],
) -> f64 {
    Grid::new(bbox_min, bbox_max, resolution).map_or(0.0, |grid| grid.solid_fraction(field))
}

struct Grid {
    min_mm: [f64; 3],
    max_mm: [f64; 3],
    extents_mm: [f64; 3],
    cells: [usize; 3],
}

impl Grid {
    fn new(min_mm: [f64; 3], max_mm: [f64; 3], cells: [usize; 3]) -> Option<Self> {
        let extents_mm = std::array::from_fn(|axis| max_mm[axis] - min_mm[axis]);
        if cells.contains(&0) || extents_mm.iter().any(|&len| !len.is_finite() || len <= 0.0) {
            return None;
        }
        Some(Self {
            min_mm,
            max_mm,
            extents_mm,
            cells,
        })
    }

    fn node_point(&self, coordinate: [usize; 3]) -> [f64; 3] {
        std::array::from_fn(|axis| {
            if coordinate[axis] == self.cells[axis] {
                self.max_mm[axis]
            } else {
                let t = coordinate[axis] as f64 / self.cells[axis] as f64;
                t.mul_add(self.extents_mm[axis], self.min_mm[axis])
            }
        })
    }

    fn extract(&self, field: &dyn ScalarField) -> Result<ImplicitMesh, EmptyIsosurfaceReason> {
        let [nx, ny, nz] = self.cells.map(|cells| cells.checked_add(1));
        let [nx, ny, nz] = [
            nx.ok_or(EmptyIsosurfaceReason::GridOverflow)?,
            ny.ok_or(EmptyIsosurfaceReason::GridOverflow)?,
            nz.ok_or(EmptyIsosurfaceReason::GridOverflow)?,
        ];
        let plane_size = nx
            .checked_mul(ny)
            .ok_or(EmptyIsosurfaceReason::GridOverflow)?;
        let total = plane_size
            .checked_mul(nz)
            .ok_or(EmptyIsosurfaceReason::GridOverflow)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(total)
            .map_err(|_| EmptyIsosurfaceReason::GridOverflow)?;
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let value = field.eval(self.node_point([x, y, z]));
                    if !value.is_finite() {
                        return Err(EmptyIsosurfaceReason::NonFiniteSample);
                    }
                    values.push(value);
                }
            }
        }

        let mut builder = MeshBuilder::default();
        for z in 0..self.cells[2] {
            for y in 0..self.cells[1] {
                for x in 0..self.cells[0] {
                    let corners = CORNERS.map(|[dx, dy, dz]| {
                        let coordinate = [x + dx, y + dy, z + dz];
                        let id = coordinate[2] * plane_size + coordinate[1] * nx + coordinate[0];
                        Node {
                            id,
                            coordinate,
                            value: values[id],
                        }
                    });
                    let inside_count = corners.iter().filter(|node| node.value < 0.0).count();
                    if inside_count == 0 || inside_count == 8 {
                        continue;
                    }
                    for tetra in TETRAHEDRA {
                        builder.add_tetra(self, tetra.map(|corner| corners[corner]))?;
                    }
                }
            }
        }

        if builder.mesh.indices.is_empty() {
            return Err(EmptyIsosurfaceReason::NoZeroCrossings);
        }

        let min_spacing = (0..3)
            .map(|axis| self.extents_mm[axis] / self.cells[axis] as f64)
            .fold(f64::INFINITY, f64::min);
        Ok(builder.finish(field, min_spacing))
    }

    fn solid_fraction(&self, field: &dyn ScalarField) -> f64 {
        let [Some(nx), Some(ny), Some(nz)] = self.cells.map(|cells| cells.checked_mul(3)) else {
            return 0.0;
        };
        let Some(total) = nx.checked_mul(ny).and_then(|xy| xy.checked_mul(nz)) else {
            return 0.0;
        };
        let spacing = std::array::from_fn::<_, 3, _>(|axis| {
            self.extents_mm[axis] / [nx, ny, nz][axis] as f64
        });
        let mut inside = 0;
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let point = std::array::from_fn(|axis| {
                        ([x, y, z][axis] as f64 + 0.5).mul_add(spacing[axis], self.min_mm[axis])
                    });
                    let value = field.eval(point);
                    inside += usize::from(value.is_finite() && value < 0.0);
                }
            }
        }
        inside as f64 / total as f64
    }
}

#[derive(Clone, Copy)]
struct Node {
    id: usize,
    coordinate: [usize; 3],
    value: f64,
}

#[derive(Default)]
struct MeshBuilder {
    mesh: ImplicitMesh,
    edge_vertices: HashMap<(usize, usize), u32>,
}

impl MeshBuilder {
    fn add_tetra(&mut self, grid: &Grid, nodes: [Node; 4]) -> Result<(), EmptyIsosurfaceReason> {
        let mut inside = [nodes[0]; 4];
        let mut outside = [nodes[0]; 4];
        let (mut n_in, mut n_out) = (0, 0);
        let mut flip = false;
        for node in nodes {
            if node.value < 0.0 {
                inside[n_in] = node;
                n_in += 1;
                // Moving an inside node ahead of each earlier outside node is one swap.
                flip ^= n_out % 2 == 1;
            } else {
                outside[n_out] = node;
                n_out += 1;
            }
        }

        // These orders point outward for the positive orientation [inside..., outside...].
        // Odd permutations of the original tetrahedron reverse that winding.
        match n_in {
            1 => {
                let a = self.crossing(grid, inside[0], outside[0])?;
                let b = self.crossing(grid, inside[0], outside[1])?;
                let c = self.crossing(grid, inside[0], outside[2])?;
                self.triangle([a, b, c], flip);
            }
            2 => {
                let a = self.crossing(grid, inside[0], outside[0])?;
                let b = self.crossing(grid, inside[0], outside[1])?;
                let c = self.crossing(grid, inside[1], outside[1])?;
                let d = self.crossing(grid, inside[1], outside[0])?;
                self.triangle([a, b, c], flip);
                self.triangle([a, c, d], flip);
            }
            3 => {
                let a = self.crossing(grid, inside[0], outside[0])?;
                let b = self.crossing(grid, inside[1], outside[0])?;
                let c = self.crossing(grid, inside[2], outside[0])?;
                self.triangle([a, b, c], flip);
            }
            _ => {}
        }
        Ok(())
    }

    fn crossing(
        &mut self,
        grid: &Grid,
        inside: Node,
        outside: Node,
    ) -> Result<u32, EmptyIsosurfaceReason> {
        // Edges always travel from the negative endpoint to the nonnegative one.
        let key = (inside.id, outside.id);
        match self.edge_vertices.entry(key) {
            Entry::Occupied(entry) => Ok(*entry.get()),
            Entry::Vacant(entry) => {
                let id = u32::try_from(self.mesh.vertices.len())
                    .map_err(|_| EmptyIsosurfaceReason::IndexOverflow)?;
                let a = grid.node_point(inside.coordinate);
                let b = grid.node_point(outside.coordinate);
                let scale = (-inside.value).max(outside.value);
                let left = -inside.value / scale;
                let right = outside.value / scale;
                // Keep crossings distinct at zero-valued nodes and under rounded interpolation.
                // Contracting the edge by a small fraction preserves the indexed topology.
                const EDGE_INSET: f64 = 1e-7;
                let t = (left / (left + right)).clamp(EDGE_INSET, 1.0 - EDGE_INSET);
                let point = std::array::from_fn(|axis| {
                    if t <= 0.5 {
                        t.mul_add(b[axis] - a[axis], a[axis])
                    } else {
                        (1.0 - t).mul_add(a[axis] - b[axis], b[axis])
                    }
                });
                self.mesh.vertices.push(point);
                entry.insert(id);
                Ok(id)
            }
        }
    }

    fn triangle(&mut self, mut ids: [u32; 3], flip: bool) {
        if flip {
            ids.swap(1, 2);
        }
        self.mesh.indices.extend_from_slice(&ids);
    }

    fn finish(mut self, field: &dyn ScalarField, min_spacing: f64) -> ImplicitMesh {
        let mut geometric = vec![[0.0; 3]; self.mesh.vertices.len()];
        for triangle in self.mesh.indices.as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|id| self.mesh.vertices[id as usize]);
            let face = cross(subtract(b, a), subtract(c, a));
            for &id in triangle {
                let sum = &mut geometric[id as usize];
                for axis in 0..3 {
                    sum[axis] += face[axis];
                }
            }
        }

        self.mesh.normals = self
            .mesh
            .vertices
            .iter()
            .zip(geometric)
            .map(|(&point, sum)| {
                let coordinate_scale = point.map(f64::abs).into_iter().fold(0.0, f64::max);
                let eps = (min_spacing * 1e-3)
                    .max(coordinate_scale * (4.0 * f64::EPSILON))
                    .max(f64::MIN_POSITIVE);
                let sampled = normal(field, point, eps);
                match unit_vector(sum) {
                    Some(outward) if dot(sampled, outward) <= 0.0 => outward,
                    _ if sampled != [0.0; 3] => sampled,
                    _ => [0.0, 0.0, 1.0],
                }
            })
            .collect();
        self.mesh
    }
}

fn subtract(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| a[axis] - b[axis])
}

fn cross([a, b, c]: [f64; 3], [x, y, z]: [f64; 3]) -> [f64; 3] {
    [b * z - c * y, c * x - a * z, a * y - b * x]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

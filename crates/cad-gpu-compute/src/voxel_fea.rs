//! Fast 3D matrix-free linear-elastic voxel FEA static solver (H2-E07).
//!
//! Voxelizes closed triangle meshes via ROI-rebased LBVH ray-crossing parity,
//! computes the exact 8-node hexahedral (H8) isotropic linear-elastic element
//! stiffness matrix via `2x2x2` Gauss-Legendre quadrature, solves `K u = f`
//! with matrix-free Jacobi-preconditioned Conjugate Gradient (PCG), and exports
//! per-voxel Von Mises stress fields alongside deterministic `Pass`/`Fail`/`Unknown`
//! engineering verdicts.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::lbvh::{LbvhTree, TriRef, cross, dot, sub};
use crate::roi::RoiAnchor;

/// Local corner signs `(xi, eta, zeta)` for the 8-node hexahedral (H8) element.
const HEX_NODE_SIGNS: [(f64, f64, f64); 8] = [
    (-1.0, -1.0, -1.0),
    (1.0, -1.0, -1.0),
    (1.0, 1.0, -1.0),
    (-1.0, 1.0, -1.0),
    (-1.0, -1.0, 1.0),
    (1.0, -1.0, 1.0),
    (1.0, 1.0, 1.0),
    (-1.0, 1.0, 1.0),
];

/// Grid corner offsets `(dx, dy, dz)` matching `HEX_NODE_SIGNS`.
const HEX_NODE_OFFSETS: [(usize, usize, usize); 8] = [
    (0, 0, 0),
    (1, 0, 0),
    (1, 1, 0),
    (0, 1, 0),
    (0, 0, 1),
    (1, 0, 1),
    (1, 1, 1),
    (0, 1, 1),
];

/// Regular 3D cubic occupancy grid in world millimeters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoxelGrid {
    pub origin_mm: [f64; 3],
    pub voxel_size_mm: f64,
    pub dims: [usize; 3],
    pub active: Vec<bool>,
}

impl VoxelGrid {
    /// Creates a solid axis-aligned box grid where every voxel is active.
    ///
    /// Returns `None` if `voxel_size_mm` is not finite/positive, `origin_mm` is
    /// non-finite, or any dimension is zero.
    #[must_use]
    pub fn solid_box(origin_mm: [f64; 3], voxel_size_mm: f64, dims: [usize; 3]) -> Option<Self> {
        let total = Self::validated_total_voxels(origin_mm, voxel_size_mm, dims)?;
        Some(Self {
            origin_mm,
            voxel_size_mm,
            dims,
            active: vec![true; total],
        })
    }

    /// Creates a custom voxel grid from an explicit occupancy mask ordered by
    /// `ix + dims[0] * (iy + dims[1] * iz)`.
    #[must_use]
    pub fn from_mask(
        origin_mm: [f64; 3],
        voxel_size_mm: f64,
        dims: [usize; 3],
        active: Vec<bool>,
    ) -> Option<Self> {
        let total = Self::validated_total_voxels(origin_mm, voxel_size_mm, dims)?;
        if active.len() != total {
            return None;
        }
        Some(Self {
            origin_mm,
            voxel_size_mm,
            dims,
            active,
        })
    }

    fn validated_total_voxels(
        origin_mm: [f64; 3],
        voxel_size_mm: f64,
        dims: [usize; 3],
    ) -> Option<usize> {
        if !origin_mm.into_iter().all(f64::is_finite)
            || !voxel_size_mm.is_finite()
            || voxel_size_mm <= 0.0
            || dims.contains(&0)
        {
            return None;
        }
        dims[0].checked_mul(dims[1])?.checked_mul(dims[2])
    }

    /// Linear index for voxel `(ix, iy, iz)` in x-fastest order.
    #[must_use]
    pub fn voxel_index(&self, ix: usize, iy: usize, iz: usize) -> usize {
        ix + self.dims[0] * (iy + self.dims[1] * iz)
    }

    /// Returns whether voxel `(ix, iy, iz)` is active.
    #[must_use]
    pub fn is_active(&self, ix: usize, iy: usize, iz: usize) -> bool {
        if ix >= self.dims[0] || iy >= self.dims[1] || iz >= self.dims[2] {
            return false;
        }
        self.active
            .get(self.voxel_index(ix, iy, iz))
            .copied()
            .unwrap_or(false)
    }

    /// Number of active solid voxels in the grid.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.iter().filter(|&&v| v).count()
    }

    /// World-space centroid of voxel `(ix, iy, iz)` in millimeters.
    #[must_use]
    pub fn voxel_center_mm(&self, ix: usize, iy: usize, iz: usize) -> [f64; 3] {
        [
            self.origin_mm[0] + (ix as f64 + 0.5) * self.voxel_size_mm,
            self.origin_mm[1] + (iy as f64 + 0.5) * self.voxel_size_mm,
            self.origin_mm[2] + (iz as f64 + 0.5) * self.voxel_size_mm,
        ]
    }

    /// Node grid dimensions `[dims[0] + 1, dims[1] + 1, dims[2] + 1]`.
    #[must_use]
    pub fn node_dims(&self) -> [usize; 3] {
        [self.dims[0] + 1, self.dims[1] + 1, self.dims[2] + 1]
    }

    /// World-space coordinates of corner node `(nx, ny, nz)` in millimeters.
    #[must_use]
    pub fn node_coords_mm(&self, nx: usize, ny: usize, nz: usize) -> [f64; 3] {
        [
            self.origin_mm[0] + nx as f64 * self.voxel_size_mm,
            self.origin_mm[1] + ny as f64 * self.voxel_size_mm,
            self.origin_mm[2] + nz as f64 * self.voxel_size_mm,
        ]
    }
}

/// Isotropic linear-elastic material properties in millimeter-Newton-MPa units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeaMaterial {
    /// Young's modulus `E` in MPa (`N/mm^2`).
    pub youngs_modulus_mpa: f64,
    /// Poisson's ratio `nu` in `[0.0, 0.49]`.
    pub poissons_ratio: f64,
    /// Tensile/Von Mises yield strength in MPa.
    pub yield_strength_mpa: f64,
}

impl FeaMaterial {
    /// Aluminum 6061-T6 preset (`E = 68,900 MPa`, `nu = 0.33`, `Sy = 276 MPa`).
    #[must_use]
    pub const fn al_6061_t6() -> Self {
        Self {
            youngs_modulus_mpa: 68_900.0,
            poissons_ratio: 0.33,
            yield_strength_mpa: 276.0,
        }
    }

    /// AISI 1045 / GB 45# carbon steel preset (`E = 210,000 MPa`, `nu = 0.30`, `Sy = 355 MPa`).
    #[must_use]
    pub const fn steel_45() -> Self {
        Self {
            youngs_modulus_mpa: 210_000.0,
            poissons_ratio: 0.30,
            yield_strength_mpa: 355.0,
        }
    }

    /// Titanium Ti-6Al-4V Grade 5 preset (`E = 113,800 MPa`, `nu = 0.342`, `Sy = 880 MPa`).
    #[must_use]
    pub const fn ti_6al_4v() -> Self {
        Self {
            youngs_modulus_mpa: 113_800.0,
            poissons_ratio: 0.342,
            yield_strength_mpa: 880.0,
        }
    }

    /// Engineering ABS polymer preset (`E = 2,300 MPa`, `nu = 0.35`, `Sy = 40 MPa`).
    #[must_use]
    pub const fn abs_plastic() -> Self {
        Self {
            youngs_modulus_mpa: 2_300.0,
            poissons_ratio: 0.35,
            yield_strength_mpa: 40.0,
        }
    }

    /// Returns `true` when all material parameters are finite and physically valid.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.youngs_modulus_mpa.is_finite()
            && self.youngs_modulus_mpa > 0.0
            && self.poissons_ratio.is_finite()
            && (0.0..=0.49).contains(&self.poissons_ratio)
            && self.yield_strength_mpa.is_finite()
            && self.yield_strength_mpa > 0.0
    }
}

/// Spatial selector for FEA boundary conditions and loads in world millimeters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeaRegion {
    /// Axis-aligned bounding box (inclusive, with tiny floating-point tolerance).
    BoxRegion {
        min_mm: [f64; 3],
        max_mm: [f64; 3],
    },
    /// Planar band `|dot(unit(normal), p) - offset_mm| <= tol_mm`.
    FacePlane {
        normal: [f64; 3],
        offset_mm: f64,
        tol_mm: f64,
    },
    /// Spherical selection `||p - center_mm|| <= radius_mm`.
    AllNodesWithin {
        center_mm: [f64; 3],
        radius_mm: f64,
    },
}

impl FeaRegion {
    /// Tests whether a world-space node coordinate lies inside this region.
    #[must_use]
    pub fn contains_point(&self, p: [f64; 3]) -> bool {
        if !p.into_iter().all(f64::is_finite) {
            return false;
        }
        const EPS: f64 = 1.0e-9;
        match *self {
            Self::BoxRegion { min_mm, max_mm } => {
                if !min_mm.into_iter().chain(max_mm).all(f64::is_finite) {
                    return false;
                }
                (0..3).all(|axis| {
                    let lo = min_mm[axis].min(max_mm[axis]) - EPS;
                    let hi = min_mm[axis].max(max_mm[axis]) + EPS;
                    p[axis] >= lo && p[axis] <= hi
                })
            }
            Self::FacePlane {
                normal,
                offset_mm,
                tol_mm,
            } => {
                if !normal.into_iter().all(f64::is_finite)
                    || !offset_mm.is_finite()
                    || !tol_mm.is_finite()
                    || tol_mm < 0.0
                {
                    return false;
                }
                let norm = dot(normal, normal).sqrt();
                if norm <= 1.0e-15 {
                    return false;
                }
                let unit = [normal[0] / norm, normal[1] / norm, normal[2] / norm];
                (dot(unit, p) - offset_mm).abs() <= tol_mm + EPS
            }
            Self::AllNodesWithin {
                center_mm,
                radius_mm,
            } => {
                if !center_mm.into_iter().all(f64::is_finite)
                    || !radius_mm.is_finite()
                    || radius_mm < 0.0
                {
                    return false;
                }
                let d = sub(p, center_mm);
                let r = radius_mm + EPS;
                dot(d, d) <= r * r
            }
        }
    }
}

/// Dirichlet zero-displacement constraint applied to active nodes in `region`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixedConstraint {
    pub region: FeaRegion,
    pub fix_x: bool,
    pub fix_y: bool,
    pub fix_z: bool,
}

impl FixedConstraint {
    /// Clamps all three translational DOFs `(x, y, z)` on every node in `region`.
    #[must_use]
    pub fn all(region: FeaRegion) -> Self {
        Self {
            region,
            fix_x: true,
            fix_y: true,
            fix_z: true,
        }
    }
}

/// Total static force load (in Newtons) uniformly distributed across active nodes in `region`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForceLoad {
    pub region: FeaRegion,
    pub total_force_n: [f64; 3],
}

/// Configuration for a linear-elastic voxel FEA static solve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoxelFeaConfig {
    pub material: FeaMaterial,
    pub constraints: Vec<FixedConstraint>,
    pub loads: Vec<ForceLoad>,
    /// Minimum required safety factor (`yield_strength_mpa / max_von_mises_mpa`) for `Pass`.
    pub min_safety_factor: f64,
    /// Relative residual convergence tolerance for PCG (default `1e-6`).
    pub tolerance: f64,
    /// Maximum PCG iterations (default `2000`).
    pub max_iters: usize,
}

impl VoxelFeaConfig {
    /// Creates a solver configuration with default engineering gate parameters
    /// (`min_safety_factor = 1.5`, `tolerance = 1e-6`, `max_iters = 2000`).
    #[must_use]
    pub fn new(
        material: FeaMaterial,
        constraints: Vec<FixedConstraint>,
        loads: Vec<ForceLoad>,
    ) -> Self {
        Self {
            material,
            constraints,
            loads,
            min_safety_factor: 1.5,
            tolerance: 1.0e-6,
            max_iters: 2000,
        }
    }

    /// Sets the minimum required safety factor for `FeaGateVerdict::Pass`.
    #[must_use]
    pub fn with_min_safety_factor(mut self, min_safety_factor: f64) -> Self {
        self.min_safety_factor = min_safety_factor;
        self
    }

    /// Sets the PCG relative residual convergence tolerance.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: f64) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Sets the maximum number of PCG iterations.
    #[must_use]
    pub fn with_max_iters(mut self, max_iters: usize) -> Self {
        self.max_iters = max_iters;
        self
    }
}

/// Deterministic three-valued L3 structural gate verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FeaGateVerdict {
    Pass,
    Fail,
    Unknown,
}

/// Complete output of a voxel FEA static solve, including grid-ordered Von Mises stress field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoxelFeaReport {
    pub verdict: FeaGateVerdict,
    pub reason: String,
    pub converged: bool,
    pub iterations: usize,
    pub relative_residual: f64,
    pub active_voxel_count: usize,
    pub active_dof_count: usize,
    pub max_displacement_mm: f64,
    pub max_von_mises_mpa: f64,
    pub min_safety_factor: f64,
    /// Grid-ordered per-voxel Von Mises stress in MPa (`0.0` for inactive voxels).
    pub voxel_von_mises_mpa: Vec<f32>,
    /// World-space centroid of the voxel with peak Von Mises stress, if any.
    pub peak_stress_point_mm: Option<[f64; 3]>,
}

impl VoxelFeaReport {
    fn unknown(
        reason: impl Into<String>,
        active_voxel_count: usize,
        active_dof_count: usize,
        grid_voxel_count: usize,
    ) -> Self {
        Self {
            verdict: FeaGateVerdict::Unknown,
            reason: reason.into(),
            converged: false,
            iterations: 0,
            relative_residual: f64::NAN,
            active_voxel_count,
            active_dof_count,
            max_displacement_mm: 0.0,
            max_von_mises_mpa: 0.0,
            min_safety_factor: 0.0,
            voxel_von_mises_mpa: vec![0.0_f32; grid_voxel_count],
            peak_stress_point_mm: None,
        }
    }
}

/// Voxelizes a closed triangle mesh into a regular 3D occupancy grid with the given
/// cubic `voxel_size_mm`, using ROI-rebased LBVH scanline ray-crossing parity with
/// 3-ray irrational offset majority voting to avoid shared-edge/vertex degeneracies.
#[must_use]
pub fn voxelize_mesh(triangles: &[TriRef], voxel_size_mm: f64) -> Option<VoxelGrid> {
    if triangles.is_empty() || !voxel_size_mm.is_finite() || voxel_size_mm <= 0.0 {
        return None;
    }

    let anchor = RoiAnchor::from_triangles(triangles);
    let local_tris = anchor.rebase_triangles(triangles);
    let tree = LbvhTree::build(&local_tris);
    let local_bounds = tree.scene_bounds()?;

    let mut dims = [0usize; 3];
    let mut local_origin = [0.0f64; 3];
    for axis in 0..3 {
        let extent = local_bounds.max[axis] - local_bounds.min[axis];
        if !extent.is_finite() || extent <= 0.0 {
            return None;
        }
        let count = ((extent / voxel_size_mm) - 1.0e-12).ceil().max(1.0) as usize;
        dims[axis] = count;
        let grid_span = count as f64 * voxel_size_mm;
        local_origin[axis] = local_bounds.min[axis] - 0.5 * (grid_span - extent);
    }

    let total = dims[0].checked_mul(dims[1])?.checked_mul(dims[2])?;
    let mut active = vec![false; total];

    // Three tiny irrational-ratio (dy, dz) perturbations avoid grazing axis-aligned
    // triangle edges or vertices while remaining well inside each voxel cell.
    const PERTURBATIONS: [(f64, f64); 3] = [
        (1.137_219e-6, 2.917_311e-6),
        (-2.319_471e-6, 1.471_828e-6),
        (1.732_051e-6, -2.183_417e-6),
    ];

    let ray_start_x = local_bounds.min[0] - voxel_size_mm.max(1.0);
    let ray_dir = [1.0, 0.0, 0.0];
    let dedup_eps = 1.0e-9 * voxel_size_mm.max(1.0);

    let mut crossings = [Vec::<f64>::new(), Vec::<f64>::new(), Vec::<f64>::new()];

    for iz in 0..dims[2] {
        let cz = local_origin[2] + (iz as f64 + 0.5) * voxel_size_mm;
        for iy in 0..dims[1] {
            let cy = local_origin[1] + (iy as f64 + 0.5) * voxel_size_mm;

            for (slot, &(dy_rel, dz_rel)) in PERTURBATIONS.iter().enumerate() {
                crossings[slot].clear();
                let origin = [
                    ray_start_x,
                    cy + dy_rel * voxel_size_mm,
                    cz + dz_rel * voxel_size_mm,
                ];
                let hits = tree.cast_ray_all_hits(origin, ray_dir);
                let mut last_dist = f64::NEG_INFINITY;
                let mut last_sign = 0i32;

                for hit in hits {
                    let tri = &local_tris[hit.triangle_index];
                    let normal = cross(sub(tri.b, tri.a), sub(tri.c, tri.a));
                    let alignment = dot(normal, ray_dir);
                    if alignment == 0.0 {
                        continue;
                    }
                    let sign = if alignment > 0.0 { 1 } else { -1 };
                    if (hit.distance_mm - last_dist).abs() <= dedup_eps && sign == last_sign {
                        continue;
                    }
                    crossings[slot].push(ray_start_x + hit.distance_mm);
                    last_dist = hit.distance_mm;
                    last_sign = sign;
                }
            }

            let mut cursors = [0usize; 3];
            for ix in 0..dims[0] {
                let cx = local_origin[0] + (ix as f64 + 0.5) * voxel_size_mm;
                let mut inside_votes = 0usize;
                for slot in 0..3 {
                    let list = &crossings[slot];
                    while cursors[slot] < list.len() && list[cursors[slot]] < cx {
                        cursors[slot] += 1;
                    }
                    if cursors[slot] % 2 == 1 {
                        inside_votes += 1;
                    }
                }
                if inside_votes >= 2 {
                    let idx = ix + dims[0] * (iy + dims[1] * iz);
                    active[idx] = true;
                }
            }
        }
    }

    Some(VoxelGrid {
        origin_mm: anchor.to_world_f64(local_origin),
        voxel_size_mm,
        dims,
        active,
    })
}

/// Voxelizes a closed triangle mesh by choosing `voxel_size_mm` so the longest
/// bounding-box axis spans `max_voxels_per_axis` voxels.
#[must_use]
pub fn voxelize_mesh_with_max_voxels(
    triangles: &[TriRef],
    max_voxels_per_axis: usize,
) -> Option<VoxelGrid> {
    if triangles.is_empty() || max_voxels_per_axis == 0 {
        return None;
    }
    let anchor = RoiAnchor::from_triangles(triangles);
    let local_tris = anchor.rebase_triangles(triangles);
    let tree = LbvhTree::build(&local_tris);
    let bounds = tree.scene_bounds()?;
    let max_extent = (0..3)
        .map(|axis| bounds.max[axis] - bounds.min[axis])
        .fold(0.0_f64, f64::max);
    if !max_extent.is_finite() || max_extent <= 0.0 {
        return None;
    }
    let voxel_size_mm = max_extent / (max_voxels_per_axis as f64);
    voxelize_mesh(triangles, voxel_size_mm)
}

/// Voxelizes `triangles` at `voxel_size_mm` and runs the linear-elastic static FEA solver.
#[must_use]
pub fn solve_mesh_voxel_fea(
    triangles: &[TriRef],
    voxel_size_mm: f64,
    config: &VoxelFeaConfig,
) -> VoxelFeaReport {
    let Some(grid) = voxelize_mesh(triangles, voxel_size_mm) else {
        return VoxelFeaReport::unknown(
            "failed to voxelize input mesh (empty, degenerate, or invalid voxel_size_mm)",
            0,
            0,
            0,
        );
    };
    solve_voxel_fea(&grid, config)
}

/// Voxelizes `triangles` with `max_voxels_per_axis` and runs the linear-elastic static FEA solver.
#[must_use]
pub fn solve_mesh_voxel_fea_auto(
    triangles: &[TriRef],
    max_voxels_per_axis: usize,
    config: &VoxelFeaConfig,
) -> VoxelFeaReport {
    let Some(grid) = voxelize_mesh_with_max_voxels(triangles, max_voxels_per_axis) else {
        return VoxelFeaReport::unknown(
            "failed to voxelize input mesh with max_voxels_per_axis",
            0,
            0,
            0,
        );
    };
    solve_voxel_fea(&grid, config)
}

/// Runs the matrix-free 3D H8 linear-elastic static FEA solver on `grid`.
#[must_use]
pub fn solve_voxel_fea(grid: &VoxelGrid, config: &VoxelFeaConfig) -> VoxelFeaReport {
    let grid_voxel_count = grid.active.len();
    let expected_len = grid
        .dims[0]
        .checked_mul(grid.dims[1])
        .and_then(|v| v.checked_mul(grid.dims[2]));
    if expected_len != Some(grid_voxel_count)
        || !grid.origin_mm.into_iter().all(f64::is_finite)
        || !grid.voxel_size_mm.is_finite()
        || grid.voxel_size_mm <= 0.0
    {
        return VoxelFeaReport::unknown("invalid VoxelGrid dimensions or voxel size", 0, 0, 0);
    }

    if !config.material.is_valid() {
        return VoxelFeaReport::unknown(
            "invalid material parameters (requires finite E > 0, 0 <= nu <= 0.49, yield > 0)",
            grid.active_count(),
            0,
            grid_voxel_count,
        );
    }
    if !config.min_safety_factor.is_finite() || config.min_safety_factor <= 0.0 {
        return VoxelFeaReport::unknown(
            "invalid min_safety_factor (must be finite and > 0)",
            grid.active_count(),
            0,
            grid_voxel_count,
        );
    }
    if !config.tolerance.is_finite() || config.tolerance <= 0.0 || config.max_iters == 0 {
        return VoxelFeaReport::unknown(
            "invalid solver tolerance or max_iters",
            grid.active_count(),
            0,
            grid_voxel_count,
        );
    }
    if config.constraints.is_empty() {
        return VoxelFeaReport::unknown(
            "no fixed boundary constraints supplied (unconstrained rigid body)",
            grid.active_count(),
            0,
            grid_voxel_count,
        );
    }
    if config.loads.is_empty() {
        return VoxelFeaReport::unknown(
            "no force loads supplied",
            grid.active_count(),
            0,
            grid_voxel_count,
        );
    }

    // 1. Build compact active node and element connectivity tables.
    let node_dims = grid.node_dims();
    let Some(total_grid_nodes) = node_dims[0]
        .checked_mul(node_dims[1])
        .and_then(|v| v.checked_mul(node_dims[2]))
    else {
        return VoxelFeaReport::unknown(
            "voxel grid node count overflow",
            grid.active_count(),
            0,
            grid_voxel_count,
        );
    };

    let mut grid_to_active_node = vec![usize::MAX; total_grid_nodes];
    let mut node_coords: Vec<[f64; 3]> = Vec::new();
    let mut elem_nodes: Vec<[usize; 8]> = Vec::new();
    let mut elem_grid_indices: Vec<usize> = Vec::new();
    let mut elem_centers: Vec<[f64; 3]> = Vec::new();

    for iz in 0..grid.dims[2] {
        for iy in 0..grid.dims[1] {
            for ix in 0..grid.dims[0] {
                let v_idx = grid.voxel_index(ix, iy, iz);
                if !grid.active[v_idx] {
                    continue;
                }
                let mut corners = [0usize; 8];
                for (local_n, &(dx, dy, dz)) in HEX_NODE_OFFSETS.iter().enumerate() {
                    let nx = ix + dx;
                    let ny = iy + dy;
                    let nz = iz + dz;
                    let g_node = nx + node_dims[0] * (ny + node_dims[1] * nz);
                    let a_node = if grid_to_active_node[g_node] == usize::MAX {
                        let id = node_coords.len();
                        grid_to_active_node[g_node] = id;
                        node_coords.push(grid.node_coords_mm(nx, ny, nz));
                        id
                    } else {
                        grid_to_active_node[g_node]
                    };
                    corners[local_n] = a_node;
                }
                elem_nodes.push(corners);
                elem_grid_indices.push(v_idx);
                elem_centers.push(grid.voxel_center_mm(ix, iy, iz));
            }
        }
    }

    let active_voxel_count = elem_nodes.len();
    let active_node_count = node_coords.len();
    let active_dof_count = active_node_count * 3;
    if active_voxel_count == 0 || active_dof_count == 0 {
        return VoxelFeaReport::unknown(
            "voxel grid contains zero active solid voxels",
            0,
            0,
            grid_voxel_count,
        );
    }

    // 2. Apply Dirichlet fixed constraints.
    let mut fixed = vec![false; active_dof_count];
    let mut fixed_dof_count = 0usize;
    for constraint in &config.constraints {
        if !constraint.fix_x && !constraint.fix_y && !constraint.fix_z {
            continue;
        }
        for (node_id, &coord) in node_coords.iter().enumerate() {
            if constraint.region.contains_point(coord) {
                let flags = [constraint.fix_x, constraint.fix_y, constraint.fix_z];
                for (axis, &is_fixed) in flags.iter().enumerate() {
                    if is_fixed && !fixed[3 * node_id + axis] {
                        fixed[3 * node_id + axis] = true;
                        fixed_dof_count += 1;
                    }
                }
            }
        }
    }

    if fixed_dof_count < 6 {
        return VoxelFeaReport::unknown(
            format!(
                "under-constrained model: only {fixed_dof_count} fixed DOFs (need >= 6 to suppress 3D rigid-body modes)"
            ),
            active_voxel_count,
            active_dof_count,
            grid_voxel_count,
        );
    }

    // 3. Assemble nodal force vector using consistent tributary-weight distribution
    //    (each active node's weight equals its count of incident active voxels, which
    //    yields the exact 1:2:4 H8 consistent nodal traction weights on boundary faces
    //    and eliminates artificial corner-node stress/displacement spikes).
    let mut node_incident_voxels = vec![0usize; active_node_count];
    for corners in &elem_nodes {
        for &node_id in corners {
            node_incident_voxels[node_id] += 1;
        }
    }

    let mut rhs = vec![0.0_f64; active_dof_count];
    let mut matched_load_nodes = 0usize;
    let mut total_force_sq = 0.0_f64;
    for load in &config.loads {
        if !load.total_force_n.into_iter().all(f64::is_finite) {
            return VoxelFeaReport::unknown(
                "non-finite force load component",
                active_voxel_count,
                active_dof_count,
                grid_voxel_count,
            );
        }
        let mag_sq = dot(load.total_force_n, load.total_force_n);
        if mag_sq == 0.0 {
            continue;
        }
        total_force_sq += mag_sq;

        let region_nodes: Vec<usize> = node_coords
            .iter()
            .enumerate()
            .filter_map(|(node_id, &coord)| load.region.contains_point(coord).then_some(node_id))
            .collect();
        if region_nodes.is_empty() {
            return VoxelFeaReport::unknown(
                "force load region matched zero active nodes",
                active_voxel_count,
                active_dof_count,
                grid_voxel_count,
            );
        }
        matched_load_nodes += region_nodes.len();
        let total_weight: f64 = region_nodes
            .iter()
            .map(|&nid| node_incident_voxels[nid] as f64)
            .sum();
        let inv_w = 1.0 / total_weight;
        for node_id in region_nodes {
            let w = (node_incident_voxels[node_id] as f64) * inv_w;
            rhs[3 * node_id] += load.total_force_n[0] * w;
            rhs[3 * node_id + 1] += load.total_force_n[1] * w;
            rhs[3 * node_id + 2] += load.total_force_n[2] * w;
        }
    }

    if total_force_sq == 0.0 || matched_load_nodes == 0 {
        return VoxelFeaReport::unknown(
            "zero net force load applied to active nodes",
            active_voxel_count,
            active_dof_count,
            grid_voxel_count,
        );
    }

    // Enforce zero RHS on fixed DOFs.
    for (dof, &is_fixed) in fixed.iter().enumerate() {
        if is_fixed {
            rhs[dof] = 0.0;
        }
    }
    let rhs_norm = l2_norm(&rhs);
    if !rhs_norm.is_finite() || rhs_norm <= 1.0e-15 {
        return VoxelFeaReport::unknown(
            "all loaded DOFs are fixed or zero; no deformable load path exists",
            active_voxel_count,
            active_dof_count,
            grid_voxel_count,
        );
    }

    // 4. Verify every loaded connected component suppresses all 6 rigid-body modes.
    if let Err(msg) =
        verify_rigid_body_stability(active_node_count, &node_coords, &elem_nodes, &fixed, &rhs)
    {
        return VoxelFeaReport::unknown(msg, active_voxel_count, active_dof_count, grid_voxel_count);
    }

    // 5. Compute exact 24x24 H8 element stiffness matrix once (shared by all cubic voxels).
    let d_mat = isotropic_elasticity_matrix(&config.material);
    let ke = compute_hex8_stiffness_matrix(grid.voxel_size_mm, &d_mat);

    // 6. Assemble Jacobi diagonal preconditioner.
    let mut diag = vec![0.0_f64; active_dof_count];
    for corners in &elem_nodes {
        for (local_n, &node_id) in corners.iter().enumerate() {
            let base_local = 3 * local_n;
            let base_global = 3 * node_id;
            diag[base_global] += ke[base_local][base_local];
            diag[base_global + 1] += ke[base_local + 1][base_local + 1];
            diag[base_global + 2] += ke[base_local + 2][base_local + 2];
        }
    }
    let mut inv_diag = vec![0.0_f64; active_dof_count];
    for dof in 0..active_dof_count {
        if !fixed[dof] && diag[dof] > 1.0e-30 {
            inv_diag[dof] = 1.0 / diag[dof];
        }
    }

    // 7. Matrix-free Jacobi-PCG solve for K u = rhs.
    let mut u = vec![0.0_f64; active_dof_count];
    let mut r = rhs;
    let mut z = vec![0.0_f64; active_dof_count];
    let mut p = vec![0.0_f64; active_dof_count];
    let mut ap = vec![0.0_f64; active_dof_count];

    let mut rz = 0.0_f64;
    for i in 0..active_dof_count {
        z[i] = r[i] * inv_diag[i];
        p[i] = z[i];
        rz += r[i] * z[i];
    }

    let mut iterations = 0usize;
    let mut relative_residual = l2_norm(&r) / rhs_norm;
    let mut converged = relative_residual <= config.tolerance;

    while !converged && iterations < config.max_iters {
        iterations += 1;
        apply_matrix_free_stiffness(&elem_nodes, &ke, &fixed, &p, &mut ap);
        let pap = dot_vec(&p, &ap);
        if !pap.is_finite() || pap <= 1.0e-30 {
            return VoxelFeaReport {
                verdict: FeaGateVerdict::Unknown,
                reason: "stiffness matrix is singular or indefinite during PCG (check constraints)"
                    .to_string(),
                converged: false,
                iterations,
                relative_residual,
                active_voxel_count,
                active_dof_count,
                max_displacement_mm: 0.0,
                max_von_mises_mpa: 0.0,
                min_safety_factor: 0.0,
                voxel_von_mises_mpa: vec![0.0_f32; grid_voxel_count],
                peak_stress_point_mm: None,
            };
        }

        let alpha = rz / pap;
        if !alpha.is_finite() {
            return VoxelFeaReport::unknown(
                "non-finite PCG step length",
                active_voxel_count,
                active_dof_count,
                grid_voxel_count,
            );
        }

        let mut r_norm_sq = 0.0_f64;
        let mut rz_new = 0.0_f64;
        for i in 0..active_dof_count {
            u[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
            r_norm_sq += r[i] * r[i];
            z[i] = r[i] * inv_diag[i];
            rz_new += r[i] * z[i];
        }

        relative_residual = r_norm_sq.sqrt() / rhs_norm;
        if !relative_residual.is_finite() {
            return VoxelFeaReport::unknown(
                "non-finite residual encountered during PCG",
                active_voxel_count,
                active_dof_count,
                grid_voxel_count,
            );
        }
        if relative_residual <= config.tolerance {
            converged = true;
            break;
        }

        let beta = if rz.abs() > 1.0e-30 { rz_new / rz } else { 0.0 };
        for i in 0..active_dof_count {
            p[i] = z[i] + beta * p[i];
        }
        rz = rz_new;
    }

    if !converged {
        return VoxelFeaReport {
            verdict: FeaGateVerdict::Unknown,
            reason: format!(
                "PCG did not converge within {iterations} iterations (relative_residual = {relative_residual:.3e} > tol {:.3e})",
                config.tolerance
            ),
            converged: false,
            iterations,
            relative_residual,
            active_voxel_count,
            active_dof_count,
            max_displacement_mm: 0.0,
            max_von_mises_mpa: 0.0,
            min_safety_factor: 0.0,
            voxel_von_mises_mpa: vec![0.0_f32; grid_voxel_count],
            peak_stress_point_mm: None,
        };
    }

    // 8. Post-process displacements and centroid Cauchy / Von Mises stresses.
    let mut max_displacement_mm = 0.0_f64;
    for node_id in 0..active_node_count {
        let ux = u[3 * node_id];
        let uy = u[3 * node_id + 1];
        let uz = u[3 * node_id + 2];
        let mag = (ux * ux + uy * uy + uz * uz).sqrt();
        if mag > max_displacement_mm {
            max_displacement_mm = mag;
        }
    }

    let b_center = compute_hex8_b_matrix(grid.voxel_size_mm, 0.0, 0.0, 0.0);
    let mut voxel_von_mises_mpa = vec![0.0_f32; grid_voxel_count];
    let mut max_von_mises_mpa = 0.0_f64;
    let mut peak_stress_point_mm: Option<[f64; 3]> = None;

    for (elem_idx, corners) in elem_nodes.iter().enumerate() {
        let mut u_e = [0.0_f64; 24];
        for (local_n, &node_id) in corners.iter().enumerate() {
            u_e[3 * local_n] = u[3 * node_id];
            u_e[3 * local_n + 1] = u[3 * node_id + 1];
            u_e[3 * local_n + 2] = u[3 * node_id + 2];
        }

        // Strain eps = B(0,0,0) * u_e
        let mut strain = [0.0_f64; 6];
        for row in 0..6 {
            let mut sum = 0.0_f64;
            for col in 0..24 {
                sum += b_center[row][col] * u_e[col];
            }
            strain[row] = sum;
        }

        // Cauchy stress sigma = D * strain
        let mut stress = [0.0_f64; 6];
        for row in 0..6 {
            let mut sum = 0.0_f64;
            for col in 0..6 {
                sum += d_mat[row][col] * strain[col];
            }
            stress[row] = sum;
        }

        let vm = von_mises_stress(&stress);
        voxel_von_mises_mpa[elem_grid_indices[elem_idx]] = vm as f32;
        if vm > max_von_mises_mpa {
            max_von_mises_mpa = vm;
            peak_stress_point_mm = Some(elem_centers[elem_idx]);
        }
    }

    let min_safety_factor = if max_von_mises_mpa > 1.0e-12 {
        config.material.yield_strength_mpa / max_von_mises_mpa
    } else {
        f64::INFINITY
    };

    let (verdict, reason) = if min_safety_factor >= config.min_safety_factor {
        (
            FeaGateVerdict::Pass,
            format!(
                "structural check passed: max Von Mises {max_von_mises_mpa:.2} MPa, safety factor {min_safety_factor:.2} >= {:.2}",
                config.min_safety_factor
            ),
        )
    } else {
        (
            FeaGateVerdict::Fail,
            format!(
                "structural check failed: max Von Mises {max_von_mises_mpa:.2} MPa, safety factor {min_safety_factor:.2} < {:.2}",
                config.min_safety_factor
            ),
        )
    };

    VoxelFeaReport {
        verdict,
        reason,
        converged: true,
        iterations,
        relative_residual,
        active_voxel_count,
        active_dof_count,
        max_displacement_mm,
        max_von_mises_mpa,
        min_safety_factor,
        voxel_von_mises_mpa,
        peak_stress_point_mm,
    }
}

/// Computes the 6x6 isotropic linear-elastic constitutive matrix `D` in Voigt order
/// `[exx, eyy, ezz, gxy, gyz, gzx]`.
fn isotropic_elasticity_matrix(mat: &FeaMaterial) -> [[f64; 6]; 6] {
    let e = mat.youngs_modulus_mpa;
    let nu = mat.poissons_ratio;
    let lambda = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
    let mu = e / (2.0 * (1.0 + nu));
    let c11 = lambda + 2.0 * mu;

    [
        [c11, lambda, lambda, 0.0, 0.0, 0.0],
        [lambda, c11, lambda, 0.0, 0.0, 0.0],
        [lambda, lambda, c11, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, mu, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0, mu, 0.0],
        [0.0, 0.0, 0.0, 0.0, 0.0, mu],
    ]
}

/// Evaluates the 6x24 strain-displacement matrix `B(xi, eta, zeta)` for a cubic H8
/// voxel of edge length `h_mm`.
fn compute_hex8_b_matrix(h_mm: f64, xi: f64, eta: f64, zeta: f64) -> [[f64; 24]; 6] {
    let mut b = [[0.0_f64; 24]; 6];
    let inv_4h = 0.25 / h_mm;

    for (a, &(sx, sy, sz)) in HEX_NODE_SIGNS.iter().enumerate() {
        let dndx = sx * (1.0 + sy * eta) * (1.0 + sz * zeta) * inv_4h;
        let dndy = sy * (1.0 + sx * xi) * (1.0 + sz * zeta) * inv_4h;
        let dndz = sz * (1.0 + sx * xi) * (1.0 + sy * eta) * inv_4h;

        let col = 3 * a;
        b[0][col] = dndx;
        b[1][col + 1] = dndy;
        b[2][col + 2] = dndz;

        b[3][col] = dndy;
        b[3][col + 1] = dndx;

        b[4][col + 1] = dndz;
        b[4][col + 2] = dndy;

        b[5][col] = dndz;
        b[5][col + 2] = dndx;
    }

    b
}

/// Computes the exact 24x24 H8 element stiffness matrix `K_e = int B^T D B dV`
/// via `2x2x2` Gauss-Legendre quadrature.
fn compute_hex8_stiffness_matrix(h_mm: f64, d_mat: &[[f64; 6]; 6]) -> [[f64; 24]; 24] {
    let mut ke = [[0.0_f64; 24]; 24];
    let gp = 1.0 / 3.0_f64.sqrt();
    let coords = [-gp, gp];
    let det_j = 0.125 * h_mm * h_mm * h_mm;

    for &xi in &coords {
        for &eta in &coords {
            for &zeta in &coords {
                let b = compute_hex8_b_matrix(h_mm, xi, eta, zeta);
                let mut db = [[0.0_f64; 24]; 6];
                for r in 0..6 {
                    for c in 0..24 {
                        let mut sum = 0.0_f64;
                        for k in 0..6 {
                            sum += d_mat[r][k] * b[k][c];
                        }
                        db[r][c] = sum;
                    }
                }
                for i in 0..24 {
                    for j in i..24 {
                        let mut sum = 0.0_f64;
                        for r in 0..6 {
                            sum += b[r][i] * db[r][j];
                        }
                        let contrib = sum * det_j;
                        ke[i][j] += contrib;
                        if i != j {
                            ke[j][i] += contrib;
                        }
                    }
                }
            }
        }
    }

    ke
}

/// Applies `y = K x` element-by-element without assembling a global sparse matrix.
fn apply_matrix_free_stiffness(
    elem_nodes: &[[usize; 8]],
    ke: &[[f64; 24]; 24],
    fixed: &[bool],
    x: &[f64],
    y: &mut [f64],
) {
    y.fill(0.0);
    let mut u_e = [0.0_f64; 24];
    let mut f_e = [0.0_f64; 24];

    for corners in elem_nodes {
        for (local_n, &node_id) in corners.iter().enumerate() {
            let base_l = 3 * local_n;
            let base_g = 3 * node_id;
            u_e[base_l] = if fixed[base_g] { 0.0 } else { x[base_g] };
            u_e[base_l + 1] = if fixed[base_g + 1] { 0.0 } else { x[base_g + 1] };
            u_e[base_l + 2] = if fixed[base_g + 2] { 0.0 } else { x[base_g + 2] };
        }

        for row in 0..24 {
            let k_row = &ke[row];
            let mut sum = 0.0_f64;
            for col in 0..24 {
                sum += k_row[col] * u_e[col];
            }
            f_e[row] = sum;
        }

        for (local_n, &node_id) in corners.iter().enumerate() {
            let base_l = 3 * local_n;
            let base_g = 3 * node_id;
            y[base_g] += f_e[base_l];
            y[base_g + 1] += f_e[base_l + 1];
            y[base_g + 2] += f_e[base_l + 2];
        }
    }

    for (dof, &is_fixed) in fixed.iter().enumerate() {
        if is_fixed {
            y[dof] = 0.0;
        }
    }
}

/// Verifies that every connected component carrying a non-zero force load has
/// Dirichlet constraints that suppress all 6 3D rigid-body modes (3 translations + 3 rotations).
fn verify_rigid_body_stability(
    active_node_count: usize,
    node_coords: &[[f64; 3]],
    elem_nodes: &[[usize; 8]],
    fixed: &[bool],
    rhs: &[f64],
) -> Result<(), String> {
    // Build node-to-element adjacency to find connected components of active nodes.
    let mut node_elems = vec![Vec::<usize>::new(); active_node_count];
    for (elem_idx, corners) in elem_nodes.iter().enumerate() {
        for &node_id in corners {
            node_elems[node_id].push(elem_idx);
        }
    }

    let mut comp_id = vec![usize::MAX; active_node_count];
    let mut queue = VecDeque::new();
    let mut num_components = 0usize;

    for start in 0..active_node_count {
        if comp_id[start] != usize::MAX {
            continue;
        }
        comp_id[start] = num_components;
        queue.push_back(start);
        while let Some(node) = queue.pop_front() {
            for &elem_idx in &node_elems[node] {
                for &nbr in &elem_nodes[elem_idx] {
                    if comp_id[nbr] == usize::MAX {
                        comp_id[nbr] = num_components;
                        queue.push_back(nbr);
                    }
                }
            }
        }
        num_components += 1;
    }

    let mut comp_has_load = vec![false; num_components];
    let mut comp_fixed_nodes: Vec<Vec<usize>> = vec![Vec::new(); num_components];

    for node_id in 0..active_node_count {
        let cid = comp_id[node_id];
        let fx = rhs[3 * node_id];
        let fy = rhs[3 * node_id + 1];
        let fz = rhs[3 * node_id + 2];
        if fx * fx + fy * fy + fz * fz > 1.0e-30 {
            comp_has_load[cid] = true;
        }
        if fixed[3 * node_id] || fixed[3 * node_id + 1] || fixed[3 * node_id + 2] {
            comp_fixed_nodes[cid].push(node_id);
        }
    }

    for cid in 0..num_components {
        if !comp_has_load[cid] {
            continue;
        }
        let fixed_nodes = &comp_fixed_nodes[cid];
        if fixed_nodes.is_empty() {
            return Err(
                "disconnected load-to-support component: loaded solid body has no fixed constraints"
                    .to_string(),
            );
        }

        // Compute centroid and characteristic span of fixed nodes for well-conditioned Gram test.
        let inv_n = 1.0 / (fixed_nodes.len() as f64);
        let mut centroid = [0.0_f64; 3];
        for &nid in fixed_nodes {
            for axis in 0..3 {
                centroid[axis] += node_coords[nid][axis] * inv_n;
            }
        }
        let mut scale = 0.0_f64;
        for &nid in fixed_nodes {
            let d = sub(node_coords[nid], centroid);
            scale = scale.max(dot(d, d).sqrt());
        }
        if scale <= 1.0e-12 {
            return Err(
                "under-constrained component: all fixed nodes coincide at a single point (rotations unconstrained)"
                    .to_string(),
            );
        }

        let mut gram = [[0.0_f64; 6]; 6];
        for &nid in fixed_nodes {
            let r = [
                (node_coords[nid][0] - centroid[0]) / scale,
                (node_coords[nid][1] - centroid[1]) / scale,
                (node_coords[nid][2] - centroid[2]) / scale,
            ];
            let rows = [
                [1.0, 0.0, 0.0, 0.0, r[2], -r[1]],
                [0.0, 1.0, 0.0, -r[2], 0.0, r[0]],
                [0.0, 0.0, 1.0, r[1], -r[0], 0.0],
            ];
            for (axis, row) in rows.iter().enumerate() {
                if fixed[3 * nid + axis] {
                    for i in 0..6 {
                        for j in 0..6 {
                            gram[i][j] += row[i] * row[j];
                        }
                    }
                }
            }
        }

        if !is_spd_6x6(&gram, 1.0e-10) {
            return Err(
                "under-constrained component: Dirichlet constraints do not suppress all 6 rigid-body modes"
                    .to_string(),
            );
        }
    }

    Ok(())
}

/// Checks positive definiteness of a symmetric 6x6 matrix via Cholesky factorization.
fn is_spd_6x6(a: &[[f64; 6]; 6], min_pivot: f64) -> bool {
    let mut l = [[0.0_f64; 6]; 6];
    for i in 0..6 {
        for j in 0..=i {
            let mut sum = a[i][j];
            for k in 0..j {
                sum -= l[i][k] * l[j][k];
            }
            if i == j {
                if !sum.is_finite() || sum <= min_pivot {
                    return false;
                }
                l[i][j] = sum.sqrt();
            } else {
                l[i][j] = sum / l[j][j];
            }
        }
    }
    true
}

/// Computes Von Mises equivalent stress from Voigt Cauchy stress
/// `[sxx, syy, szz, txy, tyz, tzx]`.
fn von_mises_stress(s: &[f64; 6]) -> f64 {
    let d1 = s[0] - s[1];
    let d2 = s[1] - s[2];
    let d3 = s[2] - s[0];
    let normal_term = 0.5 * (d1 * d1 + d2 * d2 + d3 * d3);
    let shear_term = 3.0 * (s[3] * s[3] + s[4] * s[4] + s[5] * s[5]);
    (normal_term + shear_term).max(0.0).sqrt()
}

fn dot_vec(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn l2_norm(v: &[f64]) -> f64 {
    dot_vec(v, v).sqrt()
}

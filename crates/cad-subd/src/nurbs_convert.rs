//! Exact bicubic B-Spline / Bézier / Gregory patch extraction from Catmull-Clark
//! control cages with semi-sharp and hard creases.
//!
//! Regular quad faces (all four corners interior valence-4 smooth vertices with
//! smooth 1-rings) are converted to exact `4x4` uniform bicubic B-Spline patches
//! and their equivalent `4x4` bicubic Bézier control nets via the standard
//! B-Spline-to-Bézier basis matrix.
//!
//! Extraordinary and creased quad faces are extracted as boundary-matched bicubic
//! Gregory patches (Loop-Schaefer / Chiyokura-Kimura construction):
//! - Shared boundary curves depend solely on endpoint 1-rings, guaranteeing exact
//!   watertight `G0` stitching across every seam (regular, extraordinary, or creased).
//! - Smooth interior seams enforce the exact linear `G1` cross-boundary ribbon
//!   compatibility condition so unit normals match across every smooth seam.
//! - Hard crease edges (`sharpness >= hard_crease_threshold`) clamp boundary tangents
//!   independently on each side to preserve crisp mechanical styling edges.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

use crate::cage::{CREASE_INFINITE, SubDCage, SubDError};
use crate::catmull_clark::subdivide;

/// Options controlling Catmull-Clark isolation and NURBS patch extraction.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NurbsConversionOptions {
    /// Explicit number of Catmull-Clark isolation steps before patch extraction.
    /// When `None`, the minimal number of steps needed to produce an all-quad cage
    /// with isolated extraordinary vertices and decayed fractional creases is chosen.
    pub isolation_levels: Option<u32>,
    /// Upper bound on automatic isolation steps when `isolation_levels` is `None`.
    pub max_auto_isolation_levels: u32,
    /// Minimum crease sharpness treated as a hard crease on the isolated cage.
    /// Defaults to `1.0`.
    pub hard_crease_threshold: f32,
}

impl Default for NurbsConversionOptions {
    fn default() -> Self {
        Self {
            isolation_levels: None,
            max_auto_isolation_levels: 3,
            hard_crease_threshold: 1.0,
        }
    }
}

/// Mathematical representation of an extracted bicubic patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchKind {
    /// Exact `4x4` uniform bicubic B-Spline patch (also stored in Bézier form).
    RegularBSpline,
    /// `4x4` bicubic Bézier patch without split Gregory interior points.
    BicubicBezier,
    /// Boundary-matched bicubic Gregory patch with split interior cross-boundary ribbons.
    Gregory,
}

/// Directed boundary edge of a bicubic patch in counter-clockwise order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PatchEdge {
    /// `v = 0`, `u` from `0` to `1` (`corner 0 -> corner 1`).
    Bottom = 0,
    /// `u = 1`, `v` from `0` to `1` (`corner 1 -> corner 2`).
    Right = 1,
    /// `v = 1`, `u` from `1` to `0` (`corner 2 -> corner 3`).
    Top = 2,
    /// `u = 0`, `v` from `1` to `0` (`corner 3 -> corner 0`).
    Left = 3,
}

impl PatchEdge {
    /// All four patch edges in counter-clockwise order.
    pub const ALL: [PatchEdge; 4] = [
        PatchEdge::Bottom,
        PatchEdge::Right,
        PatchEdge::Top,
        PatchEdge::Left,
    ];

    /// Convert a local edge index `0..4` into a [`PatchEdge`].
    pub fn from_index(index: usize) -> Self {
        match index % 4 {
            0 => PatchEdge::Bottom,
            1 => PatchEdge::Right,
            2 => PatchEdge::Top,
            _ => PatchEdge::Left,
        }
    }

    /// Map edge parameter `t in [0, 1]` to patch `(u, v)` coordinates.
    pub fn to_uv(self, t: f64) -> (f64, f64) {
        match self {
            PatchEdge::Bottom => (t, 0.0),
            PatchEdge::Right => (1.0, t),
            PatchEdge::Top => (1.0 - t, 1.0),
            PatchEdge::Left => (0.0, 1.0 - t),
        }
    }
}

/// Split interior control points for a bicubic Gregory patch.
///
/// Each of the four interior slots `(1,1), (2,1), (2,2), (1,2)` holds two points:
/// `*_v` controls the cross-boundary derivative along a constant-`v` edge (`v=0` or `v=1`),
/// and `*_u` controls the cross-boundary derivative along a constant-`u` edge (`u=0` or `u=1`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GregoryInterior {
    /// Interior point near `(0, 0)` active along `v = 0` (`Bottom`).
    pub f00_v: [f64; 3],
    /// Interior point near `(0, 0)` active along `u = 0` (`Left`).
    pub f00_u: [f64; 3],
    /// Interior point near `(1, 0)` active along `v = 0` (`Bottom`).
    pub f10_v: [f64; 3],
    /// Interior point near `(1, 0)` active along `u = 1` (`Right`).
    pub f10_u: [f64; 3],
    /// Interior point near `(1, 1)` active along `v = 1` (`Top`).
    pub f11_v: [f64; 3],
    /// Interior point near `(1, 1)` active along `u = 1` (`Right`).
    pub f11_u: [f64; 3],
    /// Interior point near `(0, 1)` active along `v = 1` (`Top`).
    pub f01_v: [f64; 3],
    /// Interior point near `(0, 1)` active along `u = 0` (`Left`).
    pub f01_u: [f64; 3],
}

/// Full differential geometry evaluation at a surface parameter `(u, v)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SurfaceEval {
    /// 3D point `S(u, v)`.
    pub position: [f64; 3],
    /// First partial derivative `dS/du`.
    pub du: [f64; 3],
    /// First partial derivative `dS/dv`.
    pub dv: [f64; 3],
    /// Second partial derivative `d^2S/du^2`.
    pub duu: [f64; 3],
    /// Mixed partial derivative `d^2S/(du dv)`.
    pub duv: [f64; 3],
    /// Second partial derivative `d^2S/dv^2`.
    pub dvv: [f64; 3],
    /// Outward unit normal `normalize(du x dv)`.
    pub normal: [f64; 3],
    /// Gaussian curvature `K = (L*N - M^2) / (E*G - F^2)`.
    pub gaussian_curvature: f64,
    /// Mean curvature `H = (E*N - 2*F*M + G*L) / (2*(E*G - F^2))`.
    pub mean_curvature: f64,
}

/// Extracted bicubic surface patch (`4x4` Bézier net, plus optional B-Spline net and Gregory ribbons).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BicubicPatch {
    /// Zero-based patch index inside its [`NurbsPatchSet`].
    pub id: usize,
    /// Underlying patch representation.
    pub kind: PatchKind,
    /// `4x4` bicubic Bézier control net indexed as `[u_index][v_index]`.
    pub bezier_controls: [[[f64; 3]; 4]; 4],
    /// Original `4x4` uniform bicubic B-Spline control net when [`PatchKind::RegularBSpline`].
    pub bspline_controls: Option<[[[f64; 3]; 4]; 4]>,
    /// Split Gregory interior ribbon points when [`PatchKind::Gregory`].
    pub gregory_interior: Option<GregoryInterior>,
    /// Vertex indices of the isolated quad face `[v0, v1, v2, v3]`.
    pub corner_vertices: [u32; 4],
    /// Whether each boundary edge `[Bottom, Right, Top, Left]` is a hard crease or open boundary.
    pub creased_edges: [bool; 4],
}

impl BicubicPatch {
    /// Evaluate the exact 3D position `S(u, v)` for `(u, v) in [0, 1]^2`.
    pub fn eval(&self, u: f64, v: f64) -> [f64; 3] {
        let bu = bernstein3(u);
        let bv = bernstein3(v);
        let mut p = [0.0; 3];
        for i in 0..4 {
            for j in 0..4 {
                let cp = self.effective_control_point(i, j, u, v);
                let w = bu[i] * bv[j];
                p[0] += w * cp[0];
                p[1] += w * cp[1];
                p[2] += w * cp[2];
            }
        }
        p
    }

    /// Evaluate position, first and second partial derivatives, unit normal, and curvatures.
    pub fn eval_derivatives(&self, u: f64, v: f64) -> SurfaceEval {
        let bu = bernstein3(u);
        let bv = bernstein3(v);
        let dbu = bernstein3_d1(u);
        let dbv = bernstein3_d1(v);
        let d2bu = bernstein3_d2(u);
        let d2bv = bernstein3_d2(v);

        let mut position = [0.0; 3];
        let mut du = [0.0; 3];
        let mut dv = [0.0; 3];
        let mut duu = [0.0; 3];
        let mut duv = [0.0; 3];
        let mut dvv = [0.0; 3];

        for i in 0..4 {
            for j in 0..4 {
                if self.gregory_interior.is_some() && (i == 1 || i == 2) && (j == 1 || j == 2) {
                    continue;
                }
                let cp = self.bezier_controls[i][j];
                add_scaled_in_place(&mut position, cp, bu[i] * bv[j]);
                add_scaled_in_place(&mut du, cp, dbu[i] * bv[j]);
                add_scaled_in_place(&mut dv, cp, bu[i] * dbv[j]);
                add_scaled_in_place(&mut duu, cp, d2bu[i] * bv[j]);
                add_scaled_in_place(&mut duv, cp, dbu[i] * dbv[j]);
                add_scaled_in_place(&mut dvv, cp, bu[i] * d2bv[j]);
            }
        }

        if let Some(g) = self.gregory_interior {
            let terms = [
                eval_gregory_term(u, v, g.f00_v, g.f00_u, false, false),
                eval_gregory_term(1.0 - u, v, g.f10_v, g.f10_u, true, false),
                eval_gregory_term(u, 1.0 - v, g.f01_v, g.f01_u, false, true),
                eval_gregory_term(1.0 - u, 1.0 - v, g.f11_v, g.f11_u, true, true),
            ];
            for term in terms {
                position = add(position, term.val);
                du = add(du, term.du);
                dv = add(dv, term.dv);
                duu = add(duu, term.duu);
                duv = add(duv, term.duv);
                dvv = add(dvv, term.dvv);
            }
        }

        let cross_n = cross(du, dv);
        let normal = normalize(cross_n);

        let e = dot(du, du);
        let f = dot(du, dv);
        let g_ff = dot(dv, dv);
        let det1 = e * g_ff - f * f;

        let (gaussian_curvature, mean_curvature) = if det1 > 1e-24 {
            let l = dot(duu, normal);
            let m = dot(duv, normal);
            let n2 = dot(dvv, normal);
            let k = (l * n2 - m * m) / det1;
            let h = (e * n2 - 2.0 * f * m + g_ff * l) / (2.0 * det1);
            (k, h)
        } else {
            (0.0, 0.0)
        };

        SurfaceEval {
            position,
            du,
            dv,
            duu,
            duv,
            dvv,
            normal,
            gaussian_curvature,
            mean_curvature,
        }
    }

    /// First partial derivative `dS/du`.
    pub fn du(&self, u: f64, v: f64) -> [f64; 3] {
        self.eval_derivatives(u, v).du
    }

    /// First partial derivative `dS/dv`.
    pub fn dv(&self, u: f64, v: f64) -> [f64; 3] {
        self.eval_derivatives(u, v).dv
    }

    /// Second partial derivative `d^2S/du^2`.
    pub fn duu(&self, u: f64, v: f64) -> [f64; 3] {
        self.eval_derivatives(u, v).duu
    }

    /// Mixed partial derivative `d^2S/(du dv)`.
    pub fn duv(&self, u: f64, v: f64) -> [f64; 3] {
        self.eval_derivatives(u, v).duv
    }

    /// Second partial derivative `d^2S/dv^2`.
    pub fn dvv(&self, u: f64, v: f64) -> [f64; 3] {
        self.eval_derivatives(u, v).dvv
    }

    /// Outward unit surface normal at `(u, v)`.
    pub fn normal(&self, u: f64, v: f64) -> [f64; 3] {
        self.eval_derivatives(u, v).normal
    }

    /// `(gaussian_curvature, mean_curvature)` at `(u, v)`.
    pub fn curvature(&self, u: f64, v: f64) -> (f64, f64) {
        let eval = self.eval_derivatives(u, v);
        (eval.gaussian_curvature, eval.mean_curvature)
    }

    /// Evaluate differential geometry along a specific boundary `edge` at `t in [0, 1]`.
    pub fn eval_edge(&self, edge: PatchEdge, t: f64) -> SurfaceEval {
        let (u, v) = edge.to_uv(t);
        self.eval_derivatives(u, v)
    }

    /// Boundary Bézier control points `[b0, b1, b2, b3]` along `edge` in `t: 0 -> 1` order.
    pub fn edge_bezier_curve(&self, edge: PatchEdge) -> [[f64; 3]; 4] {
        let b = &self.bezier_controls;
        match edge {
            PatchEdge::Bottom => [b[0][0], b[1][0], b[2][0], b[3][0]],
            PatchEdge::Right => [b[3][0], b[3][1], b[3][2], b[3][3]],
            PatchEdge::Top => [b[3][3], b[2][3], b[1][3], b[0][3]],
            PatchEdge::Left => [b[0][3], b[0][2], b[0][1], b[0][0]],
        }
    }

    fn effective_control_point(&self, i: usize, j: usize, u: f64, v: f64) -> [f64; 3] {
        if let Some(g) = self.gregory_interior {
            match (i, j) {
                (1, 1) => blend_gregory(u, v, g.f00_v, g.f00_u),
                (2, 1) => blend_gregory(1.0 - u, v, g.f10_v, g.f10_u),
                (1, 2) => blend_gregory(u, 1.0 - v, g.f01_v, g.f01_u),
                (2, 2) => blend_gregory(1.0 - u, 1.0 - v, g.f11_v, g.f11_u),
                _ => self.bezier_controls[i][j],
            }
        } else {
            self.bezier_controls[i][j]
        }
    }
}

/// Reference to one boundary edge of a patch inside a [`NurbsPatchSet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PatchEdgeRef {
    /// Index into `NurbsPatchSet::patches`.
    pub patch_id: usize,
    /// Which of the four edges of that patch.
    pub edge: PatchEdge,
}

/// Shared seam pairing two adjacent patch edges (oriented in opposite directions).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PatchSeam {
    /// First patch edge (`t: 0 -> 1`).
    pub a: PatchEdgeRef,
    /// Matching twin patch edge (`t': 1 - t`).
    pub b: PatchEdgeRef,
    /// Whether this seam is a hard crease edge.
    pub is_creased: bool,
    /// Crease sharpness stored on the isolated cage (`0.0` if smooth).
    pub crease_sharpness: f32,
    /// True when both incident patches are [`PatchKind::RegularBSpline`].
    pub both_regular: bool,
}

/// Uniformly sampled `(u, v)` surface grid ready for downstream OCCT
/// `quad_grid_to_bspline_solid` / `BRepBuilderAPI_Sewing` ingestion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuadSurfaceGrid {
    /// Source patch id.
    pub patch_id: usize,
    /// Number of samples along `u` (`>= 2`).
    pub nu: usize,
    /// Number of samples along `v` (`>= 2`).
    pub nv: usize,
    /// Row-major `nu * nv` sampled positions (`index = iu * nv + iv`).
    pub points: Vec<[f64; 3]>,
    /// Row-major `nu * nv` outward unit normals (`index = iu * nv + iv`).
    pub normals: Vec<[f64; 3]>,
    /// Hard-crease flags for `[Bottom, Right, Top, Left]`.
    pub creased_edges: [bool; 4],
}

/// Complete watertight collection of bicubic NURBS / Gregory patches extracted from a [`SubDCage`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NurbsPatchSet {
    /// Extracted bicubic patches, one per isolated quad face.
    pub patches: Vec<BicubicPatch>,
    /// Matched interior seams shared by pairs of patches.
    pub seams: Vec<PatchSeam>,
    /// Unmatched boundary edges (empty for a closed manifold shell).
    pub open_boundary_edges: Vec<PatchEdgeRef>,
    /// Number of Catmull-Clark isolation steps applied before extraction.
    pub isolation_levels_applied: u32,
    /// True when the extracted patch set has at least one patch and zero open boundary edges.
    pub is_closed_shell: bool,
}

impl NurbsPatchSet {
    /// Export every patch as a uniform `(u, v)` sampled grid (`samples_per_edge >= 2`).
    ///
    /// Adjacent patches sample their shared boundary at identical parameter values so
    /// downstream B-Rep sewing (`BRepBuilderAPI_Sewing` / `quad_grid_to_bspline_solid`)
    /// receives watertight matching edges.
    pub fn to_quad_grids(&self, samples_per_edge: usize) -> Vec<QuadSurfaceGrid> {
        let n = samples_per_edge.max(2);
        let denom = (n - 1) as f64;
        self.patches
            .iter()
            .map(|patch| {
                let mut points = Vec::with_capacity(n * n);
                let mut normals = Vec::with_capacity(n * n);
                for iu in 0..n {
                    let u = iu as f64 / denom;
                    for iv in 0..n {
                        let v = iv as f64 / denom;
                        let eval = patch.eval_derivatives(u, v);
                        points.push(eval.position);
                        normals.push(eval.normal);
                    }
                }
                QuadSurfaceGrid {
                    patch_id: patch.id,
                    nu: n,
                    nv: n,
                    points,
                    normals,
                    creased_edges: patch.creased_edges,
                }
            })
            .collect()
    }
}

/// Convert a `4x4` uniform bicubic B-Spline control net into its exact equivalent
/// `4x4` bicubic Bézier control net using `B = M * P * M^T`.
pub fn bspline_to_bezier(bspline: &[[[f64; 3]; 4]; 4]) -> [[[f64; 3]; 4]; 4] {
    const M: [[f64; 4]; 4] = [
        [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0, 0.0],
        [0.0, 4.0 / 6.0, 2.0 / 6.0, 0.0],
        [0.0, 2.0 / 6.0, 4.0 / 6.0, 0.0],
        [0.0, 1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0],
    ];

    let mut temp = [[[0.0; 3]; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut sum = [0.0; 3];
            for k in 0..4 {
                add_scaled_in_place(&mut sum, bspline[k][j], M[i][k]);
            }
            temp[i][j] = sum;
        }
    }

    let mut bezier = [[[0.0; 3]; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            let mut sum = [0.0; 3];
            for k in 0..4 {
                add_scaled_in_place(&mut sum, temp[i][k], M[j][k]);
            }
            bezier[i][j] = sum;
        }
    }
    bezier
}

/// Convert a [`SubDCage`] into a watertight [`NurbsPatchSet`] using default options.
pub fn to_nurbs_patch_set(cage: &SubDCage) -> Result<NurbsPatchSet, SubDError> {
    to_nurbs_patch_set_with_options(cage, NurbsConversionOptions::default())
}

/// Convert a [`SubDCage`] into a watertight [`NurbsPatchSet`] with explicit options.
pub fn to_nurbs_patch_set_with_options(
    cage: &SubDCage,
    options: NurbsConversionOptions,
) -> Result<NurbsPatchSet, SubDError> {
    cage.validate()?;
    if cage.faces.is_empty() {
        return Err(SubDError::EmptyFaces);
    }
    let initial_edges = cage.edge_incidences();
    if let Some((&(a, b), incident)) = initial_edges.iter().find(|(_, inc)| inc.len() > 2) {
        return Err(SubDError::NonManifoldEdge {
            a,
            b,
            incident_faces: incident.len(),
        });
    }

    let levels = match options.isolation_levels {
        Some(explicit) => explicit,
        None => choose_auto_isolation_levels(cage, options),
    };

    let isolated = if levels == 0 {
        cage.clone()
    } else {
        subdivide(cage, levels)?
    };

    // Ensure all faces are quads (if user explicitly requested 0 levels on a non-quad cage,
    // apply 1 Catmull-Clark step so every face is a quad).
    let (isolated, levels_applied) = if isolated.faces.iter().any(|f| f.len() != 4) {
        (subdivide(&isolated, 1)?, levels + 1)
    } else {
        (isolated, levels)
    };

    extract_patches_from_quad_cage(&isolated, levels_applied, options.hard_crease_threshold)
}

fn choose_auto_isolation_levels(cage: &SubDCage, options: NurbsConversionOptions) -> u32 {
    let has_non_quads = cage.faces.iter().any(|f| f.len() != 4);
    let edges = cage.edge_incidences();
    let mut valence = vec![0usize; cage.vertices.len()];
    let mut is_boundary_vertex = vec![false; cage.vertices.len()];
    for (&(a, b), inc) in &edges {
        valence[a as usize] += 1;
        valence[b as usize] += 1;
        if inc.len() == 1 {
            is_boundary_vertex[a as usize] = true;
            is_boundary_vertex[b as usize] = true;
        }
    }

    let mut needs_step_for_extraordinary = false;
    for face in &cage.faces {
        let extra_count = face
            .iter()
            .filter(|&&v| valence[v as usize] != 4 || is_boundary_vertex[v as usize])
            .count();
        if extra_count > 1 {
            needs_step_for_extraordinary = true;
            break;
        }
    }

    let has_fractional_crease = cage
        .edge_creases
        .values()
        .any(|&s| s > 0.0 && s < options.hard_crease_threshold);

    let mut needed = 0u32;
    if has_non_quads {
        // 1 step makes all faces quads; if an n-gon is adjacent to an extraordinary vertex,
        // 2 steps guarantees every face has at most 1 extraordinary vertex.
        needed = needed.max(if needs_step_for_extraordinary { 2 } else { 1 });
    } else if needs_step_for_extraordinary {
        needed = needed.max(1);
    }

    if has_fractional_crease {
        needed = needed.max(1);
    }

    needed.min(options.max_auto_isolation_levels)
}

/// Directed half-edge metadata for an all-quad manifold mesh.
#[derive(Clone, Copy)]
struct HalfEdgeOwner {
    face: usize,
    edge: usize,
}

/// Ordered 1-ring around a single vertex `v` in counter-clockwise order.
struct VertexRing {
    /// Edge neighbors in CCW order.
    edges: Vec<u32>,
    /// Diagonal opposite vertices in the incident quads (`diagonals[k]` is in the quad
    /// between `edges[k]` and `edges[(k+1) % edges.len()]`). Length equals `face_count`.
    diagonals: Vec<u32>,
    /// Whether `edges[k]` is a hard crease or boundary edge.
    edge_is_hard: Vec<bool>,
    /// True if the vertex lies on an open boundary (so `edges.len() == diagonals.len() + 1`).
    is_boundary: bool,
}

impl VertexRing {
    fn face_count(&self) -> usize {
        self.diagonals.len()
    }

    fn hard_edge_count(&self) -> usize {
        self.edge_is_hard.iter().filter(|&&h| h).count()
    }

    fn is_regular_smooth_interior(&self) -> bool {
        !self.is_boundary && self.edges.len() == 4 && self.hard_edge_count() == 0
    }

    fn index_of_neighbor(&self, neighbor: u32) -> Option<usize> {
        self.edges.iter().position(|&e| e == neighbor)
    }
}

/// Precomputed limit position, outgoing edge tangents, and Gregory cosine factors at a vertex.
struct VertexLimitData {
    limit_pos: [f64; 3],
    /// Outgoing Bézier tangent handle `b1` for each outgoing edge `ring.edges[k]`.
    edge_handles: Vec<[f64; 3]>,
    /// Cosine compatibility factor `c` for each outgoing smooth edge `ring.edges[k]`.
    edge_g1_cos: Vec<f64>,
}

fn extract_patches_from_quad_cage(
    cage: &SubDCage,
    levels_applied: u32,
    hard_crease_threshold: f32,
) -> Result<NurbsPatchSet, SubDError> {
    let mut half_edges: BTreeMap<(u32, u32), HalfEdgeOwner> = BTreeMap::new();
    for (face_idx, face) in cage.faces.iter().enumerate() {
        for edge_idx in 0..4 {
            let a = face[edge_idx];
            let b = face[(edge_idx + 1) % 4];
            if half_edges
                .insert(
                    (a, b),
                    HalfEdgeOwner {
                        face: face_idx,
                        edge: edge_idx,
                    },
                )
                .is_some()
            {
                return Err(SubDError::NonOrientableEdge { a, b });
            }
        }
    }

    let edge_incidences = cage.edge_incidences();
    let is_edge_hard = |a: u32, b: u32| -> bool {
        let key = (a.min(b), a.max(b));
        let boundary = edge_incidences
            .get(&key)
            .map(|inc| inc.len() == 1)
            .unwrap_or(true);
        if boundary {
            return true;
        }
        let sharpness = cage.edge_creases.get(&key).copied().unwrap_or(0.0);
        sharpness >= hard_crease_threshold || sharpness >= CREASE_INFINITE
    };

    // Build CCW 1-ring for every vertex referenced by at least one face.
    let rings = build_vertex_rings(cage, &half_edges, &is_edge_hard);
    let limits: Vec<VertexLimitData> = rings
        .iter()
        .enumerate()
        .map(|(v_idx, ring)| compute_vertex_limit_data(v_idx as u32, ring, &cage.vertices))
        .collect();

    // First pass: construct each patch (either exact RegularBSpline or initial Gregory).
    let mut patches = Vec::with_capacity(cage.faces.len());
    for (face_idx, face) in cage.faces.iter().enumerate() {
        let v = [face[0], face[1], face[2], face[3]];
        let creased_edges = [
            is_edge_hard(v[0], v[1]),
            is_edge_hard(v[1], v[2]),
            is_edge_hard(v[2], v[3]),
            is_edge_hard(v[3], v[0]),
        ];

        let all_corners_regular_smooth = v.iter().all(|&vi| rings[vi as usize].is_regular_smooth_interior());

        if all_corners_regular_smooth {
            let bspline = extract_regular_bspline_grid(v, &rings, &cage.vertices);
            let bezier = bspline_to_bezier(&bspline);
            patches.push(BicubicPatch {
                id: face_idx,
                kind: PatchKind::RegularBSpline,
                bezier_controls: bezier,
                bspline_controls: Some(bspline),
                gregory_interior: None,
                corner_vertices: v,
                creased_edges,
            });
        } else {
            let (bezier, gregory) =
                build_extraordinary_or_creased_patch(v, &rings, &limits, &cage.vertices);
            patches.push(BicubicPatch {
                id: face_idx,
                kind: PatchKind::Gregory,
                bezier_controls: bezier,
                bspline_controls: None,
                gregory_interior: Some(gregory),
                corner_vertices: v,
                creased_edges,
            });
        }
    }

    // Second pass: pair all directed half-edges into seams and open boundaries.
    let mut seams = Vec::new();
    let mut open_boundary_edges = Vec::new();
    for (&(a, b), &owner_a) in &half_edges {
        if let Some(&owner_b) = half_edges.get(&(b, a)) {
            if owner_a.face < owner_b.face
                || (owner_a.face == owner_b.face && owner_a.edge < owner_b.edge)
            {
                let key = (a.min(b), a.max(b));
                let sharpness = cage.edge_creases.get(&key).copied().unwrap_or(0.0);
                let is_creased = sharpness >= hard_crease_threshold || sharpness >= CREASE_INFINITE;
                let both_regular = patches[owner_a.face].kind == PatchKind::RegularBSpline
                    && patches[owner_b.face].kind == PatchKind::RegularBSpline;
                seams.push(PatchSeam {
                    a: PatchEdgeRef {
                        patch_id: owner_a.face,
                        edge: PatchEdge::from_index(owner_a.edge),
                    },
                    b: PatchEdgeRef {
                        patch_id: owner_b.face,
                        edge: PatchEdge::from_index(owner_b.edge),
                    },
                    is_creased,
                    crease_sharpness: sharpness,
                    both_regular,
                });
            }
        } else {
            open_boundary_edges.push(PatchEdgeRef {
                patch_id: owner_a.face,
                edge: PatchEdge::from_index(owner_a.edge),
            });
        }
    }

    // Third pass: enforce exact G1 cross-boundary ribbon compatibility on every smooth seam
    // where at least one adjacent patch is not a RegularBSpline patch.
    for seam in &seams {
        if seam.is_creased || seam.both_regular {
            continue;
        }
        enforce_seam_g1_ribbon(&mut patches, seam, &rings, &limits);
    }

    let is_closed_shell = !patches.is_empty() && open_boundary_edges.is_empty();
    Ok(NurbsPatchSet {
        patches,
        seams,
        open_boundary_edges,
        isolation_levels_applied: levels_applied,
        is_closed_shell,
    })
}

fn build_vertex_rings<F>(
    cage: &SubDCage,
    half_edges: &BTreeMap<(u32, u32), HalfEdgeOwner>,
    is_edge_hard: &F,
) -> Vec<VertexRing>
where
    F: Fn(u32, u32) -> bool,
{
    let vertex_count = cage.vertices.len();
    let mut first_outgoing: Vec<Option<(usize, usize)>> = vec![None; vertex_count];
    for (face_idx, face) in cage.faces.iter().enumerate() {
        for edge_idx in 0..4 {
            let v = face[edge_idx] as usize;
            if first_outgoing[v].is_none() {
                first_outgoing[v] = Some((face_idx, edge_idx));
            }
        }
    }

    let mut rings = Vec::with_capacity(vertex_count);
    for (v_idx, start) in first_outgoing.into_iter().enumerate() {
        let v = v_idx as u32;
        let Some((mut cur_face, mut cur_corner)) = start else {
            rings.push(VertexRing {
                edges: Vec::new(),
                diagonals: Vec::new(),
                edge_is_hard: Vec::new(),
                is_boundary: false,
            });
            continue;
        };

        // First walk clockwise as far as possible in case `v` lies on an open boundary,
        // so that the CCW walk starts at the boundary edge.
        let mut is_boundary = false;
        let start_face = cur_face;
        let start_corner = cur_corner;
        loop {
            let face = &cage.faces[cur_face];
            let next_v = face[(cur_corner + 1) % 4];
            if let Some(&twin) = half_edges.get(&(next_v, v)) {
                // In twin.face, edge `twin.edge` is `next_v -> v`, so `v` is at `(twin.edge + 1) % 4`.
                let prev_corner = (twin.edge + 1) % 4;
                if twin.face == start_face && prev_corner == start_corner {
                    break;
                }
                cur_face = twin.face;
                cur_corner = prev_corner;
            } else {
                is_boundary = true;
                break;
            }
        }

        // Now walk counter-clockwise around `v`.
        let walk_start_face = cur_face;
        let walk_start_corner = cur_corner;
        let mut edges = Vec::new();
        let mut diagonals = Vec::new();
        let mut edge_is_hard = Vec::new();

        loop {
            let face = &cage.faces[cur_face];
            let e_cur = face[(cur_corner + 1) % 4];
            let d_cur = face[(cur_corner + 2) % 4];
            let e_next = face[(cur_corner + 3) % 4];

            edges.push(e_cur);
            edge_is_hard.push(is_edge_hard(v, e_cur));
            diagonals.push(d_cur);

            if let Some(&next_owner) = half_edges.get(&(v, e_next)) {
                if next_owner.face == walk_start_face && next_owner.edge == walk_start_corner {
                    break;
                }
                cur_face = next_owner.face;
                cur_corner = next_owner.edge;
            } else {
                // Reached the opposite boundary edge.
                edges.push(e_next);
                edge_is_hard.push(is_edge_hard(v, e_next));
                is_boundary = true;
                break;
            }
        }

        rings.push(VertexRing {
            edges,
            diagonals,
            edge_is_hard,
            is_boundary,
        });
    }

    rings
}

fn compute_vertex_limit_data(
    v: u32,
    ring: &VertexRing,
    vertices: &[[f64; 3]],
) -> VertexLimitData {
    let p_v = vertices[v as usize];
    let deg = ring.edges.len();
    if deg == 0 {
        return VertexLimitData {
            limit_pos: p_v,
            edge_handles: Vec::new(),
            edge_g1_cos: Vec::new(),
        };
    }

    let hard_indices: Vec<usize> = ring
        .edge_is_hard
        .iter()
        .enumerate()
        .filter_map(|(i, &h)| h.then_some(i))
        .collect();

    if !ring.is_boundary && hard_indices.len() <= 1 {
        // Smooth interior vertex (or dart vertex, which uses the smooth limit rule).
        let n = deg;
        let nf = n as f64;
        let mut e_sum = [0.0; 3];
        let mut d_sum = [0.0; 3];
        for k in 0..n {
            e_sum = add(e_sum, vertices[ring.edges[k] as usize]);
            d_sum = add(d_sum, vertices[ring.diagonals[k] as usize]);
        }
        let limit_pos = scale(
            add(add(scale(p_v, nf * nf), scale(e_sum, 4.0)), d_sum),
            1.0 / (nf * (nf + 5.0)),
        );

        let cos_half = (PI / nf).cos();
        let sigma = 1.0 / (4.0 + cos_half * cos_half).sqrt();
        let a_w = (1.0 + sigma * cos_half) / nf;
        let b_w = sigma / nf;

        let mut u_tan = [0.0; 3];
        let mut w_tan = [0.0; 3];
        for k in 0..n {
            let ang_e = 2.0 * PI * (k as f64) / nf;
            let ang_d = (2.0 * PI * (k as f64) + PI) / nf;
            let ek = vertices[ring.edges[k] as usize];
            let dk = vertices[ring.diagonals[k] as usize];
            u_tan = add(u_tan, scale(ek, a_w * ang_e.cos()));
            u_tan = add(u_tan, scale(dk, b_w * ang_d.cos()));
            w_tan = add(w_tan, scale(ek, a_w * ang_e.sin()));
            w_tan = add(w_tan, scale(dk, b_w * ang_d.sin()));
        }

        let g1_cos = 2.0 * (2.0 * PI / nf).cos();
        let mut edge_handles = Vec::with_capacity(n);
        let mut edge_g1_cos = Vec::with_capacity(n);
        for m in 0..n {
            let ang = 2.0 * PI * (m as f64) / nf;
            let tangent = add(scale(u_tan, ang.cos()), scale(w_tan, ang.sin()));
            edge_handles.push(add(limit_pos, scale(tangent, 1.0 / 3.0)));
            edge_g1_cos.push(g1_cos);
        }

        return VertexLimitData {
            limit_pos,
            edge_handles,
            edge_g1_cos,
        };
    }

    if hard_indices.len() == 2 {
        // Crease vertex (interior smooth crease passing through `v`, or smooth open boundary).
        let ia = hard_indices[0];
        let ib = hard_indices[1];
        let ea = vertices[ring.edges[ia] as usize];
        let eb = vertices[ring.edges[ib] as usize];
        let limit_pos = scale(add(add(ea, eb), scale(p_v, 4.0)), 1.0 / 6.0);

        let mut edge_handles = vec![p_v; deg];
        let mut edge_g1_cos = vec![0.0; deg];

        // Exact cubic B-Spline crease handles along the two hard edges.
        edge_handles[ia] = scale(add(scale(p_v, 2.0), ea), 1.0 / 3.0);
        edge_handles[ib] = scale(add(scale(p_v, 2.0), eb), 1.0 / 3.0);

        // Sector 1: CCW from ia to ib.
        let sector1_faces = ib - ia;
        fill_crease_sector_handles(
            ring,
            vertices,
            p_v,
            limit_pos,
            ia,
            sector1_faces,
            &mut edge_handles,
            &mut edge_g1_cos,
        );

        // Sector 2 (only for interior crease vertices): CCW from ib wrapping around to ia.
        if !ring.is_boundary {
            let sector2_faces = deg - (ib - ia);
            fill_crease_sector_handles(
                ring,
                vertices,
                p_v,
                limit_pos,
                ib,
                sector2_faces,
                &mut edge_handles,
                &mut edge_g1_cos,
            );
        }

        return VertexLimitData {
            limit_pos,
            edge_handles,
            edge_g1_cos,
        };
    }

    // Corner vertex (3+ hard edges, or a boundary corner with 2 boundary edges + interior crease,
    // or an open quad corner where only 1 face meets the boundary).
    let limit_pos = p_v;
    let mut edge_handles = vec![p_v; deg];
    let mut edge_g1_cos = vec![0.0; deg];

    for &hi in &hard_indices {
        let eh = vertices[ring.edges[hi] as usize];
        edge_handles[hi] = scale(add(scale(p_v, 2.0), eh), 1.0 / 3.0);
    }

    // Fill each sector bounded by consecutive hard edges.
    for s in 0..hard_indices.len() {
        let start_i = hard_indices[s];
        let end_i = if s + 1 < hard_indices.len() {
            hard_indices[s + 1]
        } else if !ring.is_boundary {
            hard_indices[0]
        } else {
            continue;
        };
        let k = if end_i >= start_i {
            end_i - start_i
        } else {
            deg - start_i + end_i
        };
        if k <= 1 {
            continue;
        }
        let ta = sub(vertices[ring.edges[start_i] as usize], p_v);
        let tb = sub(vertices[ring.edges[end_i] as usize], p_v);
        let g1_cos = 2.0 * (0.5 * PI / k as f64).cos();
        for m in 1..k {
            let idx = (start_i + m) % deg;
            let ang = 0.5 * PI * (m as f64) / (k as f64);
            let tangent = add(scale(ta, ang.cos()), scale(tb, ang.sin()));
            edge_handles[idx] = add(limit_pos, scale(tangent, 1.0 / 3.0));
            edge_g1_cos[idx] = g1_cos;
        }
    }

    VertexLimitData {
        limit_pos,
        edge_handles,
        edge_g1_cos,
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_crease_sector_handles(
    ring: &VertexRing,
    vertices: &[[f64; 3]],
    p_v: [f64; 3],
    limit_pos: [f64; 3],
    start_edge: usize,
    face_count: usize,
    edge_handles: &mut [[f64; 3]],
    edge_g1_cos: &mut [f64],
) {
    if face_count <= 1 {
        return;
    }
    let deg = ring.edges.len();
    let end_edge = (start_edge + face_count) % deg;
    let ea = vertices[ring.edges[start_edge] as usize];
    let eb = vertices[ring.edges[end_edge] as usize];

    // Exact reflected regular crease formula when face_count == 2.
    if face_count == 2 {
        let mid_idx = (start_edge + 1) % deg;
        let e1 = vertices[ring.edges[mid_idx] as usize];
        let d0 = vertices[ring.diagonals[start_edge % ring.face_count()] as usize];
        let d1 = vertices[ring.diagonals[(start_edge + 1) % ring.face_count()] as usize];
        edge_handles[mid_idx] = scale(
            add(
                add(scale(p_v, 8.0), scale(e1, 4.0)),
                add(add(scale(ea, 2.0), scale(eb, 2.0)), add(d0, d1)),
            ),
            1.0 / 18.0,
        );
        edge_g1_cos[mid_idx] = 0.0;
        return;
    }

    // General smooth crease sector with k >= 3 faces.
    let kf = face_count as f64;
    let t_crease = scale(sub(ea, eb), 0.5);
    let mut t_perp = [0.0; 3];
    for j in 1..face_count {
        let e_idx = (start_edge + j) % deg;
        let ej = vertices[ring.edges[e_idx] as usize];
        let w = 4.0 * (PI * j as f64 / kf).sin();
        t_perp = add(t_perp, scale(sub(ej, p_v), w));
    }
    for j in 0..face_count {
        let d_idx = (start_edge + j) % ring.face_count();
        let dj = vertices[ring.diagonals[d_idx] as usize];
        let w = (PI * j as f64 / kf).sin() + (PI * (j + 1) as f64 / kf).sin();
        t_perp = add(t_perp, scale(sub(dj, p_v), w));
    }
    t_perp = scale(t_perp, 1.0 / (3.0 * kf));

    let g1_cos = 2.0 * (PI / kf).cos();
    for m in 1..face_count {
        let idx = (start_edge + m) % deg;
        let ang = PI * (m as f64) / kf;
        let tangent = add(scale(t_crease, ang.cos()), scale(t_perp, ang.sin()));
        edge_handles[idx] = add(limit_pos, scale(tangent, 1.0 / 3.0));
        edge_g1_cos[idx] = g1_cos;
    }
}

fn extract_regular_bspline_grid(
    v: [u32; 4],
    rings: &[VertexRing],
    vertices: &[[f64; 3]],
) -> [[[f64; 3]; 4]; 4] {
    let mut p = [[[0.0; 3]; 4]; 4];
    p[1][1] = vertices[v[0] as usize];
    p[2][1] = vertices[v[1] as usize];
    p[2][2] = vertices[v[2] as usize];
    p[1][2] = vertices[v[3] as usize];

    // Corner 0 (v0), starting at outgoing edge v0 -> v1
    let r0 = &rings[v[0] as usize];
    let k0 = r0.index_of_neighbor(v[1]).unwrap_or(0);
    p[0][2] = vertices[r0.diagonals[(k0 + 1) % 4] as usize];
    p[0][1] = vertices[r0.edges[(k0 + 2) % 4] as usize];
    p[0][0] = vertices[r0.diagonals[(k0 + 2) % 4] as usize];
    p[1][0] = vertices[r0.edges[(k0 + 3) % 4] as usize];
    p[2][0] = vertices[r0.diagonals[(k0 + 3) % 4] as usize];

    // Corner 1 (v1), starting at outgoing edge v1 -> v2
    let r1 = &rings[v[1] as usize];
    let k1 = r1.index_of_neighbor(v[2]).unwrap_or(0);
    p[3][0] = vertices[r1.diagonals[(k1 + 2) % 4] as usize];
    p[3][1] = vertices[r1.edges[(k1 + 3) % 4] as usize];
    p[3][2] = vertices[r1.diagonals[(k1 + 3) % 4] as usize];

    // Corner 2 (v2), starting at outgoing edge v2 -> v3
    let r2 = &rings[v[2] as usize];
    let k2 = r2.index_of_neighbor(v[3]).unwrap_or(0);
    p[3][3] = vertices[r2.diagonals[(k2 + 2) % 4] as usize];
    p[2][3] = vertices[r2.edges[(k2 + 3) % 4] as usize];
    p[1][3] = vertices[r2.diagonals[(k2 + 3) % 4] as usize];

    // Corner 3 (v3), starting at outgoing edge v3 -> v0
    let r3 = &rings[v[3] as usize];
    let k3 = r3.index_of_neighbor(v[0]).unwrap_or(0);
    p[0][3] = vertices[r3.diagonals[(k3 + 2) % 4] as usize];

    p
}

fn build_extraordinary_or_creased_patch(
    v: [u32; 4],
    rings: &[VertexRing],
    limits: &[VertexLimitData],
    vertices: &[[f64; 3]],
) -> ([[[f64; 3]; 4]; 4], GregoryInterior) {
    let mut b = [[[0.0; 3]; 4]; 4];

    // 4 corner limit positions
    b[0][0] = limits[v[0] as usize].limit_pos;
    b[3][0] = limits[v[1] as usize].limit_pos;
    b[3][3] = limits[v[2] as usize].limit_pos;
    b[0][3] = limits[v[3] as usize].limit_pos;

    // 8 boundary tangent handles (guaranteed identical across shared edges)
    b[1][0] = edge_handle(v[0], v[1], rings, limits);
    b[2][0] = edge_handle(v[1], v[0], rings, limits);

    b[3][1] = edge_handle(v[1], v[2], rings, limits);
    b[3][2] = edge_handle(v[2], v[1], rings, limits);

    b[2][3] = edge_handle(v[2], v[3], rings, limits);
    b[1][3] = edge_handle(v[3], v[2], rings, limits);

    b[0][2] = edge_handle(v[3], v[0], rings, limits);
    b[0][1] = edge_handle(v[0], v[3], rings, limits);

    // 4 initial interior control points via Loop-Schaefer / Coons-consistent interior formula
    let p0 = vertices[v[0] as usize];
    let p1 = vertices[v[1] as usize];
    let p2 = vertices[v[2] as usize];
    let p3 = vertices[v[3] as usize];

    b[1][1] = loop_schaefer_interior(p0, p1, p3, p2, &rings[v[0] as usize], b[0][0], b[1][0], b[0][1]);
    b[2][1] = loop_schaefer_interior(p1, p2, p0, p3, &rings[v[1] as usize], b[3][0], b[3][1], b[2][0]);
    b[2][2] = loop_schaefer_interior(p2, p3, p1, p0, &rings[v[2] as usize], b[3][3], b[2][3], b[3][2]);
    b[1][2] = loop_schaefer_interior(p3, p0, p2, p1, &rings[v[3] as usize], b[0][3], b[0][2], b[1][3]);

    let gregory = GregoryInterior {
        f00_v: b[1][1],
        f00_u: b[1][1],
        f10_v: b[2][1],
        f10_u: b[2][1],
        f11_v: b[2][2],
        f11_u: b[2][2],
        f01_v: b[1][2],
        f01_u: b[1][2],
    };

    (b, gregory)
}

#[allow(clippy::too_many_arguments)]
fn loop_schaefer_interior(
    corner: [f64; 3],
    adj_u: [f64; 3],
    adj_v: [f64; 3],
    diag: [f64; 3],
    ring: &VertexRing,
    b_corner: [f64; 3],
    b_u: [f64; 3],
    b_v: [f64; 3],
) -> [f64; 3] {
    if ring.hard_edge_count() >= 2 {
        // For crease/corner vertices, bilinearly lifting the corner handles keeps the
        // interior patch tangent-aligned with the clamped crease tangents (zero twist distortion).
        let coons = sub(add(b_u, b_v), b_corner);
        let ls = scale(
            add(add(scale(corner, 4.0), scale(add(adj_u, adj_v), 2.0)), diag),
            1.0 / 9.0,
        );
        if ring.hard_edge_count() >= 3 {
            coons
        } else {
            // Smooth blend favoring the exact reflected B-Spline interior for crease corners.
            lerp(coons, ls, 0.25)
        }
    } else {
        let n = ring.edges.len().max(3) as f64;
        scale(
            add(add(scale(corner, n), scale(add(adj_u, adj_v), 2.0)), diag),
            1.0 / (n + 5.0),
        )
    }
}

fn edge_handle(
    from: u32,
    to: u32,
    rings: &[VertexRing],
    limits: &[VertexLimitData],
) -> [f64; 3] {
    let ring = &rings[from as usize];
    let idx = ring.index_of_neighbor(to).unwrap_or(0);
    limits[from as usize].edge_handles[idx]
}

fn edge_g1_cos(
    from: u32,
    to: u32,
    rings: &[VertexRing],
    limits: &[VertexLimitData],
) -> f64 {
    let ring = &rings[from as usize];
    let idx = ring.index_of_neighbor(to).unwrap_or(0);
    limits[from as usize].edge_g1_cos[idx]
}

fn enforce_seam_g1_ribbon(
    patches: &mut [BicubicPatch],
    seam: &PatchSeam,
    rings: &[VertexRing],
    limits: &[VertexLimitData],
) {
    let idx_a = seam.a.patch_id;
    let idx_b = seam.b.patch_id;
    if idx_a == idx_b {
        return;
    }

    let edge_a = seam.a.edge;
    let edge_b = seam.b.edge;

    let v0 = patches[idx_a].corner_vertices[edge_a as usize];
    let v1 = patches[idx_a].corner_vertices[(edge_a as usize + 1) % 4];

    let curve = patches[idx_a].edge_bezier_curve(edge_a);
    let d0 = sub(curve[1], curve[0]);
    let d1 = sub(curve[2], curve[1]);
    let d2 = sub(curve[3], curve[2]);

    let c0 = edge_g1_cos(v0, v1, rings, limits);
    let c1 = -edge_g1_cos(v1, v0, rings, limits);

    let r1_req = scale(add(scale(d0, c1), scale(d1, 2.0 * c0)), 1.0 / 3.0);
    let r2_req = scale(add(scale(d1, 2.0 * c1), scale(d2, c0)), 1.0 / 3.0);

    let (p1_a, p2_a) = get_edge_ribbon_points(&patches[idx_a], edge_a);
    // On patch B, edge_b runs v1 -> v0, so its first ribbon point is near v1 (curve[2])
    // and its second ribbon point is near v0 (curve[1]).
    let (p1_b, p2_b) = get_edge_ribbon_points(&patches[idx_b], edge_b);

    let r1_cur = add(sub(p1_a, curve[1]), sub(p2_b, curve[1]));
    let r2_cur = add(sub(p2_a, curve[2]), sub(p1_b, curve[2]));

    let delta1 = sub(r1_req, r1_cur);
    let delta2 = sub(r2_req, r2_cur);

    let a_is_regular = patches[idx_a].kind == PatchKind::RegularBSpline;
    let b_is_regular = patches[idx_b].kind == PatchKind::RegularBSpline;

    // If one side is an exact RegularBSpline patch, keep it untouched (preserving its exact C2
    // interior) and apply the full ribbon correction to the Gregory side!
    let (w_a, w_b) = match (a_is_regular, b_is_regular) {
        (true, false) => (0.0, 1.0),
        (false, true) => (1.0, 0.0),
        _ => (0.5, 0.5),
    };

    if w_a > 0.0 {
        let new_p1_a = add(p1_a, scale(delta1, w_a));
        let new_p2_a = add(p2_a, scale(delta2, w_a));
        set_edge_ribbon_points(&mut patches[idx_a], edge_a, new_p1_a, new_p2_a);
    }
    if w_b > 0.0 {
        let new_p1_b = add(p1_b, scale(delta2, w_b));
        let new_p2_b = add(p2_b, scale(delta1, w_b));
        set_edge_ribbon_points(&mut patches[idx_b], edge_b, new_p1_b, new_p2_b);
    }
}

fn get_edge_ribbon_points(patch: &BicubicPatch, edge: PatchEdge) -> ([f64; 3], [f64; 3]) {
    if let Some(g) = patch.gregory_interior {
        match edge {
            PatchEdge::Bottom => (g.f00_v, g.f10_v),
            PatchEdge::Right => (g.f10_u, g.f11_u),
            PatchEdge::Top => (g.f11_v, g.f01_v),
            PatchEdge::Left => (g.f01_u, g.f00_u),
        }
    } else {
        let b = &patch.bezier_controls;
        match edge {
            PatchEdge::Bottom => (b[1][1], b[2][1]),
            PatchEdge::Right => (b[2][1], b[2][2]),
            PatchEdge::Top => (b[2][2], b[1][2]),
            PatchEdge::Left => (b[1][2], b[1][1]),
        }
    }
}

fn set_edge_ribbon_points(
    patch: &mut BicubicPatch,
    edge: PatchEdge,
    p1: [f64; 3],
    p2: [f64; 3],
) {
    let g = patch.gregory_interior.get_or_insert_with(|| {
        let b = &patch.bezier_controls;
        GregoryInterior {
            f00_v: b[1][1],
            f00_u: b[1][1],
            f10_v: b[2][1],
            f10_u: b[2][1],
            f11_v: b[2][2],
            f11_u: b[2][2],
            f01_v: b[1][2],
            f01_u: b[1][2],
        }
    });
    match edge {
        PatchEdge::Bottom => {
            g.f00_v = p1;
            g.f10_v = p2;
        }
        PatchEdge::Right => {
            g.f10_u = p1;
            g.f11_u = p2;
        }
        PatchEdge::Top => {
            g.f11_v = p1;
            g.f01_v = p2;
        }
        PatchEdge::Left => {
            g.f01_u = p1;
            g.f00_u = p2;
        }
    }
    // Keep the 4x4 Bézier interior preview synchronized as the midpoint of the split ribbon points.
    patch.bezier_controls[1][1] = lerp(g.f00_v, g.f00_u, 0.5);
    patch.bezier_controls[2][1] = lerp(g.f10_v, g.f10_u, 0.5);
    patch.bezier_controls[2][2] = lerp(g.f11_v, g.f11_u, 0.5);
    patch.bezier_controls[1][2] = lerp(g.f01_v, g.f01_u, 0.5);
}

struct GregoryTermEval {
    val: [f64; 3],
    du: [f64; 3],
    dv: [f64; 3],
    duu: [f64; 3],
    duv: [f64; 3],
    dvv: [f64; 3],
}

/// Evaluate `B_1(u_bar) * B_1(v_bar) * (u_bar * f_v + v_bar * f_u) / (u_bar + v_bar)`
/// and its exact first and second partial derivatives with respect to `(u, v)`.
fn eval_gregory_term(
    u_bar: f64,
    v_bar: f64,
    f_v: [f64; 3],
    f_u: [f64; 3],
    flip_u: bool,
    flip_v: bool,
) -> GregoryTermEval {
    let su = if flip_u { -1.0 } else { 1.0 };
    let sv = if flip_v { -1.0 } else { 1.0 };
    let s = u_bar + v_bar;

    if s <= 1e-12 {
        let avg = lerp(f_v, f_u, 0.5);
        return GregoryTermEval {
            val: [0.0; 3],
            du: [0.0; 3],
            dv: [0.0; 3],
            duu: [0.0; 3],
            duv: scale(avg, 9.0 * su * sv),
            dvv: [0.0; 3],
        };
    }

    let b_u = 3.0 * u_bar * (1.0 - u_bar) * (1.0 - u_bar);
    let db_u = 3.0 * (1.0 - u_bar) * (1.0 - 3.0 * u_bar);
    let d2b_u = 6.0 * (3.0 * u_bar - 2.0);

    let b_v = 3.0 * v_bar * (1.0 - v_bar) * (1.0 - v_bar);
    let db_v = 3.0 * (1.0 - v_bar) * (1.0 - 3.0 * v_bar);
    let d2b_v = 6.0 * (3.0 * v_bar - 2.0);

    let w = b_u * b_v;
    let w_u = db_u * b_v;
    let w_v = b_u * db_v;
    let w_uu = d2b_u * b_v;
    let w_uv = db_u * db_v;
    let w_vv = b_u * d2b_v;

    let diff = sub(f_v, f_u);
    let inv_s = 1.0 / s;
    let inv_s2 = inv_s * inv_s;
    let inv_s3 = inv_s2 * inv_s;

    let g = scale(add(scale(f_v, u_bar), scale(f_u, v_bar)), inv_s);
    let g_u = scale(diff, v_bar * inv_s2);
    let g_v = scale(diff, -u_bar * inv_s2);
    let g_uu = scale(diff, -2.0 * v_bar * inv_s3);
    let g_uv = scale(diff, (u_bar - v_bar) * inv_s3);
    let g_vv = scale(diff, 2.0 * u_bar * inv_s3);

    let val = scale(g, w);
    let du_bar = add(scale(g, w_u), scale(g_u, w));
    let dv_bar = add(scale(g, w_v), scale(g_v, w));
    let duu_bar = add(
        add(scale(g, w_uu), scale(g_u, 2.0 * w_u)),
        scale(g_uu, w),
    );
    let duv_bar = add(
        add(scale(g, w_uv), scale(g_u, w_v)),
        add(scale(g_v, w_u), scale(g_uv, w)),
    );
    let dvv_bar = add(
        add(scale(g, w_vv), scale(g_v, 2.0 * w_v)),
        scale(g_vv, w),
    );

    GregoryTermEval {
        val,
        du: scale(du_bar, su),
        dv: scale(dv_bar, sv),
        duu: duu_bar,
        duv: scale(duv_bar, su * sv),
        dvv: dvv_bar,
    }
}

fn blend_gregory(u_bar: f64, v_bar: f64, f_v: [f64; 3], f_u: [f64; 3]) -> [f64; 3] {
    let s = u_bar + v_bar;
    if s <= 1e-15 {
        lerp(f_v, f_u, 0.5)
    } else {
        scale(add(scale(f_v, u_bar), scale(f_u, v_bar)), 1.0 / s)
    }
}

fn bernstein3(t: f64) -> [f64; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * t * s * s, 3.0 * t * t * s, t * t * t]
}

fn bernstein3_d1(t: f64) -> [f64; 4] {
    let s = 1.0 - t;
    [
        -3.0 * s * s,
        3.0 * s * (1.0 - 3.0 * t),
        3.0 * t * (2.0 - 3.0 * t),
        3.0 * t * t,
    ]
}

fn bernstein3_d2(t: f64) -> [f64; 4] {
    [
        6.0 * (1.0 - t),
        6.0 * (3.0 * t - 2.0),
        6.0 * (1.0 - 3.0 * t),
        6.0 * t,
    ]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], factor: f64) -> [f64; 3] {
    [a[0] * factor, a[1] * factor, a[2] * factor]
}

fn lerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    add(scale(a, 1.0 - t), scale(b, t))
}

fn add_scaled_in_place(acc: &mut [f64; 3], v: [f64; 3], factor: f64) {
    acc[0] += v[0] * factor;
    acc[1] += v[1] * factor;
    acc[2] += v[2] * factor;
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let len = dot(a, a).sqrt();
    if len < 1e-30 {
        [0.0; 3]
    } else {
        scale(a, 1.0 / len)
    }
}

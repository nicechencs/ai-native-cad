//! Watertight boundary pairing and G0/G1/G2 continuity verification for
//! extracted [`NurbsPatchSet`] surfaces.

use serde::{Deserialize, Serialize};

use crate::nurbs_convert::{NurbsPatchSet, PatchKind};

/// Verification thresholds for [`verify_patch_set_continuity`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContinuityTolerances {
    /// Maximum allowable positional gap across any shared seam, in millimeters.
    pub max_g0_gap_mm: f64,
    /// Maximum allowable unit-normal angle deviation across smooth seams, in degrees.
    pub max_smooth_g1_deg: f64,
    /// Maximum allowable relative Gaussian/mean curvature difference across regular smooth seams.
    pub max_regular_g2_rel_diff: f64,
    /// Number of parameter samples evaluated along each shared seam (`>= 3`).
    pub samples_per_seam: usize,
}

impl Default for ContinuityTolerances {
    fn default() -> Self {
        Self {
            max_g0_gap_mm: 1e-6,
            max_smooth_g1_deg: 0.5,
            max_regular_g2_rel_diff: 1e-4,
            samples_per_seam: 9,
        }
    }
}

/// Honest structured audit report for a [`NurbsPatchSet`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuityReport {
    /// True when the patch set is non-empty, has zero open boundary edges, and
    /// satisfies `max_g0_gap_mm <= tolerances.max_g0_gap_mm`.
    pub watertight: bool,
    /// Total number of extracted bicubic patches.
    pub patch_count: usize,
    /// Number of exact `4x4` regular B-Spline patches.
    pub regular_patch_count: usize,
    /// Number of extraordinary / creased Gregory or Bézier patches.
    pub extraordinary_patch_count: usize,
    /// Total number of matched interior seams.
    pub seam_count: usize,
    /// Number of unmatched open boundary edges (`0` for a closed shell).
    pub open_boundary_edges: usize,
    /// Number of hard-creased seams.
    pub creased_edge_count: usize,
    /// Number of smooth (non-creased) interior seams.
    pub smooth_seam_count: usize,
    /// Number of smooth seams shared between two regular B-Spline patches.
    pub regular_smooth_seam_count: usize,
    /// Maximum positional gap measured across all shared seams, in millimeters.
    pub max_g0_gap_mm: f64,
    /// Maximum unit-normal angle deviation across smooth seams, in degrees.
    pub max_smooth_g1_deg: f64,
    /// Maximum relative curvature difference across regular smooth seams.
    pub max_regular_g2_rel_diff: f64,
    /// Minimum dihedral normal angle across hard-creased seams, in degrees (`None` if no creases).
    pub min_creased_dihedral_deg: Option<f64>,
    /// True when `max_smooth_g1_deg <= tolerances.max_smooth_g1_deg`.
    pub g1_smooth_passed: bool,
    /// True when `max_regular_g2_rel_diff <= tolerances.max_regular_g2_rel_diff`.
    pub g2_regular_passed: bool,
}

/// Audit closed-shell boundary pairing and G0/G1/G2 continuity across all seams
/// of a [`NurbsPatchSet`].
pub fn verify_patch_set_continuity(
    patch_set: &NurbsPatchSet,
    tolerances: ContinuityTolerances,
) -> ContinuityReport {
    let samples = tolerances.samples_per_seam.max(3);
    let patch_count = patch_set.patches.len();
    let regular_patch_count = patch_set
        .patches
        .iter()
        .filter(|p| p.kind == PatchKind::RegularBSpline)
        .count();
    let extraordinary_patch_count = patch_count - regular_patch_count;
    let open_boundary_edges = patch_set.open_boundary_edges.len();

    let mut creased_edge_count = 0usize;
    let mut smooth_seam_count = 0usize;
    let mut regular_smooth_seam_count = 0usize;

    let mut max_g0_gap_mm = 0.0f64;
    let mut max_smooth_g1_deg = 0.0f64;
    let mut max_regular_g2_rel_diff = 0.0f64;
    let mut min_creased_dihedral_deg: Option<f64> = None;

    for seam in &patch_set.seams {
        let patch_a = &patch_set.patches[seam.a.patch_id];
        let patch_b = &patch_set.patches[seam.b.patch_id];

        if seam.is_creased {
            creased_edge_count += 1;
        } else {
            smooth_seam_count += 1;
            if seam.both_regular {
                regular_smooth_seam_count += 1;
            }
        }

        for k in 0..samples {
            let t = k as f64 / (samples - 1) as f64;
            let eval_a = patch_a.eval_edge(seam.a.edge, t);
            let eval_b = patch_b.eval_edge(seam.b.edge, 1.0 - t);

            let gap = distance(eval_a.position, eval_b.position);
            if gap > max_g0_gap_mm {
                max_g0_gap_mm = gap;
            }

            let is_interior_sample = k > 0 && k + 1 < samples;
            if seam.is_creased {
                if is_interior_sample {
                    let deg = normal_angle_deg(eval_a.normal, eval_b.normal);
                    min_creased_dihedral_deg = Some(match min_creased_dihedral_deg {
                        Some(prev) => prev.min(deg),
                        None => deg,
                    });
                }
            } else {
                let deg = normal_angle_deg(eval_a.normal, eval_b.normal);
                if deg > max_smooth_g1_deg {
                    max_smooth_g1_deg = deg;
                }

                if seam.both_regular && is_interior_sample {
                    let h_denom = eval_a
                        .mean_curvature
                        .abs()
                        .max(eval_b.mean_curvature.abs())
                        .max(1.0);
                    let h_rel = (eval_a.mean_curvature - eval_b.mean_curvature).abs() / h_denom;

                    let k_denom = eval_a
                        .gaussian_curvature
                        .abs()
                        .max(eval_b.gaussian_curvature.abs())
                        .max(1.0);
                    let k_rel =
                        (eval_a.gaussian_curvature - eval_b.gaussian_curvature).abs() / k_denom;

                    let rel = h_rel.max(k_rel);
                    if rel > max_regular_g2_rel_diff {
                        max_regular_g2_rel_diff = rel;
                    }
                }
            }
        }
    }

    let watertight = patch_count > 0
        && open_boundary_edges == 0
        && max_g0_gap_mm <= tolerances.max_g0_gap_mm;
    let g1_smooth_passed = max_smooth_g1_deg <= tolerances.max_smooth_g1_deg;
    let g2_regular_passed = max_regular_g2_rel_diff <= tolerances.max_regular_g2_rel_diff;

    ContinuityReport {
        watertight,
        patch_count,
        regular_patch_count,
        extraordinary_patch_count,
        seam_count: patch_set.seams.len(),
        open_boundary_edges,
        creased_edge_count,
        smooth_seam_count,
        regular_smooth_seam_count,
        max_g0_gap_mm,
        max_smooth_g1_deg,
        max_regular_g2_rel_diff,
        min_creased_dihedral_deg,
        g1_smooth_passed,
        g2_regular_passed,
    }
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn normal_angle_deg(na: [f64; 3], nb: [f64; 3]) -> f64 {
    let len_a = (na[0] * na[0] + na[1] * na[1] + na[2] * na[2]).sqrt();
    let len_b = (nb[0] * nb[0] + nb[1] * nb[1] + nb[2] * nb[2]).sqrt();
    if len_a < 1e-15 || len_b < 1e-15 {
        return 0.0;
    }
    let cos_theta = ((na[0] * nb[0] + na[1] * nb[1] + na[2] * nb[2]) / (len_a * len_b))
        .clamp(-1.0, 1.0);
    cos_theta.acos().to_degrees()
}

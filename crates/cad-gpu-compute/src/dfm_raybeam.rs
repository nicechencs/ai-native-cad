//! CPU reference implementation of the ray-beam DFM gate.
//!
//! The evaluator intentionally uses the same triangle/LBVH representation as the
//! interference pass, with ROI centroid rebasing in `f64` for large-coordinate
//! invariance and exhaustive per-triangle coverage on faces up to 128 triangles
//! (plus smallest/largest + stratified coverage on larger faces) so localized
//! thin webs cannot slip past sparse area-weighted sampling.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::lbvh::{LbvhTree, TriRef, cross, dot, sub};
use crate::roi::RoiAnchor;

/// Maximum non-degenerate triangles on a single face slot for which every
/// triangle's centroid is unconditionally sampled.
pub(crate) const EXHAUSTIVE_TRIANGLE_SAMPLE_LIMIT: usize = 128;

/// Configuration for the ray-beam DFM pass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DfmConfig {
    pub min_wall_thickness_mm: f64,
    pub min_draft_angle_deg: f64,
    /// Signed pull direction for the mold half. A zero vector disables draft.
    pub pull_dir: [f64; 3],
    /// Candidate outward directions from the surface toward the tool. An
    /// empty list disables accessibility checking.
    pub tool_axes: Vec<[f64; 3]>,
    /// Sampling budget per face slot, with at least one sample per instance
    /// contributing nondegenerate triangles to that slot.
    pub samples_per_face: usize,
}

/// The DFM conditions reported for an individual topological face slot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DfmViolationKind {
    ThinWall,
    DraftAngle,
    ToolAccessibility,
}

/// Aggregated DFM metrics for a `face_slot_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceDfmMetrics {
    pub face_slot_id: u32,
    pub min_thickness_mm: Option<f64>,
    pub draft_angle_deg: Option<f64>,
    pub accessible: bool,
    pub violations: Vec<DfmViolationKind>,
}

/// Results of evaluating all face slots in an input triangle set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DfmReport {
    pub faces: Vec<FaceDfmMetrics>,
}

impl DfmReport {
    /// Returns true when every evaluated face passes all configured gates.
    pub fn is_clear(&self) -> bool {
        self.faces.iter().all(|face| face.violations.is_empty())
    }

    /// Number of face slots that have at least one violation.
    pub fn violation_count(&self) -> usize {
        self.faces
            .iter()
            .filter(|face| !face.violations.is_empty())
            .count()
    }
}

#[derive(Clone, Copy)]
struct Sample {
    position: [f64; 3],
    normal: [f64; 3],
    triangle_index: usize,
    instance_id: u32,
}

/// Evaluate wall thickness, draft, and tool accessibility for every face slot.
///
/// Triangles use outward winding and are automatically rebased to their `f64`
/// ROI centroid before building the LBVH and casting rays. Thickness and tool
/// rays test the sampled instance, so other assembly instances do not affect
/// part-local metrics. Every sample must have at least one unobstructed outward
/// tool axis. Draft takes the minimum signed angle across all valid triangles
/// of the slot.
pub fn evaluate_raybeam_dfm(tris: &[TriRef], config: &DfmConfig) -> DfmReport {
    if tris.is_empty() {
        return DfmReport::default();
    }

    let anchor = RoiAnchor::from_triangles(tris);
    let local_tris = anchor.rebase_triangles(tris);
    let tree = LbvhTree::build(&local_tris);
    let geometry: Vec<_> = local_tris.iter().map(triangle_geometry).collect();
    let normalized_pull = normalize(config.pull_dir);
    let tool_axes = config
        .tool_axes
        .iter()
        .copied()
        .map(normalize)
        .filter(|axis| axis != &[0.0, 0.0, 0.0])
        .collect::<Vec<_>>();

    let mut slots: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (index, tri) in local_tris.iter().enumerate() {
        slots.entry(tri.face_slot_id).or_default().push(index);
    }

    let mut faces = Vec::with_capacity(slots.len());
    for (face_slot_id, triangle_indices) in slots {
        let sample_plan = select_face_samples(
            &local_tris,
            &geometry,
            &triangle_indices,
            config.samples_per_face,
        );
        let samples: Vec<Sample> = sample_plan
            .into_iter()
            .map(|(triangle_index, bary)| sample_at(&local_tris, &geometry, triangle_index, bary))
            .collect();
        let draft_angle_deg = if normalized_pull == [0.0, 0.0, 0.0] {
            None
        } else {
            triangle_indices
                .iter()
                .filter_map(|&triangle_index| {
                    let (normal, _) = geometry[triangle_index];
                    (normal != [0.0, 0.0, 0.0]).then(|| {
                        dot(normal, normalized_pull)
                            .clamp(-1.0, 1.0)
                            .asin()
                            .to_degrees()
                    })
                })
                .reduce(f64::min)
        };

        let mut min_thickness_mm = None;
        let mut accessible = !samples.is_empty();
        for sample in &samples {
            if let Some(distance_mm) = first_opposite_wall_hit(&tree, &geometry, *sample) {
                min_thickness_mm = Some(
                    min_thickness_mm.map_or(distance_mm, |current: f64| current.min(distance_mm)),
                );
            }

            if !config.tool_axes.is_empty() && accessible {
                accessible = has_accessible_tool_axis(&tree, *sample, &tool_axes);
            }
        }

        let mut violations = Vec::new();
        if min_thickness_mm.is_some_and(|thickness| thickness < config.min_wall_thickness_mm) {
            violations.push(DfmViolationKind::ThinWall);
        }
        if draft_angle_deg.is_some_and(|angle| angle < config.min_draft_angle_deg) {
            violations.push(DfmViolationKind::DraftAngle);
        }
        if !accessible {
            violations.push(DfmViolationKind::ToolAccessibility);
        }

        faces.push(FaceDfmMetrics {
            face_slot_id,
            min_thickness_mm,
            draft_angle_deg,
            accessible,
            violations,
        });
    }

    DfmReport { faces }
}

/// Deterministic face-slot sampling plan shared by both the CPU LBVH evaluator
/// and the WebGPU compute runner for exact backend parity.
///
/// Guarantees:
/// - When a face slot has at most `EXHAUSTIVE_TRIANGLE_SAMPLE_LIMIT` (128)
///   non-degenerate triangles, every triangle's centroid is sampled so tiny
///   localized thin webs or ribs cannot slip past area-weighted sampling.
/// - On larger tessellations (> 128 triangles), both the largest and smallest
///   non-degenerate triangles per instance are sampled plus stratified
///   index-spaced centroids and area-weighted low-discrepancy interior points.
pub(crate) fn select_face_samples(
    tris: &[TriRef],
    geometry: &[([f64; 3], f64)],
    triangle_indices: &[usize],
    samples_per_face: usize,
) -> Vec<(usize, (f64, f64))> {
    let mut cumulative = Vec::with_capacity(triangle_indices.len());
    let mut nondegenerate = Vec::with_capacity(triangle_indices.len());
    let mut total_area = 0.0;
    let mut largest_by_instance: BTreeMap<u32, (usize, f64)> = BTreeMap::new();
    let mut smallest_by_instance: BTreeMap<u32, (usize, f64)> = BTreeMap::new();

    for &triangle_index in triangle_indices {
        let (_, area) = geometry[triangle_index];
        if area == 0.0 {
            continue;
        }
        total_area += area;
        cumulative.push((triangle_index, total_area));
        nondegenerate.push(triangle_index);
        let inst = tris[triangle_index].instance_id;
        largest_by_instance
            .entry(inst)
            .and_modify(|current| {
                if area > current.1 {
                    *current = (triangle_index, area);
                }
            })
            .or_insert((triangle_index, area));
        smallest_by_instance
            .entry(inst)
            .and_modify(|current| {
                if area < current.1 {
                    *current = (triangle_index, area);
                }
            })
            .or_insert((triangle_index, area));
    }
    if cumulative.is_empty() {
        return Vec::new();
    }

    let mut samples = Vec::new();
    if nondegenerate.len() <= EXHAUSTIVE_TRIANGLE_SAMPLE_LIMIT {
        for &triangle_index in &nondegenerate {
            samples.push((triangle_index, (1.0 / 3.0, 1.0 / 3.0)));
        }
    } else {
        for (&inst, &(largest_idx, _)) in &largest_by_instance {
            samples.push((largest_idx, (1.0 / 3.0, 1.0 / 3.0)));
            if let Some(&(smallest_idx, _)) = smallest_by_instance.get(&inst)
                && smallest_idx != largest_idx
            {
                samples.push((smallest_idx, (1.0 / 3.0, 1.0 / 3.0)));
            }
        }
        let stratified_target = samples_per_face.max(32).min(nondegenerate.len());
        for step in 0..stratified_target {
            let pos = (step * nondegenerate.len()) / stratified_target;
            samples.push((nondegenerate[pos], (1.0 / 3.0, 1.0 / 3.0)));
        }
    }

    let budget = samples_per_face.max(1).max(samples.len());
    let remaining = budget - samples.len();
    for index in 0..remaining {
        let target = ((index as f64 + 0.5) / remaining as f64) * total_area;
        let entry = cumulative.partition_point(|&(_, end)| end <= target);
        let triangle_index = cumulative[entry.min(cumulative.len() - 1)].0;
        samples.push((triangle_index, barycentric_sample(index)));
    }
    samples
}

fn sample_at(
    tris: &[TriRef],
    geometry: &[([f64; 3], f64)],
    triangle_index: usize,
    (u, v): (f64, f64),
) -> Sample {
    let tri = &tris[triangle_index];
    Sample {
        position: add(
            tri.a,
            add(scale(sub(tri.b, tri.a), u), scale(sub(tri.c, tri.a), v)),
        ),
        normal: geometry[triangle_index].0,
        triangle_index,
        instance_id: tri.instance_id,
    }
}

fn barycentric_sample(index: usize) -> (f64, f64) {
    // A low-discrepancy triangular sequence gives stable coverage without a
    // random number generator, and reflects samples back into the simplex.
    let mut u = ((index as f64 + 0.5) * 0.618_033_988_749_894_9).fract();
    let mut v = ((index as f64 + 0.5) * 0.414_213_562_373_095).fract();
    if u + v > 1.0 {
        u = 1.0 - u;
        v = 1.0 - v;
    }
    (u, v)
}

fn first_opposite_wall_hit(
    tree: &LbvhTree,
    geometry: &[([f64; 3], f64)],
    sample: Sample,
) -> Option<f64> {
    let direction = scale(sample.normal, -1.0);
    tree.cast_ray_nearest_where(sample.position, direction, |triangle_index, hit_tri, _| {
        triangle_index != sample.triangle_index
            && hit_tri.instance_id == sample.instance_id
            && dot(geometry[triangle_index].0, sample.normal) < 0.0
    })
    .map(|hit| hit.distance_mm)
}

fn has_accessible_tool_axis(tree: &LbvhTree, sample: Sample, tool_axes: &[[f64; 3]]) -> bool {
    tool_axes.iter().copied().any(|axis| {
        dot(axis, sample.normal) > 0.0
            && tree
                .cast_ray_nearest_where(sample.position, axis, |triangle_index, hit_tri, _| {
                    triangle_index != sample.triangle_index
                        && hit_tri.instance_id == sample.instance_id
                })
                .is_none()
    })
}

fn triangle_geometry(tri: &TriRef) -> ([f64; 3], f64) {
    let normal = cross(sub(tri.b, tri.a), sub(tri.c, tri.a));
    let length = magnitude(normal);
    if length > 0.0 && length.is_finite() {
        (normal.map(|value| value / length), length * 0.5)
    } else {
        ([0.0; 3], 0.0)
    }
}

fn normalize(vector: [f64; 3]) -> [f64; 3] {
    let length = magnitude(vector);
    if length > 0.0 && length.is_finite() {
        vector.map(|value| value / length)
    } else {
        [0.0; 3]
    }
}

fn magnitude([x, y, z]: [f64; 3]) -> f64 {
    x.hypot(y).hypot(z)
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(vector: [f64; 3], factor: f64) -> [f64; 3] {
    [vector[0] * factor, vector[1] * factor, vector[2] * factor]
}

//! Contour topology, closure, orientation, and self-intersection verification.
//!
//! Validates that every sliced layer contour is:
//! - 100% closed and non-degenerate (finite coordinates, >= 3 distinct vertices,
//!   non-zero area, no zero-length edges),
//! - properly oriented (CCW outer shells with positive area, CW inner voids
//!   with negative area), and
//! - strictly free of self-intersections and cross-contour intersections,
//!   accelerated via 2D uniform-grid spatial binning.

use std::collections::{HashMap, HashSet};

use crate::{ContourLoop, SliceLayer, SliceStack};

/// Classification of a contour topology or geometry violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContourViolationKind {
    OpenContour,
    TooFewVertices,
    NonFiniteVertex,
    DegenerateEdge,
    ZeroArea,
    OrientationMismatch,
    SelfIntersection,
    CrossContourIntersection,
}

/// A single contour validation failure detected on a slice layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContourViolation {
    pub layer_index: usize,
    pub contour_index: usize,
    pub other_contour_index: Option<usize>,
    pub segment_a: usize,
    pub segment_b: Option<usize>,
    pub kind: ContourViolationKind,
    pub detail: String,
}

/// Validation report for a single [`SliceLayer`].
#[derive(Debug, Clone, PartialEq)]
pub struct LayerVerificationReport {
    pub layer_index: usize,
    pub z_mm: f64,
    pub contour_count: usize,
    pub outer_contour_count: usize,
    pub hole_contour_count: usize,
    pub segment_count: usize,
    pub violations: Vec<ContourViolation>,
}

impl LayerVerificationReport {
    /// Returns `true` when every contour on the layer passed all checks.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Aggregated validation report across an entire [`SliceStack`].
#[derive(Debug, Clone, PartialEq)]
pub struct StackVerificationReport {
    pub layer_reports: Vec<LayerVerificationReport>,
    pub total_contours: usize,
    pub total_segments: usize,
    pub total_violations: usize,
}

impl StackVerificationReport {
    /// Returns `true` when 100% of layers and contours are valid.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.total_violations == 0
    }
}

#[derive(Clone, Copy)]
struct SegEntry {
    contour_index: usize,
    seg_index: usize,
    contour_len: usize,
    a: [f64; 2],
    b: [f64; 2],
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

/// Verifies closure, non-degeneracy, winding orientation, and strict absence of
/// self-intersections / cross-contour intersections on a single [`SliceLayer`].
#[must_use]
pub fn verify_layer_contours(layer: &SliceLayer, tol_mm: f64) -> LayerVerificationReport {
    let eps = tol_mm.abs().max(1e-12);
    let mut violations = Vec::new();
    let mut outer_contour_count = 0usize;
    let mut hole_contour_count = 0usize;
    let mut segments: Vec<SegEntry> = Vec::new();

    for (contour_index, contour) in layer.loops.iter().enumerate() {
        if contour.is_hole {
            hole_contour_count += 1;
        } else {
            outer_contour_count += 1;
        }

        if !contour.closed {
            violations.push(ContourViolation {
                layer_index: layer.layer_index,
                contour_index,
                other_contour_index: None,
                segment_a: 0,
                segment_b: None,
                kind: ContourViolationKind::OpenContour,
                detail: "contour is not marked closed".to_owned(),
            });
            continue;
        }

        let pts = canonical_closed_points(contour, eps);
        if pts.len() < 3 {
            violations.push(ContourViolation {
                layer_index: layer.layer_index,
                contour_index,
                other_contour_index: None,
                segment_a: 0,
                segment_b: None,
                kind: ContourViolationKind::TooFewVertices,
                detail: format!("closed contour has only {} unique vertices", pts.len()),
            });
            continue;
        }

        let mut has_non_finite = false;
        for (vi, &[x, y]) in pts.iter().enumerate() {
            if !x.is_finite() || !y.is_finite() {
                violations.push(ContourViolation {
                    layer_index: layer.layer_index,
                    contour_index,
                    other_contour_index: None,
                    segment_a: vi,
                    segment_b: None,
                    kind: ContourViolationKind::NonFiniteVertex,
                    detail: format!("non-finite vertex at index {vi}"),
                });
                has_non_finite = true;
                break;
            }
        }
        if has_non_finite {
            continue;
        }

        let area = signed_area(pts);
        if !area.is_finite() || area.abs() <= eps * eps * 0.25 {
            violations.push(ContourViolation {
                layer_index: layer.layer_index,
                contour_index,
                other_contour_index: None,
                segment_a: 0,
                segment_b: None,
                kind: ContourViolationKind::ZeroArea,
                detail: format!("contour has degenerate signed area {area}"),
            });
        } else if (area < 0.0) != contour.is_hole
            || (area - contour.signed_area_mm2).abs() > area.abs().max(1.0) * 1e-6
        {
            violations.push(ContourViolation {
                layer_index: layer.layer_index,
                contour_index,
                other_contour_index: None,
                segment_a: 0,
                segment_b: None,
                kind: ContourViolationKind::OrientationMismatch,
                detail: format!(
                    "winding mismatch: computed area={area}, stored={}, is_hole={}",
                    contour.signed_area_mm2, contour.is_hole
                ),
            });
        }

        let n = pts.len();
        for seg_index in 0..n {
            let a = pts[seg_index];
            let b = pts[(seg_index + 1) % n];
            let len = (b[0] - a[0]).hypot(b[1] - a[1]);
            if len <= eps {
                violations.push(ContourViolation {
                    layer_index: layer.layer_index,
                    contour_index,
                    other_contour_index: None,
                    segment_a: seg_index,
                    segment_b: None,
                    kind: ContourViolationKind::DegenerateEdge,
                    detail: format!("edge {seg_index} has degenerate length {len}"),
                });
                continue;
            }
            segments.push(SegEntry {
                contour_index,
                seg_index,
                contour_len: n,
                a,
                b,
                min_x: a[0].min(b[0]),
                max_x: a[0].max(b[0]),
                min_y: a[1].min(b[1]),
                max_y: a[1].max(b[1]),
            });
        }
    }

    check_segment_intersections(layer.layer_index, &segments, eps, &mut violations);

    LayerVerificationReport {
        layer_index: layer.layer_index,
        z_mm: layer.z_mm,
        contour_count: layer.loops.len(),
        outer_contour_count,
        hole_contour_count,
        segment_count: segments.len(),
        violations,
    }
}

/// Verifies every layer and contour in a [`SliceStack`].
#[must_use]
pub fn verify_stack_contours(stack: &SliceStack, tol_mm: f64) -> StackVerificationReport {
    let mut layer_reports = Vec::with_capacity(stack.layers.len());
    let mut total_contours = 0usize;
    let mut total_segments = 0usize;
    let mut total_violations = 0usize;

    for layer in &stack.layers {
        let report = verify_layer_contours(layer, tol_mm);
        total_contours += report.contour_count;
        total_segments += report.segment_count;
        total_violations += report.violations.len();
        layer_reports.push(report);
    }

    StackVerificationReport {
        layer_reports,
        total_contours,
        total_segments,
        total_violations,
    }
}

fn canonical_closed_points(contour: &ContourLoop, eps: f64) -> &[[f64; 2]] {
    let pts = contour.points_mm.as_slice();
    if pts.len() > 1 {
        let first = pts[0];
        let last = pts[pts.len() - 1];
        if (first[0] - last[0]).hypot(first[1] - last[1]) <= eps {
            return &pts[..pts.len() - 1];
        }
    }
    pts
}

fn check_segment_intersections(
    layer_index: usize,
    segments: &[SegEntry],
    eps: f64,
    violations: &mut Vec<ContourViolation>,
) {
    if segments.len() < 2 {
        return;
    }

    // Choose a uniform spatial bin size based on average segment span.
    let mut total_span = 0.0;
    let mut global_min_x = f64::INFINITY;
    let mut global_min_y = f64::INFINITY;
    for seg in segments {
        total_span += (seg.max_x - seg.min_x).max(seg.max_y - seg.min_y);
        global_min_x = global_min_x.min(seg.min_x);
        global_min_y = global_min_y.min(seg.min_y);
    }
    let avg_span = (total_span / segments.len() as f64).max(eps * 16.0);
    let cell_size = (avg_span * 2.0).max(1e-6);

    let mut bins: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (idx, seg) in segments.iter().enumerate() {
        let x0 = ((seg.min_x - global_min_x) / cell_size).floor() as i64;
        let x1 = ((seg.max_x - global_min_x) / cell_size).floor() as i64;
        let y0 = ((seg.min_y - global_min_y) / cell_size).floor() as i64;
        let y1 = ((seg.max_y - global_min_y) / cell_size).floor() as i64;
        // Guard against an unusually long edge spanning too many bins.
        let dx = (x1 - x0).clamp(0, 64);
        let dy = (y1 - y0).clamp(0, 64);
        for gx in x0..=(x0 + dx) {
            for gy in y0..=(y0 + dy) {
                bins.entry((gx, gy)).or_default().push(idx);
            }
        }
    }

    let mut tested_pairs: HashSet<(usize, usize)> = HashSet::new();
    for bucket in bins.values() {
        if bucket.len() < 2 {
            continue;
        }
        for bi in 0..bucket.len() {
            for bj in (bi + 1)..bucket.len() {
                let i = bucket[bi].min(bucket[bj]);
                let j = bucket[bi].max(bucket[bj]);
                if !tested_pairs.insert((i, j)) {
                    continue;
                }
                let sa = segments[i];
                let sb = segments[j];

                // Fast AABB reject.
                if sa.max_x + eps < sb.min_x
                    || sb.max_x + eps < sa.min_x
                    || sa.max_y + eps < sb.min_y
                    || sb.max_y + eps < sa.min_y
                {
                    continue;
                }

                // Skip adjacent edges within the same closed contour.
                if sa.contour_index == sb.contour_index {
                    let n = sa.contour_len;
                    if (sa.seg_index + 1) % n == sb.seg_index
                        || (sb.seg_index + 1) % n == sa.seg_index
                    {
                        continue;
                    }
                }

                if segments_intersect(sa.a, sa.b, sb.a, sb.b, eps) {
                    let (kind, other) = if sa.contour_index == sb.contour_index {
                        (ContourViolationKind::SelfIntersection, None)
                    } else {
                        (
                            ContourViolationKind::CrossContourIntersection,
                            Some(sb.contour_index),
                        )
                    };
                    violations.push(ContourViolation {
                        layer_index,
                        contour_index: sa.contour_index,
                        other_contour_index: other,
                        segment_a: sa.seg_index,
                        segment_b: Some(sb.seg_index),
                        kind,
                        detail: format!(
                            "segment ({}, {}) intersects segment ({}, {})",
                            sa.contour_index, sa.seg_index, sb.contour_index, sb.seg_index
                        ),
                    });
                }
            }
        }
    }
}

fn segments_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2], eps: f64) -> bool {
    let o1 = orient2d(a, b, c);
    let o2 = orient2d(a, b, d);
    let o3 = orient2d(c, d, a);
    let o4 = orient2d(c, d, b);

    let len_ab = (b[0] - a[0]).hypot(b[1] - a[1]);
    let len_cd = (d[0] - c[0]).hypot(d[1] - c[1]);
    let tol_ab = eps * len_ab;
    let tol_cd = eps * len_cd;

    let s1 = sign_with_tol(o1, tol_ab);
    let s2 = sign_with_tol(o2, tol_ab);
    let s3 = sign_with_tol(o3, tol_cd);
    let s4 = sign_with_tol(o4, tol_cd);

    // Proper crossing.
    if s1 * s2 < 0 && s3 * s4 < 0 {
        return true;
    }

    // Collinear or endpoint touching.
    if s1 == 0 && point_on_segment(a, b, c, eps) {
        return true;
    }
    if s2 == 0 && point_on_segment(a, b, d, eps) {
        return true;
    }
    if s3 == 0 && point_on_segment(c, d, a, eps) {
        return true;
    }
    if s4 == 0 && point_on_segment(c, d, b, eps) {
        return true;
    }

    false
}

fn orient2d(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn sign_with_tol(value: f64, tol: f64) -> i8 {
    if value > tol {
        1
    } else if value < -tol {
        -1
    } else {
        0
    }
}

fn point_on_segment(a: [f64; 2], b: [f64; 2], p: [f64; 2], eps: f64) -> bool {
    p[0] >= a[0].min(b[0]) - eps
        && p[0] <= a[0].max(b[0]) + eps
        && p[1] >= a[1].min(b[1]) - eps
        && p[1] <= a[1].max(b[1]) + eps
}

fn signed_area(points: &[[f64; 2]]) -> f64 {
    let [x, y] = points[0];
    let twice_area: f64 = points
        .windows(2)
        .map(|pair| (pair[0][0] - x) * (pair[1][1] - y) - (pair[1][0] - x) * (pair[0][1] - y))
        .sum();
    twice_area * 0.5
}

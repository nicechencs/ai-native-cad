//! Kernel-independent geometric invariants used to rebind topology references.
//!
//! The types in this module deliberately contain only serializable, kernel-neutral
//! data.  A kernel adapter is responsible for producing a fingerprint from its
//! native entity representation.

use serde::{Deserialize, Serialize};

/// The topological dimension of the entity described by a fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EntityKind {
    Face,
    Edge,
    Vertex,
}

/// Broad classes of face support surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SurfaceClass {
    Plane,
    Cylinder,
    Cone,
    Sphere,
    Torus,
    BSpline,
    SubD,
    Other,
}

/// Broad classes of edge support curves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CurveClass {
    Line,
    Circle,
    Ellipse,
    BSpline,
    Other,
}

/// A kernel-independent description of an entity's geometric invariants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometricFingerprint {
    pub entity_kind: EntityKind,
    pub surface_class: Option<SurfaceClass>,
    pub curve_class: Option<CurveClass>,
    pub centroid_mm: [f64; 3],
    pub normal_or_axis: Option<[f64; 3]>,
    pub area_or_length_mm: f64,
    pub radius_mm: Option<f64>,
    pub min_curvature: Option<f64>,
    pub max_curvature: Option<f64>,
    /// Neighboring support surface classes in canonical sorted order.
    pub adjacent_signature: Vec<SurfaceClass>,
}

/// Thresholds used by geometric matching and deterministic resolver tie checks.
///
/// Geometric tolerances control how differences affect each component score.
/// `min_score` and `ambiguity_margin` control acceptance in the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MatchTolerances {
    /// Absolute centroid tolerance in millimetres.
    pub centroid_tolerance_mm: f64,
    /// Relative centroid tolerance against a characteristic geometry scale.
    pub relative_centroid_tolerance: f64,
    /// Relative tolerance around an area or length ratio.
    pub area_relative_tolerance: f64,
    /// Absolute radius tolerance in millimetres.
    pub radius_tolerance_mm: f64,
    /// Absolute curvature tolerance in inverse millimetres.
    pub curvature_tolerance: f64,
    /// Normal/axis angular tolerance in degrees.
    pub normal_angle_tolerance_deg: f64,
    /// Minimum score accepted by lineage fallback matching.
    pub min_score: f64,
    /// Minimum score separation required to choose a winner.
    pub ambiguity_margin: f64,
    /// Absolute epsilon in the units of each query tie-breaker's metric.
    pub query_epsilon: f64,
}

impl Default for MatchTolerances {
    fn default() -> Self {
        Self {
            centroid_tolerance_mm: 1.0e-3,
            relative_centroid_tolerance: 0.10,
            area_relative_tolerance: 0.01,
            radius_tolerance_mm: 1.0e-3,
            curvature_tolerance: 1.0e-6,
            normal_angle_tolerance_deg: 1.0,
            min_score: 0.70,
            ambiguity_margin: 0.05,
            query_epsilon: 1.0e-9,
        }
    }
}

/// Computes a deterministic similarity in the inclusive range `0.0..=1.0`.
///
/// Entity kinds and explicit geometric classes are hard compatibility constraints.
/// All other available invariants contribute weighted scores. Values absent from
/// both fingerprints are omitted from the denominator, while a value present on
/// only one side is treated as a mismatch.
pub fn fingerprint_similarity(
    a: &GeometricFingerprint,
    b: &GeometricFingerprint,
    tolerances: &MatchTolerances,
) -> f64 {
    if a.entity_kind != b.entity_kind
        || classes_conflict(a.surface_class, b.surface_class)
        || classes_conflict(a.curve_class, b.curve_class)
        || !is_valid_fingerprint(a)
        || !is_valid_fingerprint(b)
    {
        return 0.0;
    }

    let rotational_axis = matches!(
        a.surface_class.or(b.surface_class),
        Some(SurfaceClass::Cylinder | SurfaceClass::Cone | SurfaceClass::Torus)
    );
    let normal_score = optional_score(a.normal_or_axis, b.normal_or_axis, |left, right| {
        normal_alignment_score(
            left,
            right,
            rotational_axis,
            tolerances.normal_angle_tolerance_deg,
        )
    });
    let measure_score = relative_value_score(
        a.area_or_length_mm,
        b.area_or_length_mm,
        tolerances.area_relative_tolerance,
    );
    let radius_score = optional_score(a.radius_mm, b.radius_mm, |left, right| {
        absolute_relative_score(left, right, tolerances.radius_tolerance_mm)
    });
    let curvature_score =
        |left, right| absolute_relative_score(left, right, tolerances.curvature_tolerance);
    let components = [
        (
            0.12,
            optional_score(a.surface_class, b.surface_class, |_, _| 1.0),
        ),
        (
            0.08,
            optional_score(a.curve_class, b.curve_class, |_, _| 1.0),
        ),
        (0.25, Some(centroid_score(a, b, tolerances))),
        (0.15, normal_score),
        (0.20, Some(measure_score)),
        (0.08, radius_score),
        (
            0.035,
            optional_score(a.min_curvature, b.min_curvature, curvature_score),
        ),
        (
            0.035,
            optional_score(a.max_curvature, b.max_curvature, curvature_score),
        ),
        (
            0.05,
            Some(adjacency_overlap(
                &a.adjacent_signature,
                &b.adjacent_signature,
            )),
        ),
    ];
    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;
    for (weight, score) in components {
        if let Some(score) = score {
            weighted_sum += score * weight;
            weight_sum += weight;
        }
    }
    (weighted_sum / weight_sum).clamp(0.0, 1.0)
}

fn classes_conflict<T: Eq>(a: Option<T>, b: Option<T>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a != b)
}

fn optional_score<T>(a: Option<T>, b: Option<T>, score: impl FnOnce(T, T) -> f64) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (Some(a), Some(b)) => Some(score(a, b)),
        (Some(_), None) | (None, Some(_)) => Some(0.0),
    }
}

fn is_valid_fingerprint(fingerprint: &GeometricFingerprint) -> bool {
    if fingerprint
        .centroid_mm
        .iter()
        .any(|value| !value.is_finite())
        || !fingerprint.area_or_length_mm.is_finite()
        || fingerprint.area_or_length_mm < 0.0
    {
        return false;
    }
    if fingerprint.normal_or_axis.is_some_and(|axis| {
        axis == [0.0; 3] || axis.into_iter().any(|component| !component.is_finite())
    }) {
        return false;
    }
    if fingerprint
        .radius_mm
        .is_some_and(|radius| !radius.is_finite() || radius < 0.0)
    {
        return false;
    }
    if fingerprint
        .min_curvature
        .is_some_and(|curvature| !curvature.is_finite())
        || fingerprint
            .max_curvature
            .is_some_and(|curvature| !curvature.is_finite())
    {
        return false;
    }
    true
}

fn characteristic_length(fingerprint: &GeometricFingerprint) -> f64 {
    let measure_length = match fingerprint.entity_kind {
        EntityKind::Face => fingerprint.area_or_length_mm.sqrt(),
        EntityKind::Edge => fingerprint.area_or_length_mm,
        EntityKind::Vertex => 0.0,
    };
    measure_length
        .max(fingerprint.radius_mm.unwrap_or(0.0))
        .max(1.0)
}

fn centroid_score(
    a: &GeometricFingerprint,
    b: &GeometricFingerprint,
    tolerances: &MatchTolerances,
) -> f64 {
    let Some(distance) = euclidean_distance(a.centroid_mm, b.centroid_mm) else {
        return 0.0;
    };
    let absolute_tolerance = nonnegative_tolerance(tolerances.centroid_tolerance_mm);
    let relative_tolerance = nonnegative_tolerance(tolerances.relative_centroid_tolerance);
    let scale = characteristic_length(a)
        .max(characteristic_length(b))
        .max(absolute_tolerance);
    let relative_distance =
        ((distance - absolute_tolerance).max(0.0) / scale) / (1.0 + relative_tolerance);
    (1.0 - relative_distance).clamp(0.0, 1.0)
}

fn normal_alignment_score(
    a: [f64; 3],
    b: [f64; 3],
    sign_independent: bool,
    angle_tolerance_deg: f64,
) -> f64 {
    let Some((a, b)) = normalize_vector(a).zip(normalize_vector(b)) else {
        return 0.0;
    };
    let dot = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| x * y)
        .sum::<f64>()
        .clamp(-1.0, 1.0);
    let cross = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let cross_magnitude = cross[0].hypot(cross[1]).hypot(cross[2]);
    let angle_deg = cross_magnitude
        .atan2(if sign_independent { dot.abs() } else { dot })
        .to_degrees();
    let tolerance = nonnegative_tolerance(angle_tolerance_deg);
    if tolerance == 0.0 {
        return u8::from(angle_deg == 0.0).into();
    }
    (-angle_deg / tolerance).exp()
}

fn relative_value_score(a: f64, b: f64, tolerance: f64) -> f64 {
    if a == b {
        return 1.0;
    }
    let relative_delta = (a - b).abs() / a.max(b);
    let tolerance = nonnegative_tolerance(tolerance);
    1.0 - (relative_delta - tolerance).max(0.0)
}

fn absolute_relative_score(a: f64, b: f64, tolerance: f64) -> f64 {
    let tolerance = nonnegative_tolerance(tolerance);
    let delta = (a - b).abs();
    if delta <= tolerance {
        return 1.0;
    }
    let scale = a.abs().max(b.abs());
    (1.0 - (delta - tolerance) / scale).clamp(0.0, 1.0)
}

fn nonnegative_tolerance(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn adjacency_overlap(a: &[SurfaceClass], b: &[SurfaceClass]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let mut a_sorted = a.to_vec();
    let mut b_sorted = b.to_vec();
    a_sorted.sort_unstable();
    b_sorted.sort_unstable();
    let mut a_index = 0;
    let mut b_index = 0;
    let mut intersection = 0usize;
    while a_index < a_sorted.len() && b_index < b_sorted.len() {
        match a_sorted[a_index].cmp(&b_sorted[b_index]) {
            std::cmp::Ordering::Equal => {
                intersection += 1;
                a_index += 1;
                b_index += 1;
            }
            std::cmp::Ordering::Less => a_index += 1,
            std::cmp::Ordering::Greater => b_index += 1,
        }
    }
    let union = a_sorted.len() + b_sorted.len() - intersection;
    intersection as f64 / union as f64
}

/// Returns a normalized vector, or `None` for non-finite or zero-length input.
pub(crate) fn normalize_vector(vector: [f64; 3]) -> Option<[f64; 3]> {
    if vector.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let scale = vector
        .iter()
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);
    if scale <= 0.0 {
        return None;
    }
    let scaled = vector.map(|value| value / scale);
    let scaled_norm = scaled.iter().map(|value| value * value).sum::<f64>().sqrt();
    Some(scaled.map(|value| value / scaled_norm))
}

/// Computes Euclidean distance if the result is finite.
pub(crate) fn euclidean_distance(a: [f64; 3], b: [f64; 3]) -> Option<f64> {
    let distance = (a[0] - b[0]).hypot(a[1] - b[1]).hypot(a[2] - b[2]);
    distance.is_finite().then_some(distance)
}

pub(crate) fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

pub(crate) fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

pub(crate) fn sub(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

pub(crate) fn vector_norm(vector: [f64; 3]) -> f64 {
    vector[0].hypot(vector[1]).hypot(vector[2])
}

/// Directed angle in degrees, in `0.0..=180.0`.
pub(crate) fn directed_angle_deg(left: [f64; 3], right: [f64; 3]) -> Option<f64> {
    let (left, right) = normalize_vector(left).zip(normalize_vector(right))?;
    Some(tidy_angle(
        dot(left, right).clamp(-1.0, 1.0).acos().to_degrees(),
    ))
}

/// Smaller angle between two undirected axes, in `0.0..=90.0`.
pub(crate) fn undirected_angle_deg(left: [f64; 3], right: [f64; 3]) -> Option<f64> {
    let (left, right) = normalize_vector(left).zip(normalize_vector(right))?;
    Some(tidy_angle(
        dot(left, right).abs().clamp(0.0, 1.0).acos().to_degrees(),
    ))
}

/// Explicit `radius_mm`, or `1 / |curvature|` when the radius was not stored.
///
/// A zero curvature is a flat direction and is ignored. When both principal
/// curvatures are set, the tighter one (larger magnitude) wins.
pub(crate) fn effective_radius_mm(fingerprint: &GeometricFingerprint) -> Option<f64> {
    if let Some(radius) = fingerprint.radius_mm {
        return (radius.is_finite() && radius >= 0.0).then_some(radius);
    }
    let mut tightest: Option<f64> = None;
    for curvature in [fingerprint.max_curvature, fingerprint.min_curvature]
        .into_iter()
        .flatten()
    {
        if !curvature.is_finite() || curvature.abs() <= 1.0e-12 {
            continue;
        }
        let radius = 1.0 / curvature.abs();
        if !radius.is_finite() {
            continue;
        }
        tightest = Some(match tightest {
            Some(current) => current.min(radius),
            None => radius,
        });
    }
    tightest
}

pub(crate) fn tidy(value: f64) -> f64 {
    if value.is_finite() && value.abs() <= 1.0e-12 {
        0.0
    } else {
        value
    }
}

fn tidy_angle(degrees: f64) -> f64 {
    for anchor in [0.0, 90.0, 180.0] {
        if (degrees - anchor).abs() <= 1.0e-9 {
            return anchor;
        }
    }
    degrees
}

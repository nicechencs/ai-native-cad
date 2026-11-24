//! Deterministic spatial queries over one topology snapshot.
//!
//! Filters select faces, edges, and vertices by kind, surface, creator, normal,
//! bounding box, size, radius, and tag. The hit list is sorted, so the same
//! entities come back in the same order no matter how the kernel listed them.
//! Deleted entities are skipped.

use serde::{Deserialize, Serialize};

use crate::fingerprint::{
    CurveClass, EntityKind, SurfaceClass, directed_angle_deg, effective_radius_mm,
    normalize_vector, undirected_angle_deg,
};
use crate::lineage::{LineageTopoRef, TopoEntityId};
use crate::resolver::{CandidateEntity, TopoError, TopoSnapshot};

/// Angle used when `normal_target` is set and `normal_angle_tol_deg` is omitted.
pub const DEFAULT_NORMAL_ANGLE_TOL_DEG: f64 = 5.0;

/// Radius band used when `radius_mm` is set and `radius_tol_mm` is omitted.
pub const DEFAULT_RADIUS_TOL_MM: f64 = 1.0e-3;

/// Extra slack so an angle that lands on the tolerance still matches.
const ANGLE_MATCH_SLACK_DEG: f64 = 1.0e-9;

/// How to order matches. Entity id is always the final tie break.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TopologySortKey {
    /// Lexicographic `entity_id`. This is the default.
    EntityId,
    /// Creator node, then output slot, then entity id.
    CreatorThenSlot,
    /// Centroid X, then Y, then Z.
    CentroidX,
    /// Centroid Y, then Z, then X.
    CentroidY,
    /// Centroid Z, then X, then Y.
    CentroidZ,
    /// Smallest area or length first.
    AreaOrLengthAsc,
    /// Largest area or length first.
    AreaOrLengthDesc,
    /// Smallest radius first. Entities without a radius stay at the end.
    RadiusAsc,
    /// Largest radius first. Entities without a radius stay at the end.
    RadiusDesc,
}

/// Geometric and provenance filters for [`query_topology`].
///
/// Every field is optional. An omitted field does not constrain the result.
/// JSON objects may omit fields; missing fields use `Default`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TopologyQueryFilter {
    pub entity_kind: Option<EntityKind>,
    pub surface_class: Option<SurfaceClass>,
    pub curve_class: Option<CurveClass>,
    pub creator_node_id: Option<String>,
    /// Requested normal for a plane, or axis for a cylinder, cone, or circle.
    ///
    /// Plane normals keep their sign: `+Z` does not match `-Z`. Cylinder, cone,
    /// torus, circle, ellipse, and line directions ignore sign.
    pub normal_target: Option<[f64; 3]>,
    /// Degrees, `0..=180`. Defaults to [`DEFAULT_NORMAL_ANGLE_TOL_DEG`] when
    /// `normal_target` is set and this field is omitted.
    pub normal_angle_tol_deg: Option<f64>,
    /// Inclusive lower corner of an axis-aligned centroid box, in millimetres.
    /// Omit it to leave that side open.
    pub centroid_min_mm: Option<[f64; 3]>,
    /// Inclusive upper corner of the centroid box, in millimetres.
    pub centroid_max_mm: Option<[f64; 3]>,
    pub min_area_or_length: Option<f64>,
    pub max_area_or_length: Option<f64>,
    /// Hole or boss radius in millimetres. Matches `radius_mm`, or `1/|curvature|`
    /// when the fingerprint has curvature but no radius.
    pub radius_mm: Option<f64>,
    pub radius_tol_mm: Option<f64>,
    /// Exact provenance tag. Matching is case sensitive.
    pub tag: Option<String>,
    pub sort_by: Option<TopologySortKey>,
    /// Keep this many hits after sorting. `None` keeps every match.
    pub limit: Option<usize>,
}

/// One entity selected by [`query_topology`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopologyHit {
    pub topo_ref: LineageTopoRef,
    pub entity_id: TopoEntityId,
    pub creator_node_id: String,
    pub local_output_slot: String,
    pub entity_kind: EntityKind,
    pub surface_class: Option<SurfaceClass>,
    pub curve_class: Option<CurveClass>,
    pub centroid_mm: [f64; 3],
    /// Stored normal or axis. For a cylinder or circle this is the axis.
    pub normal: Option<[f64; 3]>,
    /// Area in mm² for a face, length in mm for an edge, `0` for a vertex.
    pub area_or_length: f64,
    pub radius_mm: Option<f64>,
    pub adjacent_signature: Vec<SurfaceClass>,
    pub tags: Vec<String>,
}

/// Sorted matches from one snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopologyQueryResult {
    pub hits: Vec<TopologyHit>,
    /// Match count before `limit` was applied.
    pub total_matched: usize,
}

/// Select entities from `snapshot` and return them in a stable order.
///
/// An empty filter returns every live entity, sorted by entity id. A filter that
/// matches nothing returns an empty hit list, not an error. Invalid numbers are
/// reported as [`TopoError::InvalidQuery`].
pub fn query_topology(
    snapshot: &TopoSnapshot,
    filter: &TopologyQueryFilter,
) -> Result<TopologyQueryResult, TopoError> {
    validate_filter(filter)?;
    let normal_tol = filter
        .normal_angle_tol_deg
        .unwrap_or(DEFAULT_NORMAL_ANGLE_TOL_DEG);
    let radius_tol = filter.radius_tol_mm.unwrap_or(DEFAULT_RADIUS_TOL_MM);

    let mut hits = Vec::new();
    for candidate in &snapshot.candidates {
        if candidate.is_deleted() {
            continue;
        }
        if matches_filter(candidate, filter, normal_tol, radius_tol) {
            hits.push(hit_from(candidate));
        }
    }
    let total_matched = hits.len();
    sort_hits(
        &mut hits,
        filter.sort_by.unwrap_or(TopologySortKey::EntityId),
    );
    if let Some(limit) = filter.limit {
        hits.truncate(limit);
    }
    Ok(TopologyQueryResult {
        hits,
        total_matched,
    })
}

fn validate_filter(filter: &TopologyQueryFilter) -> Result<(), TopoError> {
    if let Some(target) = filter.normal_target
        && normalize_vector(target).is_none()
    {
        return Err(TopoError::InvalidQuery(
            "normal_target must be a finite non-zero vector".to_owned(),
        ));
    }
    if let Some(angle) = filter.normal_angle_tol_deg
        && (!angle.is_finite() || !(0.0..=180.0).contains(&angle))
    {
        return Err(TopoError::InvalidQuery(
            "normal_angle_tol_deg must be finite and in 0..=180".to_owned(),
        ));
    }
    match (filter.centroid_min_mm, filter.centroid_max_mm) {
        (Some(min), Some(max))
            if min
                .iter()
                .zip(max)
                .any(|(min, max)| !min.is_finite() || !max.is_finite() || *min > max) =>
        {
            return Err(TopoError::InvalidQuery(
                "centroid_min_mm must be finite and componentwise <= centroid_max_mm".to_owned(),
            ));
        }
        _ => {
            for bound in [filter.centroid_min_mm, filter.centroid_max_mm]
                .into_iter()
                .flatten()
            {
                if bound.iter().any(|value| !value.is_finite()) {
                    return Err(TopoError::InvalidQuery(
                        "centroid bounds must be finite".to_owned(),
                    ));
                }
            }
        }
    }
    for (name, bound) in [
        ("min_area_or_length", filter.min_area_or_length),
        ("max_area_or_length", filter.max_area_or_length),
    ] {
        if let Some(value) = bound
            && (!value.is_finite() || value < 0.0)
        {
            return Err(TopoError::InvalidQuery(format!(
                "{name} must be finite and non-negative"
            )));
        }
    }
    if let (Some(min), Some(max)) = (filter.min_area_or_length, filter.max_area_or_length)
        && min > max
    {
        return Err(TopoError::InvalidQuery(
            "min_area_or_length must be <= max_area_or_length".to_owned(),
        ));
    }
    if let Some(radius) = filter.radius_mm
        && (!radius.is_finite() || radius < 0.0)
    {
        return Err(TopoError::InvalidQuery(
            "radius_mm must be finite and non-negative".to_owned(),
        ));
    }
    if let Some(tolerance) = filter.radius_tol_mm
        && (!tolerance.is_finite() || tolerance < 0.0)
    {
        return Err(TopoError::InvalidQuery(
            "radius_tol_mm must be finite and non-negative".to_owned(),
        ));
    }
    Ok(())
}

fn matches_filter(
    candidate: &CandidateEntity,
    filter: &TopologyQueryFilter,
    normal_tol_deg: f64,
    radius_tol_mm: f64,
) -> bool {
    let fingerprint = &candidate.fingerprint;
    if filter
        .entity_kind
        .is_some_and(|kind| fingerprint.entity_kind != kind)
    {
        return false;
    }
    if filter
        .surface_class
        .is_some_and(|surface| fingerprint.surface_class != Some(surface))
    {
        return false;
    }
    if filter
        .curve_class
        .is_some_and(|curve| fingerprint.curve_class != Some(curve))
    {
        return false;
    }
    if filter
        .creator_node_id
        .as_ref()
        .is_some_and(|creator| &candidate.creator_node_id != creator)
    {
        return false;
    }
    if let Some(target) = filter.normal_target {
        let Some(actual) = fingerprint.normal_or_axis else {
            return false;
        };
        let angle = if direction_is_unsigned(fingerprint) {
            undirected_angle_deg(actual, target)
        } else {
            directed_angle_deg(actual, target)
        };
        let Some(angle) = angle else {
            return false;
        };
        if angle > normal_tol_deg + ANGLE_MATCH_SLACK_DEG {
            return false;
        }
    }
    if let Some(min) = filter.centroid_min_mm
        && fingerprint
            .centroid_mm
            .iter()
            .zip(min)
            .any(|(value, bound)| !value.is_finite() || *value < bound)
    {
        return false;
    }
    if let Some(max) = filter.centroid_max_mm
        && fingerprint
            .centroid_mm
            .iter()
            .zip(max)
            .any(|(value, bound)| !value.is_finite() || *value > bound)
    {
        return false;
    }
    if let Some(min) = filter.min_area_or_length
        && (!fingerprint.area_or_length_mm.is_finite() || fingerprint.area_or_length_mm < min)
    {
        return false;
    }
    if let Some(max) = filter.max_area_or_length
        && (!fingerprint.area_or_length_mm.is_finite() || fingerprint.area_or_length_mm > max)
    {
        return false;
    }
    if let Some(radius) = filter.radius_mm {
        let Some(actual) = effective_radius_mm(fingerprint) else {
            return false;
        };
        if (actual - radius).abs() > radius_tol_mm {
            return false;
        }
    }
    if let Some(tag) = filter.tag.as_ref()
        && !candidate.provenance_tags.iter().any(|value| value == tag)
    {
        return false;
    }
    true
}

/// Axes and curve tangents have no preferred sign. Plane normals do.
fn direction_is_unsigned(fingerprint: &crate::fingerprint::GeometricFingerprint) -> bool {
    if fingerprint.surface_class == Some(SurfaceClass::Plane) {
        return false;
    }
    matches!(
        fingerprint.surface_class,
        Some(SurfaceClass::Cylinder | SurfaceClass::Cone | SurfaceClass::Torus)
    ) || matches!(
        fingerprint.curve_class,
        Some(CurveClass::Circle | CurveClass::Ellipse | CurveClass::Line)
    )
}

fn hit_from(candidate: &CandidateEntity) -> TopologyHit {
    let fingerprint = &candidate.fingerprint;
    TopologyHit {
        topo_ref: candidate.to_lineage_ref(),
        entity_id: candidate.entity_id.clone(),
        creator_node_id: candidate.creator_node_id.clone(),
        local_output_slot: candidate.local_output_slot.clone(),
        entity_kind: fingerprint.entity_kind,
        surface_class: fingerprint.surface_class,
        curve_class: fingerprint.curve_class,
        centroid_mm: fingerprint.centroid_mm,
        normal: fingerprint.normal_or_axis,
        area_or_length: fingerprint.area_or_length_mm,
        radius_mm: effective_radius_mm(fingerprint),
        adjacent_signature: fingerprint.adjacent_signature.clone(),
        tags: candidate.provenance_tags.clone(),
    }
}

fn sort_hits(hits: &mut [TopologyHit], key: TopologySortKey) {
    hits.sort_by(|left, right| {
        compare_hits(left, right, key).then_with(|| left.entity_id.cmp(&right.entity_id))
    });
}

fn compare_hits(
    left: &TopologyHit,
    right: &TopologyHit,
    key: TopologySortKey,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match key {
        TopologySortKey::EntityId => Ordering::Equal,
        TopologySortKey::CreatorThenSlot => left
            .creator_node_id
            .cmp(&right.creator_node_id)
            .then_with(|| left.local_output_slot.cmp(&right.local_output_slot)),
        TopologySortKey::CentroidX => cmp_centroid(left, right, [0, 1, 2]),
        TopologySortKey::CentroidY => cmp_centroid(left, right, [1, 2, 0]),
        TopologySortKey::CentroidZ => cmp_centroid(left, right, [2, 0, 1]),
        TopologySortKey::AreaOrLengthAsc => left.area_or_length.total_cmp(&right.area_or_length),
        TopologySortKey::AreaOrLengthDesc => right.area_or_length.total_cmp(&left.area_or_length),
        TopologySortKey::RadiusAsc => cmp_optional_number(left.radius_mm, right.radius_mm, false),
        TopologySortKey::RadiusDesc => cmp_optional_number(left.radius_mm, right.radius_mm, true),
    }
}

fn cmp_centroid(left: &TopologyHit, right: &TopologyHit, axes: [usize; 3]) -> std::cmp::Ordering {
    axes.into_iter()
        .fold(std::cmp::Ordering::Equal, |order, axis| {
            order.then_with(|| left.centroid_mm[axis].total_cmp(&right.centroid_mm[axis]))
        })
}

/// `None` always sorts after a real number, in both directions.
fn cmp_optional_number(
    left: Option<f64>,
    right: Option<f64>,
    descending: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (Some(left), Some(right)) => {
            let order = left.total_cmp(&right);
            if descending { order.reverse() } else { order }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

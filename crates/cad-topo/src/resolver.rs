//! Deterministic resolution of lineage and semantic references.

use thiserror::Error;

use crate::fingerprint::{
    GeometricFingerprint, MatchTolerances, euclidean_distance, fingerprint_similarity,
    normalize_vector,
};
use crate::lineage::{EvolutionOp, EvolutionRecord, LineageTopoRef, TopoEntityId};
use crate::query::{CoordAxis, QueryTopoRef, TieBreaker, TopoFilter, TopoRef};

/// A kernel-produced candidate in one evaluation snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateEntity {
    pub entity_id: TopoEntityId,
    pub creator_node_id: String,
    pub local_output_slot: String,
    pub evolution_chain: Vec<EvolutionRecord>,
    pub fingerprint: GeometricFingerprint,
    pub provenance_tags: Vec<String>,
}

impl CandidateEntity {
    pub(crate) fn is_deleted(&self) -> bool {
        matches!(
            self.evolution_chain.last().map(|record| &record.op),
            Some(EvolutionOp::Deleted)
        )
    }

    /// Build the durable lineage reference carried by this candidate.
    ///
    /// `ref_id` is `creator::slot`. Resolution uses the creator, slot, evolution
    /// chain, and fingerprint, so the ephemeral `entity_id` stays off this label.
    pub fn to_lineage_ref(&self) -> LineageTopoRef {
        LineageTopoRef {
            ref_id: format!("{}::{}", self.creator_node_id, self.local_output_slot),
            creator_node_id: self.creator_node_id.clone(),
            local_output_slot: self.local_output_slot.clone(),
            evolution_chain: self.evolution_chain.clone(),
            fingerprint: self.fingerprint.clone(),
            provenance_tags: self.provenance_tags.clone(),
        }
    }
}

/// All kernel entities produced by one evaluation step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopoSnapshot {
    pub candidates: Vec<CandidateEntity>,
}

impl TopoSnapshot {
    pub fn new(candidates: Vec<CandidateEntity>) -> Self {
        Self { candidates }
    }
}

/// Result of resolving a reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveResult {
    Unique(TopoEntityId),
    Multiple(Vec<TopoEntityId>),
}

/// Failures retain enough context for a UI to report a deterministic warning.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TopoError {
    #[error("topology reference has no match: {reason}")]
    MissingMatch { reason: String },
    #[error("topology reference is ambiguous: {reason}")]
    AmbiguousMatch {
        candidates: Vec<TopoEntityId>,
        reason: String,
    },
    #[error("invalid topology query: {0}")]
    InvalidQuery(String),
}

/// Resolve either track against a snapshot using deterministic operations.
///
/// Lineage resolution checks exact creator/slot/chain identity first, then scores
/// same-output descendants, and only uses a shared non-empty provenance tag when
/// the creator or slot is unavailable. A score below `min_score` or a winner whose
/// runner-up is within `ambiguity_margin` is reported as missing or ambiguous.
/// Query resolution applies every filter and then each tie-breaker in declaration
/// order. Numeric ties use the absolute `query_epsilon` metric tolerance.
pub fn resolve_topo_ref(
    topo_ref: &TopoRef,
    snapshot: &TopoSnapshot,
    tolerances: &MatchTolerances,
) -> Result<ResolveResult, TopoError> {
    match topo_ref {
        TopoRef::Lineage(reference) => resolve_lineage(reference, snapshot, tolerances),
        TopoRef::Query(reference) => resolve_query(reference, snapshot, tolerances),
    }
}

fn resolve_lineage(
    reference: &LineageTopoRef,
    snapshot: &TopoSnapshot,
    tolerances: &MatchTolerances,
) -> Result<ResolveResult, TopoError> {
    validate_lineage_tolerances(tolerances)?;
    let active: Vec<&CandidateEntity> = snapshot
        .candidates
        .iter()
        .filter(|candidate| !candidate.is_deleted())
        .collect();

    let exact: Vec<&CandidateEntity> = active
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.creator_node_id == reference.creator_node_id
                && candidate.local_output_slot == reference.local_output_slot
                && candidate.fingerprint.entity_kind == reference.fingerprint.entity_kind
                && candidate.evolution_chain == reference.evolution_chain
        })
        .collect();
    if !exact.is_empty() {
        return resolve_id_set(&exact, "multiple candidates have the exact lineage");
    }

    let descendants: Vec<&CandidateEntity> = active
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.creator_node_id == reference.creator_node_id
                && candidate.local_output_slot == reference.local_output_slot
                && candidate.fingerprint.entity_kind == reference.fingerprint.entity_kind
        })
        .collect();
    if !descendants.is_empty() {
        if let Some(result) = resolve_scored(
            &descendants,
            &reference.fingerprint,
            tolerances,
            "lineage descendants",
        )? {
            return Ok(result);
        }
        return Err(TopoError::MissingMatch {
            reason: "same creator and output slot had no candidate above the minimum similarity"
                .to_owned(),
        });
    }

    let tagged: Vec<&CandidateEntity> = active
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.fingerprint.entity_kind == reference.fingerprint.entity_kind
                && has_shared_provenance_tag(&reference.provenance_tags, &candidate.provenance_tags)
        })
        .collect();
    if let Some(result) = resolve_scored(
        &tagged,
        &reference.fingerprint,
        tolerances,
        "provenance and geometric fingerprint",
    )? {
        return Ok(result);
    }

    Err(TopoError::MissingMatch {
        reason: format!(
            "no candidate matched creator '{}' and slot '{}' above the minimum similarity",
            reference.creator_node_id, reference.local_output_slot
        ),
    })
}

fn resolve_scored(
    candidates: &[&CandidateEntity],
    fingerprint: &GeometricFingerprint,
    tolerances: &MatchTolerances,
    reason: &str,
) -> Result<Option<ResolveResult>, TopoError> {
    let scored: Vec<(&CandidateEntity, f64)> = candidates
        .iter()
        .map(|candidate| {
            (
                *candidate,
                fingerprint_similarity(fingerprint, &candidate.fingerprint, tolerances),
            )
        })
        .filter(|(_, score)| score.is_finite() && *score > 0.0)
        .collect();
    if scored.is_empty() {
        return Ok(None);
    }
    let best_score = scored
        .iter()
        .map(|(_, score)| *score)
        .fold(0.0_f64, f64::max);
    if best_score < tolerances.min_score {
        return Ok(None);
    }
    let tied: Vec<&CandidateEntity> = scored
        .iter()
        .filter(|(_, score)| best_score - *score <= tolerances.ambiguity_margin)
        .map(|(candidate, _)| *candidate)
        .collect();
    if tied.len() > 1 {
        return Err(TopoError::AmbiguousMatch {
            candidates: sorted_ids(&tied),
            reason: format!("{reason} have indistinguishable fingerprint scores"),
        });
    }
    Ok(Some(ResolveResult::Unique(tied[0].entity_id.clone())))
}

fn resolve_id_set(
    candidates: &[&CandidateEntity],
    reason: &str,
) -> Result<ResolveResult, TopoError> {
    if candidates.len() == 1 {
        return Ok(ResolveResult::Unique(candidates[0].entity_id.clone()));
    }
    Err(TopoError::AmbiguousMatch {
        candidates: sorted_ids(candidates),
        reason: reason.to_owned(),
    })
}

fn resolve_query(
    reference: &QueryTopoRef,
    snapshot: &TopoSnapshot,
    tolerances: &MatchTolerances,
) -> Result<ResolveResult, TopoError> {
    let mut candidates: Vec<&CandidateEntity> = snapshot
        .candidates
        .iter()
        .filter(|candidate| {
            !candidate.is_deleted() && candidate.fingerprint.entity_kind == reference.entity_kind
        })
        .collect();
    for filter in &reference.filters {
        apply_filter(&mut candidates, filter)?;
    }
    validate_query_tolerances(reference, tolerances.query_epsilon)?;
    if candidates.is_empty() {
        return Err(TopoError::MissingMatch {
            reason: "query filters matched no active entities".to_owned(),
        });
    }

    for tie_breaker in &reference.tie_breakers {
        candidates = apply_tie_breaker(candidates, tie_breaker, tolerances.query_epsilon)?;
    }

    if reference.require_unique && candidates.len() > 1 {
        return Err(TopoError::AmbiguousMatch {
            candidates: sorted_ids(&candidates),
            reason: "query did not produce a unique candidate".to_owned(),
        });
    }
    if candidates.len() == 1 {
        Ok(ResolveResult::Unique(candidates[0].entity_id.clone()))
    } else {
        Ok(ResolveResult::Multiple(sorted_ids(&candidates)))
    }
}

fn apply_filter(
    candidates: &mut Vec<&CandidateEntity>,
    filter: &TopoFilter,
) -> Result<(), TopoError> {
    match filter {
        TopoFilter::Surface(surface) => candidates
            .retain(|candidate| candidate.fingerprint.surface_class.as_ref() == Some(surface)),
        TopoFilter::Curve(curve) => {
            candidates.retain(|candidate| candidate.fingerprint.curve_class.as_ref() == Some(curve))
        }
        TopoFilter::Normal { dir, angle_tol_deg } => {
            if !angle_tol_deg.is_finite() || !(0.0..=180.0).contains(angle_tol_deg) {
                return Err(TopoError::InvalidQuery(
                    "normal angle tolerance must be finite and between 0 and 180 degrees"
                        .to_owned(),
                ));
            }
            let direction = normalize_vector(dir.vector()).ok_or_else(|| {
                TopoError::InvalidQuery("custom normal direction must be non-zero".to_owned())
            })?;
            let minimum_dot = angle_tol_deg.to_radians().cos();
            candidates.retain(|candidate| {
                candidate
                    .fingerprint
                    .normal_or_axis
                    .and_then(normalize_vector)
                    .map(|normal| dot(normal, direction) + 1e-12 >= minimum_dot)
                    .unwrap_or(false)
            });
        }
        TopoFilter::Tagged(tag) => {
            candidates
                .retain(|candidate| candidate.provenance_tags.iter().any(|value| value == tag));
        }
        TopoFilter::WithinBBox { min_mm, max_mm } => {
            if min_mm
                .iter()
                .zip(max_mm)
                .any(|(min, max)| !min.is_finite() || !max.is_finite() || min > max)
            {
                return Err(TopoError::InvalidQuery(
                    "bounding-box minimums must be finite and no greater than maximums".to_owned(),
                ));
            }
            candidates.retain(|candidate| {
                candidate
                    .fingerprint
                    .centroid_mm
                    .iter()
                    .zip(min_mm)
                    .zip(max_mm)
                    .all(|((value, min), max)| value >= min && value <= max)
            });
        }
    }
    Ok(())
}

fn apply_tie_breaker<'a>(
    candidates: Vec<&'a CandidateEntity>,
    tie_breaker: &TieBreaker,
    epsilon: f64,
) -> Result<Vec<&'a CandidateEntity>, TopoError> {
    let maximize = matches!(
        tie_breaker,
        TieBreaker::MaxByCoord(_) | TieBreaker::MaxByRadius | TieBreaker::MaxByAreaOrLength
    );
    let values: Vec<Option<f64>> = candidates
        .iter()
        .map(|candidate| tie_value(candidate, tie_breaker))
        .map(|value| value.filter(|value| value.is_finite()))
        .collect();
    if values.iter().all(Option::is_none) {
        return Err(TopoError::MissingMatch {
            reason: "tie-breaker had no usable candidate metrics".to_owned(),
        });
    }
    let present: Vec<f64> = values.iter().flatten().copied().collect();
    let target = if maximize {
        present.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    } else {
        present.iter().copied().fold(f64::INFINITY, f64::min)
    };
    Ok(candidates
        .into_iter()
        .zip(values)
        .filter(|(_, value)| {
            value
                .map(|value| (value - target).abs() <= epsilon)
                .unwrap_or(false)
        })
        .map(|(candidate, _)| candidate)
        .collect())
}

fn validate_query_tolerances(reference: &QueryTopoRef, epsilon: f64) -> Result<(), TopoError> {
    if !epsilon.is_finite() || epsilon < 0.0 {
        return Err(TopoError::InvalidQuery(
            "tie-breaker epsilon must be finite and non-negative".to_owned(),
        ));
    }
    for tie_breaker in &reference.tie_breakers {
        match tie_breaker {
            TieBreaker::NearestTo(point) if point.iter().any(|value| !value.is_finite()) => {
                return Err(TopoError::InvalidQuery(
                    "nearest-point coordinates must be finite".to_owned(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_lineage_tolerances(tolerances: &MatchTolerances) -> Result<(), TopoError> {
    if !tolerances.min_score.is_finite()
        || !(0.0..=1.0).contains(&tolerances.min_score)
        || !tolerances.ambiguity_margin.is_finite()
        || tolerances.ambiguity_margin < 0.0
    {
        return Err(TopoError::InvalidQuery(
            "matching score thresholds must be finite, with min_score in 0..=1 and a non-negative ambiguity margin"
                .to_owned(),
        ));
    }
    Ok(())
}

fn tie_value(candidate: &CandidateEntity, tie_breaker: &TieBreaker) -> Option<f64> {
    let fingerprint = &candidate.fingerprint;
    match tie_breaker {
        TieBreaker::MaxByCoord(axis) | TieBreaker::MinByCoord(axis) => {
            Some(coord(fingerprint.centroid_mm, *axis)).filter(|value| value.is_finite())
        }
        TieBreaker::MaxByRadius | TieBreaker::MinByRadius => fingerprint
            .radius_mm
            .filter(|value| value.is_finite() && *value >= 0.0),
        TieBreaker::MaxByAreaOrLength | TieBreaker::MinByAreaOrLength => {
            Some(fingerprint.area_or_length_mm).filter(|value| value.is_finite() && *value >= 0.0)
        }
        TieBreaker::NearestTo(point) => euclidean_distance(fingerprint.centroid_mm, *point),
    }
}

fn coord(value: [f64; 3], axis: CoordAxis) -> f64 {
    match axis {
        CoordAxis::X => value[0],
        CoordAxis::Y => value[1],
        CoordAxis::Z => value[2],
    }
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.into_iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn has_shared_provenance_tag(reference_tags: &[String], candidate_tags: &[String]) -> bool {
    reference_tags.iter().any(|reference_tag| {
        !reference_tag.trim().is_empty()
            && candidate_tags
                .iter()
                .any(|candidate_tag| candidate_tag == reference_tag)
    })
}

fn sorted_ids(candidates: &[&CandidateEntity]) -> Vec<TopoEntityId> {
    let mut ids: Vec<TopoEntityId> = candidates
        .iter()
        .map(|candidate| candidate.entity_id.clone())
        .collect();
    ids.sort();
    ids
}

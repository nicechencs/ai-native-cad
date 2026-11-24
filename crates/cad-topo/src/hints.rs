//! Machine-readable hints an agent can use after a topology or geometry failure.
//!
//! The hint lists creator, centroid, normal, area, and radius. It does not ask
//! the agent to pick a kernel output slot.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::fingerprint::{
    CurveClass, EntityKind, SurfaceClass, directed_angle_deg, effective_radius_mm,
};
use crate::lineage::{LineageTopoRef, TopoEntityId};
use crate::resolver::CandidateEntity;

/// One live or caller-supplied entity, reduced to fields an agent can filter on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopoRecoveryCandidate {
    pub entity_id: TopoEntityId,
    pub topo_ref: LineageTopoRef,
    pub creator_node_id: String,
    pub local_output_slot: String,
    pub entity_kind: EntityKind,
    pub surface_class: Option<SurfaceClass>,
    pub curve_class: Option<CurveClass>,
    pub centroid_mm: [f64; 3],
    pub normal: Option<[f64; 3]>,
    pub area_or_length: f64,
    pub radius_mm: Option<f64>,
    pub adjacent_signature: Vec<SurfaceClass>,
    pub tags: Vec<String>,
}

impl TopoRecoveryCandidate {
    fn from_candidate(candidate: &CandidateEntity) -> Self {
        let fingerprint = &candidate.fingerprint;
        Self {
            entity_id: candidate.entity_id.clone(),
            topo_ref: candidate.to_lineage_ref(),
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
}

/// What an agent should try next.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RecoveryHint {
    /// Several entities matched. `suggested_disambiguator` names a geometric filter.
    AmbiguousTopology {
        candidates: Vec<TopoRecoveryCandidate>,
        suggested_disambiguator: String,
    },
    /// Nothing matched. `nearest_candidates` keeps the caller's order.
    MissingTopology {
        requested: String,
        nearest_candidates: Vec<TopoRecoveryCandidate>,
    },
    /// A fillet, chamfer, shell, or similar parameter is outside the solid.
    GeometryBoundExceeded {
        parameter: String,
        limit_mm: Option<f64>,
        reason: String,
    },
}

impl RecoveryHint {
    /// `nearest` stays in the order given. The list is capped at `max_candidates`.
    pub fn missing_topology(
        requested: impl Into<String>,
        nearest: &[CandidateEntity],
        max_candidates: usize,
    ) -> Self {
        Self::MissingTopology {
            requested: requested.into(),
            nearest_candidates: nearest
                .iter()
                .take(max_candidates)
                .map(TopoRecoveryCandidate::from_candidate)
                .collect(),
        }
    }

    /// A parameter such as a fillet radius cannot be applied.
    ///
    /// `limit_mm` is the largest value the geometry still accepts, when that
    /// bound is known.
    pub fn geometry_bound_exceeded(
        parameter: impl Into<String>,
        limit_mm: Option<f64>,
        reason: impl Into<String>,
    ) -> Self {
        Self::GeometryBoundExceeded {
            parameter: parameter.into(),
            limit_mm,
            reason: reason.into(),
        }
    }
}

/// Summarize an ambiguous match set for an agent.
///
/// Candidates are ordered by entity id, then creator, then slot, and then capped
/// at `max_candidates`. The suggestion lists centroid, radius, area, tag, and
/// creator differences. It tells the agent not to guess internal output slots.
pub fn build_ambiguous_topo_hints(
    candidates: &[CandidateEntity],
    max_candidates: usize,
) -> RecoveryHint {
    if candidates.is_empty() {
        return RecoveryHint::AmbiguousTopology {
            candidates: Vec::new(),
            suggested_disambiguator: "No topology candidates to disambiguate. Widen the geometric filter or confirm the feature exists.".to_owned(),
        };
    }

    let mut ordered: Vec<&CandidateEntity> = candidates.iter().collect();
    ordered.sort_by(|left, right| {
        left.entity_id
            .cmp(&right.entity_id)
            .then_with(|| left.creator_node_id.cmp(&right.creator_node_id))
            .then_with(|| left.local_output_slot.cmp(&right.local_output_slot))
    });
    ordered.truncate(max_candidates);
    let summaries: Vec<TopoRecoveryCandidate> = ordered
        .into_iter()
        .map(TopoRecoveryCandidate::from_candidate)
        .collect();
    let suggested_disambiguator = if summaries.is_empty() {
        "Candidate list was capped at 0. Increase max_candidates to inspect centroid, normal, area, and creator.".to_owned()
    } else {
        suggested_disambiguator(&summaries)
    };
    RecoveryHint::AmbiguousTopology {
        candidates: summaries,
        suggested_disambiguator,
    }
}

fn suggested_disambiguator(candidates: &[TopoRecoveryCandidate]) -> String {
    if candidates.len() == 1 {
        let candidate = &candidates[0];
        return format!(
            "One candidate: creator '{}', centroid [{}, {}, {}] mm, area_or_length {}, radius_mm {}.",
            candidate.creator_node_id,
            fmt_mm(candidate.centroid_mm[0]),
            fmt_mm(candidate.centroid_mm[1]),
            fmt_mm(candidate.centroid_mm[2]),
            fmt_mm(candidate.area_or_length),
            fmt_optional_mm(candidate.radius_mm),
        );
    }

    let mut notes = vec![format!(
        "{} candidates match. Do not guess internal output slots; narrow the query with geometry.",
        candidates.len()
    )];

    let mut creators: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.creator_node_id.as_str())
        .collect();
    creators.sort_unstable();
    creators.dedup();
    if creators.len() == 1 {
        notes.push(format!("shared creator_node_id '{}'.", creators[0]));
    } else {
        notes.push(format!("creator_node_id values: {}.", creators.join(", ")));
    }

    if directions_differ(candidates) {
        notes
            .push("normals differ. Filter with normal_target and normal_angle_tol_deg.".to_owned());
    }

    let radii: BTreeSet<String> = candidates
        .iter()
        .map(|candidate| fmt_optional_mm(candidate.radius_mm))
        .collect();
    if radii.len() > 1 {
        notes.push(format!(
            "radius_mm values: {}.",
            radii.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let areas: BTreeSet<String> = candidates
        .iter()
        .map(|candidate| fmt_mm(candidate.area_or_length))
        .collect();
    if areas.len() > 1 {
        notes.push(format!(
            "area_or_length values: {}.",
            areas.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let centroids = candidates
        .iter()
        .map(|candidate| {
            format!(
                "[{}, {}, {}]",
                fmt_mm(candidate.centroid_mm[0]),
                fmt_mm(candidate.centroid_mm[1]),
                fmt_mm(candidate.centroid_mm[2])
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    notes.push(format!(
        "centroids mm: {centroids}. Filter with centroid_min_mm and centroid_max_mm."
    ));

    let mut tags: Vec<&str> = candidates
        .iter()
        .flat_map(|candidate| candidate.tags.iter().map(String::as_str))
        .collect();
    tags.sort_unstable();
    tags.dedup();
    if !tags.is_empty() {
        notes.push(format!("tags: {}.", tags.join(", ")));
    }

    notes.join(" ")
}

fn directions_differ(candidates: &[TopoRecoveryCandidate]) -> bool {
    let mut first: Option<[f64; 3]> = None;
    for candidate in candidates {
        let Some(normal) = candidate.normal else {
            continue;
        };
        match first {
            None => first = Some(normal),
            Some(origin) => {
                let Some(angle) = directed_angle_deg(origin, normal) else {
                    return true;
                };
                if angle > 1.0 {
                    return true;
                }
            }
        }
    }
    false
}

fn fmt_mm(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.4}")
    } else {
        "non-finite".to_owned()
    }
}

fn fmt_optional_mm(value: Option<f64>) -> String {
    value.map(fmt_mm).unwrap_or_else(|| "none".to_owned())
}

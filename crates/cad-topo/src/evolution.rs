//! Propagate persistent topology names across one feature step.
//!
//! A surviving face keeps the creator and output slot of the parent it came from.
//! The current step appends `Split`, `Merged`, `Trimmed`, or `Survived` instead of
//! minting a new `Created` identity. A face with no parent match is created here.

use std::collections::BTreeMap;

use crate::fingerprint::{
    CurveClass, EntityKind, SurfaceClass, euclidean_distance, normalize_vector,
};
use crate::lineage::{EvolutionOp, EvolutionRecord, TopoEntityId};
use crate::resolver::{CandidateEntity, TopoSnapshot};

/// Minimum parent/child overlap accepted as the same geometric support.
const SUPPORT_OVERLAP: f64 = 0.75;
/// Boundary tolerance for point-in-polygon tests, in millimetres.
const BOUNDARY_EPS_MM: f64 = 1.0e-4;

/// How a face or edge that has no parent is named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshSlotStyle {
    /// `face:{index}` / `edge:{index}`, in kernel enumeration order.
    ///
    /// Use this when the feature itself creates the shape and the kernel lists
    /// entities in a stable order, such as a box or a cylinder.
    KernelOrder,
    /// A geometric support key that does not follow the raw face index.
    ///
    /// Use this when a later feature can insert or reorder faces.
    Geometric,
}

/// Tolerances for deciding that two entities share a geometric support.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvolutionTolerances {
    /// Absolute plane-offset and axis-distance tolerance, in millimetres.
    pub plane_offset_mm: f64,
    /// Maximum angle between normals or axes, in degrees.
    pub angle_tolerance_deg: f64,
    /// Absolute radius tolerance, in millimetres.
    pub radius_tolerance_mm: f64,
    /// Relative area or length change that still counts as the same boundary.
    pub area_relative_tolerance: f64,
    /// Absolute centroid tolerance for edges and coincident points, in millimetres.
    pub coincidence_mm: f64,
}

impl Default for EvolutionTolerances {
    fn default() -> Self {
        Self {
            plane_offset_mm: 1.0e-3,
            angle_tolerance_deg: 1.0,
            radius_tolerance_mm: 1.0e-3,
            area_relative_tolerance: 1.0e-3,
            coincidence_mm: 1.0e-2,
        }
    }
}

/// Options for one propagation step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvolutionOptions {
    pub tolerances: EvolutionTolerances,
    pub fresh_slots: FreshSlotStyle,
}

impl Default for EvolutionOptions {
    fn default() -> Self {
        Self {
            tolerances: EvolutionTolerances::default(),
            fresh_slots: FreshSlotStyle::KernelOrder,
        }
    }
}

/// Geometric observation of one face or edge.
///
/// `outer_loop` is the boundary used for overlap tests. Cylinders also carry the
/// other end ring in `secondary_loop` so the axis can be recovered without a kernel.
#[derive(Clone, Debug, PartialEq)]
pub struct TopologySample {
    pub fingerprint: crate::fingerprint::GeometricFingerprint,
    pub provenance_tags: Vec<String>,
    pub inner_loop_count: u32,
    pub outer_loop: Vec<[f64; 3]>,
    pub secondary_loop: Vec<[f64; 3]>,
}

/// One parent feature output whose faces and edges may survive into the child.
///
/// Face samples line up with the parent's face candidates, in the same order.
/// Edge samples line up with its edge candidates.
#[derive(Clone, Debug, PartialEq)]
pub struct LineageParent {
    pub snapshot: TopoSnapshot,
    pub faces: Vec<TopologySample>,
    pub edges: Vec<TopologySample>,
}

/// Build the child snapshot for `step_node_id`.
///
/// Parents are listed base-first. A boolean tool that must not donate identity,
/// such as a drill, is simply left out of `parents`. Faces are emitted first, in
/// child order, then edges. That keeps face `i` at candidate `i`.
pub fn propagate_lineage(
    step_node_id: &str,
    parents: &[LineageParent],
    faces: &[TopologySample],
    edges: &[TopologySample],
    options: &EvolutionOptions,
) -> TopoSnapshot {
    let face_candidates = propagate_group(step_node_id, parents, faces, EntityKind::Face, options);
    let edge_candidates = propagate_group(step_node_id, parents, edges, EntityKind::Edge, options);
    TopoSnapshot::new(face_candidates.into_iter().chain(edge_candidates).collect())
}

struct IndexedSample<'a> {
    candidate: &'a CandidateEntity,
    sample: &'a TopologySample,
}

fn propagate_group(
    step_node_id: &str,
    parents: &[LineageParent],
    children: &[TopologySample],
    kind: EntityKind,
    options: &EvolutionOptions,
) -> Vec<CandidateEntity> {
    let parent_entities = parent_entities(parents, kind);
    let mut assigned = if kind == EntityKind::Edge {
        assign_edges(step_node_id, &parent_entities, children, options)
    } else if parent_entities.is_empty() {
        children
            .iter()
            .enumerate()
            .map(|(index, sample)| created_entity(step_node_id, index, sample, options.fresh_slots))
            .collect()
    } else {
        assign_faces(step_node_id, &parent_entities, children, options)
    };
    disambiguate_created(step_node_id, &mut assigned, children);
    assigned
}

fn parent_entities(parents: &[LineageParent], kind: EntityKind) -> Vec<IndexedSample<'_>> {
    let mut aligned = Vec::new();
    for parent in parents {
        let samples = match kind {
            EntityKind::Face => &parent.faces,
            EntityKind::Edge => &parent.edges,
            EntityKind::Vertex => continue,
        };
        let candidates: Vec<&CandidateEntity> = parent
            .snapshot
            .candidates
            .iter()
            .filter(|candidate| candidate.fingerprint.entity_kind == kind)
            .collect();
        if candidates.len() != samples.len() {
            continue;
        }
        aligned.extend(
            candidates
                .into_iter()
                .zip(samples)
                .map(|(candidate, sample)| IndexedSample { candidate, sample }),
        );
    }
    aligned
}

fn assign_faces(
    step_node_id: &str,
    parents: &[IndexedSample<'_>],
    children: &[TopologySample],
    options: &EvolutionOptions,
) -> Vec<CandidateEntity> {
    let child_base = parents.len();
    let mut forest = UnionFind::new(parents.len() + children.len());
    for (parent_index, parent) in parents.iter().enumerate() {
        for (child_index, child) in children.iter().enumerate() {
            if same_support(parent.sample, child, &options.tolerances)
                && overlap_score(parent.sample, child) >= SUPPORT_OVERLAP
            {
                forest.unite(parent_index, child_base + child_index);
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..parents.len() + children.len() {
        groups.entry(forest.find(index)).or_default().push(index);
    }

    let mut assigned = vec![None; children.len()];
    for members in groups.values() {
        let parent_indexes: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| *index < child_base)
            .collect();
        let child_indexes: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| *index >= child_base)
            .map(|index| index - child_base)
            .collect();
        assign_cluster(
            step_node_id,
            parents,
            children,
            &parent_indexes,
            &child_indexes,
            options,
            &mut assigned,
        );
    }
    for (index, slot) in assigned.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(created_entity(
                step_node_id,
                index,
                &children[index],
                options.fresh_slots,
            ));
        }
    }
    assigned.into_iter().flatten().collect()
}

fn assign_cluster(
    step_node_id: &str,
    parents: &[IndexedSample<'_>],
    children: &[TopologySample],
    parent_indexes: &[usize],
    child_indexes: &[usize],
    options: &EvolutionOptions,
    assigned: &mut [Option<CandidateEntity>],
) {
    if child_indexes.is_empty() {
        return;
    }
    if parent_indexes.is_empty() {
        for &child_index in child_indexes {
            assigned[child_index] = Some(created_entity(
                step_node_id,
                child_index,
                &children[child_index],
                options.fresh_slots,
            ));
        }
        return;
    }

    match (parent_indexes.len(), child_indexes.len()) {
        (1, 1) => {
            let child_index = child_indexes[0];
            assigned[child_index] = Some(inherit_one(
                step_node_id,
                parents[parent_indexes[0]].candidate,
                &children[child_index],
                survive_or_trim(
                    parents[parent_indexes[0]].sample,
                    &children[child_index],
                    &options.tolerances,
                ),
            ));
        }
        (1, _) => assign_split(
            step_node_id,
            parents[parent_indexes[0]].candidate,
            children,
            child_indexes,
            assigned,
        ),
        (_, 1) => {
            let child_index = child_indexes[0];
            assigned[child_index] = Some(inherit_merge(
                step_node_id,
                parents,
                parent_indexes,
                &children[child_index],
            ));
        }
        _ => assign_mixed_cluster(
            step_node_id,
            parents,
            children,
            parent_indexes,
            child_indexes,
            options,
            assigned,
        ),
    }
}

fn assign_mixed_cluster(
    step_node_id: &str,
    parents: &[IndexedSample<'_>],
    children: &[TopologySample],
    parent_indexes: &[usize],
    child_indexes: &[usize],
    options: &EvolutionOptions,
    assigned: &mut [Option<CandidateEntity>],
) {
    let hits: Vec<Vec<usize>> = child_indexes
        .iter()
        .map(|&child_index| {
            parent_indexes
                .iter()
                .copied()
                .filter(|&parent_index| {
                    overlap_score(parents[parent_index].sample, &children[child_index])
                        >= SUPPORT_OVERLAP
                })
                .collect()
        })
        .collect();
    let covers_every_parent =
        |hit: &[usize]| parent_indexes.iter().all(|index| hit.contains(index));
    if child_indexes.len() > 1 && hits.iter().all(|hit| covers_every_parent(hit)) {
        let sources = source_labels(parents, parent_indexes);
        let primary = parents[choose_primary(parents, parent_indexes)].candidate;
        for (part_index, child_index) in centroid_order(children, child_indexes)
            .into_iter()
            .enumerate()
        {
            let mut entity = inherit_one(
                step_node_id,
                primary,
                &children[child_index],
                EvolutionOp::Merged {
                    sources: sources.clone(),
                },
            );
            entity.evolution_chain.push(EvolutionRecord {
                step_node_id: step_node_id.to_owned(),
                op: EvolutionOp::Split {
                    part_index: u32::try_from(part_index).unwrap_or(u32::MAX),
                },
                note: None,
            });
            entity.entity_id = TopoEntityId(format!("{}#split:{part_index}", primary.entity_id.0));
            assigned[child_index] = Some(entity);
        }
        return;
    }

    let mut grouped: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (offset, &child_index) in child_indexes.iter().enumerate() {
        let Some(parent_index) = best_parent(parents, &hits[offset], &children[child_index]) else {
            assigned[child_index] = Some(created_entity(
                step_node_id,
                child_index,
                &children[child_index],
                options.fresh_slots,
            ));
            continue;
        };
        grouped.entry(parent_index).or_default().push(child_index);
    }
    for (parent_index, group) in grouped {
        if group.len() == 1 {
            let child_index = group[0];
            assigned[child_index] = Some(inherit_one(
                step_node_id,
                parents[parent_index].candidate,
                &children[child_index],
                survive_or_trim(
                    parents[parent_index].sample,
                    &children[child_index],
                    &options.tolerances,
                ),
            ));
        } else {
            assign_split(
                step_node_id,
                parents[parent_index].candidate,
                children,
                &group,
                assigned,
            );
        }
    }
}

fn assign_split(
    step_node_id: &str,
    parent: &CandidateEntity,
    children: &[TopologySample],
    child_indexes: &[usize],
    assigned: &mut [Option<CandidateEntity>],
) {
    for (part_index, child_index) in centroid_order(children, child_indexes)
        .into_iter()
        .enumerate()
    {
        let part_index = u32::try_from(part_index).unwrap_or(u32::MAX);
        assigned[child_index] = Some(inherit_one(
            step_node_id,
            parent,
            &children[child_index],
            EvolutionOp::Split { part_index },
        ));
    }
}

fn inherit_merge(
    step_node_id: &str,
    parents: &[IndexedSample<'_>],
    parent_indexes: &[usize],
    child: &TopologySample,
) -> CandidateEntity {
    let primary = parents[choose_primary(parents, parent_indexes)].candidate;
    inherit_one(
        step_node_id,
        primary,
        child,
        EvolutionOp::Merged {
            sources: source_labels(parents, parent_indexes),
        },
    )
}

fn assign_edges(
    step_node_id: &str,
    parents: &[IndexedSample<'_>],
    children: &[TopologySample],
    options: &EvolutionOptions,
) -> Vec<CandidateEntity> {
    let mut pairs = Vec::new();
    for (parent_index, parent) in parents.iter().enumerate() {
        for (child_index, child) in children.iter().enumerate() {
            if let Some(score) = edge_match_score(parent.sample, child, &options.tolerances) {
                pairs.push((score, parent_index, child_index));
            }
        }
    }
    pairs.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| {
                source_label(parents[left.1].candidate)
                    .cmp(&source_label(parents[right.1].candidate))
            })
            .then(left.2.cmp(&right.2))
    });
    let mut parent_used = vec![false; parents.len()];
    let mut matches = vec![None; children.len()];
    for (_, parent_index, child_index) in pairs {
        if parent_used[parent_index] || matches[child_index].is_some() {
            continue;
        }
        parent_used[parent_index] = true;
        matches[child_index] = Some(parent_index);
    }
    children
        .iter()
        .enumerate()
        .map(|(child_index, child)| {
            matches[child_index].map_or_else(
                || created_entity(step_node_id, child_index, child, options.fresh_slots),
                |parent_index| {
                    inherit_one(
                        step_node_id,
                        parents[parent_index].candidate,
                        child,
                        survive_or_trim(parents[parent_index].sample, child, &options.tolerances),
                    )
                },
            )
        })
        .collect()
}

fn created_entity(
    step_node_id: &str,
    index: usize,
    sample: &TopologySample,
    fresh_slots: FreshSlotStyle,
) -> CandidateEntity {
    let slot = fresh_slot(fresh_slots, index, sample);
    CandidateEntity {
        entity_id: TopoEntityId(format!("{step_node_id}:{slot}")),
        creator_node_id: step_node_id.to_owned(),
        local_output_slot: slot,
        evolution_chain: vec![EvolutionRecord {
            step_node_id: step_node_id.to_owned(),
            op: EvolutionOp::Created,
            note: None,
        }],
        fingerprint: sample.fingerprint.clone(),
        provenance_tags: sample.provenance_tags.clone(),
    }
}

fn inherit_one(
    step_node_id: &str,
    parent: &CandidateEntity,
    child: &TopologySample,
    op: EvolutionOp,
) -> CandidateEntity {
    let entity_id = match op {
        EvolutionOp::Split { part_index } => {
            TopoEntityId(format!("{}#split:{part_index}", parent.entity_id.0))
        }
        _ => parent.entity_id.clone(),
    };
    let mut evolution_chain = parent.evolution_chain.clone();
    evolution_chain.push(EvolutionRecord {
        step_node_id: step_node_id.to_owned(),
        op,
        note: None,
    });
    CandidateEntity {
        entity_id,
        creator_node_id: parent.creator_node_id.clone(),
        local_output_slot: parent.local_output_slot.clone(),
        evolution_chain,
        fingerprint: child.fingerprint.clone(),
        provenance_tags: child.provenance_tags.clone(),
    }
}

fn disambiguate_created(
    step_node_id: &str,
    assigned: &mut [CandidateEntity],
    samples: &[TopologySample],
) {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, candidate) in assigned.iter().enumerate() {
        let created_here = candidate.creator_node_id == step_node_id
            && matches!(
                candidate.evolution_chain.last().map(|record| &record.op),
                Some(EvolutionOp::Created)
            );
        if created_here {
            groups
                .entry(candidate.local_output_slot.clone())
                .or_default()
                .push(index);
        }
    }
    for indexes in groups.values_mut() {
        if indexes.len() < 2 {
            continue;
        }
        indexes.sort_by(|&left, &right| {
            centroid_key(&samples[left])
                .cmp(&centroid_key(&samples[right]))
                .then(left.cmp(&right))
        });
        for (ordinal, index) in indexes.iter().copied().enumerate() {
            let slot = format!("{}#{ordinal}", assigned[index].local_output_slot);
            assigned[index].entity_id = TopoEntityId(format!("{step_node_id}:{slot}"));
            assigned[index].local_output_slot = slot;
        }
    }
}

fn survive_or_trim(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> EvolutionOp {
    if boundary_changed(parent, child, tolerances) {
        EvolutionOp::Trimmed
    } else {
        EvolutionOp::Survived
    }
}

fn boundary_changed(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> bool {
    let parent_measure = parent.fingerprint.area_or_length_mm;
    let child_measure = child.fingerprint.area_or_length_mm;
    let scale = parent_measure.abs().max(child_measure.abs()).max(1.0e-9);
    let relative = (parent_measure - child_measure).abs() / scale;
    relative > nonnegative(tolerances.area_relative_tolerance)
        || parent.inner_loop_count != child.inner_loop_count
        || parent.outer_loop.len() != child.outer_loop.len()
        || parent.secondary_loop.len() != child.secondary_loop.len()
}

fn same_support(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> bool {
    if parent.fingerprint.entity_kind != child.fingerprint.entity_kind {
        return false;
    }
    match (
        parent.fingerprint.surface_class,
        child.fingerprint.surface_class,
    ) {
        (Some(SurfaceClass::Plane), Some(SurfaceClass::Plane)) => {
            planes_coplanar(parent, child, tolerances)
        }
        (Some(SurfaceClass::Cylinder), Some(SurfaceClass::Cylinder))
        | (Some(SurfaceClass::Cone), Some(SurfaceClass::Cone))
        | (Some(SurfaceClass::Torus), Some(SurfaceClass::Torus)) => {
            cylinders_compatible(parent, child, tolerances)
        }
        (Some(SurfaceClass::Sphere), Some(SurfaceClass::Sphere)) => {
            spheres_compatible(parent, child, tolerances)
        }
        (Some(left), Some(right)) if left == right => generic_close(parent, child, tolerances),
        _ => false,
    }
}

fn planes_coplanar(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> bool {
    let Some(parent_normal) = parent.fingerprint.normal_or_axis.and_then(normalize_vector) else {
        return false;
    };
    let Some(child_normal) = child.fingerprint.normal_or_axis.and_then(normalize_vector) else {
        return false;
    };
    if dot(parent_normal, child_normal) < cos_tolerance(tolerances.angle_tolerance_deg) {
        return false;
    }
    let parent_offset = plane_offset(parent, parent_normal);
    let child_offset = plane_offset(child, parent_normal);
    parent_offset.is_finite()
        && child_offset.is_finite()
        && (parent_offset - child_offset).abs() <= nonnegative(tolerances.plane_offset_mm)
}

fn cylinders_compatible(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> bool {
    let (Some(parent_axis), Some(child_axis)) = (axis_frame(parent), axis_frame(child)) else {
        return false;
    };
    coaxial(&parent_axis, &child_axis, tolerances)
}

fn spheres_compatible(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> bool {
    let Some(parent_radius) = parent.fingerprint.radius_mm else {
        return false;
    };
    let Some(child_radius) = child.fingerprint.radius_mm else {
        return false;
    };
    let radius_gap = (parent_radius - child_radius).abs();
    let center_gap = euclidean_distance(
        parent.fingerprint.centroid_mm,
        child.fingerprint.centroid_mm,
    )
    .unwrap_or(f64::MAX);
    radius_gap <= nonnegative(tolerances.radius_tolerance_mm)
        && center_gap <= nonnegative(tolerances.coincidence_mm)
}

fn generic_close(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> bool {
    let distance = euclidean_distance(
        parent.fingerprint.centroid_mm,
        child.fingerprint.centroid_mm,
    )
    .unwrap_or(f64::MAX);
    let limit = nonnegative(tolerances.coincidence_mm)
        .max(characteristic_length(parent).max(characteristic_length(child)) * 0.05);
    distance <= limit
}

fn overlap_score(parent: &TopologySample, child: &TopologySample) -> f64 {
    let class = parent.fingerprint.surface_class;
    let score = if matches!(
        class,
        Some(SurfaceClass::Cylinder | SurfaceClass::Cone | SurfaceClass::Torus)
    ) {
        cylinder_overlap(parent, child)
    } else if class == Some(SurfaceClass::Sphere) {
        1.0
    } else {
        let child_in_parent = inside_fraction(&sample_points(child), parent);
        let parent_in_child = inside_fraction(&sample_points(parent), child);
        child_in_parent.max(parent_in_child)
    };
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn cylinder_overlap(parent: &TopologySample, child: &TopologySample) -> f64 {
    let (Some(parent_axis), Some(child_axis)) = (axis_frame(parent), axis_frame(child)) else {
        return 0.0;
    };
    if dot(parent_axis.axis, child_axis.axis).abs() < 0.5 {
        return 0.0;
    }
    axial_overlap_ratio(&parent_axis, &child_axis)
}

fn edge_match_score(
    parent: &TopologySample,
    child: &TopologySample,
    tolerances: &EvolutionTolerances,
) -> Option<f64> {
    if parent.fingerprint.curve_class != child.fingerprint.curve_class
        || parent.fingerprint.entity_kind != EntityKind::Edge
        || child.fingerprint.entity_kind != EntityKind::Edge
    {
        return None;
    }
    if !directions_agree(&parent.outer_loop, &child.outer_loop, tolerances) {
        return None;
    }
    let distance = euclidean_distance(
        parent.fingerprint.centroid_mm,
        child.fingerprint.centroid_mm,
    )?;
    let parent_length = parent.fingerprint.area_or_length_mm.max(1.0e-6);
    let child_length = child.fingerprint.area_or_length_mm.max(0.0);
    let limit = nonnegative(tolerances.coincidence_mm).max(parent_length * 0.02);
    if distance > limit {
        return None;
    }
    let growth = parent_length * nonnegative(tolerances.area_relative_tolerance)
        + nonnegative(tolerances.coincidence_mm);
    if child_length > parent_length + growth {
        return None;
    }
    Some(1.0 - distance / limit.max(1.0e-9))
}

fn directions_agree(
    parent: &[[f64; 3]],
    child: &[[f64; 3]],
    tolerances: &EvolutionTolerances,
) -> bool {
    let (Some(parent_direction), Some(child_direction)) =
        (chord_direction(parent), chord_direction(child))
    else {
        return true;
    };
    dot(parent_direction, child_direction).abs() >= cos_tolerance(tolerances.angle_tolerance_deg)
}

fn chord_direction(points: &[[f64; 3]]) -> Option<[f64; 3]> {
    let (first, last) = (points.first()?, points.last()?);
    normalize_vector(sub(*last, *first))
}

struct AxisFrame {
    point: [f64; 3],
    axis: [f64; 3],
    radius: f64,
    t0: f64,
    t1: f64,
}

fn axis_frame(sample: &TopologySample) -> Option<AxisFrame> {
    let radius = sample
        .fingerprint
        .radius_mm
        .filter(|radius| radius.is_finite() && *radius >= 0.0)?;
    if sample.outer_loop.len() >= 3 && sample.secondary_loop.len() >= 3 {
        let start = mean_point(&sample.outer_loop)?;
        let end = mean_point(&sample.secondary_loop)?;
        let axis = normalize_vector(sub(end, start))?;
        let height = euclidean_distance(start, end)?;
        return Some(AxisFrame {
            point: start,
            axis,
            radius,
            t0: 0.0,
            t1: height,
        });
    }
    None
}

fn coaxial(parent: &AxisFrame, child: &AxisFrame, tolerances: &EvolutionTolerances) -> bool {
    if dot(parent.axis, child.axis).abs() < cos_tolerance(tolerances.angle_tolerance_deg) {
        return false;
    }
    let delta = sub(child.point, parent.point);
    let along = dot(delta, parent.axis);
    let radial = euclidean_distance(delta, scale(parent.axis, along)).unwrap_or(f64::MAX);
    let radius_limit =
        nonnegative(tolerances.radius_tolerance_mm).max(parent.radius.max(child.radius) * 0.001);
    radial <= nonnegative(tolerances.plane_offset_mm).max(tolerances.coincidence_mm)
        && (parent.radius - child.radius).abs() <= radius_limit
}

fn axial_overlap_ratio(parent: &AxisFrame, child: &AxisFrame) -> f64 {
    let sign = if dot(parent.axis, child.axis) < 0.0 {
        -1.0
    } else {
        1.0
    };
    let origin = dot(sub(child.point, parent.point), parent.axis);
    let child_start = origin + sign * child.t0;
    let child_end = origin + sign * child.t1;
    let (child_low, child_high) = ordered(child_start, child_end);
    let (parent_low, parent_high) = ordered(parent.t0, parent.t1);
    let overlap = (parent_high.min(child_high) - parent_low.max(child_low)).max(0.0);
    let shorter = (parent_high - parent_low)
        .min(child_high - child_low)
        .max(1.0e-9);
    overlap / shorter
}

fn choose_primary(parents: &[IndexedSample<'_>], indexes: &[usize]) -> usize {
    indexes
        .iter()
        .copied()
        .min_by(|&left, &right| {
            let left_area = parents[left].sample.fingerprint.area_or_length_mm;
            let right_area = parents[right].sample.fingerprint.area_or_length_mm;
            right_area.total_cmp(&left_area).then_with(|| {
                source_label(parents[left].candidate).cmp(&source_label(parents[right].candidate))
            })
        })
        .unwrap_or(indexes[0])
}

fn best_parent(
    parents: &[IndexedSample<'_>],
    hits: &[usize],
    child: &TopologySample,
) -> Option<usize> {
    hits.iter().copied().max_by(|&left, &right| {
        overlap_score(parents[left].sample, child)
            .total_cmp(&overlap_score(parents[right].sample, child))
            .then_with(|| {
                prefers_shared_tag(&parents[left], child)
                    .cmp(&prefers_shared_tag(&parents[right], child))
            })
            .then_with(|| {
                source_label(parents[left].candidate).cmp(&source_label(parents[right].candidate))
            })
    })
}

fn source_labels(parents: &[IndexedSample<'_>], indexes: &[usize]) -> Vec<String> {
    let mut labels: Vec<String> = indexes
        .iter()
        .map(|&index| source_label(parents[index].candidate))
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

fn source_label(candidate: &CandidateEntity) -> String {
    format!(
        "{}:{}",
        candidate.creator_node_id, candidate.local_output_slot
    )
}

fn centroid_order(children: &[TopologySample], indexes: &[usize]) -> Vec<usize> {
    let mut ordered = indexes.to_vec();
    ordered.sort_by(|&left, &right| {
        centroid_key(&children[left])
            .cmp(&centroid_key(&children[right]))
            .then(left.cmp(&right))
    });
    ordered
}

fn centroid_key(sample: &TopologySample) -> (OrderedFloat, OrderedFloat, OrderedFloat) {
    let centroid = sample.fingerprint.centroid_mm;
    (
        OrderedFloat(centroid[0]),
        OrderedFloat(centroid[1]),
        OrderedFloat(centroid[2]),
    )
}

#[derive(Clone, Copy, PartialEq)]
struct OrderedFloat(f64);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

fn fresh_slot(style: FreshSlotStyle, index: usize, sample: &TopologySample) -> String {
    match style {
        FreshSlotStyle::KernelOrder => match sample.fingerprint.entity_kind {
            EntityKind::Face => format!("face:{index}"),
            EntityKind::Edge => format!("edge:{index}"),
            EntityKind::Vertex => format!("vertex:{index}"),
        },
        FreshSlotStyle::Geometric => geometric_slot(sample),
    }
}

fn geometric_slot(sample: &TopologySample) -> String {
    match sample.fingerprint.entity_kind {
        EntityKind::Edge => {
            return format!(
                "edge-{}:{}:l{}",
                curve_label(sample.fingerprint.curve_class),
                quant_point(sample.fingerprint.centroid_mm, 1.0e-3),
                quantize(sample.fingerprint.area_or_length_mm, 1.0e-3)
            );
        }
        EntityKind::Vertex => {
            return format!(
                "vertex:{}",
                quant_point(sample.fingerprint.centroid_mm, 1.0e-3)
            );
        }
        EntityKind::Face => {}
    }
    match sample.fingerprint.surface_class {
        Some(SurfaceClass::Plane) => {
            if let Some(normal) = sample.fingerprint.normal_or_axis.and_then(normalize_vector) {
                return format!(
                    "plane:{}@{}",
                    quant_point(normal, 1.0e-4),
                    quantize(plane_offset(sample, normal), 1.0e-3)
                );
            }
        }
        Some(class @ (SurfaceClass::Cylinder | SurfaceClass::Cone | SurfaceClass::Torus)) => {
            if let Some(frame) = axis_frame(sample) {
                let axis = canonical_direction(frame.axis);
                let location = point_nearest_origin(frame.point, axis);
                return format!(
                    "{}:{}@{}:r{}",
                    surface_label(Some(class)),
                    quant_point(axis, 1.0e-4),
                    quant_point(location, 1.0e-3),
                    quantize(frame.radius, 1.0e-3)
                );
            }
        }
        Some(SurfaceClass::Sphere) => {
            if let Some(radius) = sample.fingerprint.radius_mm {
                return format!(
                    "sphere:{}:r{}",
                    quant_point(sample.fingerprint.centroid_mm, 1.0e-3),
                    quantize(radius, 1.0e-3)
                );
            }
        }
        _ => {}
    }
    format!(
        "face-{}:{}",
        surface_label(sample.fingerprint.surface_class),
        quant_point(sample.fingerprint.centroid_mm, 1.0e-3)
    )
}

fn surface_label(class: Option<SurfaceClass>) -> &'static str {
    match class {
        Some(SurfaceClass::Plane) => "plane",
        Some(SurfaceClass::Cylinder) => "cylinder",
        Some(SurfaceClass::Cone) => "cone",
        Some(SurfaceClass::Sphere) => "sphere",
        Some(SurfaceClass::Torus) => "torus",
        Some(SurfaceClass::BSpline) => "bspline",
        Some(SurfaceClass::SubD) => "subd",
        Some(SurfaceClass::Other) | None => "other",
    }
}

fn curve_label(class: Option<CurveClass>) -> &'static str {
    match class {
        Some(CurveClass::Line) => "line",
        Some(CurveClass::Circle) => "circle",
        Some(CurveClass::Ellipse) => "ellipse",
        Some(CurveClass::BSpline) => "bspline",
        Some(CurveClass::Other) | None => "other",
    }
}

fn sample_points(sample: &TopologySample) -> Vec<[f64; 3]> {
    let mut points = sample.outer_loop.clone();
    points.push(sample.fingerprint.centroid_mm);
    points
}

fn inside_fraction(points: &[[f64; 3]], container: &TopologySample) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let hits = points
        .iter()
        .filter(|point| point_inside_sample(**point, container))
        .count();
    hits as f64 / points.len() as f64
}

fn point_inside_sample(point: [f64; 3], sample: &TopologySample) -> bool {
    if sample.outer_loop.len() < 3 {
        return euclidean_distance(point, sample.fingerprint.centroid_mm)
            .is_some_and(|distance| distance <= 1.0e-3);
    }
    let Some(normal) = sample.fingerprint.normal_or_axis.and_then(normalize_vector) else {
        return false;
    };
    let Some((axis_u, axis_v)) = ortho_basis(normal) else {
        return false;
    };
    let origin = if let Some(mean) = mean_point(&sample.outer_loop) {
        mean
    } else {
        sample.fingerprint.centroid_mm
    };
    let polygon: Vec<[f64; 2]> = sample
        .outer_loop
        .iter()
        .copied()
        .map(|corner| project(corner, origin, axis_u, axis_v))
        .collect();
    point_in_polygon(
        project(point, origin, axis_u, axis_v),
        &polygon,
        BOUNDARY_EPS_MM,
    )
}

fn point_in_polygon(point: [f64; 2], polygon: &[[f64; 2]], epsilon: f64) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    for (start, end) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
        if distance_to_segment(point, *start, *end) <= epsilon {
            return true;
        }
    }
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let start = polygon[previous];
        let end = polygon[current];
        let crosses = (start[1] > point[1]) != (end[1] > point[1]);
        if crosses {
            let x_at_point = (end[0] - start[0]) * (point[1] - start[1])
                / (end[1] - start[1] + 1.0e-30)
                + start[0];
            if point[0] < x_at_point {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

fn distance_to_segment(point: [f64; 2], start: [f64; 2], end: [f64; 2]) -> f64 {
    let segment = [end[0] - start[0], end[1] - start[1]];
    let offset = [point[0] - start[0], point[1] - start[1]];
    let length_sq = segment[0] * segment[0] + segment[1] * segment[1];
    if length_sq <= 1.0e-24 {
        return offset[0].hypot(offset[1]);
    }
    let parameter = ((offset[0] * segment[0] + offset[1] * segment[1]) / length_sq).clamp(0.0, 1.0);
    let projection = [
        start[0] + segment[0] * parameter,
        start[1] + segment[1] * parameter,
    ];
    (point[0] - projection[0]).hypot(point[1] - projection[1])
}

fn plane_offset(sample: &TopologySample, normal: [f64; 3]) -> f64 {
    let point = mean_point(&sample.outer_loop).unwrap_or(sample.fingerprint.centroid_mm);
    dot(normal, point)
}

fn point_nearest_origin(point: [f64; 3], axis: [f64; 3]) -> [f64; 3] {
    add(point, scale(axis, -dot(point, axis)))
}

fn canonical_direction(direction: [f64; 3]) -> [f64; 3] {
    let Some(axis) = normalize_vector(direction) else {
        return direction;
    };
    for component in axis {
        if component.abs() > 1.0e-8 {
            if component < 0.0 {
                return axis.map(|value| -value);
            }
            break;
        }
    }
    axis
}

fn characteristic_length(sample: &TopologySample) -> f64 {
    let measure = match sample.fingerprint.entity_kind {
        EntityKind::Face => sample.fingerprint.area_or_length_mm.max(0.0).sqrt(),
        EntityKind::Edge => sample.fingerprint.area_or_length_mm.max(0.0),
        EntityKind::Vertex => 0.0,
    };
    measure
        .max(sample.fingerprint.radius_mm.unwrap_or(0.0))
        .max(1.0)
}

fn quant_point(point: [f64; 3], step: f64) -> String {
    format!(
        "{},{},{}",
        quantize(point[0], step),
        quantize(point[1], step),
        quantize(point[2], step)
    )
}

fn quantize(value: f64, step: f64) -> i64 {
    if !value.is_finite() || !step.is_finite() || step == 0.0 {
        return 0;
    }
    (value / step).round() as i64
}

fn cos_tolerance(degrees: f64) -> f64 {
    if !degrees.is_finite() {
        return 1.0_f64.to_radians().cos();
    }
    degrees.clamp(0.0, 180.0).to_radians().cos()
}

fn nonnegative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn ordered(left: f64, right: f64) -> (f64, f64) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn ortho_basis(normal: [f64; 3]) -> Option<([f64; 3], [f64; 3])> {
    let normal = normalize_vector(normal)?;
    let helper = if normal[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let axis_u = normalize_vector(cross(helper, normal))?;
    Some((axis_u, cross(normal, axis_u)))
}

fn project(point: [f64; 3], origin: [f64; 3], axis_u: [f64; 3], axis_v: [f64; 3]) -> [f64; 2] {
    let delta = sub(point, origin);
    [dot(delta, axis_u), dot(delta, axis_v)]
}

fn mean_point(points: &[[f64; 3]]) -> Option<[f64; 3]> {
    if points.is_empty() {
        return None;
    }
    let mut sum = [0.0; 3];
    for point in points {
        if point.iter().any(|value| !value.is_finite()) {
            return None;
        }
        for axis in 0..3 {
            sum[axis] += point[axis];
        }
    }
    let scale = points.len() as f64;
    Some(sum.map(|value| value / scale))
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn sub(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(vector: [f64; 3], factor: f64) -> [f64; 3] {
    [vector[0] * factor, vector[1] * factor, vector[2] * factor]
}

// Provenance tags break ties when two parents overlap one child equally.
fn prefers_shared_tag(parent: &IndexedSample<'_>, child: &TopologySample) -> bool {
    parent
        .sample
        .provenance_tags
        .iter()
        .any(|tag| !tag.trim().is_empty() && child.provenance_tags.iter().any(|other| other == tag))
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        Self {
            parent: (0..size).collect(),
        }
    }

    fn find(&mut self, mut index: usize) -> usize {
        while self.parent[index] != index {
            self.parent[index] = self.parent[self.parent[index]];
            index = self.parent[index];
        }
        index
    }

    fn unite(&mut self, left: usize, right: usize) {
        let left = self.find(left);
        let right = self.find(right);
        if left != right {
            self.parent[right] = left;
        }
    }
}

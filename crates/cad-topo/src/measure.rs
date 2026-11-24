//! Exact measurements between two topology entities.
//!
//! Distances use the fingerprint centroid. Plane spacing treats that centroid as
//! a point on the plane. Coaxiality treats it as a point on the axis. The kernel
//! adapter is responsible for storing those points that way.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fingerprint::{
    CurveClass, EntityKind, GeometricFingerprint, MatchTolerances, SurfaceClass, cross,
    directed_angle_deg, dot, normalize_vector, sub, tidy, undirected_angle_deg, vector_norm,
};
use crate::lineage::TopoEntityId;
use crate::query::TopoRef;
use crate::resolver::{CandidateEntity, ResolveResult, TopoError, TopoSnapshot, resolve_topo_ref};

/// Which measurement to compute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeasurementKind {
    /// Euclidean distance between the two centroids, plus the delta vector.
    CentroidDistance,
    /// Perpendicular spacing of two parallel planar faces. This is wall thickness.
    ParallelPlaneDistance,
    /// Axis angle and the radial gap between two cylindrical or circular entities.
    Coaxiality,
    /// Directed angle in degrees between the stored normals or axes, from 0 to 180.
    AngleDeg,
}

/// Tolerances for parallelism and coaxiality.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasurementOptions {
    /// Maximum angle in degrees for parallel normals and coaxial axes.
    pub angle_tol_deg: f64,
    /// Maximum radial gap in millimetres that still counts as coaxial.
    pub coaxial_radial_tol_mm: f64,
}

impl Default for MeasurementOptions {
    fn default() -> Self {
        Self {
            angle_tol_deg: 1.0,
            coaxial_radial_tol_mm: 1.0e-3,
        }
    }
}

/// Numbers, units, and short notes for one measurement.
///
/// `value` is the primary number and `unit` says what it is:
/// millimetres for distance and coaxial radial offset, degrees for [`MeasurementKind::AngleDeg`].
/// The optional fields are filled only for the measurement that uses them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasurementResult {
    pub kind: MeasurementKind,
    pub left_entity_id: TopoEntityId,
    pub right_entity_id: TopoEntityId,
    pub value: f64,
    pub unit: String,
    /// Right centroid minus left centroid. Set for centroid distance.
    pub delta_mm: Option<[f64; 3]>,
    /// `(right centroid - left centroid)` dotted with the left unit normal.
    pub signed_distance_mm: Option<f64>,
    /// Absolute plane spacing. Use this as wall thickness.
    pub unsigned_distance_mm: Option<f64>,
    /// Undirected axis angle in degrees, from 0 to 90. Set for coaxiality.
    pub angular_deviation_deg: Option<f64>,
    /// Shortest distance between the two axes, in millimetres.
    pub radial_offset_mm: Option<f64>,
    pub is_coaxial: Option<bool>,
    pub notes: Vec<String>,
}

/// Why a measurement could not be computed.
#[derive(Clone, Debug, PartialEq, Error, Serialize, Deserialize)]
pub enum MeasurementError {
    #[error("entity '{entity_id}' is not a planar face")]
    NotPlanar { entity_id: String },
    #[error(
        "faces '{left_entity_id}' and '{right_entity_id}' are not parallel: axis angle is {angle_deg} deg (tolerance {tolerance_deg} deg)"
    )]
    NotParallel {
        left_entity_id: String,
        right_entity_id: String,
        angle_deg: f64,
        tolerance_deg: f64,
    },
    #[error("entity '{entity_id}' has no usable normal or axis")]
    MissingDirection { entity_id: String },
    #[error("entity '{entity_id}' is not a cylindrical face or circular edge")]
    NotCoaxialCandidate { entity_id: String },
    #[error("{side} topology reference is ambiguous")]
    AmbiguousReference {
        side: String,
        entity_ids: Vec<TopoEntityId>,
    },
    #[error("{side} topology reference has no match: {reason}")]
    Unresolved { side: String, reason: String },
    #[error("invalid measurement: {0}")]
    Invalid(String),
}

/// Measure two already resolved entities.
pub fn measure_geometry(
    left: &CandidateEntity,
    right: &CandidateEntity,
    kind: MeasurementKind,
    options: &MeasurementOptions,
) -> Result<MeasurementResult, MeasurementError> {
    validate_options(options)?;
    match kind {
        MeasurementKind::CentroidDistance => centroid_distance(left, right),
        MeasurementKind::ParallelPlaneDistance => parallel_plane_distance(left, right, options),
        MeasurementKind::Coaxiality => coaxiality(left, right, options),
        MeasurementKind::AngleDeg => angle_deg(left, right),
    }
}

/// Resolve two references against one snapshot, then measure them.
///
/// Each reference must resolve to exactly one live entity.
pub fn measure_geometry_refs(
    snapshot: &TopoSnapshot,
    left: &TopoRef,
    right: &TopoRef,
    kind: MeasurementKind,
    tolerances: &MatchTolerances,
    options: &MeasurementOptions,
) -> Result<MeasurementResult, MeasurementError> {
    let left = resolve_one(snapshot, left, tolerances, "left")?;
    let right = resolve_one(snapshot, right, tolerances, "right")?;
    measure_geometry(left, right, kind, options)
}

fn validate_options(options: &MeasurementOptions) -> Result<(), MeasurementError> {
    if !options.angle_tol_deg.is_finite() || !(0.0..=180.0).contains(&options.angle_tol_deg) {
        return Err(MeasurementError::Invalid(
            "angle_tol_deg must be finite and in 0..=180".to_owned(),
        ));
    }
    if !options.coaxial_radial_tol_mm.is_finite() || options.coaxial_radial_tol_mm < 0.0 {
        return Err(MeasurementError::Invalid(
            "coaxial_radial_tol_mm must be finite and non-negative".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_one<'a>(
    snapshot: &'a TopoSnapshot,
    topo_ref: &TopoRef,
    tolerances: &MatchTolerances,
    side: &str,
) -> Result<&'a CandidateEntity, MeasurementError> {
    match resolve_topo_ref(topo_ref, snapshot, tolerances) {
        Ok(ResolveResult::Unique(id)) => snapshot
            .candidates
            .iter()
            .find(|candidate| candidate.entity_id == id)
            .ok_or_else(|| MeasurementError::Unresolved {
                side: side.to_owned(),
                reason: format!("resolved id '{}' is not in the snapshot", id.0),
            }),
        Ok(ResolveResult::Multiple(entity_ids)) => Err(MeasurementError::AmbiguousReference {
            side: side.to_owned(),
            entity_ids,
        }),
        Err(TopoError::AmbiguousMatch { candidates, .. }) => {
            Err(MeasurementError::AmbiguousReference {
                side: side.to_owned(),
                entity_ids: candidates,
            })
        }
        Err(error) => Err(MeasurementError::Unresolved {
            side: side.to_owned(),
            reason: error.to_string(),
        }),
    }
}

fn centroid_distance(
    left: &CandidateEntity,
    right: &CandidateEntity,
) -> Result<MeasurementResult, MeasurementError> {
    let left_centroid = finite_centroid(left)?;
    let right_centroid = finite_centroid(right)?;
    let delta = sub(right_centroid, left_centroid).map(tidy);
    let distance = tidy(vector_norm(delta));
    Ok(MeasurementResult {
        kind: MeasurementKind::CentroidDistance,
        left_entity_id: left.entity_id.clone(),
        right_entity_id: right.entity_id.clone(),
        value: distance,
        unit: "mm".to_owned(),
        delta_mm: Some(delta),
        signed_distance_mm: None,
        unsigned_distance_mm: None,
        angular_deviation_deg: None,
        radial_offset_mm: None,
        is_coaxial: None,
        notes: vec![
            "euclidean distance between centroids".to_owned(),
            "delta_mm is the right centroid minus the left centroid".to_owned(),
        ],
    })
}

fn parallel_plane_distance(
    left: &CandidateEntity,
    right: &CandidateEntity,
    options: &MeasurementOptions,
) -> Result<MeasurementResult, MeasurementError> {
    let left_plane = require_plane(left)?;
    let right_plane = require_plane(right)?;
    let Some(parallel_angle) = undirected_angle_deg(left_plane.normal, right_plane.normal) else {
        return Err(MeasurementError::MissingDirection {
            entity_id: left.entity_id.0.clone(),
        });
    };
    if parallel_angle > options.angle_tol_deg + 1.0e-9 {
        return Err(MeasurementError::NotParallel {
            left_entity_id: left.entity_id.0.clone(),
            right_entity_id: right.entity_id.0.clone(),
            angle_deg: parallel_angle,
            tolerance_deg: options.angle_tol_deg,
        });
    }
    let signed = tidy(dot(
        sub(right_plane.centroid, left_plane.centroid),
        left_plane.normal,
    ));
    let unsigned = tidy(signed.abs());
    let directed = directed_angle_deg(left_plane.normal, right_plane.normal).unwrap_or(0.0);
    let alignment = if directed > 90.0 {
        "normals are anti-parallel"
    } else {
        "normals are parallel"
    };
    Ok(MeasurementResult {
        kind: MeasurementKind::ParallelPlaneDistance,
        left_entity_id: left.entity_id.clone(),
        right_entity_id: right.entity_id.clone(),
        value: unsigned,
        unit: "mm".to_owned(),
        delta_mm: None,
        signed_distance_mm: Some(signed),
        unsigned_distance_mm: Some(unsigned),
        angular_deviation_deg: None,
        radial_offset_mm: None,
        is_coaxial: None,
        notes: vec![
            "unsigned distance is the spacing between the two planes".to_owned(),
            "signed distance follows the left face unit normal".to_owned(),
            alignment.to_owned(),
        ],
    })
}

fn coaxiality(
    left: &CandidateEntity,
    right: &CandidateEntity,
    options: &MeasurementOptions,
) -> Result<MeasurementResult, MeasurementError> {
    let left_axis = require_axis(left)?;
    let right_axis = require_axis(right)?;
    let angular =
        undirected_angle_deg(left_axis.direction, right_axis.direction).ok_or_else(|| {
            MeasurementError::MissingDirection {
                entity_id: left.entity_id.0.clone(),
            }
        })?;
    let radial = tidy(axis_separation(
        left_axis.centroid,
        left_axis.direction,
        right_axis.centroid,
        right_axis.direction,
    ));
    let is_coaxial = angular <= options.angle_tol_deg + 1.0e-9
        && radial <= options.coaxial_radial_tol_mm + 1.0e-9;
    Ok(MeasurementResult {
        kind: MeasurementKind::Coaxiality,
        left_entity_id: left.entity_id.clone(),
        right_entity_id: right.entity_id.clone(),
        value: radial,
        unit: "mm".to_owned(),
        delta_mm: None,
        signed_distance_mm: None,
        unsigned_distance_mm: None,
        angular_deviation_deg: Some(angular),
        radial_offset_mm: Some(radial),
        is_coaxial: Some(is_coaxial),
        notes: vec![
            "axis direction sign is ignored".to_owned(),
            "radial offset is the shortest distance between the two axes".to_owned(),
            "centroids are treated as points on those axes".to_owned(),
        ],
    })
}

fn angle_deg(
    left: &CandidateEntity,
    right: &CandidateEntity,
) -> Result<MeasurementResult, MeasurementError> {
    let left_direction = require_direction(left)?;
    let right_direction = require_direction(right)?;
    let angle = directed_angle_deg(left_direction, right_direction).ok_or_else(|| {
        MeasurementError::MissingDirection {
            entity_id: left.entity_id.0.clone(),
        }
    })?;
    Ok(MeasurementResult {
        kind: MeasurementKind::AngleDeg,
        left_entity_id: left.entity_id.clone(),
        right_entity_id: right.entity_id.clone(),
        value: angle,
        unit: "deg".to_owned(),
        delta_mm: None,
        signed_distance_mm: None,
        unsigned_distance_mm: None,
        angular_deviation_deg: None,
        radial_offset_mm: None,
        is_coaxial: None,
        notes: vec![
            "angle is between the stored normal_or_axis vectors and stays in 0..=180".to_owned(),
            "cylinder axis sign is the sign stored on the fingerprint; coaxiality ignores that sign"
                .to_owned(),
        ],
    })
}

struct Support {
    centroid: [f64; 3],
    normal: [f64; 3],
}

fn require_plane(entity: &CandidateEntity) -> Result<Support, MeasurementError> {
    let fingerprint = &entity.fingerprint;
    if fingerprint.entity_kind != EntityKind::Face
        || fingerprint.surface_class != Some(SurfaceClass::Plane)
    {
        return Err(MeasurementError::NotPlanar {
            entity_id: entity.entity_id.0.clone(),
        });
    }
    let normal = require_direction(entity)?;
    let centroid = finite_centroid(entity)?;
    Ok(Support { centroid, normal })
}

struct Axis {
    centroid: [f64; 3],
    direction: [f64; 3],
}

fn require_axis(entity: &CandidateEntity) -> Result<Axis, MeasurementError> {
    if !is_cylindrical_or_circular(&entity.fingerprint) {
        return Err(MeasurementError::NotCoaxialCandidate {
            entity_id: entity.entity_id.0.clone(),
        });
    }
    let direction = require_direction(entity)?;
    let centroid = finite_centroid(entity)?;
    Ok(Axis {
        centroid,
        direction,
    })
}

fn is_cylindrical_or_circular(fingerprint: &GeometricFingerprint) -> bool {
    match fingerprint.entity_kind {
        EntityKind::Face => matches!(
            fingerprint.surface_class,
            Some(SurfaceClass::Cylinder | SurfaceClass::Cone)
        ),
        EntityKind::Edge => fingerprint.curve_class == Some(CurveClass::Circle),
        EntityKind::Vertex => false,
    }
}

fn require_direction(entity: &CandidateEntity) -> Result<[f64; 3], MeasurementError> {
    entity
        .fingerprint
        .normal_or_axis
        .and_then(normalize_vector)
        .ok_or_else(|| MeasurementError::MissingDirection {
            entity_id: entity.entity_id.0.clone(),
        })
}

fn finite_centroid(entity: &CandidateEntity) -> Result<[f64; 3], MeasurementError> {
    let centroid = entity.fingerprint.centroid_mm;
    if centroid.iter().any(|value| !value.is_finite()) {
        return Err(MeasurementError::Invalid(format!(
            "centroid of '{}' is not finite",
            entity.entity_id.0
        )));
    }
    Ok(centroid)
}

/// Shortest distance between two parametric axes.
fn axis_separation(
    origin_left: [f64; 3],
    direction_left: [f64; 3],
    origin_right: [f64; 3],
    direction_right: [f64; 3],
) -> f64 {
    let delta = sub(origin_right, origin_left);
    let crossed = cross(direction_left, direction_right);
    let crossed_length = vector_norm(crossed);
    if crossed_length <= 1.0e-12 {
        vector_norm(cross(delta, direction_left))
    } else {
        dot(delta, crossed).abs() / crossed_length
    }
}

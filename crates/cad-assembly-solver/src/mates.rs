//! Engineering mates on component-local anchors.
//!
//! Each anchor carries a point and an axis. The axis is a bore axis, a plane
//! normal, or a general direction, depending on the mate.

use serde::{Deserialize, Serialize};

use crate::pose::{self, RigidPose};

/// Lever arm that converts one radian of axis error into millimetres.
pub(crate) const AXIS_LEVER_MM: f64 = 10.0;

/// Point and direction fixed in a component's local frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MateAnchor {
    pub component_id: String,
    /// Anchor point in the component frame, in millimetres.
    pub origin_local_mm: [f64; 3],
    /// Local axis. It does not need to be unit length; zero is rejected.
    pub axis_local: [f64; 3],
}

/// Supported 3D assembly mates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AssemblyMateKind {
    /// Axes parallel or anti-parallel, and `b`'s point lies on `a`'s axis.
    Coaxial { a: MateAnchor, b: MateAnchor },
    /// Planes parallel, with signed distance `offset_mm` along normal `a`.
    ///
    /// `anti_aligned` asks the normals to point in opposite directions.
    /// Otherwise they must point the same way.
    Coincident {
        a: MateAnchor,
        b: MateAnchor,
        offset_mm: f64,
        anti_aligned: bool,
    },
    /// Euclidean distance between the two anchor points.
    Distance {
        a: MateAnchor,
        b: MateAnchor,
        target_mm: f64,
    },
    /// Axes parallel or anti-parallel. The points are not constrained.
    Parallel { a: MateAnchor, b: MateAnchor },
    /// Unsigned angle between the two axes, in degrees, in `[0, 180]`.
    Angle {
        a: MateAnchor,
        b: MateAnchor,
        target_deg: f64,
    },
}

/// One weighted mate in an assembly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssemblyMate {
    pub id: String,
    pub kind: AssemblyMateKind,
    /// Multiplier on this mate's residual block. `1.0` is the normal weight.
    pub weight: f64,
}

/// Anchor resolved to a component index for the inner loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnchorRef {
    pub component_index: usize,
    pub origin_local_mm: [f64; 3],
    pub axis_local: [f64; 3],
}

/// Mate geometry after component ids have been resolved.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ResidualKind {
    Coaxial {
        a: AnchorRef,
        b: AnchorRef,
    },
    Coincident {
        a: AnchorRef,
        b: AnchorRef,
        offset_mm: f64,
        anti_aligned: bool,
    },
    Distance {
        a: AnchorRef,
        b: AnchorRef,
        target_mm: f64,
    },
    Parallel {
        a: AnchorRef,
        b: AnchorRef,
    },
    Angle {
        a: AnchorRef,
        b: AnchorRef,
        target_deg: f64,
    },
}

pub(crate) fn anchors_of(kind: &AssemblyMateKind) -> (&MateAnchor, &MateAnchor) {
    match kind {
        AssemblyMateKind::Coaxial { a, b }
        | AssemblyMateKind::Coincident { a, b, .. }
        | AssemblyMateKind::Distance { a, b, .. }
        | AssemblyMateKind::Parallel { a, b }
        | AssemblyMateKind::Angle { a, b, .. } => (a, b),
    }
}

pub(crate) fn world_anchor(pose: &RigidPose, anchor: &AnchorRef) -> ([f64; 3], [f64; 3]) {
    let origin = pose.transform_point(anchor.origin_local_mm);
    let axis = pose::unit(pose.transform_dir(anchor.axis_local));
    (origin, axis)
}

/// Append the weighted residual block for `kind` at `poses`.
pub(crate) fn push_residuals(
    kind: &ResidualKind,
    poses: &[RigidPose],
    weight: f64,
    out: &mut Vec<f64>,
) {
    let start = out.len();
    match *kind {
        ResidualKind::Coaxial { a, b } => push_coaxial(poses, a, b, out),
        ResidualKind::Coincident {
            a,
            b,
            offset_mm,
            anti_aligned,
        } => push_coincident(poses, a, b, offset_mm, anti_aligned, out),
        ResidualKind::Distance { a, b, target_mm } => {
            out.push(point_distance(poses, a, b) - target_mm);
        }
        ResidualKind::Parallel { a, b } => push_parallel(poses, a, b, out),
        ResidualKind::Angle { a, b, target_deg } => {
            let err_rad = angle_rad(poses, a, b) - target_deg.to_radians();
            out.push(err_rad * AXIS_LEVER_MM);
        }
    }
    if weight != 1.0 {
        for value in &mut out[start..] {
            *value *= weight;
        }
    }
}

/// Scalar reading used to blend a mate toward its target during continuation.
///
/// Alignment-only mates return `None` because their target is identically zero.
pub(crate) fn initial_scalar(kind: &ResidualKind, poses: &[RigidPose]) -> Option<f64> {
    match *kind {
        ResidualKind::Coincident { a, b, .. } => Some(signed_distance(poses, a, b)),
        ResidualKind::Distance { a, b, .. } => Some(point_distance(poses, a, b)),
        ResidualKind::Angle { a, b, .. } => Some(angle_rad(poses, a, b).to_degrees()),
        ResidualKind::Coaxial { .. } | ResidualKind::Parallel { .. } => None,
    }
}

pub(crate) fn blend_target(kind: ResidualKind, initial: Option<f64>, t: f64) -> ResidualKind {
    let Some(initial) = initial else {
        return kind;
    };
    let lerp = |target: f64| (1.0 - t) * initial + t * target;
    match kind {
        ResidualKind::Coincident {
            a,
            b,
            offset_mm,
            anti_aligned,
        } => ResidualKind::Coincident {
            a,
            b,
            offset_mm: lerp(offset_mm),
            anti_aligned,
        },
        ResidualKind::Distance { a, b, target_mm } => ResidualKind::Distance {
            a,
            b,
            target_mm: lerp(target_mm),
        },
        ResidualKind::Angle { a, b, target_deg } => ResidualKind::Angle {
            a,
            b,
            target_deg: lerp(target_deg),
        },
        other => other,
    }
}

fn push_coaxial(poses: &[RigidPose], a: AnchorRef, b: AnchorRef, out: &mut Vec<f64>) {
    let (pa, na) = world_anchor(&poses[a.component_index], &a);
    let (pb, nb) = world_anchor(&poses[b.component_index], &b);
    // Point b on axis a: (pb - pa) × na = 0. Three smooth entries, rank 2.
    let on_axis = pose::cross(pose::sub(pb, pa), na);
    out.extend_from_slice(&on_axis);
    // Axes parallel or anti-parallel: na × nb = 0.
    let aligned = pose::scale(pose::cross(na, nb), AXIS_LEVER_MM);
    out.extend_from_slice(&aligned);
}

fn push_coincident(
    poses: &[RigidPose],
    a: AnchorRef,
    b: AnchorRef,
    offset_mm: f64,
    anti_aligned: bool,
    out: &mut Vec<f64>,
) {
    let (pa, na) = world_anchor(&poses[a.component_index], &a);
    let (pb, nb) = world_anchor(&poses[b.component_index], &b);
    let sign = if anti_aligned { -1.0 } else { 1.0 };
    // nb - sign * na is zero only at the requested pole, and smooth.
    let align = pose::scale(pose::sub(nb, pose::scale(na, sign)), AXIS_LEVER_MM);
    out.extend_from_slice(&align);
    out.push(pose::dot(pose::sub(pb, pa), na) - offset_mm);
}

fn push_parallel(poses: &[RigidPose], a: AnchorRef, b: AnchorRef, out: &mut Vec<f64>) {
    let (_, na) = world_anchor(&poses[a.component_index], &a);
    let (_, nb) = world_anchor(&poses[b.component_index], &b);
    let aligned = pose::scale(pose::cross(na, nb), AXIS_LEVER_MM);
    out.extend_from_slice(&aligned);
}

fn signed_distance(poses: &[RigidPose], a: AnchorRef, b: AnchorRef) -> f64 {
    let (pa, na) = world_anchor(&poses[a.component_index], &a);
    let (pb, _) = world_anchor(&poses[b.component_index], &b);
    pose::dot(pose::sub(pb, pa), na)
}

fn point_distance(poses: &[RigidPose], a: AnchorRef, b: AnchorRef) -> f64 {
    let (pa, _) = world_anchor(&poses[a.component_index], &a);
    let (pb, _) = world_anchor(&poses[b.component_index], &b);
    pose::length(pose::sub(pb, pa))
}

fn angle_rad(poses: &[RigidPose], a: AnchorRef, b: AnchorRef) -> f64 {
    let (_, na) = world_anchor(&poses[a.component_index], &a);
    let (_, nb) = world_anchor(&poses[b.component_index], &b);
    let sine = pose::length(pose::cross(na, nb));
    let cosine = pose::dot(na, nb).clamp(-1.0, 1.0);
    sine.atan2(cosine)
}

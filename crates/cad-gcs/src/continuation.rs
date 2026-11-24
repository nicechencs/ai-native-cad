//! Homotopy continuation for parameter changes in a 2D sketch.
//!
//! A continuation stage changes the numeric parameters of a clone of the
//! initial sketch and then uses the normal damped solver to correct that
//! clone.  Since every accepted stage is used as the next stage's initial
//! state, the solver follows the branch connected to the initial solution.

use crate::{Constraint, SketchGeom, SketchSystem};

/// Controls the number and convergence criteria of continuation stages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContinuationConfig {
    /// Number of homotopy increments from the initial to the target sketch.
    pub steps: usize,
    /// Maximum damped-solver iterations for each increment.
    pub max_iters_per_step: usize,
    /// Check all [`OrientationLock`] values after every accepted stage.
    pub enforce_chirality: bool,
    /// RMS residual required for a stage to be accepted.
    pub residual_tol: f64,
}

impl Default for ContinuationConfig {
    fn default() -> Self {
        Self {
            steps: 12,
            max_iters_per_step: 100,
            enforce_chirality: true,
            residual_tol: 1e-8,
        }
    }
}

/// A signed area constraint for three sketch points.
///
/// `expected_sign` is normally `1.0` or `-1.0`.  The lock is satisfied when
/// the signed cross product has the same sign as `expected_sign`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrientationLock {
    pub a: usize,
    pub b: usize,
    pub c: usize,
    pub expected_sign: f64,
}

/// Result of a continuation attempt.
#[derive(Debug, Clone)]
pub struct ContinuationReport {
    /// True when an orientation lock rejected a stage (including stage zero).
    pub flipped: bool,
    /// Number of stages accepted before the attempt stopped.
    pub steps_completed: usize,
    /// RMS residual of the last accepted state.
    pub final_residual: f64,
    /// Last accepted sketch.  If no stage was accepted, this is the initial
    /// sketch clone.
    pub solved_sketch: SketchSystem,
}

/// Solves a parameter change by taking small, warm-started homotopy steps.
///
/// The initial and target sketches must have compatible point, geometry, and
/// constraint topology.  Constraint names may differ.  Distances, radii,
/// tangent-line numeric data, fixed point coordinates, and circle radii are
/// linearly interpolated at each stage.  A stage that does not converge or
/// violates an orientation lock is rejected and the last accepted state is
/// returned.  A zero `steps` value performs one final stage.  Incompatible
/// topology returns the untouched initial clone with zero completed stages.
pub fn solve_with_continuation(
    initial_sketch: &SketchSystem,
    target_sketch: &SketchSystem,
    locks: &[OrientationLock],
    config: &ContinuationConfig,
) -> ContinuationReport {
    let mut solved_sketch = initial_sketch.clone();
    let initial_residual = solved_sketch.residual_rms();

    if !compatible_topology(initial_sketch, target_sketch) {
        return ContinuationReport {
            flipped: false,
            steps_completed: 0,
            final_residual: initial_residual,
            solved_sketch,
        };
    }

    if config.enforce_chirality
        && !locks
            .iter()
            .all(|lock| orientation_ok(&solved_sketch, lock))
    {
        return ContinuationReport {
            flipped: true,
            steps_completed: 0,
            final_residual: initial_residual,
            solved_sketch,
        };
    }

    // A zero-step configuration still performs one final solve.  This makes
    // the function useful with configuration values assembled dynamically and
    // avoids returning an unprocessed target by accident.
    let stage_count = config.steps.max(1);
    let tolerance = config.residual_tol.max(0.0);

    for stage in 1..=stage_count {
        let t = stage as f64 / stage_count as f64;
        let mut candidate = solved_sketch.clone();
        interpolate_stage(initial_sketch, target_sketch, t, &mut candidate);

        let solve_report = candidate.solve(config.max_iters_per_step, tolerance);
        let candidate_residual = candidate.residual_rms();

        // Check chirality before convergence.  A failed solve can still have
        // crossed to another branch, and that event must be reported as a
        // flip instead of being hidden as a generic convergence failure.
        if config.enforce_chirality && !locks.iter().all(|lock| orientation_ok(&candidate, lock)) {
            return ContinuationReport {
                flipped: true,
                steps_completed: stage - 1,
                final_residual: solved_sketch.residual_rms(),
                solved_sketch,
            };
        }

        let converged = solve_report.status == crate::SolveStatus::Converged
            && candidate_residual.is_finite()
            && candidate_residual <= tolerance;
        if !converged {
            return ContinuationReport {
                flipped: false,
                steps_completed: stage - 1,
                final_residual: solved_sketch.residual_rms(),
                solved_sketch,
            };
        }

        solved_sketch = candidate;
    }

    ContinuationReport {
        flipped: false,
        steps_completed: stage_count,
        final_residual: solved_sketch.residual_rms(),
        solved_sketch,
    }
}

fn interpolate_stage(
    initial: &SketchSystem,
    target: &SketchSystem,
    t: f64,
    candidate: &mut SketchSystem,
) {
    for ((initial_point, target_point), candidate_point) in initial
        .points
        .iter()
        .zip(&target.points)
        .zip(&mut candidate.points)
    {
        if initial_point.fixed {
            candidate_point.x = lerp(initial_point.x, target_point.x, t);
            candidate_point.y = lerp(initial_point.y, target_point.y, t);
        }
    }

    for ((initial_geom, target_geom), candidate_geom) in initial
        .geoms
        .iter()
        .zip(&target.geoms)
        .zip(&mut candidate.geoms)
    {
        if let (
            SketchGeom::Circle {
                radius: initial_radius,
                radius_fixed: initial_fixed,
                ..
            },
            SketchGeom::Circle {
                radius: target_radius,
                radius_fixed: target_fixed,
                ..
            },
            SketchGeom::Circle {
                radius: candidate_radius,
                ..
            },
        ) = (initial_geom, target_geom, candidate_geom)
        {
            if *initial_fixed && *target_fixed {
                *candidate_radius = lerp(*initial_radius, *target_radius, t);
            }
        }
    }

    for ((initial_constraint, target_constraint), candidate_constraint) in initial
        .constraints
        .iter()
        .zip(&target.constraints)
        .zip(&mut candidate.constraints)
    {
        match (initial_constraint, target_constraint, candidate_constraint) {
            (
                Constraint::Distance {
                    target: initial_target,
                    ..
                },
                Constraint::Distance {
                    target: target_target,
                    ..
                },
                Constraint::Distance {
                    target: candidate_target,
                    ..
                },
            )
            | (
                Constraint::Radius {
                    target: initial_target,
                    ..
                },
                Constraint::Radius {
                    target: target_target,
                    ..
                },
                Constraint::Radius {
                    target: candidate_target,
                    ..
                },
            ) => {
                *candidate_target = lerp(*initial_target, *target_target, t);
            }
            (
                Constraint::TangentLineArc {
                    line_dir: initial_dir,
                    line_point: initial_point,
                    ..
                },
                Constraint::TangentLineArc {
                    line_dir: target_dir,
                    line_point: target_point,
                    ..
                },
                Constraint::TangentLineArc {
                    line_dir: candidate_dir,
                    line_point: candidate_point,
                    ..
                },
            ) => {
                let mut aligned_target_dir = *target_dir;
                let direction_dot = initial_dir[0] * target_dir[0] + initial_dir[1] * target_dir[1];
                if direction_dot.is_finite() && direction_dot < 0.0 {
                    aligned_target_dir[0] = -aligned_target_dir[0];
                    aligned_target_dir[1] = -aligned_target_dir[1];
                }
                for axis in 0..2 {
                    candidate_dir[axis] = lerp(initial_dir[axis], aligned_target_dir[axis], t);
                    candidate_point[axis] = lerp(initial_point[axis], target_point[axis], t);
                }
            }
            _ => {}
        }
    }
    if t >= 1.0 {
        // Preserve target-side names and every numeric field exactly at the
        // final stage.  The topology check above guarantees this replacement
        // does not change the variable layout.
        candidate.constraints.clone_from(&target.constraints);
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    if t >= 1.0 {
        b
    } else if t <= 0.0 {
        a
    } else {
        // Weighted endpoints avoid overflowing `b - a` for finite values
        // with opposite signs, while keeping every interpolated value within
        // the endpoint range.
        a * (1.0 - t) + b * t
    }
}

fn orientation_ok(sketch: &SketchSystem, lock: &OrientationLock) -> bool {
    if !lock.expected_sign.is_finite() || lock.expected_sign == 0.0 {
        return false;
    }
    let (Some(a), Some(b), Some(c)) = (
        sketch.points.get(lock.a),
        sketch.points.get(lock.b),
        sketch.points.get(lock.c),
    ) else {
        return false;
    };
    let cross = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    cross.is_finite() && cross != 0.0 && cross.signum() == lock.expected_sign.signum()
}

fn compatible_topology(initial: &SketchSystem, target: &SketchSystem) -> bool {
    initial.points.len() == target.points.len()
        && initial.geoms.len() == target.geoms.len()
        && initial.constraints.len() == target.constraints.len()
        && initial
            .points
            .iter()
            .zip(&target.points)
            .all(|(a, b)| a.id == b.id && a.fixed == b.fixed)
        && initial
            .geoms
            .iter()
            .zip(&target.geoms)
            .all(|(a, b)| same_geometry_shape(a, b))
        && initial
            .constraints
            .iter()
            .zip(&target.constraints)
            .all(|(a, b)| same_constraint_shape(a, b))
}

fn same_geometry_shape(a: &SketchGeom, b: &SketchGeom) -> bool {
    match (a, b) {
        (
            SketchGeom::Line {
                id: aid,
                p1: ap1,
                p2: ap2,
            },
            SketchGeom::Line {
                id: bid,
                p1: bp1,
                p2: bp2,
            },
        ) => aid == bid && ap1 == bp1 && ap2 == bp2,
        (
            SketchGeom::Arc {
                id: aid,
                p1: ap1,
                p2: ap2,
                center: ac,
            },
            SketchGeom::Arc {
                id: bid,
                p1: bp1,
                p2: bp2,
                center: bc,
            },
        ) => aid == bid && ap1 == bp1 && ap2 == bp2 && ac == bc,
        (
            SketchGeom::Circle {
                id: aid,
                center: ac,
                radius_fixed: arf,
                ..
            },
            SketchGeom::Circle {
                id: bid,
                center: bc,
                radius_fixed: brf,
                ..
            },
        ) => aid == bid && ac == bc && arf == brf,
        (
            SketchGeom::Rectangle {
                id: aid,
                corners: ac,
            },
            SketchGeom::Rectangle {
                id: bid,
                corners: bc,
            },
        ) => aid == bid && ac == bc,
        _ => false,
    }
}

fn same_constraint_shape(a: &Constraint, b: &Constraint) -> bool {
    match (a, b) {
        (
            Constraint::Coincident {
                p1: ap1, p2: ap2, ..
            },
            Constraint::Coincident {
                p1: bp1, p2: bp2, ..
            },
        ) => ap1 == bp1 && ap2 == bp2,
        (
            Constraint::PointOnLine {
                point: ap,
                line_p1: al1,
                line_p2: al2,
                ..
            },
            Constraint::PointOnLine {
                point: bp,
                line_p1: bl1,
                line_p2: bl2,
                ..
            },
        ) => ap == bp && al1 == bl1 && al2 == bl2,
        (
            Constraint::Horizontal {
                p1: ap1, p2: ap2, ..
            },
            Constraint::Horizontal {
                p1: bp1, p2: bp2, ..
            },
        )
        | (
            Constraint::Vertical {
                p1: ap1, p2: ap2, ..
            },
            Constraint::Vertical {
                p1: bp1, p2: bp2, ..
            },
        ) => ap1 == bp1 && ap2 == bp2,
        (
            Constraint::Parallel {
                a1: aa1,
                a2: aa2,
                b1: ab1,
                b2: ab2,
                ..
            },
            Constraint::Parallel {
                a1: ba1,
                a2: ba2,
                b1: bb1,
                b2: bb2,
                ..
            },
        )
        | (
            Constraint::Perpendicular {
                a1: aa1,
                a2: aa2,
                b1: ab1,
                b2: ab2,
                ..
            },
            Constraint::Perpendicular {
                a1: ba1,
                a2: ba2,
                b1: bb1,
                b2: bb2,
                ..
            },
        ) => aa1 == ba1 && aa2 == ba2 && ab1 == bb1 && ab2 == bb2,
        (
            Constraint::TangentLineArc {
                arc_id: aa,
                at_point: ap,
                ..
            },
            Constraint::TangentLineArc {
                arc_id: ba,
                at_point: bp,
                ..
            },
        ) => aa == ba && ap == bp,
        (
            Constraint::TangentArcs {
                arc1: aa1,
                arc2: aa2,
                ..
            },
            Constraint::TangentArcs {
                arc1: ba1,
                arc2: ba2,
                ..
            },
        ) => aa1 == ba1 && aa2 == ba2,
        (
            Constraint::Distance {
                p1: ap1, p2: ap2, ..
            },
            Constraint::Distance {
                p1: bp1, p2: bp2, ..
            },
        ) => ap1 == bp1 && ap2 == bp2,
        (Constraint::Radius { edge_id: ae, .. }, Constraint::Radius { edge_id: be, .. }) => {
            ae == be
        }
        _ => false,
    }
}

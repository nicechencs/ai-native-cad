//! Damped Levenberg-Marquardt solver for 6-DOF assembly mates.
//!
//! Free poses are packed as `[tx, ty, tz, rx, ry, rz]` with translations in
//! millimetres and rotations in radians. Each iteration builds a centred
//! finite-difference Jacobian and solves
//! `(Jᵀ J + λ I) Δx = -Jᵀ r` by Gaussian elimination with partial pivoting.
//! `continuation_steps` walks scalar mate targets from the measured initial
//! value to the requested value, warm-starting each stage from the last one.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::mates::{
    AnchorRef, AssemblyMate, AssemblyMateKind, MateAnchor, ResidualKind, anchors_of, blend_target,
    initial_scalar, push_residuals,
};
use crate::pose::{self, AssemblyComponent, RigidPose};

/// Knobs for one assembly solve.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SolveConfig {
    /// Maximum Levenberg-Marquardt iterations in each continuation stage.
    pub max_iters: usize,
    /// Euclidean residual norm that counts as converged.
    pub residual_tol: f64,
    /// Initial damping added to the diagonal of `Jᵀ J`.
    pub initial_lambda: f64,
    /// Homotopy stages from the measured mates to the requested targets.
    ///
    /// `0` and `1` both solve the requested targets in a single stage.
    pub continuation_steps: usize,
}

impl Default for SolveConfig {
    fn default() -> Self {
        Self {
            max_iters: 40,
            residual_tol: 1e-8,
            initial_lambda: 1e-3,
            continuation_steps: 4,
        }
    }
}

/// Pose solution and mate diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SolveReport {
    /// True when the Euclidean residual is at or below [`SolveConfig::residual_tol`].
    pub converged: bool,
    /// Levenberg-Marquardt iterations summed across continuation stages.
    pub iterations: usize,
    /// Euclidean norm of the weighted residual vector.
    pub final_residual: f64,
    /// Solved pose of every component, including grounded ones.
    pub solved_poses: BTreeMap<String, RigidPose>,
    /// Euclidean norm of each mate's weighted residual block.
    pub mate_residuals: BTreeMap<String, f64>,
    /// Dimension of the numerical nullspace of the final Jacobian.
    ///
    /// This is the count of free motions left after the mates are applied,
    /// not the number of optimization variables.
    pub free_dof: usize,
    /// Mate ids that still carry residual after a solve that did not converge.
    pub conflicting_mates: Vec<String>,
}

/// Why a system could not be solved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AssemblySolverError {
    /// Two components share an id.
    #[error("duplicate component id `{0}`")]
    DuplicateComponent(String),

    /// Two mates share an id.
    #[error("duplicate mate id `{0}`")]
    DuplicateMate(String),

    /// A mate names a component that is not in the system.
    #[error("mate `{mate}` references unknown component `{component}`")]
    UnknownComponent { mate: String, component: String },

    /// An anchor axis has zero length, so its direction is undefined.
    #[error("mate `{mate}` has a zero-length axis on component `{component}`")]
    DegenerateAxis { mate: String, component: String },

    /// A numeric field on a component or mate is not finite.
    #[error("non-finite value in {0}")]
    NonFinite(&'static str),

    /// `SolveConfig` contains a value the solver cannot use.
    #[error("invalid solve config: {0}")]
    InvalidConfig(&'static str),
}

/// Components, mates, and the pose solver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssemblySystem {
    pub components: Vec<AssemblyComponent>,
    pub mates: Vec<crate::mates::AssemblyMate>,
}

impl AssemblySystem {
    pub fn new(components: Vec<AssemblyComponent>, mates: Vec<AssemblyMate>) -> Self {
        Self { components, mates }
    }

    /// Solve mate residuals for every component that is not grounded.
    pub fn solve(&self, config: &SolveConfig) -> Result<SolveReport, AssemblySolverError> {
        validate_config(config)?;
        let model = Model::prepare(self)?;
        let stages = config.continuation_steps.max(1);
        let mut variables = model.pack();
        let mut iterations = 0usize;

        for stage in 1..=stages {
            let t = stage as f64 / stages as f64;
            let (next, used) = levenberg_marquardt(&model, &variables, t, config);
            variables = next;
            iterations += used;
        }

        let poses = model.unpack(&variables);
        let blocks = model.residual_blocks(&poses, 1.0);
        let flat = flatten_blocks(&blocks);
        let final_residual = l2(&flat);
        let converged = final_residual.is_finite() && final_residual <= config.residual_tol;

        let mut mate_residuals = BTreeMap::new();
        let mut conflicting_mates = Vec::new();
        for (id, block) in &blocks {
            let norm = l2(block);
            if !converged && norm > config.residual_tol {
                conflicting_mates.push(id.clone());
            }
            mate_residuals.insert(id.clone(), norm);
        }
        conflicting_mates.sort();

        let free_dof = if model.free_components.is_empty() {
            0
        } else {
            let jacobian = finite_difference_jacobian(&model, &variables, 1.0);
            numerical_nullity(&jacobian, variables.len())
        };

        let mut solved_poses = BTreeMap::new();
        for (component, pose) in self.components.iter().zip(poses.iter()) {
            solved_poses.insert(component.id.clone(), *pose);
        }

        Ok(SolveReport {
            converged,
            iterations,
            final_residual,
            solved_poses,
            mate_residuals,
            free_dof,
            conflicting_mates,
        })
    }
}

struct PreparedMate {
    id: String,
    weight: f64,
    kind: ResidualKind,
    /// Measured scalar at the initial pose. `None` for alignment-only mates.
    initial_scalar: Option<f64>,
}

struct Model {
    template: Vec<RigidPose>,
    /// Component indices that contribute six free variables, in input order.
    free_components: Vec<usize>,
    mates: Vec<PreparedMate>,
}

impl Model {
    fn prepare(system: &AssemblySystem) -> Result<Self, AssemblySolverError> {
        let mut seen_components = BTreeSet::new();
        let mut index_of = BTreeMap::new();
        for (index, component) in system.components.iter().enumerate() {
            if !seen_components.insert(component.id.clone()) {
                return Err(AssemblySolverError::DuplicateComponent(
                    component.id.clone(),
                ));
            }
            if !pose_is_finite(&component.pose) {
                return Err(AssemblySolverError::NonFinite("component pose"));
            }
            index_of.insert(component.id.clone(), index);
        }

        let mut seen_mates = BTreeSet::new();
        let mut mates = Vec::with_capacity(system.mates.len());
        let template: Vec<RigidPose> = system.components.iter().map(|c| c.pose).collect();

        for mate in &system.mates {
            if !seen_mates.insert(mate.id.clone()) {
                return Err(AssemblySolverError::DuplicateMate(mate.id.clone()));
            }
            if !mate.weight.is_finite() {
                return Err(AssemblySolverError::NonFinite("mate weight"));
            }
            if !kind_is_finite(&mate.kind) {
                return Err(AssemblySolverError::NonFinite("mate parameter"));
            }
            let (anchor_a, anchor_b) = anchors_of(&mate.kind);
            let a = resolve_anchor(&mate.id, anchor_a, &index_of)?;
            let b = resolve_anchor(&mate.id, anchor_b, &index_of)?;
            let kind = bind_kind(&mate.kind, a, b);
            let initial_scalar = initial_scalar(&kind, &template);
            mates.push(PreparedMate {
                id: mate.id.clone(),
                weight: mate.weight,
                kind,
                initial_scalar,
            });
        }

        let free_components = system
            .components
            .iter()
            .enumerate()
            .filter_map(|(index, component)| (!component.grounded).then_some(index))
            .collect();

        Ok(Self {
            template,
            free_components,
            mates,
        })
    }

    fn pack(&self) -> Vec<f64> {
        let mut variables = Vec::with_capacity(self.free_components.len() * 6);
        for &index in &self.free_components {
            let pose = self.template[index];
            variables.extend_from_slice(&pose.translation_mm);
            variables.push(pose.rotation_deg[0].to_radians());
            variables.push(pose.rotation_deg[1].to_radians());
            variables.push(pose.rotation_deg[2].to_radians());
        }
        variables
    }

    fn unpack(&self, variables: &[f64]) -> Vec<RigidPose> {
        let mut poses = self.template.clone();
        for (block, &index) in self.free_components.iter().enumerate() {
            let offset = block * 6;
            poses[index].translation_mm = [
                variables[offset],
                variables[offset + 1],
                variables[offset + 2],
            ];
            poses[index].rotation_deg = [
                variables[offset + 3].to_degrees(),
                variables[offset + 4].to_degrees(),
                variables[offset + 5].to_degrees(),
            ];
        }
        poses
    }

    fn residual_blocks(&self, poses: &[RigidPose], t: f64) -> Vec<(String, Vec<f64>)> {
        self.mates
            .iter()
            .map(|mate| {
                let kind = blend_target(mate.kind, mate.initial_scalar, t);
                let mut block = Vec::new();
                push_residuals(&kind, poses, mate.weight, &mut block);
                (mate.id.clone(), block)
            })
            .collect()
    }

    fn residuals(&self, poses: &[RigidPose], t: f64) -> Vec<f64> {
        let mut out = Vec::new();
        for mate in &self.mates {
            let kind = blend_target(mate.kind, mate.initial_scalar, t);
            push_residuals(&kind, poses, mate.weight, &mut out);
        }
        out
    }
}

fn levenberg_marquardt(
    model: &Model,
    initial: &[f64],
    t: f64,
    config: &SolveConfig,
) -> (Vec<f64>, usize) {
    let mut variables = initial.to_vec();
    if variables.is_empty() || model.mates.is_empty() {
        return (variables, 0);
    }

    let mut lambda = config.initial_lambda;
    let mut iterations = 0usize;

    while iterations < config.max_iters {
        let poses = model.unpack(&variables);
        let residual = model.residuals(&poses, t);
        let residual_norm = l2(&residual);
        if !residual_norm.is_finite() {
            break;
        }
        if residual_norm <= config.residual_tol {
            break;
        }

        iterations += 1;
        tracing::trace!(
            component = "cad-assembly-solver",
            operation = "lm_iteration",
            iter = iterations,
            residual_norm,
            lambda,
            t,
            "assembly solver iteration"
        );
        let jacobian = finite_difference_jacobian(model, &variables, t);
        let mut accepted = false;
        for _attempt in 0..12 {
            let (mut normal, mut rhs) = normal_equations(&jacobian, &residual, lambda);
            let Some(mut step) = gaussian_elimination(&mut normal, &mut rhs) else {
                lambda = (lambda * 10.0).min(LAMBDA_MAX);
                continue;
            };
            limit_step(&mut step);
            if l2(&step) < 1e-14 {
                return (variables, iterations);
            }
            if accept_step(model, &mut variables, &step, residual_norm, t, &mut lambda) {
                accepted = true;
                break;
            }
            if lambda >= LAMBDA_MAX {
                return (variables, iterations);
            }
        }
        if !accepted {
            break;
        }
    }

    (variables, iterations)
}

fn accept_step(
    model: &Model,
    variables: &mut Vec<f64>,
    step: &[f64],
    residual_norm: f64,
    t: f64,
    lambda: &mut f64,
) -> bool {
    let mut trial_step = step.to_vec();
    for _halving in 0..8 {
        let candidate = add_scaled(variables, &trial_step, 1.0);
        if candidate.iter().all(|v| v.is_finite()) {
            let poses = model.unpack(&candidate);
            let next = model.residuals(&poses, t);
            let next_norm = l2(&next);
            if next_norm.is_finite() && next_norm < residual_norm {
                *variables = candidate;
                *lambda = (*lambda * 0.3).max(LAMBDA_MIN);
                return true;
            }
        }
        for value in &mut trial_step {
            *value *= 0.5;
        }
    }
    *lambda = (*lambda * 10.0).min(LAMBDA_MAX);
    false
}

fn finite_difference_jacobian(model: &Model, variables: &[f64], t: f64) -> Vec<Vec<f64>> {
    let base_poses = model.unpack(variables);
    let base = model.residuals(&base_poses, t);
    let rows = base.len();
    let cols = variables.len();
    let mut jacobian = vec![vec![0.0; cols]; rows];
    if rows == 0 || cols == 0 {
        return jacobian;
    }

    let mut plus = variables.to_vec();
    let mut minus = variables.to_vec();
    for col in 0..cols {
        let step = finite_difference_step(variables[col]);
        plus[col] = variables[col] + step;
        minus[col] = variables[col] - step;
        let plus_residual = model.residuals(&model.unpack(&plus), t);
        let minus_residual = model.residuals(&model.unpack(&minus), t);
        let span = plus[col] - minus[col];
        for row in 0..rows {
            let upper = plus_residual.get(row).copied().unwrap_or(0.0);
            let lower = minus_residual.get(row).copied().unwrap_or(0.0);
            jacobian[row][col] = (upper - lower) / span;
        }
        plus[col] = variables[col];
        minus[col] = variables[col];
    }
    jacobian
}

fn finite_difference_step(value: f64) -> f64 {
    1e-6_f64.max(1e-8 * value.abs())
}

/// Build `Jᵀ J + λ I` and `-Jᵀ r`.
fn normal_equations(
    jacobian: &[Vec<f64>],
    residual: &[f64],
    lambda: f64,
) -> (Vec<Vec<f64>>, Vec<f64>) {
    let cols = jacobian.first().map(|row| row.len()).unwrap_or(0);
    let mut normal = vec![vec![0.0; cols]; cols];
    let mut rhs = vec![0.0; cols];
    for (row, &residual_i) in jacobian.iter().zip(residual.iter()) {
        for i in 0..cols {
            rhs[i] -= row[i] * residual_i;
            for k in i..cols {
                normal[i][k] += row[i] * row[k];
            }
        }
    }
    for i in 0..cols {
        for k in 0..i {
            normal[i][k] = normal[k][i];
        }
        normal[i][i] += lambda;
    }
    (normal, rhs)
}

/// Gaussian elimination with partial pivoting. Returns `None` when a pivot
/// underflows or the factors are non-finite.
fn gaussian_elimination(a: &mut [Vec<f64>], b: &mut [f64]) -> Option<Vec<f64>> {
    let n = b.len();
    if n == 0 {
        return Some(Vec::new());
    }
    for k in 0..n {
        let mut pivot = k;
        let mut best = a[k][k].abs();
        for i in (k + 1)..n {
            let value = a[i][k].abs();
            if value > best {
                best = value;
                pivot = i;
            }
        }
        if !best.is_finite() || best < 1e-14 {
            return None;
        }
        if pivot != k {
            a.swap(k, pivot);
            b.swap(k, pivot);
        }
        let diag = a[k][k];
        for i in (k + 1)..n {
            let factor = a[i][k] / diag;
            if factor == 0.0 {
                continue;
            }
            for j in k..n {
                a[i][j] -= factor * a[k][j];
            }
            b[i] -= factor * b[k];
        }
    }

    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = b[i];
        for j in (i + 1)..n {
            sum -= a[i][j] * x[j];
        }
        if !a[i][i].is_finite() || a[i][i].abs() < 1e-14 {
            return None;
        }
        x[i] = sum / a[i][i];
        if !x[i].is_finite() {
            return None;
        }
    }
    Some(x)
}

/// One-sided Jacobi singular values of an `m × n` Jacobian.
fn numerical_nullity(jacobian: &[Vec<f64>], columns: usize) -> usize {
    if columns == 0 {
        return 0;
    }
    if jacobian.is_empty() {
        return columns;
    }
    let mut working = jacobian.to_vec();
    for _sweep in 0..32 {
        let mut changed = false;
        for p in 0..columns {
            for q in (p + 1)..columns {
                let mut alpha = 0.0;
                let mut beta = 0.0;
                let mut gamma = 0.0;
                for row in &working {
                    alpha += row[p] * row[p];
                    beta += row[q] * row[q];
                    gamma += row[p] * row[q];
                }
                let scale = alpha.sqrt() * beta.sqrt();
                if scale <= 0.0 || gamma.abs() <= 1e-12 * scale {
                    continue;
                }
                changed = true;
                let tau = (beta - alpha) / (2.0 * gamma);
                let tangent = if tau >= 0.0 {
                    1.0 / (tau + tau.hypot(1.0))
                } else {
                    -1.0 / (-tau + tau.hypot(1.0))
                };
                let cosine = 1.0 / (1.0 + tangent * tangent).sqrt();
                let sine = tangent * cosine;
                for row in &mut working {
                    let bp = row[p];
                    let bq = row[q];
                    row[p] = cosine * bp - sine * bq;
                    row[q] = sine * bp + cosine * bq;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut singular = vec![0.0; columns];
    for row in &working {
        for (value, &entry) in singular.iter_mut().zip(row.iter()) {
            *value += entry * entry;
        }
    }
    for value in &mut singular {
        *value = value.sqrt();
    }
    let sigma_max = singular.iter().copied().fold(0.0_f64, f64::max);
    let tolerance = 1e-6 * sigma_max.max(1e-9);
    let rank = singular.iter().filter(|&&sigma| sigma > tolerance).count();
    columns.saturating_sub(rank)
}

fn limit_step(step: &mut [f64]) {
    let mut scale = 1.0_f64;
    for (index, &value) in step.iter().enumerate() {
        let cap = if index % 6 < 3 { 100.0 } else { 1.0 };
        if value.is_finite() {
            scale = scale.max(value.abs() / cap);
        }
    }
    if scale > 1.0 {
        for value in step {
            *value /= scale;
        }
    }
}

fn validate_config(config: &SolveConfig) -> Result<(), AssemblySolverError> {
    if !config.residual_tol.is_finite() || config.residual_tol < 0.0 {
        return Err(AssemblySolverError::InvalidConfig(
            "residual_tol must be finite and >= 0",
        ));
    }
    if !config.initial_lambda.is_finite() || config.initial_lambda <= 0.0 {
        return Err(AssemblySolverError::InvalidConfig(
            "initial_lambda must be finite and > 0",
        ));
    }
    Ok(())
}

fn resolve_anchor(
    mate_id: &str,
    anchor: &MateAnchor,
    index_of: &BTreeMap<String, usize>,
) -> Result<AnchorRef, AssemblySolverError> {
    let Some(&component_index) = index_of.get(&anchor.component_id) else {
        return Err(AssemblySolverError::UnknownComponent {
            mate: mate_id.to_string(),
            component: anchor.component_id.clone(),
        });
    };
    if !pose::finite3(anchor.origin_local_mm) || !pose::finite3(anchor.axis_local) {
        return Err(AssemblySolverError::NonFinite("mate anchor"));
    }
    if pose::length(anchor.axis_local) < 1e-12 {
        return Err(AssemblySolverError::DegenerateAxis {
            mate: mate_id.to_string(),
            component: anchor.component_id.clone(),
        });
    }
    Ok(AnchorRef {
        component_index,
        origin_local_mm: anchor.origin_local_mm,
        axis_local: anchor.axis_local,
    })
}

fn bind_kind(kind: &AssemblyMateKind, a: AnchorRef, b: AnchorRef) -> ResidualKind {
    match *kind {
        AssemblyMateKind::Coaxial { .. } => ResidualKind::Coaxial { a, b },
        AssemblyMateKind::Coincident {
            offset_mm,
            anti_aligned,
            ..
        } => ResidualKind::Coincident {
            a,
            b,
            offset_mm,
            anti_aligned,
        },
        AssemblyMateKind::Distance { target_mm, .. } => ResidualKind::Distance { a, b, target_mm },
        AssemblyMateKind::Parallel { .. } => ResidualKind::Parallel { a, b },
        AssemblyMateKind::Angle { target_deg, .. } => ResidualKind::Angle { a, b, target_deg },
    }
}

fn kind_is_finite(kind: &AssemblyMateKind) -> bool {
    match *kind {
        AssemblyMateKind::Coincident { offset_mm, .. } => offset_mm.is_finite(),
        AssemblyMateKind::Distance { target_mm, .. } => target_mm.is_finite(),
        AssemblyMateKind::Angle { target_deg, .. } => target_deg.is_finite(),
        AssemblyMateKind::Coaxial { .. } | AssemblyMateKind::Parallel { .. } => true,
    }
}

fn pose_is_finite(pose: &RigidPose) -> bool {
    pose::finite3(pose.translation_mm) && pose::finite3(pose.rotation_deg)
}

fn flatten_blocks(blocks: &[(String, Vec<f64>)]) -> Vec<f64> {
    blocks
        .iter()
        .flat_map(|(_, block)| block.iter().copied())
        .collect()
}

fn add_scaled(base: &[f64], step: &[f64], scale: f64) -> Vec<f64> {
    base.iter()
        .zip(step.iter())
        .map(|(value, delta)| value + scale * delta)
        .collect()
}

fn l2(values: &[f64]) -> f64 {
    values.iter().map(|value| value * value).sum::<f64>().sqrt()
}

const LAMBDA_MIN: f64 = 1e-12;
const LAMBDA_MAX: f64 = 1e12;

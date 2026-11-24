//! Jacobian rank and degrees-of-freedom diagnostics for a sketch system.
//!
//! The analysis uses a centred finite-difference Jacobian and a one-sided
//! Jacobi SVD.

use crate::{SketchSystem, SolverVariables};

/// Constraint-system classification based on the numerical Jacobian.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DofStatus {
    /// Every variable is determined by an independent constraint direction.
    WellConstrained,
    /// The Jacobian has a non-trivial null space.
    UnderConstrained { free_dof: usize },
    /// The equations contain dependent rows at the inspected point.
    OverConstrained { redundant_or_conflicting: usize },
}

/// Numerical rank and null-space information for a sketch.
#[derive(Debug, Clone, PartialEq)]
pub struct DofAnalysis {
    pub variable_count: usize,
    pub equation_count: usize,
    pub jacobian_rank: usize,
    pub status: DofStatus,
    /// Singular values, sorted from largest to smallest.  There is one entry
    /// per variable, including near-zero values for the right-null space.
    pub singular_values: Vec<f64>,
    /// Orthonormal right-null-space vectors, one vector per free degree of
    /// freedom. Each vector lists x and y for every free point in sketch
    /// order, followed by the radii of free circles in geometry order.
    pub free_directions: Vec<Vec<f64>>,
    /// Explicit constraint indices involved in a non-zero residual or a
    /// linearly dependent row when the system is over-constrained.
    pub conflicting_constraint_indices: Vec<usize>,
}

/// Analyze the local degrees of freedom of `sketch`.
///
/// A singular value is counted in the rank when it exceeds
/// `max(abs(rank_tol), 1e-12) * max(sigma_max, 1e-12)`. Non-finite tolerances
/// use a relative threshold of `1e-8`. Finite differences are centered at the
/// current coordinates and circle radii.
pub fn analyze_dof(sketch: &SketchSystem, rank_tol: f64) -> DofAnalysis {
    let variables = SolverVariables::new(sketch);
    let values = &variables.values;
    let lookup = variables.lookup();
    let (base_residuals, sources) = sketch.eval_residuals_with_sources(values, &lookup);
    let equation_count = base_residuals.len();
    let variable_count = values.len();

    let jacobian = centered_jacobian(sketch, values, &lookup, equation_count);
    let (singular_values, right_vectors) = jacobian_svd(&jacobian, variable_count);

    let sigma_max = singular_values.first().copied().unwrap_or(0.0);
    let relative_tol = if rank_tol.is_finite() {
        rank_tol.abs().max(1e-12)
    } else {
        1e-8
    };
    let singular_tol = relative_tol * sigma_max.max(1e-12);
    let jacobian_rank = singular_values
        .iter()
        .filter(|&&sigma| sigma > singular_tol)
        .count();
    let free_directions = right_vectors
        .into_iter()
        .skip(jacobian_rank)
        .collect::<Vec<_>>();

    // A row dependency is an over-constraint even if the current point happens
    // to satisfy all rows. Non-zero rows are only reported as conflicts after
    // this classification, so an unsolved under-constrained sketch remains
    // under-constrained while its solver can still remove the residual.
    let over_constrained = equation_count > jacobian_rank;

    let mut conflicting_constraint_indices = Vec::new();
    if over_constrained {
        let residual_scale = base_residuals.iter().map(|r| r.abs()).fold(1.0, f64::max);
        let residual_tol = (10.0 * relative_tol).min(1e-2) * residual_scale;
        conflicting_constraint_indices.extend(base_residuals.iter().zip(&sources).filter_map(
            |(residual, &source)| {
                if residual.abs() > residual_tol {
                    source
                } else {
                    None
                }
            },
        ));
        conflicting_constraint_indices
            .extend(dependent_row_indices(&jacobian, jacobian_rank).filter_map(|row| sources[row]));
        conflicting_constraint_indices.sort_unstable();
        conflicting_constraint_indices.dedup();
    }

    let status = if over_constrained {
        DofStatus::OverConstrained {
            redundant_or_conflicting: equation_count - jacobian_rank,
        }
    } else if !free_directions.is_empty() {
        DofStatus::UnderConstrained {
            free_dof: free_directions.len(),
        }
    } else {
        DofStatus::WellConstrained
    };

    DofAnalysis {
        variable_count,
        equation_count,
        jacobian_rank,
        status,
        singular_values,
        free_directions,
        conflicting_constraint_indices,
    }
}

fn centered_jacobian(
    sketch: &SketchSystem,
    values: &[f64],
    lookup: &crate::VarLookup<'_>,
    rows: usize,
) -> Vec<Vec<f64>> {
    let cols = values.len();
    let mut jacobian = vec![vec![0.0; cols]; rows];
    let mut plus = values.to_vec();
    let mut minus = values.to_vec();
    for col in 0..cols {
        // Keep a local perturbation while staying above coordinate roundoff.
        let step = 1e-6_f64.max(16.0 * f64::EPSILON * values[col].abs());
        plus[col] += step;
        minus[col] -= step;
        let plus_residuals = sketch.eval_residuals(&plus, lookup);
        let minus_residuals = sketch.eval_residuals(&minus, lookup);
        let denominator = plus[col] - minus[col];
        for row in 0..rows {
            jacobian[row][col] = (plus_residuals[row] - minus_residuals[row]) / denominator;
        }
        plus[col] = values[col];
        minus[col] = values[col];
    }
    jacobian
}

/// Returns singular values and right singular vectors of an `m x n` matrix.
/// The right vectors are represented as columns, sorted together with the
/// singular values in descending order. This one-sided Jacobi method
/// orthogonalizes the columns directly and avoids squaring the Jacobian's
/// condition number by explicitly forming `J^T J`.
fn jacobian_svd(jacobian: &[Vec<f64>], columns: usize) -> (Vec<f64>, Vec<Vec<f64>>) {
    if columns == 0 {
        return (Vec::new(), Vec::new());
    }

    let mut orthogonalized = jacobian.to_vec();
    let mut right_vectors = identity(columns);
    let max_sweeps = 64;
    for _ in 0..max_sweeps {
        let mut changed = false;
        for p in 0..columns {
            for q in (p + 1)..columns {
                let mut alpha = 0.0;
                let mut beta = 0.0;
                let mut gamma = 0.0;
                for row in &orthogonalized {
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
                let t = if tau >= 0.0 {
                    1.0 / (tau + tau.hypot(1.0))
                } else {
                    -1.0 / (-tau + tau.hypot(1.0))
                };
                let cosine = 1.0 / (1.0 + t * t).sqrt();
                let sine = t * cosine;
                for row in &mut orthogonalized {
                    let bp = row[p];
                    let bq = row[q];
                    row[p] = cosine * bp - sine * bq;
                    row[q] = sine * bp + cosine * bq;
                }
                for row in &mut right_vectors {
                    let vp = row[p];
                    let vq = row[q];
                    row[p] = cosine * vp - sine * vq;
                    row[q] = sine * vp + cosine * vq;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let norms = (0..columns)
        .map(|column| column_norm(&orthogonalized, column))
        .collect::<Vec<_>>();
    let mut order = (0..columns).collect::<Vec<_>>();
    order.sort_unstable_by(|&a, &b| norms[b].total_cmp(&norms[a]));
    let mut singular_values = Vec::with_capacity(columns);
    let mut sorted_vectors = Vec::with_capacity(columns);
    for index in order {
        singular_values.push(norms[index]);
        let mut vector = Vec::with_capacity(columns);
        for row in &right_vectors {
            vector.push(row[index]);
        }
        sorted_vectors.push(vector);
    }
    (singular_values, sorted_vectors)
}

fn column_norm(matrix: &[Vec<f64>], column: usize) -> f64 {
    matrix
        .iter()
        .map(|row| row[column].powi(2))
        .sum::<f64>()
        .sqrt()
}

fn dependent_row_indices(jacobian: &[Vec<f64>], rank: usize) -> impl Iterator<Item = usize> {
    // Pick a row basis with pivoted Gram–Schmidt. Use the SVD rank so near
    // dependencies are classified consistently by both diagnostics.
    let mut rows = jacobian.iter().cloned().enumerate().collect::<Vec<_>>();
    for i in 0..rank {
        let mut pivot = i;
        let mut pivot_norm = norm(&rows[i].1);
        for (j, (_, row)) in rows.iter().enumerate().skip(i + 1) {
            let row_norm = norm(row);
            if row_norm > pivot_norm {
                pivot = j;
                pivot_norm = row_norm;
            }
        }
        if pivot_norm == 0.0 {
            break;
        }
        rows.swap(i, pivot);
        let (selected, pending) = rows.split_at_mut(i + 1);
        let basis = &mut selected[i].1;
        for value in &mut *basis {
            *value /= pivot_norm;
        }
        for (_, row) in pending {
            let coefficient = dot(row, basis);
            for (value, basis_value) in row.iter_mut().zip(&*basis) {
                *value -= coefficient * basis_value;
            }
        }
    }
    rows.into_iter().skip(rank).map(|(index, _)| index)
}

fn identity(size: usize) -> Vec<Vec<f64>> {
    let mut matrix = vec![vec![0.0; size]; size];
    for i in 0..size {
        matrix[i][i] = 1.0;
    }
    matrix
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn norm(values: &[f64]) -> f64 {
    dot(values, values).sqrt()
}

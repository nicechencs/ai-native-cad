use serde::{Deserialize, Serialize};

pub type PointId = usize;
pub type EdgeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point2 {
    pub id: PointId,
    pub x: f64,
    pub y: f64,
    pub fixed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SketchGeom {
    Line {
        id: EdgeId,
        p1: PointId,
        p2: PointId,
    },
    Arc {
        id: EdgeId,
        p1: PointId,
        p2: PointId,
        center: PointId,
    },
    Circle {
        id: EdgeId,
        center: PointId,
        radius: f64,
        radius_fixed: bool,
    },
    Rectangle {
        id: EdgeId,
        corners: [PointId; 4],
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Constraint {
    Coincident {
        name: String,
        p1: PointId,
        p2: PointId,
    },
    PointOnLine {
        name: String,
        point: PointId,
        line_p1: PointId,
        line_p2: PointId,
    },
    Horizontal {
        name: String,
        p1: PointId,
        p2: PointId,
    },
    Vertical {
        name: String,
        p1: PointId,
        p2: PointId,
    },
    Parallel {
        name: String,
        a1: PointId,
        a2: PointId,
        b1: PointId,
        b2: PointId,
    },
    Perpendicular {
        name: String,
        a1: PointId,
        a2: PointId,
        b1: PointId,
        b2: PointId,
    },
    TangentLineArc {
        name: String,
        arc_id: EdgeId,
        line_dir: [f64; 2],
        line_point: [f64; 2],
        at_point: Option<PointId>,
    },
    TangentArcs {
        name: String,
        arc1: EdgeId,
        arc2: EdgeId,
    },
    Distance {
        name: String,
        p1: PointId,
        p2: PointId,
        target: f64,
    },
    Radius {
        name: String,
        edge_id: EdgeId,
        target: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SolveStatus {
    Converged,
    OverConstrained,
    FailedToConverge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintResidual {
    pub name: String,
    pub residual: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SolveReport {
    pub status: SolveStatus,
    pub iterations: usize,
    pub rms_residual: f64,
    pub degrees_of_freedom: isize,
    pub residuals: Vec<ConstraintResidual>,
}

#[derive(Debug, Default, Clone)]
pub struct SketchSystem {
    pub points: Vec<Point2>,
    pub geoms: Vec<SketchGeom>,
    pub constraints: Vec<Constraint>,
}

impl SketchSystem {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_point(&mut self, x: f64, y: f64, fixed: bool) -> PointId {
        let id = self.points.len();
        self.points.push(Point2 { id, x, y, fixed });
        id
    }

    pub fn add_line(&mut self, p1: PointId, p2: PointId) -> EdgeId {
        let id = self.geoms.len();
        self.geoms.push(SketchGeom::Line { id, p1, p2 });
        id
    }

    pub fn add_arc(&mut self, p1: PointId, p2: PointId, center: Option<(f64, f64)>) -> EdgeId {
        let (cx, cy) = center.unwrap_or_else(|| {
            let a = self.points[p1];
            let b = self.points[p2];
            let mx = 0.5 * (a.x + b.x);
            let my = 0.5 * (a.y + b.y);
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            // Deterministic non-degenerate initial offset perpendicular to chord
            (mx - 0.5 * dy, my + 0.5 * dx)
        });
        let center_id = self.add_point(cx, cy, false);
        let id = self.geoms.len();
        self.geoms.push(SketchGeom::Arc {
            id,
            p1,
            p2,
            center: center_id,
        });
        id
    }

    pub fn add_circle(&mut self, cx: f64, cy: f64, radius: f64, fixed_center: bool) -> EdgeId {
        let center = self.add_point(cx, cy, fixed_center);
        let id = self.geoms.len();
        self.geoms.push(SketchGeom::Circle {
            id,
            center,
            radius,
            radius_fixed: true,
        });
        id
    }

    pub fn add_rectangle(&mut self, x: f64, y: f64, w: f64, h: f64, fixed: bool) -> EdgeId {
        let p0 = self.add_point(x, y, fixed);
        let p1 = self.add_point(x + w, y, fixed);
        let p2 = self.add_point(x + w, y + h, fixed);
        let p3 = self.add_point(x, y + h, fixed);
        let id = self.geoms.len();
        self.geoms.push(SketchGeom::Rectangle {
            id,
            corners: [p0, p1, p2, p3],
        });
        id
    }

    pub fn add_constraint(&mut self, c: Constraint) {
        self.constraints.push(c);
    }

    /// Solves the 2D constraint system using damped Levenberg-Marquardt.
    pub fn solve(&mut self, max_iters: usize, tol: f64) -> SolveReport {
        // Build O(1) variable lookup tables indexed directly by PointId / EdgeId (R22)
        let mut var_map: Vec<(VarKind, usize)> = Vec::new();
        let mut point_var_idx: Vec<(Option<usize>, Option<usize>)> =
            vec![(None, None); self.points.len()];
        for p in &self.points {
            if !p.fixed {
                let ix = var_map.len();
                var_map.push((VarKind::PointX, p.id));
                let iy = var_map.len();
                var_map.push((VarKind::PointY, p.id));
                point_var_idx[p.id] = (Some(ix), Some(iy));
            }
        }
        let mut circle_var_idx: Vec<Option<usize>> = vec![None; self.geoms.len()];
        for g in &self.geoms {
            if let SketchGeom::Circle {
                id, radius_fixed, ..
            } = g
            {
                if !radius_fixed {
                    let ir = var_map.len();
                    var_map.push((VarKind::CircleRadius, *id));
                    circle_var_idx[*id] = Some(ir);
                }
            }
        }
        let lookup = VarLookup {
            point_vars: &point_var_idx,
            circle_vars: &circle_var_idx,
        };

        let n_vars = var_map.len();
        let mut x_vec = Vec::with_capacity(n_vars);
        for (kind, id) in &var_map {
            match kind {
                VarKind::PointX => x_vec.push(self.points[*id].x),
                VarKind::PointY => x_vec.push(self.points[*id].y),
                VarKind::CircleRadius => {
                    if let SketchGeom::Circle { radius, .. } = self.geoms[*id] {
                        x_vec.push(radius);
                    }
                }
            }
        }

        let r0 = self.eval_residuals(&x_vec, &lookup);
        let n_eqs = r0.len();

        if n_eqs == 0 {
            return SolveReport {
                status: SolveStatus::Converged,
                iterations: 0,
                rms_residual: 0.0,
                degrees_of_freedom: n_vars as isize,
                residuals: Vec::new(),
            };
        }

        if n_vars == 0 {
            let rms = rms_norm(&r0);
            let status = if rms <= tol {
                SolveStatus::Converged
            } else {
                SolveStatus::OverConstrained
            };
            return SolveReport {
                status,
                iterations: 0,
                rms_residual: rms,
                degrees_of_freedom: -(n_eqs as isize),
                residuals: self.named_residuals(&r0),
            };
        }

        let mut lambda = 1e-3;
        let mut iter = 0usize;
        let mut current_r = r0;
        let mut current_cost = 0.5 * dot(&current_r, &current_r);

        while iter < max_iters {
            let rms = rms_norm(&current_r);
            if rms < tol {
                break;
            }
            iter += 1;

            let jac = self.numeric_jacobian(&x_vec, &lookup, &current_r);
            // Compute A = J^T J + lambda * diag(J^T J) and g = -J^T r
            let mut a = vec![vec![0.0; n_vars]; n_vars];
            let mut g = vec![0.0; n_vars];
            for row in 0..n_eqs {
                for j in 0..n_vars {
                    let j_rj = jac[row][j];
                    g[j] -= j_rj * current_r[row];
                    for k in j..n_vars {
                        a[j][k] += j_rj * jac[row][k];
                    }
                }
            }
            for j in 0..n_vars {
                for k in 0..j {
                    a[j][k] = a[k][j];
                }
                let diag = a[j][j].max(1e-6);
                a[j][j] += lambda * diag + 1e-9;
            }

            let Some(step) = solve_linear_system(&mut a, &mut g) else {
                lambda *= 10.0;
                continue;
            };

            let mut candidate_x = x_vec.clone();
            for j in 0..n_vars {
                candidate_x[j] += step[j];
            }
            let cand_r = self.eval_residuals(&candidate_x, &lookup);
            let cand_cost = 0.5 * dot(&cand_r, &cand_r);

            if cand_cost < current_cost {
                x_vec = candidate_x;
                current_r = cand_r;
                current_cost = cand_cost;
                lambda = (lambda * 0.3).max(1e-10);
                if norm(&step) < 1e-12 {
                    break;
                }
            } else {
                lambda *= 5.0;
                if lambda > 1e12 {
                    break;
                }
            }
        }

        // Write solved coordinates back
        self.apply_vars(&x_vec, &var_map);

        let final_rms = rms_norm(&current_r);
        let named = self.named_residuals(&current_r);
        let dof = (n_vars as isize) - (n_eqs as isize);

        let status = if final_rms <= tol * 10.0 {
            SolveStatus::Converged
        } else if n_eqs > n_vars {
            SolveStatus::OverConstrained
        } else {
            SolveStatus::FailedToConverge
        };

        SolveReport {
            status,
            iterations: iter,
            rms_residual: final_rms,
            degrees_of_freedom: dof,
            residuals: named,
        }
    }

    fn apply_vars(&mut self, x: &[f64], var_map: &[(VarKind, usize)]) {
        for (idx, (kind, id)) in var_map.iter().enumerate() {
            match kind {
                VarKind::PointX => self.points[*id].x = x[idx],
                VarKind::PointY => self.points[*id].y = x[idx],
                VarKind::CircleRadius => {
                    if let SketchGeom::Circle { radius, .. } = &mut self.geoms[*id] {
                        *radius = x[idx].abs().max(1e-4);
                    }
                }
            }
        }
    }

    fn numeric_jacobian(
        &self,
        x: &[f64],
        lookup: &VarLookup<'_>,
        base_r: &[f64],
    ) -> Vec<Vec<f64>> {
        let eps = 1e-7;
        let m = base_r.len();
        let n = x.len();
        let mut jac = vec![vec![0.0; n]; m];
        let mut xp = x.to_vec();
        for j in 0..n {
            let orig = xp[j];
            xp[j] = orig + eps;
            let rp = self.eval_residuals(&xp, lookup);
            for i in 0..m {
                jac[i][j] = (rp[i] - base_r[i]) / eps;
            }
            xp[j] = orig;
        }
        jac
    }

    #[inline]
    fn point_xy(&self, pid: PointId, x: &[f64], lookup: &VarLookup<'_>) -> (f64, f64) {
        let (ix, iy) = lookup.point_vars[pid];
        let px = match ix {
            Some(i) => x[i],
            None => self.points[pid].x,
        };
        let py = match iy {
            Some(i) => x[i],
            None => self.points[pid].y,
        };
        (px, py)
    }

    #[inline]
    fn circle_radius(&self, eid: EdgeId, x: &[f64], lookup: &VarLookup<'_>) -> f64 {
        if let Some(i) = lookup.circle_vars[eid] {
            return x[i];
        }
        if let SketchGeom::Circle { radius, .. } = self.geoms[eid] {
            radius
        } else {
            0.0
        }
    }

    fn eval_residuals(&self, x: &[f64], var_map: &VarLookup<'_>) -> Vec<f64> {
        let mut res = Vec::new();
        // Implicit arc equal-radius constraints: |p1 - c| - |p2 - c| = 0
        for g in &self.geoms {
            if let SketchGeom::Arc { p1, p2, center, .. } = g {
                let (x1, y1) = self.point_xy(*p1, x, var_map);
                let (x2, y2) = self.point_xy(*p2, x, var_map);
                let (cx, cy) = self.point_xy(*center, x, var_map);
                let r1 = ((x1 - cx).powi(2) + (y1 - cy).powi(2)).sqrt();
                let r2 = ((x2 - cx).powi(2) + (y2 - cy).powi(2)).sqrt();
                res.push(r1 - r2);
            }
        }

        for c in &self.constraints {
            match c {
                Constraint::Coincident { p1, p2, .. } => {
                    let (x1, y1) = self.point_xy(*p1, x, var_map);
                    let (x2, y2) = self.point_xy(*p2, x, var_map);
                    res.push(x1 - x2);
                    res.push(y1 - y2);
                }
                Constraint::PointOnLine {
                    point,
                    line_p1,
                    line_p2,
                    ..
                } => {
                    let (px, py) = self.point_xy(*point, x, var_map);
                    let (ax, ay) = self.point_xy(*line_p1, x, var_map);
                    let (bx, by) = self.point_xy(*line_p2, x, var_map);
                    let dx = bx - ax;
                    let dy = by - ay;
                    let len = (dx * dx + dy * dy).sqrt().max(1e-9);
                    res.push(((px - ax) * dy - (py - ay) * dx) / len);
                }
                Constraint::Horizontal { p1, p2, .. } => {
                    let (_, y1) = self.point_xy(*p1, x, var_map);
                    let (_, y2) = self.point_xy(*p2, x, var_map);
                    res.push(y1 - y2);
                }
                Constraint::Vertical { p1, p2, .. } => {
                    let (x1, _) = self.point_xy(*p1, x, var_map);
                    let (x2, _) = self.point_xy(*p2, x, var_map);
                    res.push(x1 - x2);
                }
                Constraint::Parallel { a1, a2, b1, b2, .. } => {
                    let (ax1, ay1) = self.point_xy(*a1, x, var_map);
                    let (ax2, ay2) = self.point_xy(*a2, x, var_map);
                    let (bx1, by1) = self.point_xy(*b1, x, var_map);
                    let (bx2, by2) = self.point_xy(*b2, x, var_map);
                    let dax = ax2 - ax1;
                    let day = ay2 - ay1;
                    let dbx = bx2 - bx1;
                    let dby = by2 - by1;
                    let la = (dax * dax + day * day).sqrt().max(1e-9);
                    let lb = (dbx * dbx + dby * dby).sqrt().max(1e-9);
                    res.push((dax * dby - day * dbx) / (la * lb) * 10.0);
                }
                Constraint::Perpendicular { a1, a2, b1, b2, .. } => {
                    let (ax1, ay1) = self.point_xy(*a1, x, var_map);
                    let (ax2, ay2) = self.point_xy(*a2, x, var_map);
                    let (bx1, by1) = self.point_xy(*b1, x, var_map);
                    let (bx2, by2) = self.point_xy(*b2, x, var_map);
                    let dax = ax2 - ax1;
                    let day = ay2 - ay1;
                    let dbx = bx2 - bx1;
                    let dby = by2 - by1;
                    let la = (dax * dax + day * day).sqrt().max(1e-9);
                    let lb = (dbx * dbx + dby * dby).sqrt().max(1e-9);
                    res.push((dax * dbx + day * dby) / (la * lb) * 10.0);
                }
                Constraint::TangentLineArc {
                    arc_id,
                    line_dir,
                    line_point,
                    at_point,
                    ..
                } => {
                    if let SketchGeom::Arc { p1, p2, center, .. } = self.geoms[*arc_id] {
                        let (cx, cy) = self.point_xy(center, x, var_map);
                        let (x1, y1) = self.point_xy(p1, x, var_map);
                        let r = ((x1 - cx).powi(2) + (y1 - cy).powi(2)).sqrt();
                        let lnorm = (line_dir[0] * line_dir[0] + line_dir[1] * line_dir[1])
                            .sqrt()
                            .max(1e-9);
                        let tx = line_dir[0] / lnorm;
                        let ty = line_dir[1] / lnorm;
                        if let Some(at_pid) = at_point {
                            let (ax, ay) = self.point_xy(*at_pid, x, var_map);
                            // Radius vector (ax - cx, ay - cy) perpendicular to line tangent (tx, ty)
                            res.push((ax - cx) * tx + (ay - cy) * ty);
                            // Also point on target line
                            let nx = -ty;
                            let ny = tx;
                            res.push((ax - line_point[0]) * nx + (ay - line_point[1]) * ny);
                        } else {
                            let (x2, y2) = self.point_xy(p2, x, var_map);
                            let _ = (x2, y2);
                            let nx = -ty;
                            let ny = tx;
                            let dist = ((cx - line_point[0]) * nx + (cy - line_point[1]) * ny).abs();
                            res.push(dist - r);
                        }
                    }
                }
                Constraint::TangentArcs { arc1, arc2, .. } => {
                    if let (
                        SketchGeom::Arc {
                            p1: a1p1,
                            center: c1,
                            ..
                        },
                        SketchGeom::Arc {
                            p1: a2p1,
                            center: c2,
                            ..
                        },
                    ) = (&self.geoms[*arc1], &self.geoms[*arc2])
                    {
                        let (c1x, c1y) = self.point_xy(*c1, x, var_map);
                        let (c2x, c2y) = self.point_xy(*c2, x, var_map);
                        let (p1x, p1y) = self.point_xy(*a1p1, x, var_map);
                        let (p2x, p2y) = self.point_xy(*a2p1, x, var_map);
                        let r1 = ((p1x - c1x).powi(2) + (p1y - c1y).powi(2)).sqrt();
                        let r2 = ((p2x - c2x).powi(2) + (p2y - c2y).powi(2)).sqrt();
                        let d = ((c1x - c2x).powi(2) + (c1y - c2y).powi(2)).sqrt();
                        res.push(d - (r1 + r2));
                    }
                }
                Constraint::Distance { p1, p2, target, .. } => {
                    let (x1, y1) = self.point_xy(*p1, x, var_map);
                    let (x2, y2) = self.point_xy(*p2, x, var_map);
                    let d = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
                    res.push(d - target);
                }
                Constraint::Radius {
                    edge_id, target, ..
                } => match self.geoms[*edge_id] {
                    SketchGeom::Arc { p1, center, .. } => {
                        let (x1, y1) = self.point_xy(p1, x, var_map);
                        let (cx, cy) = self.point_xy(center, x, var_map);
                        let r = ((x1 - cx).powi(2) + (y1 - cy).powi(2)).sqrt();
                        res.push(r - target);
                    }
                    SketchGeom::Circle { id, .. } => {
                        let r = self.circle_radius(id, x, var_map);
                        res.push(r - target);
                    }
                    _ => {}
                },
            }
        }
        res
    }

    fn named_residuals(&self, raw: &[f64]) -> Vec<ConstraintResidual> {
        let mut out = Vec::new();
        let mut idx = 0usize;
        for g in &self.geoms {
            if let SketchGeom::Arc { id, .. } = g {
                out.push(ConstraintResidual {
                    name: format!("arc_{id}_equal_radius"),
                    residual: raw.get(idx).copied().unwrap_or(0.0).abs(),
                });
                idx += 1;
            }
        }
        for c in &self.constraints {
            let (name, count) = match c {
                Constraint::Coincident { name, .. } => (name.clone(), 2),
                Constraint::TangentLineArc { name, at_point, .. } => {
                    (name.clone(), if at_point.is_some() { 2 } else { 1 })
                }
                Constraint::PointOnLine { name, .. }
                | Constraint::Horizontal { name, .. }
                | Constraint::Vertical { name, .. }
                | Constraint::Parallel { name, .. }
                | Constraint::Perpendicular { name, .. }
                | Constraint::TangentArcs { name, .. }
                | Constraint::Distance { name, .. }
                | Constraint::Radius { name, .. } => (name.clone(), 1),
            };
            let mut max_r = 0.0f64;
            for _ in 0..count {
                if let Some(v) = raw.get(idx) {
                    max_r = max_r.max(v.abs());
                }
                idx += 1;
            }
            out.push(ConstraintResidual {
                name,
                residual: max_r,
            });
        }
        out
    }

    /// Samples a closed region formed by the given edge IDs into a closed 2D polygon loop.
    /// Strictly validates segment connectivity, closure, positive area, and non-self-intersection (R14).
    pub fn sample_closed_region(&self, edge_ids: &[EdgeId], arc_segments: usize) -> Result<Vec<[f64; 2]>, String> {
        if edge_ids.is_empty() {
            return Err("empty closed region".to_string());
        }
        // Single circle or rectangle
        if edge_ids.len() == 1 {
            let geom = self
                .geoms
                .get(edge_ids[0])
                .ok_or_else(|| format!("unknown sketch edge id {}", edge_ids[0]))?;
            match geom {
                SketchGeom::Circle { center, radius, .. } => {
                    if *radius <= 1e-6 {
                        return Err("degenerate circle with zero radius".to_string());
                    }
                    let c = self.points[*center];
                    let n = arc_segments.max(24);
                    let mut pts = Vec::with_capacity(n);
                    for i in 0..n {
                        let theta = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
                        pts.push([c.x + radius * theta.cos(), c.y + radius * theta.sin()]);
                    }
                    return Ok(pts);
                }
                SketchGeom::Rectangle { corners, .. } => {
                    let poly: Vec<[f64; 2]> = corners
                        .iter()
                        .map(|&pid| [self.points[pid].x, self.points[pid].y])
                        .collect();
                    if polygon_area_2d(&poly) <= 1e-6 {
                        return Err("degenerate rectangle with zero area".to_string());
                    }
                    return Ok(poly);
                }
                SketchGeom::Line { .. } | SketchGeom::Arc { .. } => {
                    return Err("open sketch contour: a single line or arc cannot form a closed region".to_string());
                }
            }
        }

        const CONN_TOL: f64 = 1e-3;
        let mut poly: Vec<[f64; 2]> = Vec::new();
        let mut loop_start: Option<[f64; 2]> = None;
        let mut cur_end: Option<[f64; 2]> = None;

        for (idx, &eid) in edge_ids.iter().enumerate() {
            let geom = self
                .geoms
                .get(eid)
                .ok_or_else(|| format!("unknown sketch edge id {eid}"))?;
            match geom {
                SketchGeom::Line { p1, p2, .. } => {
                    let a = [self.points[*p1].x, self.points[*p1].y];
                    let b = [self.points[*p2].x, self.points[*p2].y];
                    if idx == 0 {
                        loop_start = Some(a);
                        poly.push(a);
                        poly.push(b);
                        cur_end = Some(b);
                    } else {
                        let prev = cur_end.unwrap();
                        if dist2(prev, a) <= CONN_TOL {
                            poly.push(b);
                            cur_end = Some(b);
                        } else if dist2(prev, b) <= CONN_TOL {
                            poly.push(a);
                            cur_end = Some(a);
                        } else {
                            return Err(format!(
                                "open sketch contour: edge {idx} is disconnected from previous edge"
                            ));
                        }
                    }
                }
                SketchGeom::Arc { p1, p2, center, .. } => {
                    let a = self.points[*p1];
                    let b = self.points[*p2];
                    let c = self.points[*center];
                    let a_pt = [a.x, a.y];
                    let b_pt = [b.x, b.y];
                    let reverse = if idx == 0 {
                        loop_start = Some(a_pt);
                        poly.push(a_pt);
                        false
                    } else {
                        let prev = cur_end.unwrap();
                        if dist2(prev, a_pt) <= CONN_TOL {
                            false
                        } else if dist2(prev, b_pt) <= CONN_TOL {
                            true
                        } else {
                            return Err(format!(
                                "open sketch contour: arc {idx} is disconnected from previous edge"
                            ));
                        }
                    };

                    let (start_p, end_p) = if reverse { (b, a) } else { (a, b) };
                    let r = ((start_p.x - c.x).powi(2) + (start_p.y - c.y).powi(2)).sqrt();
                    let a1 = (start_p.y - c.y).atan2(start_p.x - c.x);
                    let a2 = (end_p.y - c.y).atan2(end_p.x - c.x);
                    let mut dtheta = a2 - a1;
                    while dtheta > std::f64::consts::PI {
                        dtheta -= 2.0 * std::f64::consts::PI;
                    }
                    while dtheta <= -std::f64::consts::PI {
                        dtheta += 2.0 * std::f64::consts::PI;
                    }
                    let segs = arc_segments.max(8);
                    let mut chosen_dtheta = dtheta;
                    if edge_ids.len() >= 3 {
                        let alt_dtheta = if dtheta >= 0.0 {
                            dtheta - 2.0 * std::f64::consts::PI
                        } else {
                            dtheta + 2.0 * std::f64::consts::PI
                        };
                        let mut test_short = poly.clone();
                        for s in 1..=segs {
                            let t = (s as f64) / (segs as f64);
                            let th = a1 + dtheta * t;
                            test_short.push([c.x + r * th.cos(), c.y + r * th.sin()]);
                        }
                        if has_self_intersection_2d(&test_short) {
                            let mut test_alt = poly.clone();
                            for s in 1..=segs {
                                let t = (s as f64) / (segs as f64);
                                let th = a1 + alt_dtheta * t;
                                test_alt.push([c.x + r * th.cos(), c.y + r * th.sin()]);
                            }
                            if !has_self_intersection_2d(&test_alt) {
                                chosen_dtheta = alt_dtheta;
                            }
                        }
                    }
                    for s in 1..=segs {
                        let t = (s as f64) / (segs as f64);
                        let theta = a1 + chosen_dtheta * t;
                        poly.push([c.x + r * theta.cos(), c.y + r * theta.sin()]);
                    }
                    cur_end = Some([end_p.x, end_p.y]);
                }
                SketchGeom::Circle { .. } | SketchGeom::Rectangle { .. } => {
                    return Err("circle/rectangle cannot be mixed inside multi-edge region".to_string());
                }
            }
        }

        // Strict end-to-start closure check
        match (loop_start, cur_end) {
            (Some(s), Some(e)) if dist2(s, e) <= CONN_TOL => {
                poly.pop();
            }
            (Some(s), Some(e)) => {
                return Err(format!(
                    "open sketch contour: end ({:.3}, {:.3}) does not close to start ({:.3}, {:.3})",
                    e[0], e[1], s[0], s[1]
                ));
            }
            _ => return Err("empty closed region".to_string()),
        }

        if poly.len() < 3 {
            return Err("closed region has fewer than 3 vertices".to_string());
        }
        if has_self_intersection_2d(&poly) {
            return Err("closed region is self-intersecting".to_string());
        }
        if polygon_area_2d(&poly) <= 1e-4 {
            return Err("closed region has zero or degenerate area".to_string());
        }
        Ok(poly)
    }
}

struct VarLookup<'a> {
    point_vars: &'a [(Option<usize>, Option<usize>)],
    circle_vars: &'a [Option<usize>],
}

fn polygon_area_2d(pts: &[[f64; 2]]) -> f64 {
    let mut sum = 0.0;
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        sum += pts[i][0] * pts[j][1] - pts[j][0] * pts[i][1];
    }
    0.5 * sum.abs()
}

fn has_self_intersection_2d(poly: &[[f64; 2]]) -> bool {
    let n = poly.len();
    if n < 4 {
        return false;
    }
    for i in 0..n {
        let i_next = (i + 1) % n;
        for j in (i + 2)..n {
            let j_next = (j + 1) % n;
            if j_next == i {
                continue;
            }
            if segments_properly_intersect(poly[i], poly[i_next], poly[j], poly[j_next]) {
                return true;
            }
        }
    }
    false
}

fn segments_properly_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let orient = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| -> f64 {
        (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    };
    let o1 = orient(a, b, c);
    let o2 = orient(a, b, d);
    let o3 = orient(c, d, a);
    let o4 = orient(c, d, b);
    o1 * o2 < -1e-9 && o3 * o4 < -1e-9
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarKind {
    PointX,
    PointY,
    CircleRadius,
}

fn dist2(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

fn rms_norm(a: &[f64]) -> f64 {
    if a.is_empty() {
        0.0
    } else {
        (dot(a, a) / (a.len() as f64)).sqrt()
    }
}

fn solve_linear_system(a: &mut [Vec<f64>], b: &mut [f64]) -> Option<Vec<f64>> {
    let n = b.len();
    for i in 0..n {
        let mut max_row = i;
        let mut max_val = a[i][i].abs();
        for r in (i + 1)..n {
            if a[r][i].abs() > max_val {
                max_val = a[r][i].abs();
                max_row = r;
            }
        }
        if max_val < 1e-14 {
            return None;
        }
        if max_row != i {
            a.swap(i, max_row);
            b.swap(i, max_row);
        }
        let pivot = a[i][i];
        for r in (i + 1)..n {
            let factor = a[r][i] / pivot;
            if factor.abs() < 1e-16 {
                continue;
            }
            for c in i..n {
                a[r][c] -= factor * a[i][c];
            }
            b[r] -= factor * b[i];
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = b[i];
        for c in (i + 1)..n {
            sum -= a[i][c] * x[c];
        }
        x[i] = sum / a[i][i];
    }
    Some(x)
}

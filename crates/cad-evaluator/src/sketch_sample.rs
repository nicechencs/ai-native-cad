use cad_gcs::{EdgeId, PointId, SketchGeom, SketchSystem};
use cad_kernel_occt::SketchPlane;

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn delta(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn sample_sketch_path(
    sys: &SketchSystem,
    ids: &[EdgeId],
    plane: SketchPlane,
) -> Result<Vec<[f64; 3]>, String> {
    let mut points: Vec<[f64; 2]> = Vec::new();
    let xy = |i: PointId| [sys.points[i].x, sys.points[i].y];
    let distance = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
    for (index, &id) in ids.iter().enumerate() {
        let (mut a, mut b, center) = match sys.geoms[id] {
            SketchGeom::Line { p1, p2, .. } => (xy(p1), xy(p2), None),
            SketchGeom::Arc { p1, p2, center, .. } => (xy(p1), xy(p2), Some(xy(center))),
            _ => return Err("region::path supports lines and arcs only".into()),
        };
        if distance(a, b) < 1e-6 {
            return Err(format!("degenerate path edge {index}"));
        }
        if let Some(&prev) = points.last() {
            if distance(prev, a) > 1e-3 {
                if distance(prev, b) <= 1e-3 {
                    std::mem::swap(&mut a, &mut b);
                } else {
                    return Err(format!("disconnected path edge {index}"));
                }
            }
        } else {
            points.push(a);
        }
        if let Some(c) = center {
            let r = distance(a, c);
            if r <= 1e-6 || (r - distance(b, c)).abs() > 1e-3 {
                return Err(format!("invalid radius on path arc {index}"));
            }
            let start = (a[1] - c[1]).atan2(a[0] - c[0]);
            let end = (b[1] - c[1]).atan2(b[0] - c[0]);
            let sweep = (end - start).sin().atan2((end - start).cos());
            let segments = ((sweep.abs() / std::f64::consts::PI) * 24.0).ceil() as usize;
            for i in 1..segments {
                let theta = start + sweep * i as f64 / segments as f64;
                points.push([c[0] + r * theta.cos(), c[1] + r * theta.sin()]);
            }
        }
        points.push(b);
    }
    Ok(points
        .into_iter()
        .map(|[u, v]| plane.to_3d(u, v, 0.0))
        .collect())
}

const DEFAULT_ELLIPSE_SEGMENTS: usize = 48;
const MIN_ELLIPSE_SEGMENTS: usize = 12;
const MAX_ELLIPSE_SEGMENTS: usize = 512;

pub(crate) const DEFAULT_SPLINE_SEGMENTS_PER_SPAN: usize = 12;
pub(crate) const MIN_SPLINE_SEGMENTS_PER_SPAN: usize = 4;
pub(crate) const MAX_SPLINE_SEGMENTS_PER_SPAN: usize = 64;

pub(crate) fn sample_ellipse_polygon(
    center: [f64; 2],
    rx: f64,
    ry: f64,
    segments: Option<f64>,
) -> Result<Vec<[f64; 2]>, String> {
    if !rx.is_finite() || !ry.is_finite() || rx <= 0.0 || ry <= 0.0 {
        return Err(format!(
            "ellipse requires positive semi-axes rx and ry, found rx={rx}, ry={ry}"
        ));
    }
    let n = segments
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s.round() as usize).clamp(12, 512))
        .unwrap_or(DEFAULT_ELLIPSE_SEGMENTS)
        .clamp(MIN_ELLIPSE_SEGMENTS, MAX_ELLIPSE_SEGMENTS);
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let theta = 2.0 * std::f64::consts::PI * (i as f64) / (n as f64);
        pts.push([center[0] + rx * theta.cos(), center[1] + ry * theta.sin()]);
    }
    Ok(pts)
}

pub fn sample_closed_spline_polygon(
    control_points: &[[f64; 2]],
    segments_per_span: usize,
) -> Result<Vec<[f64; 2]>, String> {
    if control_points.len() < 3 {
        return Err(format!(
            "spline requires at least 3 control points, found {}",
            control_points.len()
        ));
    }
    if control_points
        .iter()
        .any(|p| !p[0].is_finite() || !p[1].is_finite())
    {
        return Err("spline control points must have finite coordinates".to_string());
    }

    let mut cleaned: Vec<[f64; 2]> = Vec::with_capacity(control_points.len());
    for &pt in control_points {
        if cleaned
            .last()
            .is_none_or(|prev| (pt[0] - prev[0]).hypot(pt[1] - prev[1]) > 1e-6)
        {
            cleaned.push(pt);
        }
    }
    if cleaned.len() >= 2 {
        let first = cleaned[0];
        let last = cleaned[cleaned.len() - 1];
        if (first[0] - last[0]).hypot(first[1] - last[1]) <= 1e-6 {
            cleaned.pop();
        }
    }
    if cleaned.len() < 3 {
        return Err(format!(
            "spline requires at least 3 distinct control points, found {}",
            cleaned.len()
        ));
    }

    let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
    for &[x, y] in &cleaned {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let bbox_area = (max_x - min_x) * (max_y - min_y);
    if !bbox_area.is_finite()
        || (max_x - min_x) <= 1e-6
        || (max_y - min_y) <= 1e-6
        || bbox_area <= 1e-9
    {
        return Err(
            "spline control points have zero bounding-box area (degenerate/collinear)".to_string(),
        );
    }

    let p0 = cleaned[0];
    let mut max_cross = 0.0_f64;
    for i in 1..cleaned.len() {
        let dx1 = cleaned[i][0] - p0[0];
        let dy1 = cleaned[i][1] - p0[1];
        for p2 in cleaned.iter().skip(i + 1) {
            let dx2 = p2[0] - p0[0];
            let dy2 = p2[1] - p0[1];
            max_cross = max_cross.max((dx1 * dy2 - dy1 * dx2).abs());
        }
    }
    if max_cross <= 1e-6 {
        return Err("spline control points are collinear and do not form a 2D area".to_string());
    }

    let segs = if segments_per_span == 0 {
        DEFAULT_SPLINE_SEGMENTS_PER_SPAN
    } else {
        segments_per_span.clamp(MIN_SPLINE_SEGMENTS_PER_SPAN, MAX_SPLINE_SEGMENTS_PER_SPAN)
    };

    let n = cleaned.len();
    let mut sampled: Vec<[f64; 2]> = Vec::with_capacity(n * segs);
    for i in 0..n {
        let p_prev = cleaned[(i + n - 1) % n];
        let p_curr = cleaned[i];
        let p_next = cleaned[(i + 1) % n];
        let p_next2 = cleaned[(i + 2) % n];

        for step in 0..segs {
            let t = (step as f64) / (segs as f64);
            let t2 = t * t;
            let t3 = t2 * t;
            let x = 0.5
                * ((2.0 * p_curr[0])
                    + (-p_prev[0] + p_next[0]) * t
                    + (2.0 * p_prev[0] - 5.0 * p_curr[0] + 4.0 * p_next[0] - p_next2[0]) * t2
                    + (-p_prev[0] + 3.0 * p_curr[0] - 3.0 * p_next[0] + p_next2[0]) * t3);
            let y = 0.5
                * ((2.0 * p_curr[1])
                    + (-p_prev[1] + p_next[1]) * t
                    + (2.0 * p_prev[1] - 5.0 * p_curr[1] + 4.0 * p_next[1] - p_next2[1]) * t2
                    + (-p_prev[1] + 3.0 * p_curr[1] - 3.0 * p_next[1] + p_next2[1]) * t3);
            let pt = [x, y];
            if sampled
                .last()
                .is_none_or(|prev| (pt[0] - prev[0]).hypot(pt[1] - prev[1]) > 1e-6)
            {
                sampled.push(pt);
            }
        }
    }

    if sampled.len() >= 2 {
        let first = sampled[0];
        let last = sampled[sampled.len() - 1];
        if (first[0] - last[0]).hypot(first[1] - last[1]) <= 1e-6 {
            sampled.pop();
        }
    }
    if sampled.len() < 3 {
        return Err("sampled spline polygon has fewer than 3 distinct vertices".to_string());
    }

    let m = sampled.len();
    for i in 0..m {
        let a = sampled[i];
        let b = sampled[(i + 1) % m];
        for j in (i + 2)..m {
            if i == 0 && j == m - 1 {
                continue;
            }
            let c = sampled[j];
            let d = sampled[(j + 1) % m];
            if segments_intersect_2d(a, b, c, d) {
                return Err("closed spline polygon is self-intersecting".to_string());
            }
        }
    }

    let mut signed_area = 0.0_f64;
    for i in 0..sampled.len() {
        let a = sampled[i];
        let b = sampled[(i + 1) % sampled.len()];
        signed_area += a[0] * b[1] - b[0] * a[1];
    }
    signed_area *= 0.5;
    if !signed_area.is_finite() || signed_area.abs() <= 1e-6 {
        return Err("sampled spline polygon has zero area".to_string());
    }
    if signed_area < 0.0 {
        sampled.reverse();
    }

    Ok(sampled)
}

fn segments_intersect_2d(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let eps = 1e-8;
    if a[0].max(b[0]) + eps < c[0].min(d[0])
        || c[0].max(d[0]) + eps < a[0].min(b[0])
        || a[1].max(b[1]) + eps < c[1].min(d[1])
        || c[1].max(d[1]) + eps < a[1].min(b[1])
    {
        return false;
    }
    let r = [b[0] - a[0], b[1] - a[1]];
    let s = [d[0] - c[0], d[1] - c[1]];
    let q = [c[0] - a[0], c[1] - a[1]];
    let r_cross_s = r[0] * s[1] - r[1] * s[0];
    let q_cross_r = q[0] * r[1] - q[1] * r[0];
    let q_cross_s = q[0] * s[1] - q[1] * s[0];

    if r_cross_s.abs() > 1e-12 {
        let t = q_cross_s / r_cross_s;
        let u = q_cross_r / r_cross_s;
        (-eps..=1.0 + eps).contains(&t) && (-eps..=1.0 + eps).contains(&u)
    } else if q_cross_r.abs() <= 1e-12 {
        let r_len_sq = r[0] * r[0] + r[1] * r[1];
        if r_len_sq <= 1e-12 {
            return false;
        }
        let t0 = (q[0] * r[0] + q[1] * r[1]) / r_len_sq;
        let t1 = ((d[0] - a[0]) * r[0] + (d[1] - a[1]) * r[1]) / r_len_sq;
        let (tmin, tmax) = if t0 < t1 { (t0, t1) } else { (t1, t0) };
        tmax >= -eps && tmin <= 1.0 + eps
    } else {
        false
    }
}

pub(crate) const DEFAULT_HELIX_SEGMENTS_PER_TURN: usize = 24;
pub(crate) const MIN_HELIX_SEGMENTS_PER_TURN: usize = 8;
pub(crate) const MAX_HELIX_SEGMENTS_PER_TURN: usize = 96;

pub fn sample_helix_path(
    radius: f64,
    pitch: f64,
    turns: f64,
    origin: [f64; 3],
    segments_per_turn: usize,
    left_handed: bool,
) -> Result<Vec<[f64; 3]>, String> {
    if !radius.is_finite() || radius <= 0.0 {
        return Err(format!(
            "path::helix requires positive `radius`, found {radius}"
        ));
    }
    if !pitch.is_finite() || pitch <= 0.0 {
        return Err(format!(
            "path::helix requires positive `pitch`, found {pitch}"
        ));
    }
    if !turns.is_finite() || turns <= 0.0 {
        return Err(format!(
            "path::helix requires positive `turns`, found {turns}"
        ));
    }
    if origin.iter().any(|v| !v.is_finite()) {
        return Err("path::helix `origin` must have finite coordinates".to_string());
    }

    let segs_per_turn = if segments_per_turn == 0 {
        DEFAULT_HELIX_SEGMENTS_PER_TURN
    } else {
        segments_per_turn.clamp(MIN_HELIX_SEGMENTS_PER_TURN, MAX_HELIX_SEGMENTS_PER_TURN)
    };
    let total_segments = ((turns * segs_per_turn as f64).ceil() as usize).clamp(8, 2048);
    let sign = if left_handed { -1.0 } else { 1.0 };
    let total_theta = turns * std::f64::consts::TAU;
    let [ox, oy, oz] = origin;

    let mut pts = Vec::with_capacity(total_segments + 1);
    for i in 0..=total_segments {
        let frac = (i as f64) / (total_segments as f64);
        let theta = frac * total_theta;
        let x = ox + radius * (sign * theta).cos();
        let y = oy + radius * (sign * theta).sin();
        let z = oz + pitch * (theta / std::f64::consts::TAU);
        pts.push([x, y, z]);
    }

    if pts.windows(2).any(|pair| {
        let d = delta(pair[0], pair[1]);
        dot(d, d) <= 1e-12
    }) {
        return Err("path::helix generated degenerate consecutive points".to_string());
    }

    Ok(pts)
}

pub fn sample_spline3d_path(
    control_points: &[[f64; 3]],
    segments_per_span: usize,
) -> Result<Vec<[f64; 3]>, String> {
    if control_points.len() < 3 {
        return Err(format!(
            "path::spline3d requires at least 3 control points, found {}",
            control_points.len()
        ));
    }
    if control_points.iter().flatten().any(|c| !c.is_finite()) {
        return Err("path::spline3d control points must have finite coordinates".to_string());
    }

    let dist3 = |a: [f64; 3], b: [f64; 3]| -> f64 {
        let d = delta(a, b);
        dot(d, d).sqrt()
    };

    let mut cleaned: Vec<[f64; 3]> = Vec::with_capacity(control_points.len());
    for &pt in control_points {
        if cleaned.last().is_none_or(|&prev| dist3(pt, prev) > 1e-5) {
            cleaned.push(pt);
        }
    }
    if cleaned.len() < 3 {
        return Err(format!(
            "path::spline3d requires at least 3 distinct control points, found {}",
            cleaned.len()
        ));
    }

    let segs = if segments_per_span == 0 {
        DEFAULT_SPLINE_SEGMENTS_PER_SPAN
    } else {
        segments_per_span.clamp(MIN_SPLINE_SEGMENTS_PER_SPAN, MAX_SPLINE_SEGMENTS_PER_SPAN)
    };

    let n = cleaned.len();
    let mut sampled: Vec<[f64; 3]> = Vec::with_capacity((n - 1) * segs + 1);
    for i in 0..(n - 1) {
        let p_prev = if i == 0 { cleaned[0] } else { cleaned[i - 1] };
        let p_curr = cleaned[i];
        let p_next = cleaned[i + 1];
        let p_next2 = if i + 2 >= n {
            cleaned[n - 1]
        } else {
            cleaned[i + 2]
        };

        for step in 0..segs {
            let t = (step as f64) / (segs as f64);
            let t2 = t * t;
            let t3 = t2 * t;
            let pt: [f64; 3] = std::array::from_fn(|k| {
                0.5 * ((2.0 * p_curr[k])
                    + (-p_prev[k] + p_next[k]) * t
                    + (2.0 * p_prev[k] - 5.0 * p_curr[k] + 4.0 * p_next[k] - p_next2[k]) * t2
                    + (-p_prev[k] + 3.0 * p_curr[k] - 3.0 * p_next[k] + p_next2[k]) * t3)
            });
            if sampled.last().is_none_or(|&prev| dist3(pt, prev) > 1e-5) {
                sampled.push(pt);
            }
        }
    }
    let end_pt = cleaned[n - 1];
    if sampled
        .last()
        .is_none_or(|&prev| dist3(end_pt, prev) > 1e-5)
    {
        sampled.push(end_pt);
    } else if let Some(last) = sampled.last_mut() {
        *last = end_pt;
    }

    let total_len: f64 = sampled.windows(2).map(|w| dist3(w[0], w[1])).sum();
    if sampled.len() < 2 || !total_len.is_finite() || total_len <= 1e-3 {
        return Err(format!(
            "path::spline3d total path length must exceed 1e-3 mm, found {total_len}"
        ));
    }

    Ok(sampled)
}


use super::ViewKind;
use cad_kernel_occt::{CheckReport, CurveType, SolidShape, SurfaceType};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

type Point = [f64; 2];
type Key = (i64, i64);
const EPS: f64 = 1e-7;
/// Same bound as `OcctImportOptions::linear_deflection_mm`. Tessellated faces
/// do not share vertices exactly; a larger gap is still an open profile.
const PROFILE_SNAP_MM: f64 = 0.05;

/// Exports a 1:1 XY cut projection in millimetres. Blind pockets and hidden edges
/// are not through-cuts: the profiles bound the union of the projected solid faces.
pub fn render_dxf(shape: &SolidShape, check: &CheckReport) -> Result<String, String> {
    let profiles = projected_profiles(shape, ViewKind::Top)?;
    let edge_circles = shape.edges.iter().filter_map(|edge| {
        if edge.curve_type != CurveType::Circle {
            return None;
        }
        let ring = edge.polyline.as_chunks::<3>().0;
        if ring.len() < 9 || (0..3).any(|i| (ring[0][i] - ring[ring.len() - 1][i]).abs() > EPS) {
            return None;
        }
        horizontal_circle(&ring[..ring.len() - 1])
    });
    // Counterbores can have analytic cylindrical rims without individual edge records.
    let wall_circles = shape
        .faces
        .iter()
        .filter(|f| f.surface_type == SurfaceType::Cylinder && f.radius_hint.is_some())
        .flat_map(|f| std::iter::once(&f.outer_loop).chain(f.top_loop.iter()))
        .filter_map(|ring| horizontal_circle(ring));
    let circles: Vec<_> = edge_circles.chain(wall_circles).collect();

    let mut out = begin_file(false);
    for profile in profiles {
        let layer = if area(&profile) > 0.0 {
            "OUTLINE"
        } else {
            "HOLES"
        };
        let analytic = circles.iter().find(|circle| circle.matches(&profile)).copied();
        let fitted = if analytic.is_none() && layer == "HOLES" {
            fit_circle_profile(&profile)
        } else {
            None
        };
        if let Some(Circle {
            center: c,
            radius: r,
            ..
        }) = analytic.or(fitted)
        {
            let _ = write!(
                out,
                "0\nCIRCLE\n100\nAcDbEntity\n8\n{layer}\n100\nAcDbCircle\n10\n{:.9}\n20\n{:.9}\n30\n0.0\n40\n{r:.9}\n",
                c[0], c[1]
            );
        } else {
            let _ = write!(
                out,
                "0\nLWPOLYLINE\n100\nAcDbEntity\n8\n{layer}\n100\nAcDbPolyline\n90\n{}\n70\n1\n",
                profile.len()
            );
            for [x, y] in profile {
                let _ = write!(out, "10\n{x:.9}\n20\n{y:.9}\n");
            }
        }
    }
    Ok(end_file(out, check))
}

/// Exports an orthographic native B-Rep engineering view of the XY plane,
/// seen from +Z. `VISIBLE` is solid and `HIDDEN` is dashed. Use [`render_dxf`]
/// for closed through-cut outlines; the engineering view is not a cut profile.
pub fn render_dxf_view(shape: &SolidShape, check: &CheckReport) -> Result<String, String> {
    let projection = super::hlr::project(shape, ViewKind::Top)?
        .ok_or("exact DXF views require a native OCCT B-Rep; use --out-dxf for cut profiles")?;
    let mut out = begin_file(true);
    for (layer, paths) in [
        ("VISIBLE", projection.visible),
        ("HIDDEN", projection.hidden),
    ] {
        for path in paths {
            // PLINEGEN (128) keeps the hidden dash pattern continuous through
            // the points generated when sampling a curved B-Rep edge.
            let _ = write!(
                out,
                "0\nLWPOLYLINE\n100\nAcDbEntity\n8\n{layer}\n100\nAcDbPolyline\n90\n{}\n70\n128\n",
                path.len()
            );
            for [x, y] in path {
                let _ = write!(out, "10\n{x:.9}\n20\n{y:.9}\n");
            }
        }
    }
    Ok(end_file(out, check))
}

fn begin_file(hlr: bool) -> String {
    let mut out = String::new();
    out.push_str("0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1015\n9\n$INSUNITS\n70\n4\n9\n$MEASUREMENT\n70\n1\n0\nENDSEC\n");
    let _ = write!(
        out,
        "0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLTYPE\n70\n{}\n",
        if hlr { 2 } else { 1 }
    );
    out.push_str("0\nLTYPE\n100\nAcDbSymbolTableRecord\n100\nAcDbLinetypeTableRecord\n2\nCONTINUOUS\n70\n0\n3\nSolid line\n72\n65\n73\n0\n40\n0.0\n");
    if hlr {
        out.push_str("0\nLTYPE\n100\nAcDbSymbolTableRecord\n100\nAcDbLinetypeTableRecord\n2\nHIDDEN\n70\n0\n3\nHidden edges\n72\n65\n73\n2\n40\n3.0\n49\n2.0\n74\n0\n49\n-1.0\n74\n0\n");
    }
    out.push_str("0\nENDTAB\n0\nTABLE\n2\nLAYER\n70\n4\n");
    let profile_layers = [("OUTLINE", 7, "CONTINUOUS"), ("HOLES", 1, "CONTINUOUS")];
    let drawing_layers = [("VISIBLE", 7, "CONTINUOUS"), ("HIDDEN", 8, "HIDDEN")];
    for (layer, color, line_type, plot) in std::iter::once(("0", 7, "CONTINUOUS", 1))
        .chain((if hlr { drawing_layers } else { profile_layers }).map(|(n, c, t)| (n, c, t, 1)))
        .chain(std::iter::once(("DIMENSIONS", 4, "CONTINUOUS", 0)))
    {
        let _ = write!(
            out,
            "0\nLAYER\n100\nAcDbSymbolTableRecord\n100\nAcDbLayerTableRecord\n2\n{layer}\n70\n0\n62\n{color}\n6\n{line_type}\n290\n{plot}\n"
        );
    }
    out.push_str("0\nENDTAB\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n");
    out
}

fn end_file(mut out: String, check: &CheckReport) -> String {
    add_dimensions(&mut out, check);
    out.push_str("0\nENDSEC\n0\nEOF\n");
    out.replace('\n', "\r\n")
}

fn add_dimensions(out: &mut String, check: &CheckReport) {
    let bb = check.bbox_mm;
    let [w, h, _] = bb.size();
    if !bb.is_valid() || w <= EPS || h <= EPS {
        return;
    }
    let spacing = (w.max(h) * 0.06).max(2.0);
    let [x0, y0, _] = bb.min;
    let [x1, y1, _] = bb.max;
    let y = y0 - spacing;
    let x = x0 - spacing;
    for ([ax, ay], [bx, by]) in [
        ([x0, y], [x1, y]),
        ([x0, y0], [x0, y]),
        ([x1, y0], [x1, y]),
        ([x, y0], [x, y1]),
        ([x0, y0], [x, y0]),
        ([x0, y1], [x, y1]),
    ] {
        let _ = write!(
            out,
            "0\nLINE\n100\nAcDbEntity\n8\nDIMENSIONS\n100\nAcDbLine\n10\n{ax:.9}\n20\n{ay:.9}\n30\n0.0\n11\n{bx:.9}\n21\n{by:.9}\n31\n0.0\n"
        );
    }
    let height = spacing * 0.35;
    for (x, y, value, angle) in [
        (x0 + 0.3 * w, y - height * 1.4, w, 0),
        (x - height * 0.4, y0 + 0.3 * h, h, 90),
    ] {
        let _ = write!(
            out,
            "0\nTEXT\n100\nAcDbEntity\n8\nDIMENSIONS\n100\nAcDbText\n10\n{x:.9}\n20\n{y:.9}\n30\n0.0\n40\n{height:.9}\n1\n{value:.3} mm\n50\n{angle}\n100\nAcDbText\n"
        );
    }
}

#[derive(Clone, Copy)]
struct Bounds {
    min: Point,
    max: Point,
}

impl Bounds {
    fn new(points: &[Point]) -> Self {
        let mut bb = Self {
            min: points[0],
            max: points[0],
        };
        for p in points {
            for (i, &v) in p.iter().enumerate() {
                bb.min[i] = bb.min[i].min(v);
                bb.max[i] = bb.max[i].max(v);
            }
        }
        bb
    }

    fn contains(self, p: Point) -> bool {
        (0..2).all(|i| p[i] >= self.min[i] && p[i] <= self.max[i])
    }
}

struct Region {
    outer: Vec<Point>,
    holes: Vec<Vec<Point>>,
    bounds: Bounds,
}

impl Region {
    fn new(outer: Vec<Point>, holes: Vec<Vec<Point>>) -> Option<Self> {
        (outer.len() >= 3 && area(&outer).abs() > EPS * EPS).then(|| Self {
            bounds: Bounds::new(&outer),
            outer,
            holes,
        })
    }

    fn contains(&self, p: Point) -> bool {
        self.bounds.contains(p)
            && inside(p, &self.outer)
            && !self.holes.iter().any(|hole| inside(p, hole))
    }
}

#[derive(Clone, Copy)]
struct Segment {
    a_key: Key,
    b_key: Key,
    a: Point,
    b: Point,
    bounds: Bounds,
}

const VERTEX_WELD_MM: f64 = 1.0e-4;
const PROBE_DISTANCES_MM: [f64; 4] = [EPS * 4.0, 1.0e-4, 1.0e-3, 0.01];

pub(super) fn projected_profiles(
    shape: &SolidShape,
    view: ViewKind,
) -> Result<Vec<Vec<Point>>, String> {
    let project = |p| {
        let (x, y, _) = view.project(p);
        [x, y]
    };
    let mut regions = Vec::new();
    for face in &shape.faces {
        if face.surface_type == SurfaceType::Plane && face.outer_loop.len() >= 3 {
            if view.project(face.normal_hint).2.abs() <= EPS {
                continue;
            }
            let ring_points = |ring: &[[f64; 3]]| ring.iter().copied().map(project).collect();
            let holes = face
                .inner_loops
                .iter()
                .map(|ring| ring_points(ring))
                .collect();
            regions.extend(Region::new(ring_points(&face.outer_loop), holes));
        } else {
            for triangle in face.indices.as_chunks::<3>().0 {
                let points: Vec<_> = triangle
                    .iter()
                    .filter_map(|&i| {
                        let n = i as usize * 3;
                        Some(project([
                            *face.vertices.get(n)?,
                            *face.vertices.get(n + 1)?,
                            *face.vertices.get(n + 2)?,
                        ]))
                    })
                    .collect();
                regions.extend(Region::new(points, Vec::new()));
            }
        }
    }

    if regions
        .iter()
        .flat_map(|r| std::iter::once(&r.outer).chain(&r.holes))
        .flatten()
        .flatten()
        .any(|v| !v.is_finite() || v.abs() >= (i64::MAX as f64) * EPS)
    {
        return Err("projection contains coordinates outside the supported range".into());
    }

    // Split face boundaries at overlaps and intersections. On each subsegment,
    // material on both sides means an internal edge, not a cutting path.
    let unique: BTreeSet<_> = regions
        .iter()
        .flat_map(|r| {
            std::iter::once(&r.outer)
                .chain(&r.holes)
                .flat_map(|ring| edges(ring))
        })
        .filter_map(|(a, b)| {
            let [a, b] = [key(a), key(b)];
            (a != b).then_some((a.min(b), a.max(b)))
        })
        .collect();
    let segments: Vec<_> = unique
        .into_iter()
        .map(|(a_key, b_key)| {
            let [a, b] = [point(a_key), point(b_key)];
            Segment {
                a_key,
                b_key,
                a,
                b,
                bounds: Bounds::new(&[a, b]),
            }
        })
        .collect();
    let mut splits: Vec<Vec<(f64, Key)>> = segments
        .iter()
        .map(|s| vec![(0.0, s.a_key), (1.0, s.b_key)])
        .collect();
    for (i, a) in segments.iter().enumerate() {
        for (j, b) in segments.iter().enumerate().skip(i + 1) {
            if b.bounds.min[0] > a.bounds.max[0] + EPS {
                break;
            }
            if b.bounds.min[1] > a.bounds.max[1] + EPS || a.bounds.min[1] > b.bounds.max[1] + EPS {
                continue;
            }
            let (left, right) = splits.split_at_mut(j);
            split(a, b, &mut left[i], &mut right[0]);
        }
    }
    let nearby_pad = PROBE_DISTANCES_MM[PROBE_DISTANCES_MM.len() - 1] + EPS * 4.0;
    let mut boundary: BTreeMap<Key, BTreeSet<Key>> = BTreeMap::new();
    for (s, ts) in segments.iter().zip(&mut splits) {
        let d = sub(s.b, s.a);
        let len = length(d);
        ts.sort_by(|x, y| x.0.total_cmp(&y.0).then_with(|| x.1.cmp(&y.1)));
        ts.dedup_by(|b, a| b.1 == a.1 || (b.0 - a.0).abs() * len <= EPS);
        let n = [-d[1] / len, d[0] / len];
        let nearby: Vec<_> = regions
            .iter()
            .filter(|r| {
                (0..2).all(|i| {
                    r.bounds.min[i] <= s.bounds.max[i] + nearby_pad
                        && s.bounds.min[i] <= r.bounds.max[i] + nearby_pad
                })
            })
            .collect();
        for pair in ts.windows(2) {
            let (t0, k0) = pair[0];
            let (t1, k1) = pair[1];
            if k0 == k1 {
                continue;
            }
            let mid = add(s.a, d, (t0 + t1) * 0.5);
            let filled = |side| nearby.iter().any(|r| r.contains(add(mid, n, side)));
            // A 4e-7 mm probe misses tessellated triangles whose edge is the
            // silhouette. Step out to 0.01 mm, still far below a rib or hole,
            // and keep the edge when exactly one side is material.
            let mut classified = None;
            for probe in PROBE_DISTANCES_MM {
                match (filled(probe), filled(-probe)) {
                    (true, false) => {
                        classified = Some((k0, k1));
                        break;
                    }
                    (false, true) => {
                        classified = Some((k1, k0));
                        break;
                    }
                    (true, true) => break,
                    (false, false) => continue,
                }
            }
            let Some((a, b)) = classified else {
                continue;
            };
            if a != b {
                boundary.entry(a).or_default().insert(b);
            }
        }
    }
    weld_boundary(&mut boundary, VERTEX_WELD_MM);
    let mut profiles = Vec::new();
    while let Some((&start, _)) = boundary.first_key_value() {
        let mut ring = vec![point(start)];
        let mut current = start;
        let mut incoming = None;
        loop {
            let next = boundary.get(&current).and_then(|choices| {
                choices.iter().copied().max_by(|&a, &b| {
                    let turn = |next| {
                        let d = sub(point(next), point(current));
                        incoming.map_or(0.0, |last| cross(last, d).atan2(dot(last, d)))
                    };
                    turn(a).total_cmp(&turn(b))
                })
            });
            let Some(next) = next else {
                if ring.len() >= 3 && length(sub(point(current), point(start))) <= PROFILE_SNAP_MM {
                    break;
                }
                let Some(stitched) = closest_unfinished(&boundary, current) else {
                    return Err(
                        "DXF projection contains an open boundary; no cutting profile was exported"
                            .into(),
                    );
                };
                ring.push(point(stitched));
                incoming = Some(sub(point(stitched), point(current)));
                current = stitched;
                continue;
            };
            let choices = boundary.get_mut(&current).unwrap();
            choices.remove(&next);
            if choices.is_empty() {
                boundary.remove(&current);
            }
            if next == start {
                break;
            }
            ring.push(point(next));
            incoming = Some(sub(point(next), point(current)));
            current = next;
        }
        if ring.len() >= 3 && area(&ring).abs() > EPS * EPS {
            profiles.push(ring);
        }
    }
    if profiles.is_empty() {
        return Err("DXF projection has no closed XY profiles".into());
    }
    Ok(profiles)
}

/// Collapse numerically coincident boundary vertices (within `tol_mm`, e.g. 0.1 µm)
/// caused by triple intersections or collinear quantization drift, dropping self-loops
/// and cancelling opposite sliver edge pairs.
fn weld_boundary(boundary: &mut BTreeMap<Key, BTreeSet<Key>>, tol_mm: f64) {
    let mut vertices: BTreeSet<Key> = BTreeSet::new();
    for (&u, vs) in boundary.iter() {
        vertices.insert(u);
        vertices.extend(vs.iter().copied());
    }
    let mut reps: Vec<Key> = Vec::with_capacity(vertices.len());
    let mut remap: BTreeMap<Key, Key> = BTreeMap::new();
    for v in vertices {
        let vp = point(v);
        let rep = reps
            .iter()
            .copied()
            .find(|&r| length(sub(point(r), vp)) <= tol_mm)
            .unwrap_or_else(|| {
                reps.push(v);
                v
            });
        remap.insert(v, rep);
    }
    let mut welded: BTreeMap<Key, BTreeSet<Key>> = BTreeMap::new();
    for (u, vs) in std::mem::take(boundary) {
        let ru = remap[&u];
        for v in vs {
            let rv = remap[&v];
            if ru == rv {
                continue;
            }
            if welded.get_mut(&rv).is_some_and(|back| back.remove(&ru)) {
                if welded.get(&rv).is_some_and(BTreeSet::is_empty) {
                    welded.remove(&rv);
                }
            } else {
                welded.entry(ru).or_default().insert(rv);
            }
        }
    }
    *boundary = welded;
}

fn split(a: &Segment, b: &Segment, ta: &mut Vec<(f64, Key)>, tb: &mut Vec<(f64, Key)>) {
    let r = sub(a.b, a.a);
    let s = sub(b.b, b.a);
    let v = sub(b.a, a.a);
    let c = cross(r, s);
    let (lr, ls) = (length(r), length(s));
    let (ea, eb) = (EPS / lr, EPS / ls);
    let valid = |t: f64, tol: f64| t >= -tol && t <= 1.0 + tol;
    if c.abs() > EPS * lr.max(ls) {
        let (t, u) = (cross(v, s) / c, cross(v, r) / c);
        if valid(t, ea) && valid(u, eb) {
            let tc = t.clamp(0.0, 1.0);
            let uc = u.clamp(0.0, 1.0);
            let pa = add(a.a, r, tc);
            let pb = add(b.a, s, uc);
            let k = key([(pa[0] + pb[0]) * 0.5, (pa[1] + pb[1]) * 0.5]);
            ta.push((tc, k));
            tb.push((uc, k));
        }
    } else if cross(v, r).abs() <= EPS * lr {
        for (p, k) in [(b.a, b.a_key), (b.b, b.b_key)] {
            let t = dot(sub(p, a.a), r) / dot(r, r);
            if valid(t, ea) {
                ta.push((t.clamp(0.0, 1.0), k));
            }
        }
        for (p, k) in [(a.a, a.a_key), (a.b, a.b_key)] {
            let u = dot(sub(p, b.a), s) / dot(s, s);
            if valid(u, eb) {
                tb.push((u.clamp(0.0, 1.0), k));
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Circle {
    pub(crate) center: Point,
    pub(crate) radius: f64,
    pub(crate) max_chord: f64,
}

/// Fit a closed profile to a circle. Tessellated OCCT holes are not within
/// `1e-6` mm of an analytic rim, but they are circles at the DXF tolerance.
pub(crate) fn fit_circle_profile(ring: &[Point]) -> Option<Circle> {
    if ring.len() < 8 {
        return None;
    }
    let a = ring[0];
    let b = sub(ring[ring.len() / 3], a);
    let c = sub(ring[ring.len() * 2 / 3], a);
    let det = 2.0 * cross(b, c);
    if det.abs() <= EPS * EPS {
        return None;
    }
    let (u, v) = (dot(b, b), dot(c, c));
    let center = [
        a[0] + (u * c[1] - v * b[1]) / det,
        a[1] + (b[0] * v - c[0] * u) / det,
    ];
    let radius = length(sub(a, center));
    if radius <= EPS {
        return None;
    }
    let max_chord = edges(ring)
        .map(|(p, q)| length(sub(p, q)))
        .fold(0.0, f64::max);
    // A long closing chord is an arc or an open trim, not a full hole.
    if max_chord >= radius {
        return None;
    }
    if ring
        .iter()
        .any(|p| (length(sub(*p, center)) - radius).abs() > PROFILE_SNAP_MM)
    {
        return None;
    }
    Some(Circle {
        center,
        radius,
        max_chord,
    })
}

impl Circle {
    fn matches(&self, ring: &[Point]) -> bool {
        ring.len() >= 8
            && ring
                .iter()
                .all(|p| (length(sub(*p, self.center)) - self.radius).abs() < EPS * 10.0)
            && edges(ring).all(|(a, b)| length(sub(a, b)) <= self.max_chord + EPS * 10.0)
    }
}

fn horizontal_circle(ring: &[[f64; 3]]) -> Option<Circle> {
    if ring.len() < 8 || ring.iter().any(|p| (p[2] - ring[0][2]).abs() > EPS) {
        return None;
    }
    let points: Vec<_> = ring.iter().map(|p| [p[0], p[1]]).collect();
    let pts = points.as_slice();
    let a = pts[0];
    let b = sub(pts[pts.len() / 3], a);
    let c = sub(pts[pts.len() * 2 / 3], a);
    let det = 2.0 * cross(b, c);
    if det.abs() <= EPS * EPS {
        return None;
    }
    let (u, v) = (dot(b, b), dot(c, c));
    let center = [
        a[0] + (u * c[1] - v * b[1]) / det,
        a[1] + (b[0] * v - c[0] * u) / det,
    ];
    let radius = length(sub(a, center));
    let max_chord = edges(pts)
        .map(|(a, b)| length(sub(a, b)))
        .fold(0.0, f64::max);
    // Distinguish a full circle from a circular trim with a long closing chord.
    (max_chord < radius
        && pts
            .iter()
            .all(|p| (length(sub(*p, center)) - radius).abs() <= EPS))
    .then_some(Circle {
        center,
        radius,
        max_chord,
    })
}

fn edges(points: &[Point]) -> impl Iterator<Item = (Point, Point)> + '_ {
    points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
}
fn area(polygon: &[Point]) -> f64 {
    edges(polygon)
        .map(|(a, b)| cross(sub(a, polygon[0]), sub(b, polygon[0])))
        .sum::<f64>()
        * 0.5
}
fn inside([x, y]: Point, ring: &[Point]) -> bool {
    edges(ring)
        .filter(|(a, b)| {
            (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0]
        })
        .count()
        % 2
        == 1
}
fn point((x, y): Key) -> Point {
    [x as f64 * EPS, y as f64 * EPS]
}
pub(super) fn key([x, y]: Point) -> Key {
    ((x / EPS).round() as i64, (y / EPS).round() as i64)
}

/// Nearest vertex that still has an unused boundary edge, within the mesh snap.
pub(super) fn closest_unfinished(
    boundary: &BTreeMap<Key, BTreeSet<Key>>,
    current: Key,
) -> Option<Key> {
    let origin = point(current);
    let mut best: Option<(f64, Key)> = None;
    for (&candidate, choices) in boundary {
        if candidate == current || choices.is_empty() {
            continue;
        }
        let distance = length(sub(point(candidate), origin));
        if distance <= PROFILE_SNAP_MM && best.is_none_or(|(best_d, _)| distance < best_d) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}
fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1]]
}
fn add(a: Point, b: Point, t: f64) -> Point {
    [a[0] + b[0] * t, a[1] + b[1] * t]
}
fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}
fn cross(a: Point, b: Point) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}
fn length(p: Point) -> f64 {
    p[0].hypot(p[1])
}

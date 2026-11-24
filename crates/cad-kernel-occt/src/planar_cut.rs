//! Boundary reconstruction for a planar stock and a planar cutting tool.
use super::{
    BRepEdge, BRepFace, CsgNode, SurfaceType, cross, dot3, make_line_edge, norm, ortho_basis,
    point_in_polygon_2d, project_to_2d, retriangulate_face_in_place, scale3, sub,
};
use std::collections::{BTreeMap, BTreeSet};

const EPS: f64 = 1e-7;
type Point = [f64; 3];
type Key = [i64; 3];
type Edge = (Key, Key);

#[derive(Clone, Copy)]
struct Plane {
    normal: Point,
    offset: f64,
}

impl Plane {
    fn distance(self, p: Point) -> f64 {
        dot3(self.normal, p) - self.offset
    }

    fn reversed(self) -> Self {
        Self {
            normal: scale3(self.normal, -1.),
            offset: -self.offset,
        }
    }

    fn coincides(self, other: Self) -> bool {
        norm(sub(self.normal, other.normal)) <= EPS && (self.offset - other.offset).abs() <= EPS
    }
}

struct Patch<'a> {
    plane: Plane,
    source: &'a BRepFace,
    ring: Vec<Point>,
}

pub(super) fn cut(
    stock: &[BRepFace],
    tool: &[BRepFace],
    result: &CsgNode,
) -> Option<(Vec<BRepFace>, Vec<BRepEdge>)> {
    let patches: Vec<_> = stock
        .iter()
        .chain(tool)
        .map(convex_patch)
        .collect::<Option<_>>()?;
    let mut planes: Vec<Plane> = Vec::new();
    for patch in &patches {
        if !planes
            .iter()
            .any(|p| p.coincides(patch.plane) || p.coincides(patch.plane.reversed()))
        {
            planes.push(patch.plane);
        }
    }

    // Splitting at every supporting plane makes occupancy constant on each side
    // of a fragment. Discard both buried stock faces and cutter faces outside stock.
    let mut groups: Vec<(Plane, Vec<Patch<'_>>)> = Vec::new();
    for patch in patches {
        let mut pieces = vec![patch.ring];
        for &plane in &planes {
            if plane.coincides(patch.plane) || plane.coincides(patch.plane.reversed()) {
                continue;
            }
            pieces = pieces
                .into_iter()
                .flat_map(|ring| split(ring, plane))
                .collect();
            if pieces.len() > 4096 {
                return None;
            }
        }
        for mut ring in pieces {
            let center = mean_point(&ring);
            let plane = patch.plane;
            let front = result.contains(std::array::from_fn(|i| {
                center[i] + plane.normal[i] * (4. * EPS)
            }));
            let back = result.contains(std::array::from_fn(|i| {
                center[i] - plane.normal[i] * (4. * EPS)
            }));
            let outward = match (front, back) {
                (false, true) => plane,
                (true, false) => {
                    ring.reverse();
                    plane.reversed()
                }
                _ => continue,
            };
            let piece = Patch {
                plane: outward,
                source: patch.source,
                ring,
            };
            if let Some((_, group)) = groups
                .iter_mut()
                .find(|(plane, _)| plane.coincides(outward))
            {
                group.push(piece);
            } else {
                groups.push((outward, vec![piece]));
            }
        }
    }

    let points: BTreeMap<_, _> = groups
        .iter()
        .flat_map(|(_, patches)| patches)
        .flat_map(|patch| &patch.ring)
        .map(|&p| (point_key(p), p))
        .collect();
    let mut faces = Vec::new();
    for (plane, patches) in groups {
        faces.extend(merge_plane(plane, patches, &points)?);
    }
    let edges = closed_edges(&faces)?;
    Some((faces, edges))
}

fn mean_point(points: &[Point]) -> Point {
    std::array::from_fn(|i| points.iter().map(|p| p[i]).sum::<f64>() / points.len() as f64)
}

fn point_key(p: Point) -> Key {
    p.map(|v| (v / EPS).round() as i64)
}

fn ring_edges<P: Copy>(ring: &[P]) -> impl Iterator<Item = (P, P)> + '_ {
    ring.iter()
        .copied()
        .zip(ring.iter().copied().cycle().skip(1))
}

fn area_vector(ring: &[Point]) -> Point {
    let origin = ring[0];
    ring_edges(ring)
        .map(|(a, b)| cross(sub(a, origin), sub(b, origin)))
        .fold([0.; 3], |a, b| std::array::from_fn(|i| a[i] + b[i]))
}

fn convex_patch(face: &BRepFace) -> Option<Patch<'_>> {
    if face.surface_type != SurfaceType::Plane
        || face.top_loop.is_some()
        || !face.inner_loops.is_empty()
        || face.outer_loop.len() < 3
    {
        return None;
    }
    let mut ring = face.outer_loop.clone();
    let area = area_vector(&ring);
    if norm(area) < EPS * EPS {
        return None;
    }
    let mut normal = scale3(area, 1. / norm(area));
    if dot3(normal, face.normal_hint) < 0. {
        normal = scale3(normal, -1.);
        ring.reverse();
    }
    let plane = Plane {
        normal,
        offset: dot3(normal, ring[0]),
    };
    if ring.iter().any(|&p| plane.distance(p).abs() > EPS)
        || ring_edges(&ring).any(|(a, b)| {
            let direction = sub(b, a);
            ring.iter()
                .any(|&p| dot3(cross(direction, sub(p, a)), normal) < -EPS)
        })
    {
        return None;
    }
    Some(Patch {
        plane,
        source: face,
        ring,
    })
}

fn split(ring: Vec<Point>, plane: Plane) -> Vec<Vec<Point>> {
    let distances: Vec<_> = ring.iter().map(|&p| plane.distance(p)).collect();
    if !distances.iter().any(|&d| d > EPS) || !distances.iter().any(|&d| d < -EPS) {
        return vec![ring];
    }
    let mut front = Vec::new();
    let mut back = Vec::new();
    for ((a, b), (da, db)) in ring_edges(&ring).zip(ring_edges(&distances)) {
        if da >= -EPS {
            front.push(a);
        }
        if da <= EPS {
            back.push(a);
        }
        if (da > EPS && db < -EPS) || (da < -EPS && db > EPS) {
            let t = da / (da - db);
            let p = std::array::from_fn(|i| a[i] + t * (b[i] - a[i]));
            front.push(p);
            back.push(p);
        }
    }
    vec![front, back]
}

// Split every segment where it meets another patch, including T-junctions on a plane.
fn subdivide(ring: &[Point], points: &BTreeMap<Key, Point>) -> Vec<Key> {
    let mut subdivided = Vec::new();
    for (a, b) in ring_edges(ring) {
        let d = sub(b, a);
        let length = norm(d);
        if length <= EPS {
            continue;
        }
        let unit = scale3(d, 1. / length);
        let mut interior: Vec<_> = points
            .iter()
            .filter_map(|(&key, &p)| {
                let v = sub(p, a);
                let position = dot3(v, unit);
                (position > EPS && position < length - EPS && norm(cross(v, unit)) < EPS)
                    .then_some((position, key))
            })
            .collect();
        interior.sort_by(|a, b| a.0.total_cmp(&b.0));
        subdivided.push(point_key(a));
        subdivided.extend(interior.into_iter().map(|(_, key)| key));
    }
    subdivided
}

fn merge_plane(
    plane: Plane,
    patches: Vec<Patch<'_>>,
    points: &BTreeMap<Key, Point>,
) -> Option<Vec<BRepFace>> {
    let mut seen = BTreeSet::new();
    let mut boundary = BTreeSet::new();
    for patch in &patches {
        let mut ring = subdivide(&patch.ring, points);
        let start = ring.iter().enumerate().min_by_key(|(_, p)| *p)?.0;
        ring.rotate_left(start);
        if !seen.insert(ring.clone()) {
            continue;
        }
        for (a, b) in ring_edges(&ring) {
            if !boundary.remove(&(b, a)) && !boundary.insert((a, b)) {
                return None;
            }
        }
    }
    let mut next = BTreeMap::new();
    for (a, b) in boundary {
        if next.insert(a, b).is_some() {
            return None;
        }
    }
    let mut outers = Vec::new();
    let mut holes = Vec::new();
    while let Some((&start, _)) = next.first_key_value() {
        let mut current = start;
        let mut ring = Vec::new();
        loop {
            ring.push(points[&current]);
            current = next.remove(&current)?;
            if current == start {
                break;
            }
        }
        if ring.len() < 3 {
            return None;
        }
        let winding = dot3(area_vector(&ring), plane.normal);
        if winding > EPS * EPS {
            outers.push(ring);
        } else if winding < -EPS * EPS {
            holes.push(ring);
        } else {
            return None;
        }
    }
    let (u, v) = ortho_basis(plane.normal);
    let project = |p| project_to_2d(p, [0.; 3], u, v);
    outers.sort_by(|a, b| norm(area_vector(a)).total_cmp(&norm(area_vector(b))));
    let mut faces = Vec::new();
    for (index, ring) in outers.into_iter().enumerate() {
        let projected: Vec<_> = ring.iter().copied().map(project).collect();
        let mut inner = Vec::new();
        holes.retain(|hole| {
            let [x, y] = project(hole[0]);
            if point_in_polygon_2d(x, y, &projected) {
                inner.push(hole.clone());
                false
            } else {
                true
            }
        });
        let projected_holes: Vec<Vec<_>> = inner
            .iter()
            .map(|hole| hole.iter().copied().map(project).collect())
            .collect();
        let sources: Vec<_> = patches
            .iter()
            .filter_map(|patch| {
                let [x, y] = project(mean_point(&patch.ring));
                (point_in_polygon_2d(x, y, &projected)
                    && !projected_holes
                        .iter()
                        .any(|hole| point_in_polygon_2d(x, y, hole)))
                .then_some(patch.source)
            })
            .collect();
        let mut face = (*sources.first()?).clone();
        if index != 0 {
            face.face_id = format!("{}_split{index}", face.face_id);
        }
        face.center_hint = mean_point(&ring);
        face.normal_hint = plane.normal;
        face.radius_hint = None;
        face.outer_loop = ring;
        face.inner_loops = inner;
        face.exact_interface = sources.iter().all(|source| source.exact_interface);
        face.provenance_tags = sources
            .iter()
            .flat_map(|source| source.provenance_tags.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        retriangulate_face_in_place(&mut face);
        if face.indices.is_empty() {
            return None;
        }
        faces.push(face);
    }
    holes.is_empty().then_some(faces)
}

fn closed_edges(faces: &[BRepFace]) -> Option<Vec<BRepEdge>> {
    let mut uses = BTreeMap::<Edge, (BRepEdge, usize, i32)>::new();
    for face in faces {
        for ring in std::iter::once(&face.outer_loop).chain(&face.inner_loops) {
            for (a, b) in ring_edges(ring) {
                let (ka, kb) = (point_key(a), point_key(b));
                if ka == kb {
                    return None;
                }
                let (key, direction) = if ka < kb {
                    ((ka, kb), 1)
                } else {
                    ((kb, ka), -1)
                };
                let next = uses.len();
                let entry = uses.entry(key).or_insert_with(|| {
                    let mut edge =
                        make_line_edge(&format!("planar_cut_edge_{next}"), a, b, face.ast_span);
                    edge.provenance_tags = face.provenance_tags.clone();
                    (edge, 0, 0)
                });
                entry.1 += 1;
                entry.2 += direction;
            }
        }
    }
    if uses.is_empty()
        || uses
            .values()
            .any(|(_, count, direction)| (*count, *direction) != (2, 0))
    {
        return None;
    }
    Some(uses.into_values().map(|(edge, _, _)| edge).collect())
}

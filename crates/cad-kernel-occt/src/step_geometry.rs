use crate::types::{BRepFace, SurfaceType};
use std::collections::{BTreeMap, HashMap};

pub(super) type Point = [f64; 3];
pub(super) type VertexKey = (i64, i64, i64);
pub(super) type EdgeKey = (VertexKey, VertexKey);
const TOLERANCE: f64 = 1e-4;

pub(super) fn vertex_key(p: Point) -> VertexKey {
    let [x, y, z] = p.map(|v| (v / TOLERANCE).round() as i64);
    (x, y, z)
}

pub(super) fn edge_key(a: Point, b: Point) -> EdgeKey {
    let (a, b) = (vertex_key(a), vertex_key(b));
    (a.min(b), a.max(b))
}

pub(super) fn sub(a: Point, b: Point) -> Point {
    std::array::from_fn(|i| a[i] - b[i])
}

fn add(a: Point, b: Point) -> Point {
    std::array::from_fn(|i| a[i] + b[i])
}

fn scale(p: Point, k: f64) -> Point {
    p.map(|v| v * k)
}

fn dot(a: Point, b: Point) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}

pub(super) fn cross(a: Point, b: Point) -> Point {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(super) fn length(p: Point) -> f64 {
    dot(p, p).sqrt()
}

fn average(points: &[Point]) -> Point {
    scale(
        points.iter().copied().fold([0.; 3], add),
        1. / points.len() as f64,
    )
}

pub(super) fn loop_edges(points: &[Point]) -> impl Iterator<Item = (Point, Point)> + '_ {
    points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
}

#[derive(Clone, Copy)]
pub(super) struct Circle {
    pub center: Point,
    pub axis: Point,
    pub radius: f64,
}

pub(super) enum Surface {
    Plane {
        origin: Point,
        normal: Point,
    },
    Round {
        origin: Point,
        axis: Point,
        radial: Point,
        radius: f64,
        semi_angle: f64,
        outward: bool,
    },
}

pub(super) struct Face<'a> {
    pub source: &'a BRepFace,
    pub loops: Vec<Vec<Point>>,
    pub surface: Surface,
}

pub(super) struct Shell<'a> {
    pub faces: Vec<Face<'a>>,
    pub arcs: HashMap<EdgeKey, Circle>,
    triangles: Vec<[Point; 3]>,
    volume: f64,
}

pub(super) struct Body<'a> {
    pub outer: Shell<'a>,
    pub voids: Vec<Shell<'a>>,
}

enum Patch<'a> {
    Flat(Face<'a>),
    Round {
        faces: [Face<'a>; 2],
        arcs: Vec<(EdgeKey, Circle)>,
    },
    Facets(Vec<Face<'a>>),
}

impl<'a> Patch<'a> {
    fn faces(&self) -> &[Face<'a>] {
        match self {
            Self::Flat(face) => std::slice::from_ref(face),
            Self::Round { faces, .. } => faces,
            Self::Facets(faces) => faces,
        }
    }
}

pub(super) fn prepare_bodies<'a>(
    sources: impl IntoIterator<Item = &'a BRepFace>,
) -> Result<Vec<Body<'a>>, String> {
    let mut patches = Vec::new();
    let mut meshes = Vec::new();
    for source in sources {
        let triangles = triangles(source)?;
        let patch = boundary_loops(&triangles)
            .and_then(|loops| {
                if matches!(
                    source.surface_type,
                    SurfaceType::Cylinder | SurfaceType::Cone
                ) {
                    if let Some(round) = round_patch(source, &loops) {
                        return Some(round);
                    }
                }
                planar_patch(source, loops, &triangles).map(Patch::Flat)
            })
            .unwrap_or_else(|| facets(source, &triangles));
        patches.push(patch);
        meshes.push(triangles);
    }
    remove_incompatible_curves(&mut patches, &meshes);

    let mut edge_uses = BTreeMap::<EdgeKey, Vec<(usize, bool)>>::new();
    for (patch_id, patch) in patches.iter().enumerate() {
        for face in patch.faces() {
            for (a, b) in face.loops.iter().flat_map(|ring| loop_edges(ring)) {
                let edge = edge_key(a, b);
                edge_uses
                    .entry(edge)
                    .or_default()
                    .push((patch_id, vertex_key(a) == edge.0));
            }
        }
    }
    let mut neighbors = vec![Vec::new(); patches.len()];
    for uses in edge_uses.values() {
        // An edge shared by more than two faces may belong to separate bodies that touch.
        // Find the connected shells through ordinary edges before checking those incidences.
        if let [(a, _), (b, _)] = uses.as_slice() {
            neighbors[*a].push(*b);
            neighbors[*b].push(*a);
        }
    }
    let mut shell_ids = vec![usize::MAX; patches.len()];
    let mut shells = Vec::new();
    for start in 0..patches.len() {
        if shell_ids[start] != usize::MAX {
            continue;
        }
        let id = shells.len();
        shell_ids[start] = id;
        let mut queue = vec![start];
        while let Some(patch) = queue.pop() {
            for &neighbor in &neighbors[patch] {
                if shell_ids[neighbor] == usize::MAX {
                    shell_ids[neighbor] = id;
                    queue.push(neighbor);
                }
            }
        }
        shells.push(Shell {
            faces: Vec::new(),
            arcs: HashMap::new(),
            triangles: Vec::new(),
            volume: 0.,
        });
    }
    for (edge, uses) in edge_uses {
        let mut per_shell = BTreeMap::<usize, (usize, usize)>::new();
        for (patch, forward) in uses {
            let counts = per_shell.entry(shell_ids[patch]).or_default();
            if forward {
                counts.0 += 1;
            } else {
                counts.1 += 1;
            }
        }
        if per_shell.values().any(|&counts| counts != (1, 1)) {
            return Err(format!("STEP shell is open or non-manifold near edge {edge:?} (expected two opposite face boundaries)"));
        }
    }
    for ((patch, triangles), shell_id) in patches.into_iter().zip(meshes).zip(shell_ids) {
        let shell = &mut shells[shell_id];
        shell.triangles.extend(triangles);
        match patch {
            Patch::Flat(face) => shell.faces.push(face),
            Patch::Round { faces, arcs } => {
                shell.faces.extend(faces);
                shell.arcs.extend(arcs);
            }
            Patch::Facets(faces) => shell.faces.extend(faces),
        }
    }
    group_voids(shells)
}

fn triangles(face: &BRepFace) -> Result<Vec<[Point; 3]>, String> {
    let vertices: Vec<Point> = face
        .vertices
        .chunks_exact(3)
        .map(|v| [v[0], v[1], v[2]])
        .collect();
    let mut triangles = Vec::new();
    for indices in face.indices.chunks_exact(3) {
        let [Some(&a), Some(&b), Some(&c)] =
            std::array::from_fn(|i| vertices.get(indices[i] as usize))
        else {
            return Err(format!(
                "STEP face {} has an invalid triangle index",
                face.face_id
            ));
        };
        if [a, b, c].iter().flatten().any(|v| !v.is_finite()) {
            return Err(format!(
                "STEP face {} has non-finite coordinates",
                face.face_id
            ));
        }
        if vertex_key(a) != vertex_key(b)
            && vertex_key(b) != vertex_key(c)
            && vertex_key(c) != vertex_key(a)
        {
            triangles.push([a, b, c]);
        }
    }
    if triangles.is_empty() {
        return Err(format!(
            "STEP face {} has no non-degenerate triangles",
            face.face_id
        ));
    }
    Ok(triangles)
}

fn boundary_loops(triangles: &[[Point; 3]]) -> Option<Vec<Vec<Point>>> {
    let mut edges = BTreeMap::<EdgeKey, (i32, Point, Point)>::new();
    for triangle in triangles {
        for (a, b) in loop_edges(triangle) {
            let key = edge_key(a, b);
            let (direction, start, end) = if vertex_key(a) == key.0 {
                (1, a, b)
            } else {
                (-1, b, a)
            };
            let entry = edges.entry(key).or_insert((0, start, end));
            entry.0 += direction;
        }
    }
    let mut next = BTreeMap::new();
    for (count, start, end) in edges.into_values() {
        let (start, end) = match count {
            0 => continue,
            1 => (start, end),
            -1 => (end, start),
            _ => return None,
        };
        if next
            .insert(vertex_key(start), (start, vertex_key(end)))
            .is_some()
        {
            return None;
        }
    }
    let mut loops = Vec::new();
    while let Some(&start) = next.keys().next() {
        let mut current = start;
        let mut ring = Vec::new();
        loop {
            let (point, end) = next.remove(&current)?;
            ring.push(point);
            if end == start {
                break;
            }
            current = end;
        }
        if ring.len() < 3 {
            return None;
        }
        loops.push(ring);
    }
    (!loops.is_empty()).then_some(loops)
}

fn area_vector(ring: &[Point]) -> Point {
    let origin = ring[0];
    loop_edges(ring)
        .map(|(a, b)| cross(sub(a, origin), sub(b, origin)))
        .fold([0.; 3], add)
}

fn planar_patch<'a>(
    source: &'a BRepFace,
    mut loops: Vec<Vec<Point>>,
    triangles: &[[Point; 3]],
) -> Option<Face<'a>> {
    let (outer, area) = loops
        .iter()
        .enumerate()
        .map(|(i, ring)| (i, area_vector(ring)))
        .max_by(|(_, a), (_, b)| length(*a).total_cmp(&length(*b)))?;
    if length(area) < 1e-12 {
        return None;
    }
    loops.swap(0, outer);
    let origin = loops[0][0];
    let normal = scale(area, 1. / length(area));
    if loops[1..]
        .iter()
        .any(|ring| dot(area_vector(ring), normal) >= 0.)
        || triangles
            .iter()
            .flatten()
            .any(|p| dot(sub(*p, origin), normal).abs() > TOLERANCE / 10.)
    {
        return None;
    }
    Some(Face {
        source,
        loops,
        surface: Surface::Plane { origin, normal },
    })
}

fn facets<'a>(source: &'a BRepFace, triangles: &[[Point; 3]]) -> Patch<'a> {
    Patch::Facets(
        triangles
            .iter()
            .map(|&[a, b, c]| {
                let normal = cross(sub(b, a), sub(c, a));
                Face {
                    source,
                    loops: vec![vec![a, b, c]],
                    surface: Surface::Plane {
                        origin: a,
                        normal: scale(normal, 1. / length(normal)),
                    },
                }
            })
            .collect(),
    )
}

fn round_patch<'a>(source: &'a BRepFace, loops: &[Vec<Point>]) -> Option<Patch<'a>> {
    let [first, second] = loops else {
        return None;
    };
    let n = first.len();
    if n != second.len() || n < 4 {
        return None;
    }
    let (mut lower, mut upper) = (first, second);
    let (mut origin, mut end) = (average(lower), average(upper));
    let (mut radius, mut end_radius) = (length(sub(lower[0], origin)), length(sub(upper[0], end)));
    if radius > end_radius && source.surface_type == SurfaceType::Cone {
        std::mem::swap(&mut lower, &mut upper);
        std::mem::swap(&mut origin, &mut end);
        std::mem::swap(&mut radius, &mut end_radius);
    }
    let height = length(sub(end, origin));
    if height <= TOLERANCE || radius <= TOLERANCE || end_radius <= TOLERANCE {
        return None;
    }
    let axis = scale(sub(end, origin), 1. / height);
    let radial = scale(sub(lower[0], origin), 1. / radius);
    let out = dot(cross(sub(lower[0], origin), sub(lower[1], origin)), axis) > 0.;
    let offset = upper
        .iter()
        .position(|p| length(sub(sub(*p, end), scale(radial, end_radius))) < TOLERANCE / 10.)?;
    let upper: Vec<_> = (0..n).map(|i| upper[(offset + n - i) % n]).collect();
    let sense = if out { 1. } else { -1. };
    for (a, b) in loop_edges(lower) {
        let a = sub(a, origin);
        let b = sub(b, origin);
        if dot(a, axis).abs() > TOLERANCE / 10.
            || (length(a) - radius).abs() > TOLERANCE / 10.
            || sense * dot(cross(a, b), axis) <= 0.
        {
            return None;
        }
    }
    if lower.iter().zip(&upper).any(|(a, b)| {
        length(sub(
            scale(sub(*a, origin), end_radius / radius),
            sub(*b, end),
        )) > TOLERANCE / 10.
    }) {
        return None;
    }

    let semi_angle = if (end_radius - radius).abs() <= 1e-8 {
        0.
    } else {
        (end_radius - radius).atan2(height)
    };
    let make_half = |start: usize, stop: usize| {
        let mut ring: Vec<_> = (start..=stop).map(|i| lower[i % n]).collect();
        ring.extend((start..=stop).rev().map(|i| upper[i % n]));
        Face {
            source,
            loops: vec![ring],
            surface: Surface::Round {
                origin,
                axis,
                radial,
                radius,
                semi_angle,
                outward: out,
            },
        }
    };
    let arcs = loop_edges(lower)
        .map(|(a, b)| {
            (
                edge_key(a, b),
                Circle {
                    center: origin,
                    axis,
                    radius,
                },
            )
        })
        .chain(loop_edges(&upper).map(|(a, b)| {
            (
                edge_key(a, b),
                Circle {
                    center: end,
                    axis,
                    radius: end_radius,
                },
            )
        }))
        .collect();
    Some(Patch::Round {
        faces: [make_half(0, n / 2), make_half(n / 2, n)],
        arcs,
    })
}

fn remove_incompatible_curves(patches: &mut [Patch<'_>], meshes: &[Vec<[Point; 3]>]) {
    loop {
        let mut known_arcs = HashMap::<EdgeKey, Vec<(usize, Circle)>>::new();
        for (id, patch) in patches.iter().enumerate() {
            if let Patch::Round { arcs, .. } = patch {
                for &(key, arc) in arcs {
                    known_arcs.entry(key).or_default().push((id, arc));
                }
            }
        }
        let mut fallback = vec![false; patches.len()];
        for (id, patch) in patches.iter().enumerate() {
            for face in patch.faces() {
                for (a, b) in face.loops.iter().flat_map(|ring| loop_edges(ring)) {
                    let key = edge_key(a, b);
                    let Some(arcs) = known_arcs.get(&key) else {
                        continue;
                    };
                    for &(owner, arc) in arcs {
                        if owner == id {
                            continue;
                        }
                        let compatible = match face.surface {
                            Surface::Plane { origin, normal } => {
                                dot(sub(arc.center, origin), normal).abs()
                                    + arc.radius * length(cross(arc.axis, normal))
                                    <= TOLERANCE / 10.
                            }
                            Surface::Round { .. } => arcs.iter().any(|&(other, other_arc)| {
                                other == id
                                    && length(sub(arc.center, other_arc.center)) <= TOLERANCE / 10.
                                    && length(cross(arc.axis, other_arc.axis)) * arc.radius
                                        <= TOLERANCE / 10.
                                    && (arc.radius - other_arc.radius).abs() <= TOLERANCE / 10.
                            }),
                        };
                        if !compatible {
                            fallback[owner] = true;
                        }
                    }
                }
            }
        }
        if !fallback.iter().any(|&value| value) {
            break;
        }
        for (id, patch) in patches.iter_mut().enumerate() {
            if fallback[id] {
                *patch = facets(patch.faces()[0].source, &meshes[id]);
            }
        }
    }
}

fn group_voids<'a>(shells: Vec<Shell<'a>>) -> Result<Vec<Body<'a>>, String> {
    let mut bodies = Vec::new();
    let mut voids = Vec::new();
    for mut shell in shells {
        let reference = shell.triangles[0][0];
        shell.volume = shell
            .triangles
            .iter()
            .map(|&[a, b, c]| {
                dot(
                    sub(a, reference),
                    cross(sub(b, reference), sub(c, reference)),
                ) / 6.
            })
            .sum();
        if shell.volume > 1e-9 {
            bodies.push(Body {
                outer: shell,
                voids: Vec::new(),
            });
        } else if shell.volume < -1e-9 {
            voids.push(shell);
        } else {
            return Err("STEP shell encloses zero volume".into());
        }
    }
    for inner in voids {
        let mut vertices = BTreeMap::new();
        for &point in inner.triangles.iter().flatten() {
            vertices.insert(vertex_key(point), point);
        }
        let body = bodies
            .iter_mut()
            .filter(|body| {
                body.outer.volume > -inner.volume
                    && vertices.values().all(|&point| contains_point(&body.outer, point))
            })
            .min_by(|a, b| a.outer.volume.total_cmp(&b.outer.volume))
            .ok_or_else(|| format!(
                "STEP cavity near face '{}' is not fully enclosed by any solid; its subtraction boundary may cross the outer shell",
                inner.faces[0].source.face_id
            ))?;
        body.voids.push(inner);
    }
    if bodies.is_empty() {
        return Err("STEP export contains no closed solids".into());
    }
    Ok(bodies)
}

fn contains_point(shell: &Shell<'_>, point: Point) -> bool {
    // Sum the oriented solid angle of the mesh around the query point.
    let angle: f64 = shell
        .triangles
        .iter()
        .map(|triangle| {
            let [a, b, c] = triangle.map(|p| sub(p, point));
            let (la, lb, lc) = (length(a), length(b), length(c));
            2. * dot(a, cross(b, c))
                .atan2(la * lb * lc + dot(a, b) * lc + dot(b, c) * la + dot(c, a) * lb)
        })
        .sum();
    angle.abs() > 2. * std::f64::consts::PI
}

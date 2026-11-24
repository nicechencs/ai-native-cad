//! Weighted Catmull-Clark subdivision with Pixar uniform semi-sharp creases.
//!
//! One step builds a face point, an edge point, and a moved copy of every
//! original vertex, then emits one quad per old corner. Original vertices keep
//! their indices. Edge points and face points are appended after them.
//!
//! Crease sharpness decays by 1 each step and is clamped at 0. Sharpness
//! `>= 10` is infinite and stays 10. A boundary edge is geometrically sharp
//! because it has only one face. A semi-sharp edge (`0 < s < 1`) blends the
//! smooth edge point with the midpoint. A vertex blends the parent mask with
//! the child mask when a crease decays through this step.

use std::collections::BTreeMap;

use crate::cage::{SubDCage, SubDError, fit_u32};

/// Triangle mesh extracted from a control cage.
#[derive(Debug, Clone, PartialEq)]
pub struct SubDMesh {
    /// Vertex positions copied from the cage.
    pub vertices: Vec<[f64; 3]>,
    /// Triangle corners. Quads become two triangles; n-gons use a fan.
    pub triangles: Vec<[u32; 3]>,
    /// Unit vertex normals. Each triangle contributes its area-weighted cross product.
    pub normals: Vec<[f64; 3]>,
    /// Component-wise minimum of `vertices`.
    pub bbox_min: [f64; 3],
    /// Component-wise maximum of `vertices`.
    pub bbox_max: [f64; 3],
}

#[derive(Clone, Copy)]
struct IncidentEdge {
    neighbor: u32,
    /// Crease stored on the cage. `0.0` when the edge is smooth.
    user_crease: f32,
    boundary: bool,
}

/// Subdivide `levels` times. Level 0 returns a clone of `cage`.
pub fn subdivide(cage: &SubDCage, levels: u32) -> Result<SubDCage, SubDError> {
    if levels == 0 {
        return Ok(cage.clone());
    }
    let mut current = subdivide_once(cage)?;
    for _ in 1..levels {
        current = subdivide_once(&current)?;
    }
    Ok(current)
}

/// Apply one Catmull-Clark step.
///
/// Non-manifold edges (three or more faces) return an error. Boundary edges
/// are kept and treated as sharp.
pub fn subdivide_once(cage: &SubDCage) -> Result<SubDCage, SubDError> {
    cage.validate()?;
    if cage.faces.is_empty() {
        return Err(SubDError::EmptyFaces);
    }
    let edges = cage.edge_incidences();
    if let Some((&(a, b), incident)) = edges.iter().find(|(_, incident)| incident.len() > 2) {
        return Err(SubDError::NonManifoldEdge {
            a,
            b,
            incident_faces: incident.len(),
        });
    }

    let face_points = face_centroids(cage);
    let adjacency = vertex_adjacency(cage, &edges);
    let layout = IndexLayout::new(cage.vertices.len(), edges.len(), cage.faces.len())?;

    let mut edge_points = Vec::with_capacity(edges.len());
    let mut edge_slot: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    let mut child_creases: BTreeMap<(u32, u32), f32> = BTreeMap::new();
    for (slot, (&key, incident_faces)) in edges.iter().enumerate() {
        let (a, b) = key;
        let boundary = incident_faces.len() == 1;
        let user_crease = cage.edge_creases.get(&key).copied().unwrap_or(0.0);
        let point = edge_point(
            cage.vertices[a as usize],
            cage.vertices[b as usize],
            incident_faces,
            &face_points,
            user_crease,
            boundary,
        );
        let point_index = layout
            .edge_base
            .checked_add(fit_u32(slot)?)
            .ok_or(SubDError::InvalidCount)?;
        edge_slot.insert(key, point_index);
        edge_points.push(point);

        let child = stored_child_crease(user_crease);
        if child > 0.0 {
            child_creases.insert((a, point_index), child);
            child_creases.insert((b, point_index), child);
        }
    }

    let mut vertices = Vec::with_capacity(layout.total);
    for (index, vertex) in cage.vertices.iter().enumerate() {
        vertices.push(moved_vertex(
            *vertex,
            &adjacency[index],
            &face_points,
            &cage.vertices,
        ));
    }
    vertices.extend(edge_points);
    vertices.extend(face_points);

    let mut faces = Vec::new();
    for (face_index, face) in cage.faces.iter().enumerate() {
        let face_point_index = layout
            .face_base
            .checked_add(fit_u32(face_index)?)
            .ok_or(SubDError::InvalidCount)?;
        let corner_count = face.len();
        for corner in 0..corner_count {
            let current = face[corner];
            let next = face[(corner + 1) % corner_count];
            let previous = face[(corner + corner_count - 1) % corner_count];
            let next_edge = edge_slot[&ordered_edge(current, next)];
            let previous_edge = edge_slot[&ordered_edge(previous, current)];
            faces.push(vec![current, next_edge, face_point_index, previous_edge]);
        }
    }

    Ok(SubDCage {
        vertices,
        faces,
        edge_creases: child_creases,
    })
}

/// Triangulate the cage and build smooth area-weighted normals plus a bounding box.
pub fn to_triangle_mesh(cage: &SubDCage) -> SubDMesh {
    let mut triangles = Vec::new();
    let vertex_count = cage.vertices.len();
    for face in &cage.faces {
        if face.len() < 3 || face.iter().any(|index| *index as usize >= vertex_count) {
            continue;
        }
        let root = face[0];
        for corner in 1..face.len() - 1 {
            triangles.push([root, face[corner], face[corner + 1]]);
        }
    }

    let mut normals = vec![[0.0; 3]; vertex_count];
    for triangle in &triangles {
        let a = cage.vertices[triangle[0] as usize];
        let b = cage.vertices[triangle[1] as usize];
        let c = cage.vertices[triangle[2] as usize];
        let weighted = cross(sub(b, a), sub(c, a));
        for index in triangle {
            let slot = &mut normals[*index as usize];
            *slot = add(*slot, weighted);
        }
    }
    for normal in &mut normals {
        *normal = normalize(*normal);
    }

    let (bbox_min, bbox_max) = bounding_box(&cage.vertices);
    SubDMesh {
        vertices: cage.vertices.clone(),
        triangles,
        normals,
        bbox_min,
        bbox_max,
    }
}

struct IndexLayout {
    edge_base: u32,
    face_base: u32,
    total: usize,
}

impl IndexLayout {
    fn new(vertex_count: usize, edge_count: usize, face_count: usize) -> Result<Self, SubDError> {
        let total = vertex_count
            .checked_add(edge_count)
            .and_then(|count| count.checked_add(face_count))
            .ok_or(SubDError::InvalidCount)?;
        let edge_base = fit_u32(vertex_count)?;
        let face_base = edge_base
            .checked_add(fit_u32(edge_count)?)
            .ok_or(SubDError::InvalidCount)?;
        let _end = face_base
            .checked_add(fit_u32(face_count)?)
            .ok_or(SubDError::InvalidCount)?;
        Ok(Self {
            edge_base,
            face_base,
            total,
        })
    }
}

fn face_centroids(cage: &SubDCage) -> Vec<[f64; 3]> {
    cage.faces
        .iter()
        .map(|face| {
            let mut sum = [0.0; 3];
            for index in face {
                sum = add(sum, cage.vertices[*index as usize]);
            }
            scale(sum, 1.0 / face.len() as f64)
        })
        .collect()
}

fn vertex_adjacency(cage: &SubDCage, edges: &BTreeMap<(u32, u32), Vec<usize>>) -> Vec<VertexAdj> {
    let mut adjacency = vec![
        VertexAdj {
            edges: Vec::new(),
            faces: Vec::new(),
        };
        cage.vertices.len()
    ];
    for (&(a, b), incident_faces) in edges {
        let boundary = incident_faces.len() == 1;
        let user_crease = cage.edge_creases.get(&(a, b)).copied().unwrap_or(0.0);
        adjacency[a as usize].edges.push(IncidentEdge {
            neighbor: b,
            user_crease,
            boundary,
        });
        adjacency[b as usize].edges.push(IncidentEdge {
            neighbor: a,
            user_crease,
            boundary,
        });
    }
    for (face_index, face) in cage.faces.iter().enumerate() {
        for &vertex in face {
            adjacency[vertex as usize].faces.push(face_index);
        }
    }
    adjacency
}

#[derive(Clone)]
struct VertexAdj {
    edges: Vec<IncidentEdge>,
    faces: Vec<usize>,
}

fn moved_vertex(
    position: [f64; 3],
    adjacency: &VertexAdj,
    face_points: &[[f64; 3]],
    vertices: &[[f64; 3]],
) -> [f64; 3] {
    let valence = adjacency.edges.len();
    if valence == 0 {
        return position;
    }
    let smooth = smooth_mask(position, adjacency, face_points, vertices);
    let parent: Vec<IncidentEdge> = adjacency
        .edges
        .iter()
        .copied()
        .filter(|edge| rule_sharpness(edge) > 0.0)
        .collect();
    let child: Vec<IncidentEdge> = adjacency
        .edges
        .iter()
        .copied()
        .filter(|edge| child_rule_sharpness(edge) > 0.0)
        .collect();
    let parent_position = mask_position(position, smooth, &parent, vertices);
    let child_position = mask_position(position, smooth, &child, vertices);
    lerp(
        child_position,
        parent_position,
        f64::from(transition_weight(&adjacency.edges)),
    )
}

fn smooth_mask(
    position: [f64; 3],
    adjacency: &VertexAdj,
    face_points: &[[f64; 3]],
    vertices: &[[f64; 3]],
) -> [f64; 3] {
    let valence = adjacency.edges.len();
    let n = valence as f64;
    let face_average = if adjacency.faces.is_empty() {
        position
    } else {
        let mut sum = [0.0; 3];
        for &face_index in &adjacency.faces {
            sum = add(sum, face_points[face_index]);
        }
        scale(sum, 1.0 / adjacency.faces.len() as f64)
    };
    let mut midpoint_sum = [0.0; 3];
    for edge in &adjacency.edges {
        let neighbor = vertices[edge.neighbor as usize];
        midpoint_sum = add(midpoint_sum, scale(add(position, neighbor), 0.5));
    }
    let midpoint_average = scale(midpoint_sum, 1.0 / n);
    // (F + 2R + (n - 3) P) / n
    scale(
        add(
            add(face_average, scale(midpoint_average, 2.0)),
            scale(position, n - 3.0),
        ),
        1.0 / n,
    )
}

fn mask_position(
    position: [f64; 3],
    smooth: [f64; 3],
    sharp_edges: &[IncidentEdge],
    vertices: &[[f64; 3]],
) -> [f64; 3] {
    match sharp_edges.len() {
        0 | 1 => smooth,
        2 => {
            let previous = vertices[sharp_edges[0].neighbor as usize];
            let next = vertices[sharp_edges[1].neighbor as usize];
            // (P_prev + 6P + P_next) / 8
            scale(add(add(previous, next), scale(position, 6.0)), 0.125)
        }
        _ => position,
    }
}

/// Average fractional sharpness of edges that are sharp now and smooth in the child.
///
/// `1` keeps the parent mask. OpenSubdiv uses this weight when a semi-sharp
/// feature decays across the step. Infinite and fully sharp edges contribute
/// nothing here because their children stay sharp.
fn transition_weight(edges: &[IncidentEdge]) -> f32 {
    let mut sum = 0.0f32;
    let mut count = 0u32;
    for edge in edges {
        let parent = rule_sharpness(edge);
        let child = child_rule_sharpness(edge);
        if parent > 0.0 && child <= 0.0 {
            sum += parent.min(1.0);
            count += 1;
        }
    }
    if count == 0 { 1.0 } else { sum / count as f32 }
}

fn rule_sharpness(edge: &IncidentEdge) -> f32 {
    if edge.boundary {
        crate::CREASE_INFINITE
    } else {
        edge.user_crease
    }
}

fn child_rule_sharpness(edge: &IncidentEdge) -> f32 {
    if edge.boundary {
        crate::CREASE_INFINITE
    } else {
        stored_child_crease(edge.user_crease)
    }
}

fn stored_child_crease(sharpness: f32) -> f32 {
    if sharpness >= crate::CREASE_INFINITE {
        crate::CREASE_INFINITE
    } else if sharpness > 1.0 {
        sharpness - 1.0
    } else {
        0.0
    }
}

fn edge_point(
    start: [f64; 3],
    end: [f64; 3],
    incident_faces: &[usize],
    face_points: &[[f64; 3]],
    crease: f32,
    boundary: bool,
) -> [f64; 3] {
    let midpoint = scale(add(start, end), 0.5);
    if boundary || crease >= 1.0 || incident_faces.len() != 2 {
        return midpoint;
    }
    let smooth = scale(
        add(
            add(start, end),
            add(
                face_points[incident_faces[0]],
                face_points[incident_faces[1]],
            ),
        ),
        0.25,
    );
    if crease <= 0.0 {
        smooth
    } else {
        lerp(smooth, midpoint, f64::from(crease))
    }
}

fn ordered_edge(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

fn bounding_box(vertices: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    if vertices.is_empty() {
        return ([0.0; 3], [0.0; 3]);
    }
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    (min, max)
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], factor: f64) -> [f64; 3] {
    [a[0] * factor, a[1] * factor, a[2] * factor]
}

fn lerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    add(scale(a, 1.0 - t), scale(b, t))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let len = length(a);
    if len < 1e-30 {
        [0.0; 3]
    } else {
        scale(a, 1.0 / len)
    }
}

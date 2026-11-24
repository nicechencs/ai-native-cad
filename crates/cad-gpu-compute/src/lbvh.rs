//! Morton-ordered hierarchy for triangle overlap and ray queries.

use crate::morton::morton_3d;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb3 {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb3 {
    /// Tests overlap, including touching boundaries.
    pub fn intersects(&self, other: &Self) -> bool {
        (0..3).all(|axis| self.min[axis] <= other.max[axis] && other.min[axis] <= self.max[axis])
    }

    pub fn min_distance_sq(&self, other: &Self) -> f64 {
        (0..3)
            .map(|axis| {
                let gap = (self.min[axis] - other.max[axis])
                    .max(other.min[axis] - self.max[axis])
                    .max(0.0);
                gap * gap
            })
            .sum()
    }

    /// Pads both ends of each axis by a nonnegative distance.
    pub fn expand(&self, distance: f64) -> Self {
        let distance = distance.max(0.0);
        Self {
            min: self.min.map(|value| value - distance),
            max: self.max.map(|value| value + distance),
        }
    }

    /// Tests the forward half-ray, including origins inside the box.
    pub fn ray_intersects(&self, origin: [f64; 3], direction: [f64; 3]) -> bool {
        valid_ray(origin, direction) && self.ray_enters_before(origin, direction, f64::INFINITY)
    }

    fn ray_enters_before(&self, origin: [f64; 3], direction: [f64; 3], limit: f64) -> bool {
        let mut near = 0.0_f64;
        let mut far = limit;
        for axis in 0..3 {
            if direction[axis] == 0.0 {
                if origin[axis] < self.min[axis] || origin[axis] > self.max[axis] {
                    return false;
                }
                continue;
            }
            let start = (self.min[axis] - origin[axis]) / direction[axis];
            let end = (self.max[axis] - origin[axis]) / direction[axis];
            near = near.max(start.min(end));
            far = far.min(start.max(end));
            if near > far {
                return false;
            }
        }
        true
    }

    fn union(self, other: Self) -> Self {
        Self {
            min: std::array::from_fn(|axis| self.min[axis].min(other.min[axis])),
            max: std::array::from_fn(|axis| self.max[axis].max(other.max[axis])),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriRef {
    pub instance_id: u32,
    pub face_slot_id: u32,
    pub a: [f64; 3],
    pub b: [f64; 3],
    pub c: [f64; 3],
}

impl TriRef {
    pub fn aabb(&self) -> Aabb3 {
        Aabb3 {
            min: std::array::from_fn(|axis| self.a[axis].min(self.b[axis]).min(self.c[axis])),
            max: std::array::from_fn(|axis| self.a[axis].max(self.b[axis]).max(self.c[axis])),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// Index of the triangle in the original input slice.
    pub triangle_index: usize,
    pub instance_id: u32,
    pub face_slot_id: u32,
    /// Distance along the normalized direction, in millimeters.
    pub distance_mm: f64,
}

/// One node in the 48-byte storage layout shared by both WGSL kernels.
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuLbvhNode {
    pub lo: [f32; 4],
    pub hi: [f32; 4],
    /// Escape index, page-local triangle index (`u32::MAX` for an internal
    /// node), and two padding words.
    pub links: [u32; 4],
}

const _: () = assert!(size_of::<GpuLbvhNode>() == 48);

#[derive(Clone, Debug)]
struct Node {
    bounds: Aabb3,
    triangle_index: Option<usize>,
    // Nodes are in preorder. A missed subtree advances directly to escape.
    escape: usize,
}

#[derive(Clone, Debug, Default)]
pub struct LbvhTree {
    triangles: Vec<TriRef>,
    nodes: Vec<Node>,
}

type LeafKey = (u32, usize);

impl LbvhTree {
    /// Builds a radix hierarchy over triangle AABB centers. Input coordinates
    /// are finite millimeter coordinates in a common frame.
    pub fn build(triangles: &[TriRef]) -> Self {
        let bounds: Vec<_> = triangles.iter().map(TriRef::aabb).collect();
        let Some(scene_bounds) = bounds.iter().copied().reduce(Aabb3::union) else {
            return Self::default();
        };
        let mut leaves: Vec<LeafKey> = bounds
            .iter()
            .enumerate()
            .map(|(index, bounds)| {
                let [x, y, z] =
                    std::array::from_fn(|axis| bounds.min[axis].midpoint(bounds.max[axis]));
                (
                    morton_3d(x, y, z, scene_bounds.min, scene_bounds.max),
                    index,
                )
            })
            .collect();
        // The original index breaks ties between quantized centers. This
        // bounds radix depth even when all Morton codes are identical.
        leaves.sort_unstable();

        let mut nodes = Vec::with_capacity(2 * triangles.len() - 1);
        build_subtree(&mut nodes, &leaves, &bounds);
        Self {
            triangles: triangles.to_vec(),
            nodes,
        }
    }

    /// Returns the root scene bounding box, or `None` for an empty tree.
    #[must_use]
    pub fn scene_bounds(&self) -> Option<Aabb3> {
        self.nodes.first().map(|node| node.bounds)
    }

    /// Returns the input triangles stored in this tree.
    #[must_use]
    pub fn triangles(&self) -> &[TriRef] {
        &self.triangles
    }

    /// Returns original triangle indices whose AABBs overlap `bounds`.
    /// The result follows Morton traversal order.
    pub fn query_aabb(&self, bounds: &Aabb3) -> Vec<usize> {
        let mut results = Vec::new();
        let mut index = 0;
        while let Some(node) = self.nodes.get(index) {
            if !node.bounds.intersects(bounds) {
                index = node.escape;
                continue;
            }
            if let Some(triangle_index) = node.triangle_index {
                results.push(triangle_index);
            }
            index += 1;
        }
        results
    }

    /// Produces the node records for one GPU geometry page. Build each page
    /// from its corresponding slice of triangles, preserving their input order
    /// in the triangle or metadata buffer. Rebase both buffers to the same
    /// ROI origin before converting to `f32`. Returns `None` if the node page
    /// exceeds 128 MiB or the rebased bounds are outside the finite `f32` range.
    pub fn gpu_nodes(&self, roi_origin_mm: [f64; 3]) -> Option<Vec<GpuLbvhNode>> {
        const MAX_PAGE_BYTES: usize = 128 * 1024 * 1024;
        if self.nodes.len() > MAX_PAGE_BYTES / size_of::<GpuLbvhNode>() {
            return None;
        }

        let rebase = |point: [f64; 3]| {
            let [x, y, z] = std::array::from_fn(|axis| (point[axis] - roi_origin_mm[axis]) as f32);
            let bounds = [x, y, z, 0.0];
            bounds.into_iter().all(f32::is_finite).then_some(bounds)
        };
        self.nodes
            .iter()
            .map(|node| {
                Some(GpuLbvhNode {
                    lo: rebase(node.bounds.min)?,
                    hi: rebase(node.bounds.max)?,
                    // The page size check bounds both indices below u32::MAX.
                    links: [
                        node.escape as u32,
                        node.triangle_index.map_or(u32::MAX, |i| i as u32),
                        0,
                        0,
                    ],
                })
            })
            .collect()
    }

    /// Returns two-sided ray hits in ascending distance, breaking ties by
    /// original triangle index. Hits at the origin are excluded.
    pub fn cast_ray_all_hits(&self, origin: [f64; 3], direction: [f64; 3]) -> Vec<RayHit> {
        let mut hits = Vec::new();
        self.visit_ray_hits(origin, direction, |hit, _| {
            hits.push(hit);
            f64::INFINITY
        });
        hits.sort_unstable_by(|a, b| {
            a.distance_mm
                .total_cmp(&b.distance_mm)
                .then_with(|| a.triangle_index.cmp(&b.triangle_index))
        });
        hits
    }

    pub(crate) fn cast_ray_nearest_where(
        &self,
        origin: [f64; 3],
        direction: [f64; 3],
        mut accept: impl FnMut(usize, &TriRef, f64) -> bool,
    ) -> Option<RayHit> {
        let mut nearest: Option<RayHit> = None;
        self.visit_ray_hits(origin, direction, |hit, triangle| {
            if nearest.is_none_or(|best| {
                (hit.distance_mm, hit.triangle_index) < (best.distance_mm, best.triangle_index)
            }) && accept(hit.triangle_index, triangle, hit.distance_mm)
            {
                nearest = Some(hit);
            }
            nearest.map_or(f64::INFINITY, |hit| hit.distance_mm)
        });
        nearest
    }

    fn visit_ray_hits(
        &self,
        origin: [f64; 3],
        direction: [f64; 3],
        mut visit: impl FnMut(RayHit, &TriRef) -> f64,
    ) {
        if !valid_ray(origin, direction) {
            return;
        }
        let scale = direction.map(f64::abs).into_iter().fold(0.0, f64::max);
        let direction = direction.map(|component| component / scale);
        let magnitude = dot(direction, direction).sqrt();
        let direction = direction.map(|component| component / magnitude);
        let mut limit = f64::INFINITY;
        let mut index = 0;
        while let Some(node) = self.nodes.get(index) {
            if !node.bounds.ray_enters_before(origin, direction, limit) {
                index = node.escape;
                continue;
            }
            if let Some(triangle_index) = node.triangle_index {
                let triangle = &self.triangles[triangle_index];
                if let Some(distance_mm) = ray_triangle(origin, direction, triangle) {
                    let hit = RayHit {
                        triangle_index,
                        instance_id: triangle.instance_id,
                        face_slot_id: triangle.face_slot_id,
                        distance_mm,
                    };
                    limit = visit(hit, triangle);
                }
            }
            index += 1;
        }
    }
}

fn build_subtree(nodes: &mut Vec<Node>, leaves: &[LeafKey], bounds: &[Aabb3]) -> Aabb3 {
    let index = nodes.len();
    nodes.push(Node {
        bounds: bounds[leaves[0].1],
        triangle_index: None,
        escape: 0,
    });
    let node_bounds = if let [(_, triangle_index)] = leaves {
        nodes[index].triangle_index = Some(*triangle_index);
        bounds[*triangle_index]
    } else {
        let first = leaves[0];
        let prefix = common_prefix(first, leaves[leaves.len() - 1]);
        let split = leaves.partition_point(|&key| common_prefix(first, key) > prefix);
        let left = build_subtree(nodes, &leaves[..split], bounds);
        let right = build_subtree(nodes, &leaves[split..], bounds);
        left.union(right)
    };
    nodes[index].bounds = node_bounds;
    nodes[index].escape = nodes.len();
    node_bounds
}

fn common_prefix(a: LeafKey, b: LeafKey) -> u32 {
    if a.0 == b.0 {
        u32::BITS + (a.1 ^ b.1).leading_zeros()
    } else {
        (a.0 ^ b.0).leading_zeros()
    }
}

fn valid_ray(origin: [f64; 3], direction: [f64; 3]) -> bool {
    origin.into_iter().chain(direction).all(f64::is_finite)
        && direction.into_iter().any(|component| component != 0.0)
}

fn ray_triangle(origin: [f64; 3], direction: [f64; 3], triangle: &TriRef) -> Option<f64> {
    let edge_ab = sub(triangle.b, triangle.a);
    let edge_ac = sub(triangle.c, triangle.a);
    let p = cross(direction, edge_ac);
    let determinant = dot(edge_ab, p);
    if determinant == 0.0 {
        return None;
    }

    let from_a = sub(origin, triangle.a);
    let u = dot(from_a, p) / determinant;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(from_a, edge_ab);
    let v = dot(direction, q) / determinant;
    if !(0.0..=1.0 - u).contains(&v) {
        return None;
    }
    let distance = dot(edge_ac, q) / determinant;
    (distance.is_finite() && distance > 0.0).then_some(distance)
}

pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

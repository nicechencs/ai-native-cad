//! Portable WebGPU compute kernels. Storage records have 16-byte strides or
//! multiples thereof. Bind each page at no more than 128 MiB, and use the
//! same ROI-relative millimeter coordinates for all pages in a job.
//! [`crate::LbvhTree::gpu_nodes`] exports the shared node layout.

/// Pipeline A broad phase. Dispatch `main` with 64 threads per workgroup.
///
/// Group 0 bindings are a 32-byte uniform, query AABBs, BVH nodes, target
/// metadata, and a candidate output page. The host builds one preorder LBVH
/// per target page and dispatches each query page against each target page,
/// including itself. A leaf's primitive index is local to the target page.
/// Each metadata vector stores `(instance_id, face_slot_id, triangle_id, 0)`;
/// triangle IDs refer to the original input across pages.
/// Include the ROI's `f32` rounding margin in the uniform's clearance.
///
/// Initialize the output header's count and overflow flag to zero. Output
/// records store `(triangle_a, triangle_b, face_a, face_b)`, ordered by
/// increasing instance IDs. Count is bounded by capacity. On overflow the
/// host subdivides the query range or target page and retries. The output is
/// conservative and can feed the CPU triangle narrow phase.
pub const LBVH_BROADPHASE_WGSL: &str = r#"
const NO_TRIANGLE: u32 = 0xffffffffu;

struct Params {
    query_start: u32,
    query_count: u32,
    node_count: u32,
    output_capacity: u32,
    clearance_mm: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

struct Query {
    lo: vec4<f32>,
    hi: vec4<f32>,
    ids: vec4<u32>,
};

// links.x is the escape index, links.y the local triangle index or NO_TRIANGLE.
// The root is at index 0 and each internal node's first child is at i + 1.
struct Node {
    lo: vec4<f32>,
    hi: vec4<f32>,
    links: vec4<u32>,
};

struct CandidatePage {
    count: atomic<u32>,
    overflow: atomic<u32>,
    _pad0: u32,
    _pad1: u32,
    pairs: array<vec4<u32>>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> queries: array<Query>;
@group(0) @binding(2) var<storage, read> nodes: array<Node>;
@group(0) @binding(3) var<storage, read> triangle_ids: array<vec4<u32>>;
@group(0) @binding(4) var<storage, read_write> output: CandidatePage;

fn write_candidate(pair: vec4<u32>) {
    let capacity = min(params.output_capacity, arrayLength(&output.pairs));
    var slot = atomicLoad(&output.count);
    loop {
        if (slot >= capacity) {
            atomicStore(&output.overflow, 1u);
            return;
        }
        let reservation = atomicCompareExchangeWeak(&output.count, slot, slot + 1u);
        if (reservation.exchanged) {
            output.pairs[slot] = pair;
            return;
        }
        slot = reservation.old_value;
    }
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let query_index = params.query_start + invocation.x;
    if (invocation.x >= params.query_count || query_index >= arrayLength(&queries)) {
        return;
    }
    let query = queries[query_index];
    let padding = vec3<f32>(max(params.clearance_mm, 0.0));
    let lo = query.lo.xyz - padding;
    let hi = query.hi.xyz + padding;
    let node_count = min(params.node_count, arrayLength(&nodes));
    var i = 0u;
    for (var step = 0u; step < node_count && i < node_count; step += 1u) {
        let node = nodes[i];
        if (!all(lo <= node.hi.xyz) || !all(node.lo.xyz <= hi)) {
            i = node.links.x;
        } else if (node.links.y == NO_TRIANGLE) {
            i += 1u;
        } else {
            if (node.links.y < arrayLength(&triangle_ids)) {
                let other = triangle_ids[node.links.y];
                if (query.ids.x < other.x) {
                    write_candidate(vec4<u32>(query.ids.z, other.z, query.ids.y, other.y));
                }
            }
            i = node.links.x;
        }
    }
}
"#;

/// Pipeline B ray traversal and per-face reduction.
///
/// For each ray page, zero binding 4 and dispatch `main` against each BVH
/// and triangle page in sequence. Every dispatch updates the same page-local
/// results, retaining the closest hit across the geometry pages. Dispatch
/// `aggregate` once per sample page after traversal. Each sample references
/// a contiguous range of rays wholly inside the bound ray page. Workgroups
/// have 64 threads in both entry points. Cap a sample page at 4,194,240 samples
/// for adapters with the default 65,535 workgroups per dispatch dimension.
///
/// Group 0 bindings are the 48-byte uniform, rays, preorder LBVH nodes,
/// triangles, mutable ray results, samples, and mutable face stats (0–6).
/// Bind only the resources used by the chosen entry point when using an
/// automatically derived pipeline layout. Bind a face page that includes
/// the dense page-local indices referenced by the samples. Initialize the
/// first two words of each face record to `u32::MAX` and the rest to zero.
///
/// Ray metadata is `(source_triangle_id, source_instance_id, mode, 0)`.
/// Modes are thickness (0), tool (1), positive pull (2), and negative pull
/// (3). Generate outward tool rays only. If tool axes are configured but a
/// sample has no outward axis, set bit 0 of its sample flags. The ray
/// result stores `(distance_bits, hit_triangle_id, hit_face_slot_id, hit)`.
/// Face stats store minimum thickness in micrometers, minimum signed draft
/// in hundredths of a degree biased by 18000, and counts of inaccessible
/// samples, undercuts, and total samples. Normals follow outward winding.
pub const RAYBEAM_DFM_WGSL: &str = r#"
const NO_TRIANGLE: u32 = 0xffffffffu;

struct Params {
    ray_count: u32,
    sample_count: u32,
    node_count: u32,
    triangle_count: u32,
    pull_dir: vec4<f32>,
    max_distance_mm: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

struct Node {
    lo: vec4<f32>,
    hi: vec4<f32>,
    links: vec4<u32>,
};

struct Triangle {
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
    ids: vec4<u32>,  // instance_id, face_slot_id, triangle_id, padding
};

struct Ray {
    origin: vec4<f32>,
    direction: vec4<f32>,
    normal: vec4<f32>,
    ids: vec4<u32>,
};

struct Sample {
    normal: vec4<f32>,
    info: vec4<u32>,  // first_ray, ray_count, dense face index, flags
};

struct FaceStats {
    min_thickness_um: atomic<u32>,
    min_draft_cdeg_bias: atomic<u32>,
    inaccessible_samples: atomic<u32>,
    undercut_samples: atomic<u32>,
    total_samples: atomic<u32>,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> rays: array<Ray>;
@group(0) @binding(2) var<storage, read> nodes: array<Node>;
@group(0) @binding(3) var<storage, read> triangles: array<Triangle>;
@group(0) @binding(4) var<storage, read_write> ray_results: array<vec4<u32>>;
@group(0) @binding(5) var<storage, read> samples: array<Sample>;
@group(0) @binding(6) var<storage, read_write> face_stats: array<FaceStats>;

fn ray_enters(origin: vec3<f32>, direction: vec3<f32>, node: Node, limit: f32) -> bool {
    var near_t = 0.0;
    var far_t = limit;
    for (var axis = 0u; axis < 3u; axis += 1u) {
        let d = direction[axis];
        if (d == 0.0) {
            if (origin[axis] < node.lo[axis] || origin[axis] > node.hi[axis]) {
                return false;
            }
        } else {
            let t0 = (node.lo[axis] - origin[axis]) / d;
            let t1 = (node.hi[axis] - origin[axis]) / d;
            near_t = max(near_t, min(t0, t1));
            far_t = min(far_t, max(t0, t1));
            if (near_t > far_t) {
                return false;
            }
        }
    }
    return true;
}

fn hit_distance(origin: vec3<f32>, direction: vec3<f32>, triangle: Triangle) -> f32 {
    let ab = triangle.b.xyz - triangle.a.xyz;
    let ac = triangle.c.xyz - triangle.a.xyz;
    let p = cross(direction, ac);
    let determinant = dot(ab, p);
    if (determinant == 0.0) {
        return -1.0;
    }
    let from_a = origin - triangle.a.xyz;
    let u = dot(from_a, p) / determinant;
    let q = cross(from_a, ab);
    let v = dot(direction, q) / determinant;
    if (u < 0.0 || v < 0.0 || u + v > 1.0) {
        return -1.0;
    }
    return dot(ac, q) / determinant;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let index = invocation.x;
    if (index >= min(params.ray_count, min(arrayLength(&rays), arrayLength(&ray_results)))) {
        return;
    }
    let ray = rays[index];
    if (all(ray.direction.xyz == vec3<f32>(0.0))) {
        return;
    }
    let direction = normalize(ray.direction.xyz);
    var best = ray_results[index];
    var limit = select(params.max_distance_mm, bitcast<f32>(best.x), best.w != 0u);
    let node_count = min(params.node_count, arrayLength(&nodes));
    let triangle_count = min(params.triangle_count, arrayLength(&triangles));
    var i = 0u;
    for (var step = 0u; step < node_count && i < node_count; step += 1u) {
        let node = nodes[i];
        if (!ray_enters(ray.origin.xyz, direction, node, limit)) {
            i = node.links.x;
        } else if (node.links.y == NO_TRIANGLE) {
            i += 1u;
        } else {
            if (node.links.y < triangle_count) {
                let triangle = triangles[node.links.y];
                if (triangle.ids.z != ray.ids.x && triangle.ids.x == ray.ids.y) {
                    let normal = cross(triangle.b.xyz - triangle.a.xyz, triangle.c.xyz - triangle.a.xyz);
                    if (ray.ids.z != 0u || dot(normal, ray.normal.xyz) < 0.0) {
                        let distance = hit_distance(ray.origin.xyz, direction, triangle);
                        let closer = distance < limit || best.w == 0u || triangle.ids.z < best.y;
                        if (distance > 0.0 && distance <= limit && closer) {
                            limit = distance;
                            best = vec4<u32>(bitcast<u32>(distance), triangle.ids.z, triangle.ids.y, 1u);
                        }
                    }
                }
            }
            i = node.links.x;
        }
    }
    ray_results[index] = best;
}

@compute @workgroup_size(64)
fn aggregate(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let index = invocation.x;
    if (index >= min(params.sample_count, arrayLength(&samples))) {
        return;
    }
    let sample = samples[index];
    let face = sample.info.z;
    if (face >= arrayLength(&face_stats)) {
        return;
    }

    var tool_tested = (sample.info.w & 1u) != 0u;
    var tool_clear = false;
    var pull_positive_blocked = false;
    var pull_negative_blocked = false;
    let ray_count = min(params.ray_count, min(arrayLength(&rays), arrayLength(&ray_results)));
    for (var j = 0u; j < sample.info.y && sample.info.x + j < ray_count; j += 1u) {
        let ray_index = sample.info.x + j;
        let hit = ray_results[ray_index];
        switch rays[ray_index].ids.z {
            case 0u: {
                if (hit.w != 0u) {
                    let um = u32(min(round(bitcast<f32>(hit.x) * 1000.0), 4294967040.0));
                    atomicMin(&face_stats[face].min_thickness_um, um);
                }
            }
            case 1u: {
                tool_tested = true;
                tool_clear = tool_clear || hit.w == 0u;
            }
            case 2u: { pull_positive_blocked = pull_positive_blocked || hit.w != 0u; }
            case 3u: { pull_negative_blocked = pull_negative_blocked || hit.w != 0u; }
            default: {}
        }
    }
    if (tool_tested && !tool_clear) {
        atomicAdd(&face_stats[face].inaccessible_samples, 1u);
    }
    if (pull_positive_blocked && pull_negative_blocked) {
        atomicAdd(&face_stats[face].undercut_samples, 1u);
    }
    if (any(params.pull_dir.xyz != vec3<f32>(0.0))) {
        let cosine = dot(normalize(sample.normal.xyz), normalize(params.pull_dir.xyz));
        let draft = degrees(asin(clamp(cosine, -1.0, 1.0)));
        atomicMin(&face_stats[face].min_draft_cdeg_bias, u32(round(draft * 100.0) + 18000.0));
    }
    atomicAdd(&face_stats[face].total_samples, 1u);
}
"#;

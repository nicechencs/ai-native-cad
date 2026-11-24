//! Deterministic 1-pixel picking and section-plane / render-pass configuration
//! over `CompactScene`. Works in both GPU (`wgpu-renderer`) and headless CPU modes.

use crate::camera::{Camera, ProjectionMode};
use crate::compact_scene::{CompactScene, PartPrototype, SceneInstance};
use cad_kernel_occt::Mat4;
use cad_syntax::Span;
use serde::{Deserialize, Serialize};

const EDGE_PICK_RADIUS_PX: f64 = 1.5;
const EDGE_DEPTH_BIAS_MM: f64 = 1e-2;
const RAY_EPSILON: f64 = 1e-9;

/// World-space clipping plane (`a*x + b*y + c*z + d > 0` is clipped away)
/// with optional hardware stencil capping and 45° ISO hatch lines.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SectionPlane {
    /// Plane equation `[a, b, c, d]` in world millimetres (`dot(n, p) + d > 0` is clipped).
    pub equation: [f64; 4],
    /// Enables hardware two-sided stencil capping over the sliced solid interior.
    pub enable_stencil_cap: bool,
    /// Draws 45° ISO mechanical hatch lines on the stencil-capped section face.
    pub show_cap_hatch: bool,
}

impl SectionPlane {
    /// Creates a section plane from `[a, b, c, d]` with stencil capping and hatch enabled.
    pub fn from_equation(equation: [f64; 4]) -> Self {
        Self {
            equation,
            enable_stencil_cap: true,
            show_cap_hatch: true,
        }
    }

    /// Creates a section plane where `dot(point, normal) > offset_mm` is clipped away.
    pub fn from_normal_offset(normal: [f64; 3], offset_mm: f64) -> Self {
        Self::from_equation([normal[0], normal[1], normal[2], -offset_mm])
    }

    pub fn with_stencil_cap(mut self, enabled: bool) -> Self {
        self.enable_stencil_cap = enabled;
        self
    }

    pub fn with_cap_hatch(mut self, enabled: bool) -> Self {
        self.show_cap_hatch = enabled;
        self
    }

    /// Normalized plane normal pointing toward the clipped half-space.
    pub fn normal(&self) -> [f64; 3] {
        let n = [self.equation[0], self.equation[1], self.equation[2]];
        let len = norm3(n);
        if len < 1e-12 {
            [0.0, 0.0, 1.0]
        } else {
            [n[0] / len, n[1] / len, n[2] / len]
        }
    }

    /// Signed offset along [`Self::normal`] where the plane sits (`dot(n, p) = offset_mm`).
    pub fn offset_mm(&self) -> f64 {
        let len = norm3([self.equation[0], self.equation[1], self.equation[2]]);
        if len < 1e-12 {
            0.0
        } else {
            -self.equation[3] / len
        }
    }

    /// Signed distance from `world_point_mm` to the plane; positive values are clipped.
    pub fn signed_distance_mm(&self, world_point_mm: [f64; 3]) -> f64 {
        let n = [self.equation[0], self.equation[1], self.equation[2]];
        let len = norm3(n);
        if len < 1e-12 {
            return 0.0;
        }
        (n[0] * world_point_mm[0]
            + n[1] * world_point_mm[1]
            + n[2] * world_point_mm[2]
            + self.equation[3])
            / len
    }

    /// Returns `true` if `world_point_mm` lies on the clipped side of the plane.
    pub fn is_clipped(&self, world_point_mm: [f64; 3]) -> bool {
        self.signed_distance_mm(world_point_mm) > 1e-7
    }
}

/// Controls which topological primitives are eligible for 1-pixel picking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PickTargetMode {
    #[default]
    FacesAndEdges,
    FacesOnly,
    EdgesOnly,
}

/// Unified render pass & picking configuration shared by GPU and CPU paths.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RenderPassOptions {
    pub section_plane: Option<SectionPlane>,
    pub show_edges: bool,
    pub pick_target_mode: PickTargetMode,
}

impl Default for RenderPassOptions {
    fn default() -> Self {
        Self {
            section_plane: None,
            show_edges: true,
            pick_target_mode: PickTargetMode::FacesAndEdges,
        }
    }
}

impl RenderPassOptions {
    pub fn with_section_plane(mut self, plane: SectionPlane) -> Self {
        self.section_plane = Some(plane);
        self
    }

    pub fn with_edges(mut self, show_edges: bool) -> Self {
        self.show_edges = show_edges;
        self
    }

    pub fn with_pick_target_mode(mut self, mode: PickTargetMode) -> Self {
        self.pick_target_mode = mode;
        self
    }
}

/// Result of a 1-pixel face or edge pick query on a [`CompactScene`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PickHit {
    pub instance_index: u32,
    pub prototype_index: u32,
    pub face_slot_id: Option<u32>,
    pub face_id: String,
    pub edge_slot_id: Option<u32>,
    pub world_point_mm: [f64; 3],
    /// Linear distance along the viewing ray in millimetres.
    pub depth: f64,
    pub ast_span: Span,
    pub is_edge: bool,
}

/// World-space viewing ray generated for a viewport pixel `(x, y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickRay {
    pub origin_mm: [f64; 3],
    pub direction: [f64; 3],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CameraBasis {
    pub eye_mm: [f64; 3],
    pub forward: [f64; 3],
    pub side: [f64; 3],
    pub lifted: [f64; 3],
}

pub(crate) fn camera_basis(camera: &Camera) -> CameraBasis {
    let eye_mm = camera.eye_mm();
    let to_target = [
        camera.target_mm[0] - eye_mm[0],
        camera.target_mm[1] - eye_mm[1],
        camera.target_mm[2] - eye_mm[2],
    ];
    let forward = normalize3(to_target);
    let up = if forward[2].abs() > 0.95 {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    let side = normalize3(cross3(forward, up));
    let lifted = cross3(side, forward);
    CameraBasis {
        eye_mm,
        forward,
        side,
        lifted,
    }
}

/// Generates the world-space viewing ray passing through pixel center `(x + 0.5, y + 0.5)`.
pub fn pixel_ray(camera: &Camera, width: u32, height: u32, x: u32, y: u32) -> Option<PickRay> {
    if width == 0 || height == 0 || x >= width || y >= height {
        return None;
    }
    let basis = camera_basis(camera);
    let aspect = f64::from(width) / f64::from(height);
    let ndc_x = ((f64::from(x) + 0.5) / f64::from(width)) * 2.0 - 1.0;
    let ndc_y = 1.0 - ((f64::from(y) + 0.5) / f64::from(height)) * 2.0;

    match camera.projection {
        ProjectionMode::Orthographic => {
            let half_h = 0.5 * camera.ortho_height_mm.max(1e-6);
            let half_w = half_h * aspect;
            let origin_mm = add3(
                basis.eye_mm,
                add3(
                    scale3(basis.side, ndc_x * half_w),
                    scale3(basis.lifted, ndc_y * half_h),
                ),
            );
            Some(PickRay {
                origin_mm,
                direction: basis.forward,
            })
        }
        ProjectionMode::Perspective => {
            let fov = camera.fov_y_deg.clamp(1.0, 175.0).to_radians();
            let half_h = (0.5 * fov).tan();
            let half_w = half_h * aspect;
            let dir = normalize3(add3(
                basis.forward,
                add3(
                    scale3(basis.side, ndc_x * half_w),
                    scale3(basis.lifted, ndc_y * half_h),
                ),
            ));
            Some(PickRay {
                origin_mm: basis.eye_mm,
                direction: dir,
            })
        }
    }
}

/// Projects a world-space point (mm) into viewport pixel coordinates `(x, y)` (top-left origin).
pub fn project_world_point_to_pixel(
    camera: &Camera,
    width: u32,
    height: u32,
    world_point_mm: [f64; 3],
) -> Option<(u32, u32)> {
    let (px, py, _depth) = project_world_point_subpixel(camera, width, height, world_point_mm)?;
    let ix = px.floor() as i64;
    let iy = py.floor() as i64;
    if ix < 0 || iy < 0 || ix >= i64::from(width) || iy >= i64::from(height) {
        None
    } else {
        Some((ix as u32, iy as u32))
    }
}

pub(crate) fn project_world_point_subpixel(
    camera: &Camera,
    width: u32,
    height: u32,
    world_point_mm: [f64; 3],
) -> Option<(f64, f64, f64)> {
    if width == 0 || height == 0 {
        return None;
    }
    let basis = camera_basis(camera);
    let rel = sub3(world_point_mm, basis.eye_mm);
    let vx = dot3(rel, basis.side);
    let vy = dot3(rel, basis.lifted);
    let depth = dot3(rel, basis.forward);
    if depth <= 0.0 {
        return None;
    }
    let aspect = f64::from(width) / f64::from(height);
    let (ndc_x, ndc_y) = match camera.projection {
        ProjectionMode::Orthographic => {
            let half_h = 0.5 * camera.ortho_height_mm.max(1e-6);
            let half_w = half_h * aspect;
            (vx / half_w, vy / half_h)
        }
        ProjectionMode::Perspective => {
            let fov = camera.fov_y_deg.clamp(1.0, 175.0).to_radians();
            let half_h = (0.5 * fov).tan() * depth;
            let half_w = half_h * aspect;
            (vx / half_w, vy / half_h)
        }
    };
    if ndc_x.abs() > 1.0 + 1e-6 || ndc_y.abs() > 1.0 + 1e-6 {
        return None;
    }
    let px = (ndc_x * 0.5 + 0.5) * f64::from(width);
    let py = (0.5 - ndc_y * 0.5) * f64::from(height);
    Some((px, py, depth))
}

#[cfg(feature = "wgpu-renderer")]
pub(crate) fn unproject_pixel_ndc_depth(
    camera: &Camera,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    ndc_z: f64,
    near: f64,
    far: f64,
) -> Option<([f64; 3], f64)> {
    let ray = pixel_ray(camera, width, height, x, y)?;
    let z = ndc_z.clamp(0.0, 1.0);
    let basis = camera_basis(camera);
    let view_depth = match camera.projection {
        ProjectionMode::Orthographic => near + z * (far - near),
        ProjectionMode::Perspective => {
            let denom = far - z * (far - near);
            if denom.abs() < 1e-9 {
                far
            } else {
                (near * far) / denom
            }
        }
    };
    let cos_theta = dot3(ray.direction, basis.forward).max(1e-6);
    let ray_t = view_depth / cos_theta;
    let world_point_mm = add3(ray.origin_mm, scale3(ray.direction, ray_t));
    Some((world_point_mm, ray_t))
}

/// Deterministic 1-pixel pick query over [`CompactScene`] prototypes and instances
/// using default [`RenderPassOptions`] (faces & edges, no section plane).
pub fn pick_pixel(
    scene: &CompactScene,
    camera: &Camera,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
) -> Option<PickHit> {
    pick_pixel_with_options(
        scene,
        camera,
        width,
        height,
        x,
        y,
        &RenderPassOptions::default(),
    )
}

/// Deterministic 1-pixel pick query over [`CompactScene`] respecting [`SectionPlane`],
/// edge visibility, and [`PickTargetMode`].
pub fn pick_pixel_with_options(
    scene: &CompactScene,
    camera: &Camera,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    options: &RenderPassOptions,
) -> Option<PickHit> {
    let ray = pixel_ray(camera, width, height, x, y)?;
    let proto_bounds: Vec<Option<([f64; 3], [f64; 3])>> = scene
        .prototypes
        .iter()
        .map(prototype_local_bounds)
        .collect();

    let mut best_face: Option<PickHit> = None;

    // Even in EdgesOnly mode we still evaluate face depth to avoid picking occluded back edges.
    for (instance_idx, instance) in scene.instances.iter().enumerate() {
        let proto_idx = instance.prototype_index as usize;
        let Some(prototype) = scene.prototypes.get(proto_idx) else {
            continue;
        };
        if prototype.vertices.is_empty() || prototype.indices.is_empty() {
            continue;
        }
        let rot = Mat4::rotate_euler_deg(
            instance.rotation_deg[0],
            instance.rotation_deg[1],
            instance.rotation_deg[2],
        );
        let local_ray = ray_to_anchor_local(ray, instance, prototype.anchor_mm, &rot);
        if let Some((bmin, bmax)) = proto_bounds[proto_idx] {
            let max_t = best_face.as_ref().map_or(f64::INFINITY, |hit| hit.depth);
            if !ray_intersects_aabb(local_ray, bmin, bmax, max_t) {
                continue;
            }
        }

        for tri in prototype.indices.chunks_exact(3) {
            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let (Some(v0), Some(v1), Some(v2)) = (
                prototype.vertices.get(i0),
                prototype.vertices.get(i1),
                prototype.vertices.get(i2),
            ) else {
                continue;
            };
            let p0 = [
                f64::from(v0.position[0]),
                f64::from(v0.position[1]),
                f64::from(v0.position[2]),
            ];
            let p1 = [
                f64::from(v1.position[0]),
                f64::from(v1.position[1]),
                f64::from(v1.position[2]),
            ];
            let p2 = [
                f64::from(v2.position[0]),
                f64::from(v2.position[1]),
                f64::from(v2.position[2]),
            ];
            let Some(t) = ray_triangle_intersect(local_ray, p0, p1, p2) else {
                continue;
            };
            if t < 0.0 || best_face.as_ref().is_some_and(|hit| t >= hit.depth) {
                continue;
            }
            let local_hit = add3(local_ray.origin_mm, scale3(local_ray.direction, t));
            let world_point_mm = anchor_local_to_world(local_hit, instance, prototype.anchor_mm, &rot);
            if options
                .section_plane
                .is_some_and(|plane| plane.is_clipped(world_point_mm))
            {
                continue;
            }
            let face_slot = v0.face_slot_id;
            let Some(face_rec) = prototype.face_table.get(face_slot as usize) else {
                continue;
            };
            best_face = Some(PickHit {
                instance_index: instance_idx as u32,
                prototype_index: instance.prototype_index,
                face_slot_id: Some(face_slot),
                face_id: face_rec.face_id.clone(),
                edge_slot_id: None,
                world_point_mm,
                depth: t,
                ast_span: face_rec.ast_span,
                is_edge: false,
            });
        }
    }

    let allow_edges = options.show_edges
        && matches!(
            options.pick_target_mode,
            PickTargetMode::FacesAndEdges | PickTargetMode::EdgesOnly
        );
    let mut best_edge: Option<(f64, PickHit)> = None;
    if allow_edges {
        let target_px = [f64::from(x) + 0.5, f64::from(y) + 0.5];
        let face_depth_limit = best_face
            .as_ref()
            .map_or(f64::INFINITY, |hit| hit.depth + EDGE_DEPTH_BIAS_MM);
        for (instance_idx, instance) in scene.instances.iter().enumerate() {
            let proto_idx = instance.prototype_index as usize;
            let Some(prototype) = scene.prototypes.get(proto_idx) else {
                continue;
            };
            if prototype.edges.is_empty() {
                continue;
            }
            let rot = Mat4::rotate_euler_deg(
                instance.rotation_deg[0],
                instance.rotation_deg[1],
                instance.rotation_deg[2],
            );
            for segment in &prototype.edges {
                let a_local = [
                    f64::from(segment.point_a[0]),
                    f64::from(segment.point_a[1]),
                    f64::from(segment.point_a[2]),
                ];
                let b_local = [
                    f64::from(segment.point_b[0]),
                    f64::from(segment.point_b[1]),
                    f64::from(segment.point_b[2]),
                ];
                let a_world = anchor_local_to_world(a_local, instance, prototype.anchor_mm, &rot);
                let b_world = anchor_local_to_world(b_local, instance, prototype.anchor_mm, &rot);
                let (Some((ax, ay, az)), Some((bx, by, bz))) = (
                    project_world_point_subpixel(camera, width, height, a_world),
                    project_world_point_subpixel(camera, width, height, b_world),
                ) else {
                    continue;
                };
                let (dist_px, seg_t) =
                    point_to_segment_2d(target_px, [ax, ay], [bx, by]);
                if dist_px > EDGE_PICK_RADIUS_PX {
                    continue;
                }
                let world_point_mm = lerp3(a_world, b_world, seg_t);
                if options
                    .section_plane
                    .is_some_and(|plane| plane.is_clipped(world_point_mm))
                {
                    continue;
                }
                let depth = az + (bz - az) * seg_t;
                if depth > face_depth_limit {
                    continue;
                }
                let is_better = best_edge.as_ref().is_none_or(|(best_dist, best_hit)| {
                    depth + 1e-4 < best_hit.depth
                        || ((depth - best_hit.depth).abs() <= 1e-4 && dist_px < *best_dist)
                });
                if !is_better {
                    continue;
                }
                let Some(edge_rec) = prototype.edge_table.get(segment.edge_slot_id as usize) else {
                    continue;
                };
                best_edge = Some((
                    dist_px,
                    PickHit {
                        instance_index: instance_idx as u32,
                        prototype_index: instance.prototype_index,
                        face_slot_id: None,
                        face_id: edge_rec.edge_id.clone(),
                        edge_slot_id: Some(segment.edge_slot_id),
                        world_point_mm,
                        depth,
                        ast_span: edge_rec.ast_span,
                        is_edge: true,
                    },
                ));
            }
        }
    }

    match options.pick_target_mode {
        PickTargetMode::FacesOnly => best_face,
        PickTargetMode::EdgesOnly => best_edge.map(|(_, hit)| hit),
        PickTargetMode::FacesAndEdges => best_edge.map(|(_, hit)| hit).or(best_face),
    }
}

fn prototype_local_bounds(prototype: &PartPrototype) -> Option<([f64; 3], [f64; 3])> {
    let first = prototype.vertices.first()?;
    let mut min = [
        f64::from(first.position[0]),
        f64::from(first.position[1]),
        f64::from(first.position[2]),
    ];
    let mut max = min;
    for v in &prototype.vertices[1..] {
        for axis in 0..3 {
            let val = f64::from(v.position[axis]);
            if val < min[axis] {
                min[axis] = val;
            }
            if val > max[axis] {
                max[axis] = val;
            }
        }
    }
    Some((min, max))
}

fn ray_to_anchor_local(
    ray: PickRay,
    instance: &SceneInstance,
    anchor_mm: [f64; 3],
    rot: &Mat4,
) -> PickRay {
    let shifted = sub3(ray.origin_mm, instance.origin_mm);
    let local_origin_part = mul_transpose3(rot, shifted);
    let origin_mm = sub3(local_origin_part, anchor_mm);
    let direction = mul_transpose3(rot, ray.direction);
    PickRay {
        origin_mm,
        direction,
    }
}

fn anchor_local_to_world(
    local_pos: [f64; 3],
    instance: &SceneInstance,
    anchor_mm: [f64; 3],
    rot: &Mat4,
) -> [f64; 3] {
    let part_pos = add3(local_pos, anchor_mm);
    let rotated = mul_mat3(rot, part_pos);
    add3(rotated, instance.origin_mm)
}

fn ray_intersects_aabb(ray: PickRay, min: [f64; 3], max: [f64; 3], max_t: f64) -> bool {
    let mut t_min = 0.0_f64;
    let mut t_max = max_t;
    for axis in 0..3 {
        let dir = ray.direction[axis];
        if dir.abs() < 1e-12 {
            if ray.origin_mm[axis] < min[axis] - 1e-6 || ray.origin_mm[axis] > max[axis] + 1e-6 {
                return false;
            }
        } else {
            let inv = 1.0 / dir;
            let mut t0 = (min[axis] - ray.origin_mm[axis]) * inv;
            let mut t1 = (max[axis] - ray.origin_mm[axis]) * inv;
            if t0 > t1 {
                std::mem::swap(&mut t0, &mut t1);
            }
            t_min = t_min.max(t0);
            t_max = t_max.min(t1);
            if t_max < t_min {
                return false;
            }
        }
    }
    true
}

fn ray_triangle_intersect(
    ray: PickRay,
    v0: [f64; 3],
    v1: [f64; 3],
    v2: [f64; 3],
) -> Option<f64> {
    let e1 = sub3(v1, v0);
    let e2 = sub3(v2, v0);
    let pvec = cross3(ray.direction, e2);
    let det = dot3(e1, pvec);
    if det.abs() < RAY_EPSILON {
        return None;
    }
    let inv_det = 1.0 / det;
    let tvec = sub3(ray.origin_mm, v0);
    let u = dot3(tvec, pvec) * inv_det;
    if !(-1e-6..=1.0 + 1e-6).contains(&u) {
        return None;
    }
    let qvec = cross3(tvec, e1);
    let v = dot3(ray.direction, qvec) * inv_det;
    if v < -1e-6 || u + v > 1.0 + 1e-6 {
        return None;
    }
    let t = dot3(e2, qvec) * inv_det;
    if t >= 0.0 { Some(t) } else { None }
}

fn point_to_segment_2d(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, f64) {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len_sq = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len_sq < 1e-12 {
        0.0
    } else {
        (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len_sq).clamp(0.0, 1.0)
    };
    let proj = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    let dx = p[0] - proj[0];
    let dy = p[1] - proj[1];
    ((dx * dx + dy * dy).sqrt(), t)
}

fn mul_mat3(m: &Mat4, v: [f64; 3]) -> [f64; 3] {
    [
        m.m[0][0] * v[0] + m.m[0][1] * v[1] + m.m[0][2] * v[2],
        m.m[1][0] * v[0] + m.m[1][1] * v[1] + m.m[1][2] * v[2],
        m.m[2][0] * v[0] + m.m[2][1] * v[1] + m.m[2][2] * v[2],
    ]
}

fn mul_transpose3(m: &Mat4, v: [f64; 3]) -> [f64; 3] {
    [
        m.m[0][0] * v[0] + m.m[1][0] * v[1] + m.m[2][0] * v[2],
        m.m[0][1] * v[0] + m.m[1][1] * v[1] + m.m[2][1] * v[2],
        m.m[0][2] * v[0] + m.m[1][2] * v[1] + m.m[2][2] * v[2],
    ]
}

fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale3(v: [f64; 3], s: f64) -> [f64; 3] {
    [v[0] * s, v[1] * s, v[2] * s]
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm3(v: [f64; 3]) -> f64 {
    dot3(v, v).sqrt()
}

fn normalize3(v: [f64; 3]) -> [f64; 3] {
    let len = norm3(v);
    if len < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

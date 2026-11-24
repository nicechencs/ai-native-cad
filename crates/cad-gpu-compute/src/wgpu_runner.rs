//! Headless WebGPU execution for the Morton and ray-beam DFM passes.

use std::{collections::BTreeMap, sync::mpsc};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::{
    DfmConfig, DfmReport, DfmViolationKind, FaceDfmMetrics, LbvhTree, TriRef,
    dfm_raybeam::select_face_samples,
    lbvh::{cross, dot, sub},
    morton::morton_3d,
    shaders::{LBVH_BROADPHASE_WGSL, RAYBEAM_DFM_WGSL},
};
pub use crate::backend::{GpuComputeBackend, GpuComputeError};

const WORKGROUP_SIZE: u32 = 64;
const MORTON_BINS: usize = 1024;
// The aggregation shader stores unsigned micrometers with a reserved sentinel.
const MAX_DFM_DISTANCE_MM: f32 = 4_294_966.0;

#[derive(Debug)]
pub struct GpuComputeContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: wgpu::AdapterInfo,
    morton: wgpu::ComputePipeline,
    _broadphase: wgpu::ComputePipeline,
    dfm_trace: wgpu::ComputePipeline,
    dfm_aggregate: wgpu::ComputePipeline,
}

impl GpuComputeBackend {
    pub(crate) fn from_context_result(result: Result<GpuComputeContext, GpuComputeError>) -> Self {
        match result {
            Ok(context) => Self::Wgpu(context),
            Err(error) => {
                warn_gpu_fallback(
                    "auto_backend",
                    gpu_error_type(&error),
                    format!("WebGPU backend unavailable ({error}); switching to CPU fallback"),
                );
                Self::CpuFallback
            }
        }
    }
}

impl GpuComputeContext {
    pub fn try_new() -> Result<Self, GpuComputeError> {
        pollster::block_on(async {
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
            let mut last_error = GpuComputeError::NoAdapter;
            for force_fallback_adapter in [false, true] {
                let Some(adapter) = instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::HighPerformance,
                        force_fallback_adapter,
                        compatible_surface: None,
                    })
                    .await
                else {
                    continue;
                };
                warn_if_fallback_adapter(&adapter.get_info(), force_fallback_adapter);
                match Self::from_adapter(adapter).await {
                    Ok(context) => return Ok(context),
                    Err(error) => last_error = error,
                }
            }
            Err(last_error)
        })
    }

    async fn from_adapter(adapter: wgpu::Adapter) -> Result<Self, GpuComputeError> {
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("cad-gpu-compute"),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                    ..Default::default()
                },
                None,
            )
            .await?;

        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let morton = shader(&device, "Morton", MORTON_WGSL);
        let broadphase = shader(&device, "LBVH broad phase", LBVH_BROADPHASE_WGSL);
        let dfm = shader(&device, "Ray-beam DFM", RAYBEAM_DFM_WGSL);
        let morton = pipeline(&device, &morton, "main");
        let broadphase = pipeline(&device, &broadphase, "main");
        let dfm_trace = pipeline(&device, &dfm, "main");
        let dfm_aggregate = pipeline(&device, &dfm, "aggregate");
        let validation_error = device.pop_error_scope().await;
        let memory_error = device.pop_error_scope().await;
        let internal_error = device.pop_error_scope().await;
        if let Some(error) = validation_error.or(memory_error).or(internal_error) {
            return Err(GpuComputeError::Pipeline(error));
        }

        Ok(Self {
            device,
            queue,
            adapter: adapter.get_info(),
            morton,
            _broadphase: broadphase,
            dfm_trace,
            dfm_aggregate,
        })
    }

    pub fn adapter_info(&self) -> String {
        format!(
            "{} ({:?}, {:?})",
            self.adapter.name, self.adapter.backend, self.adapter.device_type
        )
    }

    pub fn is_software_adapter(&self) -> bool {
        self.adapter.device_type == wgpu::DeviceType::Cpu
    }

    pub fn compute_morton_codes(
        &self,
        centroids: &[[f32; 3]],
        scene_min: [f32; 3],
        scene_max: [f32; 3],
    ) -> Result<Vec<u32>, GpuComputeError> {
        if centroids.is_empty() {
            return Ok(Vec::new());
        }

        let thresholds = morton_thresholds(scene_min, scene_max);
        let thresholds = self.upload(
            "Morton thresholds",
            &thresholds,
            wgpu::BufferUsages::STORAGE,
        );
        let batch_size = self
            .dispatch_capacity()
            .min(self.storage_capacity::<[u32; 4]>());
        let mut codes = Vec::with_capacity(centroids.len());
        for batch in centroids.chunks(batch_size) {
            let points: Vec<_> = batch
                .iter()
                .map(|point| {
                    let [x, y, z] = point.map(f32::to_bits);
                    [x, y, z, 0]
                })
                .collect();
            let params = self.upload(
                "Morton parameters",
                &[[batch.len() as u32, 0, 0, 0]],
                wgpu::BufferUsages::UNIFORM,
            );
            let points = self.upload("Morton centroids", &points, wgpu::BufferUsages::STORAGE);
            let output = self.buffer(
                "Morton codes",
                byte_size::<u32>(batch.len()),
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            );
            let bindings = self.bind(
                &self.morton,
                &[(0, &params), (1, &points), (2, &thresholds), (3, &output)],
            );
            codes.extend(self.readback::<u32>(&output, batch.len(), |encoder| {
                dispatch(encoder, &self.morton, &bindings, batch.len());
            })?);
        }
        Ok(codes)
    }

    /// The GPU kernel reports thickness in micrometers and draft in hundredths
    /// of a degree. Violation thresholds are applied to those reported values.
    pub fn analyze_dfm_raybeams(
        &self,
        triangles: &[TriRef],
        config: &DfmConfig,
    ) -> Result<DfmReport, GpuComputeError> {
        if triangles.is_empty() {
            return Ok(DfmReport::default());
        }

        let triangle_capacity = self
            .storage_capacity::<GpuTriangle>()
            .min(self.storage_capacity::<GpuNode>().div_ceil(2));
        if triangles.len() > triangle_capacity {
            return Err(GpuComputeError::WorkloadLimit);
        }
        let work = DfmWork::prepare(
            triangles,
            config,
            self.dispatch_capacity()
                .min(self.storage_capacity::<GpuRay>()),
            self.dispatch_capacity()
                .min(self.storage_capacity::<GpuSample>()),
            self.storage_capacity::<[u32; 8]>(),
        )?;
        if work.rays.is_empty() {
            return Ok(work.report(
                &vec![[u32::MAX, u32::MAX, 0, 0, 0, 0, 0, 0]; work.face_ids.len()],
                config,
            ));
        }
        let nodes: Vec<_> = LbvhTree::build(triangles)
            .gpu_nodes(work.origin)
            .ok_or(GpuComputeError::DfmGeometryRange)?
            .into_iter()
            .map(|node| GpuNode {
                lo: node.lo,
                hi: node.hi,
                links: node.links,
            })
            .collect();

        let params = self.upload(
            "DFM parameters",
            &[DfmParams {
                counts: [
                    work.rays.len() as u32,
                    work.samples.len() as u32,
                    nodes.len() as u32,
                    work.triangles.len() as u32,
                ],
                pull: vector4(normalize(config.pull_dir)),
                distance: [MAX_DFM_DISTANCE_MM, 0.0, 0.0, 0.0],
            }],
            wgpu::BufferUsages::UNIFORM,
        );
        let storage = wgpu::BufferUsages::STORAGE;
        let rays = self.upload("DFM rays", &work.rays, storage);
        let nodes = self.upload("DFM nodes", &nodes, storage);
        let triangles = self.upload("DFM triangles", &work.triangles, storage);
        let hits = self.buffer("DFM hits", byte_size::<[u32; 4]>(work.rays.len()), storage);
        let samples = self.upload("DFM samples", &work.samples, storage);
        let stats = self.upload(
            "DFM face stats",
            &vec![[u32::MAX, u32::MAX, 0, 0, 0, 0, 0, 0]; work.face_ids.len()],
            storage | wgpu::BufferUsages::COPY_SRC,
        );
        let trace = self.bind(
            &self.dfm_trace,
            &[
                (0, &params),
                (1, &rays),
                (2, &nodes),
                (3, &triangles),
                (4, &hits),
            ],
        );
        let aggregate = self.bind(
            &self.dfm_aggregate,
            &[
                (0, &params),
                (1, &rays),
                (4, &hits),
                (5, &samples),
                (6, &stats),
            ],
        );

        let stats = self.readback(&stats, work.face_ids.len(), |encoder| {
            dispatch(encoder, &self.dfm_trace, &trace, work.rays.len());
            dispatch(encoder, &self.dfm_aggregate, &aggregate, work.samples.len());
        })?;
        Ok(work.report(&stats, config))
    }

    fn dispatch_capacity(&self) -> usize {
        (self.device.limits().max_compute_workgroups_per_dimension * WORKGROUP_SIZE) as usize
    }

    fn storage_capacity<T>(&self) -> usize {
        self.device.limits().max_storage_buffer_binding_size as usize / size_of::<T>()
    }

    fn upload<T: Pod>(&self, label: &str, data: &[T], usage: wgpu::BufferUsages) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: bytemuck::cast_slice(data),
                usage,
            })
    }

    fn buffer(&self, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        })
    }

    fn bind(
        &self,
        pipeline: &wgpu::ComputePipeline,
        buffers: &[(u32, &wgpu::Buffer)],
    ) -> wgpu::BindGroup {
        let entries: Vec<_> = buffers
            .iter()
            .map(|&(binding, buffer)| wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        })
    }

    fn readback<T: Pod>(
        &self,
        output: &wgpu::Buffer,
        len: usize,
        encode: impl FnOnce(&mut wgpu::CommandEncoder),
    ) -> Result<Vec<T>, GpuComputeError> {
        let size = byte_size::<T>(len);
        let staging = self.buffer(
            "Compute readback",
            size,
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        );
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encode(&mut encoder);
        encoder.copy_buffer_to_buffer(output, 0, &staging, 0, size);
        let submission = self.queue.submit([encoder.finish()]);

        let (send, receive) = mpsc::channel();
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
        let _ = self.device.poll(wgpu::Maintain::wait_for(submission));
        receive
            .recv()
            .map_err(|_| GpuComputeError::ReadbackDropped)??;
        let data = slice.get_mapped_range();
        let result = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        staging.unmap();
        Ok(result)
    }
}

fn shader(device: &wgpu::Device, label: &str, source: &'static str) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

fn pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    entry: &str,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: shader,
        entry_point: Some(entry),
        compilation_options: Default::default(),
        cache: None,
    })
}

fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    bindings: &wgpu::BindGroup,
    count: usize,
) {
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bindings, &[]);
    pass.dispatch_workgroups((count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
}

fn byte_size<T>(count: usize) -> u64 {
    count as u64 * size_of::<T>() as u64
}

// WGSL has no portable f64 arithmetic. Find the first f32 key at each CPU
// quantization boundary, then let the GPU quantize and interleave using only
// integers. The sorted keys cover subnormals, both zero signs and infinities.
fn morton_thresholds(min: [f32; 3], max: [f32; 3]) -> Vec<[u32; 4]> {
    let mut thresholds = vec![[u32::MAX; 4]; MORTON_BINS - 1];
    for axis in 0..3 {
        if !min[axis].is_finite() || !max[axis].is_finite() || min[axis] >= max[axis] {
            continue;
        }
        let bounds_min = [f64::from(min[axis]), 0.0, 0.0];
        let bounds_max = [f64::from(max[axis]), 0.0, 0.0];
        let mut low = ordered_key(min[axis]);
        let last = ordered_key(max[axis]);
        for (bin, threshold) in thresholds.iter_mut().enumerate() {
            // Spreading the bits of a single axis preserves order.
            let target = morton_3d((bin + 1) as f64, 0.0, 0.0, [0.0; 3], [1024.0, 0.0, 0.0]);
            let mut high = last;
            while low < high {
                let mid = low + (high - low) / 2;
                let point = f64::from(from_ordered_key(mid));
                if morton_3d(point, 0.0, 0.0, bounds_min, bounds_max) < target {
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
            threshold[axis] = low;
        }
    }
    thresholds
}

fn ordered_key(value: f32) -> u32 {
    let bits = value.to_bits();
    if bits & 0x8000_0000 != 0 {
        !bits
    } else {
        bits ^ 0x8000_0000
    }
}

fn from_ordered_key(key: u32) -> f32 {
    let bits = if key & 0x8000_0000 != 0 {
        key ^ 0x8000_0000
    } else {
        !key
    };
    f32::from_bits(bits)
}

const MORTON_WGSL: &str = r#"
@group(0) @binding(0) var<uniform> params: vec4<u32>;
@group(0) @binding(1) var<storage, read> points: array<vec4<u32>>;
@group(0) @binding(2) var<storage, read> thresholds: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read_write> codes: array<u32>;

fn quantize(bits: u32, axis: u32) -> u32 {
    if ((bits & 0x7fffffffu) > 0x7f800000u) {
        return 0u;
    }
    let key = select(bits ^ 0x80000000u, ~bits, (bits & 0x80000000u) != 0u);
    var low = 0u;
    var high = arrayLength(&thresholds);
    while (low < high) {
        let mid = low + (high - low) / 2u;
        if (key >= thresholds[mid][axis]) {
            low = mid + 1u;
        } else {
            high = mid;
        }
    }
    return low;
}

fn spread(value: u32) -> u32 {
    let a = (value | (value << 16u)) & 0x030000ffu;
    let b = (a | (a << 8u)) & 0x0300f00fu;
    let c = (b | (b << 4u)) & 0x030c30c3u;
    return (c | (c << 2u)) & 0x09249249u;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let i = invocation.x;
    if (i < params.x) {
        let point = points[i];
        codes[i] = spread(quantize(point.x, 0u))
            | (spread(quantize(point.y, 1u)) << 1u)
            | (spread(quantize(point.z, 2u)) << 2u);
    }
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DfmParams {
    counts: [u32; 4],
    pull: [f32; 4],
    distance: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuNode {
    lo: [f32; 4],
    hi: [f32; 4],
    links: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuTriangle {
    a: [f32; 4],
    b: [f32; 4],
    c: [f32; 4],
    ids: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuRay {
    origin: [f32; 4],
    direction: [f32; 4],
    normal: [f32; 4],
    ids: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuSample {
    normal: [f32; 4],
    info: [u32; 4],
}

struct DfmWork {
    origin: [f64; 3],
    triangles: Vec<GpuTriangle>,
    face_ids: Vec<u32>,
    rays: Vec<GpuRay>,
    samples: Vec<GpuSample>,
}

impl DfmWork {
    fn prepare(
        triangles: &[TriRef],
        config: &DfmConfig,
        ray_limit: usize,
        sample_limit: usize,
        face_limit: usize,
    ) -> Result<Self, GpuComputeError> {
        let origin = dfm_origin(triangles)?;
        let geometry: Vec<_> = triangles.iter().map(triangle_geometry).collect();
        let mut slots: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
        let mut gpu_triangles = Vec::with_capacity(triangles.len());
        for (index, tri) in triangles.iter().enumerate() {
            slots.entry(tri.face_slot_id).or_default().push(index);
            gpu_triangles.push(GpuTriangle {
                a: vector4(sub(tri.a, origin)),
                b: vector4(sub(tri.b, origin)),
                c: vector4(sub(tri.c, origin)),
                ids: [tri.instance_id, tri.face_slot_id, index as u32, 0],
            });
        }
        if slots.len() > face_limit {
            return Err(GpuComputeError::WorkloadLimit);
        }

        let axes: Vec<_> = config
            .tool_axes
            .iter()
            .copied()
            .map(normalize)
            .filter(|axis| *axis != [0.0; 3])
            .collect();
        let draft_enabled = normalize(config.pull_dir) != [0.0; 3];
        let mut work = Self {
            origin,
            triangles: gpu_triangles,
            face_ids: Vec::with_capacity(slots.len()),
            rays: Vec::new(),
            samples: Vec::new(),
        };
        for (face_index, (face_id, indices)) in slots.into_iter().enumerate() {
            work.face_ids.push(face_id);
            for &index in &indices {
                let (normal, area) = geometry[index];
                if area == 0.0 {
                    continue;
                }
                // Draft covers every nondegenerate triangle, even if its face
                // has too few ray samples to visit it.
                if draft_enabled {
                    if work.samples.len() == sample_limit {
                        return Err(GpuComputeError::WorkloadLimit);
                    }
                    work.samples.push(GpuSample {
                        normal: vector4(normal),
                        info: [0, 0, face_index as u32, 0],
                    });
                }
            }
            let sampling = select_face_samples(triangles, &geometry, &indices, config.samples_per_face);
            if sampling.is_empty() {
                continue;
            }
            if sampling.len() > sample_limit - work.samples.len()
                || sampling.len() > ray_limit - work.rays.len()
            {
                return Err(GpuComputeError::WorkloadLimit);
            }
            for (index, (u, v)) in sampling {
                let tri = &triangles[index];
                let normal = geometry[index].0;
                let ab = sub(tri.b, tri.a);
                let ac = sub(tri.c, tri.a);
                let a = sub(tri.a, origin);
                let position = vector4(std::array::from_fn(|axis| {
                    a[axis] + (ab[axis] * u + ac[axis] * v)
                }));
                let first = work.rays.len();
                let directions = [(normal.map(|value| -value), 0)].into_iter().chain(
                    axes.iter()
                        .copied()
                        .filter(|axis| dot(*axis, normal) > 0.0)
                        .map(|axis| (axis, 1)),
                );
                for (direction, mode) in directions {
                    if work.rays.len() == ray_limit {
                        return Err(GpuComputeError::WorkloadLimit);
                    }
                    work.rays.push(GpuRay {
                        origin: position,
                        direction: vector4(direction),
                        normal: vector4(normal),
                        ids: [index as u32, tri.instance_id, mode, 0],
                    });
                }
                work.samples.push(GpuSample {
                    normal: vector4(normal),
                    info: [
                        first as u32,
                        (work.rays.len() - first) as u32,
                        face_index as u32,
                        u32::from(!config.tool_axes.is_empty()),
                    ],
                });
            }
        }
        Ok(work)
    }

    fn report(&self, stats: &[[u32; 8]], config: &DfmConfig) -> DfmReport {
        let faces = self
            .face_ids
            .iter()
            .zip(stats)
            .map(|(&face_slot_id, stat)| {
                let min_thickness_mm = (stat[0] != u32::MAX).then(|| f64::from(stat[0]) / 1000.0);
                let draft_angle_deg =
                    (stat[1] != u32::MAX).then(|| (f64::from(stat[1]) - 18000.0) / 100.0);
                let accessible = stat[2] == 0 && stat[4] != 0;
                let mut violations = Vec::new();
                if min_thickness_mm
                    .is_some_and(|thickness| thickness < config.min_wall_thickness_mm)
                {
                    violations.push(DfmViolationKind::ThinWall);
                }
                if draft_angle_deg.is_some_and(|angle| angle < config.min_draft_angle_deg) {
                    violations.push(DfmViolationKind::DraftAngle);
                }
                if !accessible {
                    violations.push(DfmViolationKind::ToolAccessibility);
                }
                FaceDfmMetrics {
                    face_slot_id,
                    min_thickness_mm,
                    draft_angle_deg,
                    accessible,
                    violations,
                }
            })
            .collect();
        DfmReport { faces }
    }
}

fn dfm_origin(triangles: &[TriRef]) -> Result<[f64; 3], GpuComputeError> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for point in triangles.iter().flat_map(|tri| [tri.a, tri.b, tri.c]) {
        for (axis, value) in point.into_iter().enumerate() {
            if !value.is_finite() {
                return Err(GpuComputeError::DfmGeometryRange);
            }
            lo[axis] = lo[axis].min(value);
            hi[axis] = hi[axis].max(value);
        }
    }
    if magnitude(sub(hi, lo)) > f64::from(MAX_DFM_DISTANCE_MM) {
        return Err(GpuComputeError::DfmGeometryRange);
    }
    Ok(std::array::from_fn(|axis| lo[axis].midpoint(hi[axis])))
}

fn triangle_geometry(tri: &TriRef) -> ([f64; 3], f64) {
    let normal = cross(sub(tri.b, tri.a), sub(tri.c, tri.a));
    let length = magnitude(normal);
    if length > 0.0 && length.is_finite() {
        (normal.map(|value| value / length), length * 0.5)
    } else {
        ([0.0; 3], 0.0)
    }
}

fn magnitude([x, y, z]: [f64; 3]) -> f64 {
    x.hypot(y).hypot(z)
}

fn normalize(vector: [f64; 3]) -> [f64; 3] {
    let length = magnitude(vector);
    if length > 0.0 && length.is_finite() {
        vector.map(|value| value / length)
    } else {
        [0.0; 3]
    }
}

fn vector4([x, y, z]: [f64; 3]) -> [f32; 4] {
    [x as f32, y as f32, z as f32, 0.0]
}

pub(crate) fn warn_gpu_fallback(operation: &str, error_type: &str, message: impl std::fmt::Display) {
    tracing::warn!(
        component = "cad-gpu-compute",
        operation,
        code = cad_observe::codes::R010_GPU_FALLBACK,
        error_type,
        "{message}"
    );
}

pub(crate) fn gpu_error_type(error: &GpuComputeError) -> &'static str {
    match error {
        GpuComputeError::NoAdapter => "NoAdapter",
        GpuComputeError::Device(_) => "Device",
        GpuComputeError::Pipeline(_) => "Pipeline",
        GpuComputeError::Map(_) => "Map",
        GpuComputeError::ReadbackDropped => "ReadbackDropped",
        GpuComputeError::WorkloadLimit => "WorkloadLimit",
        GpuComputeError::DfmGeometryRange => "DfmGeometryRange",
    }
}

fn warn_if_fallback_adapter(info: &wgpu::AdapterInfo, force_fallback_adapter: bool) {
    if force_fallback_adapter || info.device_type == wgpu::DeviceType::Cpu {
        let error_type = if force_fallback_adapter {
            "FallbackAdapter"
        } else {
            "SoftwareAdapter"
        };
        warn_gpu_fallback(
            "select_adapter",
            error_type,
            format!(
                "hardware WebGPU adapter unavailable; using fallback adapter {} ({:?}, {:?})",
                info.name, info.backend, info.device_type
            ),
        );
    }
}

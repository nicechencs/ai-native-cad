//! 离屏 wgpu 视口。不依赖窗口，能画出实体、硬件模板剖面封口、宽边线，并执行 1 像素 GPU 拾取。

use crate::camera::{Camera, ProjectionMode, camera_relative_translation};
use crate::compact_scene::{CompactScene, SceneInstance};
use crate::picking::{
    PickHit, PickTargetMode, RenderPassOptions, SectionPlane, unproject_pixel_ndc_depth,
};
use bytemuck::{Pod, Zeroable};
use cad_kernel_occt::Mat4;
use cad_syntax::Span;
use serde::Serialize;
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;
use wgpu::util::DeviceExt;

const UNIFORM_ALIGN: u64 = 256;
const SAMPLE_COUNT: u32 = 4;
const CLIP_NEAR_MM: f32 = 1.0;
const CLIP_FAR_MM: f32 = 20_000.0;

/// 世界坐标里的剖切平面。满足 `dot(point, normal) > offset_mm` 的部分不画。
#[derive(Debug, Clone, Copy)]
pub struct ClipPlane {
    pub normal: [f64; 3],
    pub offset_mm: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RenderOptions {
    pub clip: Option<ClipPlane>,
}

impl From<&RenderOptions> for RenderPassOptions {
    fn from(options: &RenderOptions) -> Self {
        Self {
            section_plane: options.clip.map(|clip| {
                SectionPlane::from_normal_offset(clip.normal, clip.offset_mm)
                    .with_stencil_cap(false)
                    .with_cap_hatch(false)
            }),
            show_edges: true,
            pick_target_mode: PickTargetMode::FacesAndEdges,
        }
    }
}

/// 读回的 RGBA 图，第一行在画面上方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl RenderImage {
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut png_bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut png_bytes, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|err| format!("png header: {err}"))?;
        writer
            .write_image_data(&self.rgba)
            .map_err(|err| format!("png pixels: {err}"))?;
        drop(writer);
        Ok(png_bytes)
    }
}

#[derive(Debug, Clone)]
pub struct RenderedFrame {
    pub image: RenderImage,
    picks: Vec<u32>,
    camera: Camera,
}

impl RenderedFrame {
    /// `x` 从左往右，`y` 从上往下。背景返回 `None`。
    pub fn pick_pixel(&self, scene: &CompactScene, x: u32, y: u32) -> Option<PickHit> {
        if x >= self.image.width || y >= self.image.height {
            return None;
        }
        let pixel = ((y * self.image.width + x) * 4) as usize;
        let raw = [
            self.picks.get(pixel).copied()?,
            self.picks.get(pixel + 1).copied()?,
            self.picks.get(pixel + 2).copied()?,
            self.picks.get(pixel + 3).copied()?,
        ];
        decode_pick_pixel(
            scene,
            &self.camera,
            self.image.width,
            self.image.height,
            x,
            y,
            raw,
        )
    }

    /// 把点选结果画成纯色图，并给出颜色和面的对照表。
    pub fn semantic_mask(&self, scene: &CompactScene) -> (RenderImage, Vec<SemanticEntry>) {
        let mut rgba = vec![0u8; (self.image.width * self.image.height * 4) as usize];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        let mut entries = BTreeMap::<(u32, bool, String), SemanticEntry>::new();
        for y in 0..self.image.height {
            for x in 0..self.image.width {
                let Some(hit) = self.pick_pixel(scene, x, y) else {
                    continue;
                };
                let color = mask_color(hit.instance_index, &hit.face_id, hit.is_edge);
                let pixel = ((y * self.image.width + x) * 4) as usize;
                rgba[pixel..pixel + 3].copy_from_slice(&color);
                let component_id = scene
                    .instances
                    .get(hit.instance_index as usize)
                    .and_then(|instance| instance.component_id.clone());
                entries
                    .entry((hit.instance_index, hit.is_edge, hit.face_id.clone()))
                    .or_insert(SemanticEntry {
                        color: format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]),
                        instance_index: hit.instance_index,
                        component_id,
                        face_id: hit.face_id,
                        is_edge: hit.is_edge,
                        ast_span: hit.ast_span,
                    });
            }
        }
        (
            RenderImage {
                width: self.image.width,
                height: self.image.height,
                rgba,
            },
            entries.into_values().collect(),
        )
    }
}

fn decode_pick_pixel(
    scene: &CompactScene,
    camera: &Camera,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    raw: [u32; 4],
) -> Option<PickHit> {
    let instance_raw = raw[0];
    if instance_raw == 0 {
        return None;
    }
    let instance_index = instance_raw - 1;
    let instance = scene.instances.get(instance_index as usize)?;
    let prototype = scene.prototypes.get(instance.prototype_index as usize)?;
    let packed = raw[1];
    let is_edge = packed >> 31 == 1;
    let slot = packed & 0x7fff_ffff;
    let ndc_z = f64::from(f32::from_bits(raw[3]));
    let (world_point_mm, depth) = unproject_pixel_ndc_depth(
        camera,
        width,
        height,
        x,
        y,
        ndc_z,
        f64::from(CLIP_NEAR_MM),
        f64::from(CLIP_FAR_MM),
    )
    .unwrap_or(([0.0; 3], 0.0));
    if is_edge {
        let edge = prototype.edge_table.get(slot as usize)?;
        Some(PickHit {
            instance_index,
            prototype_index: instance.prototype_index,
            face_slot_id: None,
            face_id: edge.edge_id.clone(),
            edge_slot_id: Some(slot),
            world_point_mm,
            depth,
            ast_span: edge.ast_span,
            is_edge: true,
        })
    } else {
        let face = prototype.face_table.get(slot as usize)?;
        Some(PickHit {
            instance_index,
            prototype_index: instance.prototype_index,
            face_slot_id: Some(slot),
            face_id: face.face_id.clone(),
            edge_slot_id: None,
            world_point_mm,
            depth,
            ast_span: face.ast_span,
            is_edge: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticEntry {
    pub color: String,
    pub instance_index: u32,
    pub component_id: Option<String>,
    pub face_id: String,
    pub is_edge: bool,
    pub ast_span: Span,
}

fn mask_color(instance: u32, id: &str, is_edge: bool) -> [u8; 3] {
    let mut hash = 2_166_136_261u32;
    for byte in id.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash ^= instance.wrapping_mul(0x9e37_79b1);
    if is_edge {
        hash ^= 0x0100_0000;
    }
    [
        40 + ((hash & 0xff) as u8 % 200),
        40 + (((hash >> 8) & 0xff) as u8 % 200),
        40 + (((hash >> 16) & 0xff) as u8 % 200),
    ]
}

pub struct RenderEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    width: u32,
    height: u32,
    sample_count: u32,
    solid: wgpu::RenderPipeline,
    stencil_mask: wgpu::RenderPipeline,
    section_cap: wgpu::RenderPipeline,
    edge: wgpu::RenderPipeline,
    pick_solid: wgpu::RenderPipeline,
    pick_edge: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    pub adapter_name: String,
}

#[derive(Clone, Copy)]
pub enum AdapterChoice {
    Hardware,
    Software,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Frame {
    view: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    viewport: [f32; 4],
    clip: [f32; 4],
    clip_on: u32,
    cap_hatch: u32,
    _pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Xform {
    r0: [f32; 4],
    r1: [f32; 4],
    r2: [f32; 4],
    meta: [u32; 4],
}

impl RenderEngine {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        Self::with_choice(width, height, AdapterChoice::Hardware).or_else(|hardware| {
            Self::with_choice(width, height, AdapterChoice::Software)
                .map_err(|software| format!("{hardware}; software fallback: {software}"))
        })
    }

    pub fn with_choice(width: u32, height: u32, choice: AdapterChoice) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("render size must be non-zero".to_string());
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = choice;
            return Err(
                "synchronous RenderEngine::with_choice is unsupported on wasm32; use async WebGPU initialization"
                    .to_string(),
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            pollster::block_on(Self::create(width, height, choice))
        }
    }

    pub fn render(
        &self,
        scene: &CompactScene,
        camera: &Camera,
        options: &RenderOptions,
    ) -> Result<RenderedFrame, String> {
        let pass_options = RenderPassOptions::from(options);
        self.render_with_options(scene, camera, &pass_options)
    }

    /// Renders color + optional hardware stencil section cap + semantic pick buffer.
    pub fn render_with_options(
        &self,
        scene: &CompactScene,
        camera: &Camera,
        options: &RenderPassOptions,
    ) -> Result<RenderedFrame, String> {
        let frame = frame_uniform(camera, self.width, self.height, options.section_plane);
        let (bind, draws) = self.prepare_scene(scene, camera, &frame);
        let color_target = texture(
            &self.device,
            self.width,
            self.height,
            1,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let color_msaa = (self.sample_count > 1).then(|| {
            texture(
                &self.device,
                self.width,
                self.height,
                self.sample_count,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
        });
        let depth_stencil = texture(
            &self.device,
            self.width,
            self.height,
            self.sample_count,
            wgpu::TextureFormat::Depth24PlusStencil8,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let pick = texture(
            &self.device,
            self.width,
            self.height,
            1,
            wgpu::TextureFormat::Rgba32Uint,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let pick_depth = texture(
            &self.device,
            self.width,
            self.height,
            1,
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let color_view = color_target.create_view(&Default::default());
        let msaa_view = color_msaa
            .as_ref()
            .map(|tex| tex.create_view(&Default::default()));
        let depth_view = depth_stencil.create_view(&Default::default());
        let pick_view = pick.create_view(&Default::default());
        let pick_depth_view = pick_depth.create_view(&Default::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("color"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view.as_ref().unwrap_or(&color_view),
                    resolve_target: msaa_view.as_ref().map(|_| &color_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.025,
                            b: 0.03,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            self.draw_color_pass(&mut pass, &draws, &bind, options);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pick"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &pick_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &pick_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            self.draw_pick_pass(&mut pass, &draws, &bind, options);
        }

        let color_bytes = schedule_copy(
            &self.device,
            &mut encoder,
            &color_target,
            self.width,
            self.height,
            4,
        );
        let pick_bytes = schedule_copy(
            &self.device,
            &mut encoder,
            &pick,
            self.width,
            self.height,
            16,
        );
        self.queue.submit(Some(encoder.finish()));
        let rgba = map_buffer(
            &self.device,
            color_bytes,
            padded_bytes(self.width * 4),
            self.width,
            self.height,
            4,
        )?;
        let pick_raw = map_buffer(
            &self.device,
            pick_bytes,
            padded_bytes(self.width * 16),
            self.width,
            self.height,
            16,
        )?;
        let picks = pick_raw
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("4 bytes")))
            .collect();
        Ok(RenderedFrame {
            image: RenderImage {
                width: self.width,
                height: self.height,
                rgba,
            },
            picks,
            camera: *camera,
        })
    }

    /// 1-pixel hardware ID-target picking with `set_scissor_rect(x, y, 1, 1)` and 1x1 texel readback.
    pub fn pick_pixel(
        &self,
        scene: &CompactScene,
        camera: &Camera,
        x: u32,
        y: u32,
        options: &RenderPassOptions,
    ) -> Result<Option<PickHit>, String> {
        if x >= self.width || y >= self.height {
            return Ok(None);
        }
        let frame = frame_uniform(camera, self.width, self.height, options.section_plane);
        let (bind, draws) = self.prepare_scene(scene, camera, &frame);
        let pick = texture(
            &self.device,
            self.width,
            self.height,
            1,
            wgpu::TextureFormat::Rgba32Uint,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let pick_depth = texture(
            &self.device,
            self.width,
            self.height,
            1,
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let pick_view = pick.create_view(&Default::default());
        let pick_depth_view = pick_depth.create_view(&Default::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pick-1px"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pick-1px-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &pick_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &pick_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            pass.set_scissor_rect(x, y, 1, 1);
            self.draw_pick_pass(&mut pass, &draws, &bind, options);
        }

        let staging = copy_single_pixel(&self.device, &mut encoder, &pick, x, y);
        self.queue.submit(Some(encoder.finish()));
        let bytes = map_buffer(&self.device, staging, 256, 1, 1, 16)?;
        let raw = [
            u32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes")),
            u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes")),
            u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")),
            u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")),
        ];
        Ok(decode_pick_pixel(
            scene,
            camera,
            self.width,
            self.height,
            x,
            y,
            raw,
        ))
    }

    /// 把世界坐标投到像素。规则与着色器一致：左上角是原点。
    pub fn project_pixel(&self, camera: &Camera, world_rel: [f32; 3]) -> Option<(u32, u32)> {
        let frame = frame_uniform(camera, self.width, self.height, None);
        let clip = mul_point(frame.view_proj, world_rel);
        if clip[3] <= 0.0 {
            return None;
        }
        let ndc_x = clip[0] / clip[3];
        let ndc_y = clip[1] / clip[3];
        if !(ndc_x.abs() <= 1.0 && ndc_y.abs() <= 1.0) {
            return None;
        }
        let x = (ndc_x * 0.5 + 0.5) * self.width as f32;
        let y = (0.5 - ndc_y * 0.5) * self.height as f32;
        let x = x.floor() as i32;
        let y = y.floor() as i32;
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            None
        } else {
            Some((x as u32, y as u32))
        }
    }

    fn prepare_scene(
        &self,
        scene: &CompactScene,
        camera: &Camera,
        frame: &Frame,
    ) -> (wgpu::BindGroup, Vec<InstanceDraw>) {
        let xforms = xforms_of(scene, camera);
        let frame_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("frame"),
                contents: bytemuck::bytes_of(frame),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let xform_buffer = padded_uniform_buffer(&self.device, &xforms);
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame-bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &xform_buffer,
                        offset: 0,
                        size: Some(
                            std::num::NonZeroU64::new(std::mem::size_of::<Xform>() as u64).unwrap(),
                        ),
                    }),
                },
            ],
        });
        let draws = self.draw_buffers(scene);
        (bind, draws)
    }

    fn draw_buffers(&self, scene: &CompactScene) -> Vec<InstanceDraw> {
        scene
            .instances
            .iter()
            .enumerate()
            .filter_map(|(index, instance)| {
                let prototype = scene.prototypes.get(instance.prototype_index as usize)?;
                if prototype.vertices.is_empty() || prototype.indices.is_empty() {
                    return None;
                }
                let vertices = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("vertices"),
                        contents: bytemuck::cast_slice(&prototype.vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                let indices = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("indices"),
                        contents: bytemuck::cast_slice(&prototype.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    });
                let edges = if prototype.edges.is_empty() {
                    None
                } else {
                    Some((
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("edges"),
                                contents: bytemuck::cast_slice(&prototype.edges),
                                usage: wgpu::BufferUsages::VERTEX,
                            }),
                        prototype.edges.len() as u32,
                    ))
                };
                Some(InstanceDraw {
                    xform_index: index,
                    vertices,
                    indices,
                    index_count: prototype.indices.len() as u32,
                    edges,
                })
            })
            .collect()
    }

    fn draw_color_pass<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        draws: &'a [InstanceDraw],
        bind: &'a wgpu::BindGroup,
        options: &RenderPassOptions,
    ) {
        // 1. Solid shaded pass
        for draw in draws {
            let offset = (draw.xform_index as u64) * UNIFORM_ALIGN;
            pass.set_bind_group(0, bind, &[offset as u32]);
            pass.set_pipeline(&self.solid);
            pass.set_vertex_buffer(0, draw.vertices.slice(..));
            pass.set_index_buffer(draw.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..draw.index_count, 0, 0..1);
        }

        // 2. Optional hardware stencil section capping + 45 deg ISO hatch
        if options
            .section_plane
            .is_some_and(|plane| plane.enable_stencil_cap)
            && !draws.is_empty()
        {
            pass.set_stencil_reference(0);
            for draw in draws {
                let offset = (draw.xform_index as u64) * UNIFORM_ALIGN;
                pass.set_bind_group(0, bind, &[offset as u32]);
                pass.set_pipeline(&self.stencil_mask);
                pass.set_vertex_buffer(0, draw.vertices.slice(..));
                pass.set_index_buffer(draw.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..draw.index_count, 0, 0..1);
            }
            pass.set_bind_group(0, bind, &[0]);
            pass.set_pipeline(&self.section_cap);
            pass.draw(0..3, 0..1);
        }

        // 3. Optional anti-aliased wide topological edges
        if options.show_edges {
            for draw in draws {
                if let Some((edges, count)) = &draw.edges {
                    let offset = (draw.xform_index as u64) * UNIFORM_ALIGN;
                    pass.set_bind_group(0, bind, &[offset as u32]);
                    pass.set_pipeline(&self.edge);
                    pass.set_vertex_buffer(0, edges.slice(..));
                    pass.draw(0..6, 0..*count);
                }
            }
        }
    }

    fn draw_pick_pass<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        draws: &'a [InstanceDraw],
        bind: &'a wgpu::BindGroup,
        options: &RenderPassOptions,
    ) {
        let draw_faces = matches!(
            options.pick_target_mode,
            PickTargetMode::FacesAndEdges | PickTargetMode::FacesOnly
        );
        let draw_edges = options.show_edges
            && matches!(
                options.pick_target_mode,
                PickTargetMode::FacesAndEdges | PickTargetMode::EdgesOnly
            );
        for draw in draws {
            let offset = (draw.xform_index as u64) * UNIFORM_ALIGN;
            pass.set_bind_group(0, bind, &[offset as u32]);
            if draw_faces {
                pass.set_pipeline(&self.pick_solid);
                pass.set_vertex_buffer(0, draw.vertices.slice(..));
                pass.set_index_buffer(draw.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..draw.index_count, 0, 0..1);
            }
            if draw_edges && let Some((edges, count)) = &draw.edges {
                pass.set_pipeline(&self.pick_edge);
                pass.set_vertex_buffer(0, edges.slice(..));
                pass.draw(0..6, 0..*count);
            }
        }
    }
}

struct InstanceDraw {
    xform_index: usize,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    edges: Option<(wgpu::Buffer, u32)>,
}

async fn request_device(
    choice: AdapterChoice,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: matches!(choice, AdapterChoice::Software),
        })
        .await
        .ok_or_else(|| match choice {
            AdapterChoice::Hardware => "no hardware GPU adapter".to_string(),
            AdapterChoice::Software => "no software GPU adapter".to_string(),
        })?;
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("cad-render"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
            },
            None,
        )
        .await
        .map_err(|err| format!("request device: {err}"))?;
    Ok((adapter, device, queue))
}

impl RenderEngine {
    async fn create(width: u32, height: u32, choice: AdapterChoice) -> Result<Self, String> {
        let (adapter, device, queue) = request_device(choice).await?;
        let sample_count = if adapter
            .get_texture_format_features(wgpu::TextureFormat::Rgba8UnormSrgb)
            .flags
            .sample_count_supported(SAMPLE_COUNT)
            && adapter
                .get_texture_format_features(wgpu::TextureFormat::Depth24PlusStencil8)
                .flags
                .sample_count_supported(SAMPLE_COUNT)
        {
            SAMPLE_COUNT
        } else {
            1
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame-layout"),
            entries: &[uniform_entry(0, false), uniform_entry(1, true)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cad"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let solid = pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_solid",
            "fs_color",
            sample_count,
            wgpu::TextureFormat::Depth24PlusStencil8,
            false,
            true,
            false,
        );
        let stencil_mask = stencil_mask_pipeline(&device, &pipeline_layout, &shader, sample_count);
        let section_cap = section_cap_pipeline(&device, &pipeline_layout, &shader, sample_count);
        let edge = pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_edge",
            "fs_edge",
            sample_count,
            wgpu::TextureFormat::Depth24PlusStencil8,
            true,
            false,
            true,
        );
        let pick_solid = pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_solid",
            "fs_pick",
            1,
            wgpu::TextureFormat::Depth32Float,
            false,
            true,
            false,
        );
        let pick_edge = pipeline(
            &device,
            &pipeline_layout,
            &shader,
            "vs_edge",
            "fs_pick_edge",
            1,
            wgpu::TextureFormat::Depth32Float,
            true,
            false,
            false,
        );
        Ok(Self {
            device,
            queue,
            width,
            height,
            sample_count,
            solid,
            stencil_mask,
            section_cap,
            edge,
            pick_solid,
            pick_edge,
            layout,
            adapter_name: adapter.get_info().name,
        })
    }
}

fn uniform_entry(binding: u32, dynamic: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: None,
        },
        count: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    vs: &str,
    fs: &str,
    samples: u32,
    depth_format: wgpu::TextureFormat,
    edge: bool,
    depth_write: bool,
    blend: bool,
) -> wgpu::RenderPipeline {
    let format = if fs.contains("pick") {
        wgpu::TextureFormat::Rgba32Uint
    } else {
        wgpu::TextureFormat::Rgba8UnormSrgb
    };
    let solid_attributes = solid_attrs();
    let edge_attributes = edge_attrs();
    let buffers = if edge {
        vec![wgpu::VertexBufferLayout {
            array_stride: 32,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &edge_attributes,
        }]
    } else {
        vec![wgpu::VertexBufferLayout {
            array_stride: 32,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &solid_attributes,
        }]
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vs),
            buffers: &buffers,
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: depth_format,
            depth_write_enabled: depth_write,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: blend.then_some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}

fn stencil_mask_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    samples: u32,
) -> wgpu::RenderPipeline {
    let solid_attributes = solid_attrs();
    let buffers = [wgpu::VertexBufferLayout {
        array_stride: 32,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &solid_attributes,
    }];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("stencil_mask"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_solid"),
            buffers: &buffers,
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Always,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::DecrementWrap,
                },
                back: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Always,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementWrap,
                },
                read_mask: 0xff,
                write_mask: 0xff,
            },
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_stencil_mask"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::empty(),
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}

fn section_cap_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    samples: u32,
) -> wgpu::RenderPipeline {
    let cap_stencil = wgpu::StencilFaceState {
        compare: wgpu::CompareFunction::NotEqual,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op: wgpu::StencilOperation::Keep,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("section_cap"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_cap"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: wgpu::StencilState {
                front: cap_stencil,
                back: cap_stencil,
                read_mask: 0xff,
                write_mask: 0x00,
            },
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_cap"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}

fn solid_attrs() -> [wgpu::VertexAttribute; 4] {
    [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32,
            offset: 12,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 16,
            shader_location: 2,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32,
            offset: 28,
            shader_location: 3,
        },
    ]
}

fn edge_attrs() -> [wgpu::VertexAttribute; 4] {
    [
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32,
            offset: 12,
            shader_location: 1,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 16,
            shader_location: 2,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Uint32,
            offset: 28,
            shader_location: 3,
        },
    ]
}

fn texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    samples: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn padded_uniform_buffer(device: &wgpu::Device, xforms: &[Xform]) -> wgpu::Buffer {
    let stride = UNIFORM_ALIGN as usize;
    let mut bytes = vec![0u8; stride * xforms.len().max(1)];
    for (index, xform) in xforms.iter().enumerate() {
        let start = index * stride;
        bytes[start..start + std::mem::size_of::<Xform>()]
            .copy_from_slice(bytemuck::bytes_of(xform));
    }
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("xforms"),
        contents: &bytes,
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

fn schedule_copy(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    pixel_bytes: u32,
) -> wgpu::Buffer {
    copy_texture(
        device,
        encoder,
        texture,
        padded_bytes(width * pixel_bytes),
        height,
    )
}

fn copy_texture(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    padded: u32,
    height: u32,
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width: texture.width(),
            height,
            depth_or_array_layers: 1,
        },
    );
    buffer
}

fn copy_single_pixel(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    x: u32,
    y: u32,
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pick-1px-readback"),
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    buffer
}

fn map_buffer(
    device: &wgpu::Device,
    buffer: wgpu::Buffer,
    padded: u32,
    width: u32,
    height: u32,
    pixel_bytes: u32,
) -> Result<Vec<u8>, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (device, buffer, padded, width, height, pixel_bytes);
        return Err("synchronous GPU buffer readback is unsupported on wasm32".to_string());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let slice = buffer.slice(..);
        let (send, recv) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
        device.poll(wgpu::Maintain::Wait);
        recv.recv()
            .map_err(|_| "readback closed".to_string())?
            .map_err(|err| format!("map: {err}"))?;
        let mapped = slice.get_mapped_range();
        let row = (width * pixel_bytes) as usize;
        let mut tight = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            let start = y * padded as usize;
            tight.extend_from_slice(&mapped[start..start + row]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(tight)
    }
}

fn padded_bytes(unpadded: u32) -> u32 {
    unpadded.div_ceil(256) * 256
}

fn xforms_of(scene: &CompactScene, camera: &Camera) -> Vec<Xform> {
    scene
        .instances
        .iter()
        .enumerate()
        .map(|(index, instance)| {
            let anchor = scene
                .prototypes
                .get(instance.prototype_index as usize)
                .map(|prototype| prototype.anchor_mm)
                .unwrap_or([0.0; 3]);
            xform_of(instance, anchor, camera, (index as u32) + 1)
        })
        .collect()
}

fn xform_of(
    instance: &SceneInstance,
    anchor: [f64; 3],
    camera: &Camera,
    instance_id: u32,
) -> Xform {
    let translation = camera_relative_translation(
        instance.origin_mm,
        instance.rotation_deg,
        anchor,
        camera.target_mm,
        [0.0; 3],
    );
    let matrix = Mat4::rotate_euler_deg(
        instance.rotation_deg[0],
        instance.rotation_deg[1],
        instance.rotation_deg[2],
    );
    let row = |index: usize, extra: f32| {
        [
            matrix.m[index][0] as f32,
            matrix.m[index][1] as f32,
            matrix.m[index][2] as f32,
            extra,
        ]
    };
    Xform {
        r0: row(0, translation[0]),
        r1: row(1, translation[1]),
        r2: row(2, translation[2]),
        meta: [instance_id, 0, 0, 0],
    }
}

fn frame_uniform(
    camera: &Camera,
    width: u32,
    height: u32,
    section: Option<SectionPlane>,
) -> Frame {
    let view = look_at(eye_relative(camera), [0.0; 3], up_for(camera));
    let aspect = width as f32 / height as f32;
    let projection = match camera.projection {
        ProjectionMode::Orthographic => ortho(
            camera.ortho_height_mm as f32,
            aspect,
            CLIP_NEAR_MM,
            CLIP_FAR_MM,
        ),
        ProjectionMode::Perspective => perspective(
            camera.fov_y_deg.clamp(1.0, 175.0) as f32,
            aspect,
            CLIP_NEAR_MM,
            CLIP_FAR_MM,
        ),
    };
    let (clip_value, clip_on, cap_hatch) = match section {
        Some(plane) => {
            let normal = plane.normal();
            let offset_mm = plane.offset_mm();
            let target_dot = normal[0] * camera.target_mm[0]
                + normal[1] * camera.target_mm[1]
                + normal[2] * camera.target_mm[2];
            let offset_rel = (offset_mm - target_dot) as f32;
            (
                [normal[0] as f32, normal[1] as f32, normal[2] as f32, offset_rel],
                1,
                u32::from(plane.show_cap_hatch),
            )
        }
        None => ([0.0, 0.0, 1.0, 0.0], 0, 0),
    };
    Frame {
        view,
        view_proj: mul(projection, view),
        viewport: [width as f32, height as f32, 1.5, 0.001],
        clip: clip_value,
        clip_on,
        cap_hatch,
        _pad: [0; 2],
    }
}

fn eye_relative(camera: &Camera) -> [f32; 3] {
    let eye = camera.eye_mm();
    [
        (eye[0] - camera.target_mm[0]) as f32,
        (eye[1] - camera.target_mm[1]) as f32,
        (eye[2] - camera.target_mm[2]) as f32,
    ]
}

fn up_for(camera: &Camera) -> [f32; 3] {
    let eye = eye_relative(camera);
    let forward = normalize([-eye[0], -eye[1], -eye[2]]);
    if forward[2].abs() > 0.95 {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let forward = normalize([center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]]);
    let side = normalize(cross(forward, up));
    let lifted = cross(side, forward);
    [
        [side[0], lifted[0], -forward[0], 0.0],
        [side[1], lifted[1], -forward[1], 0.0],
        [side[2], lifted[2], -forward[2], 0.0],
        [-dot(side, eye), -dot(lifted, eye), dot(forward, eye), 1.0],
    ]
}

fn ortho(height: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let width = height * aspect;
    let depth = far - near;
    [
        [2.0 / width, 0.0, 0.0, 0.0],
        [0.0, 2.0 / height, 0.0, 0.0],
        [0.0, 0.0, -1.0 / depth, 0.0],
        [0.0, 0.0, -near / depth, 1.0],
    ]
}

fn perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fov_y_deg.to_radians() * 0.5).tan();
    let depth = near - far;
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far / depth, -1.0],
        [0.0, 0.0, (near * far) / depth, 0.0],
    ]
}

fn mul(left: [[f32; 4]; 4], right: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            out[col][row] = (0..4).map(|k| left[k][row] * right[col][k]).sum();
        }
    }
    out
}

fn mul_point(matrix: [[f32; 4]; 4], point: [f32; 3]) -> [f32; 4] {
    let src = [point[0], point[1], point[2], 1.0];
    let mut out = [0.0; 4];
    for row in 0..4 {
        out[row] = (0..4).map(|col| matrix[col][row] * src[col]).sum();
    }
    out
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(value: [f32; 3]) -> [f32; 3] {
    let length = dot(value, value).sqrt();
    if length < 1e-8 {
        [0.0, 0.0, 1.0]
    } else {
        [value[0] / length, value[1] / length, value[2] / length]
    }
}

const SHADER: &str = r#"
struct Frame {
    view: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    viewport: vec4<f32>,
    clip: vec4<f32>,
    clip_on: u32,
    cap_hatch: u32,
    _pad0: u32,
    _pad1: u32,
}
struct Xform {
    r0: vec4<f32>,
    r1: vec4<f32>,
    r2: vec4<f32>,
    info: vec4<u32>,
}
@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var<uniform> xform: Xform;

struct SolidIn {
    @location(0) position: vec3<f32>,
    @location(1) face_slot: u32,
    @location(2) normal: vec3<f32>,
    @location(3) flags: u32,
}
struct SolidOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) view_normal: vec3<f32>,
    @location(1) world_rel: vec3<f32>,
    @location(2) @interpolate(flat) face_slot: u32,
    @location(3) @interpolate(flat) flags: u32,
    @location(4) @interpolate(flat) instance_id: u32,
}

fn apply(p: vec3<f32>) -> vec3<f32> {
    return vec3(
        dot(xform.r0.xyz, p) + xform.r0.w,
        dot(xform.r1.xyz, p) + xform.r1.w,
        dot(xform.r2.xyz, p) + xform.r2.w,
    );
}
fn rotate_dir(n: vec3<f32>) -> vec3<f32> {
    return vec3(dot(xform.r0.xyz, n), dot(xform.r1.xyz, n), dot(xform.r2.xyz, n));
}
fn clipped(world_rel: vec3<f32>) -> bool {
    return frame.clip_on == 1u && dot(world_rel, frame.clip.xyz) > frame.clip.w;
}

@vertex
fn vs_solid(input: SolidIn) -> SolidOut {
    var out: SolidOut;
    let world_rel = apply(input.position);
    let view_pos = frame.view * vec4(world_rel, 1.0);
    out.clip = frame.view_proj * vec4(world_rel, 1.0);
    out.view_normal = normalize((frame.view * vec4(rotate_dir(input.normal), 0.0)).xyz);
    out.world_rel = world_rel;
    out.face_slot = input.face_slot;
    out.flags = input.flags;
    out.instance_id = xform.info.x;
    let _unused = view_pos;
    return out;
}

@fragment
fn fs_color(input: SolidOut) -> @location(0) vec4<f32> {
    if (clipped(input.world_rel)) { discard; }
    let normal = normalize(input.view_normal);
    let light = normalize(vec3<f32>(0.25, 0.55, 0.80));
    let wrap = clamp(dot(normal, light) * 0.5 + 0.5, 0.0, 1.0);
    let steel = mix(vec3<f32>(0.16, 0.20, 0.25), vec3<f32>(0.82, 0.86, 0.90), wrap);
    let rim = pow(1.0 - abs(normal.z), 2.0);
    return vec4(steel + rim * vec3<f32>(0.05, 0.06, 0.07), 1.0);
}

@fragment
fn fs_stencil_mask(input: SolidOut) -> @location(0) vec4<f32> {
    if (clipped(input.world_rel)) { discard; }
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}

@vertex
fn vs_cap(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    if (vi == 0u) { return vec4<f32>(-1.0, -1.0, 0.0, 1.0); }
    if (vi == 1u) { return vec4<f32>(3.0, -1.0, 0.0, 1.0); }
    return vec4<f32>(-1.0, 3.0, 0.0, 1.0);
}

@fragment
fn fs_cap(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var color = vec3<f32>(0.52, 0.58, 0.65);
    if (frame.cap_hatch == 1u) {
        let diag = (pos.x + pos.y) % 8.0;
        if (diag < 1.6) {
            color = vec3<f32>(0.22, 0.28, 0.35);
        }
    }
    return vec4<f32>(color, 1.0);
}

@fragment
fn fs_pick(input: SolidOut) -> @location(0) vec4<u32> {
    if (clipped(input.world_rel)) { discard; }
    let depth_bits = bitcast<u32>(input.clip.z);
    return vec4(input.instance_id, input.face_slot, input.flags, depth_bits);
}

struct EdgeIn {
    @location(0) point_a: vec3<f32>,
    @location(1) edge_slot: u32,
    @location(2) point_b: vec3<f32>,
    @location(3) style: u32,
}
struct EdgeOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) across: f32,
    @location(1) world_rel: vec3<f32>,
    @location(2) @interpolate(flat) edge_slot: u32,
    @location(3) @interpolate(flat) instance_id: u32,
}

fn corner(index: u32) -> vec2<f32> {
    if (index == 0u) { return vec2(-1.0, 0.0); }
    if (index == 1u) { return vec2(-1.0, 1.0); }
    if (index == 2u) { return vec2(1.0, 0.0); }
    if (index == 3u) { return vec2(1.0, 0.0); }
    if (index == 4u) { return vec2(-1.0, 1.0); }
    return vec2(1.0, 1.0);
}

@vertex
fn vs_edge(input: EdgeIn, @builtin(vertex_index) vi: u32) -> EdgeOut {
    var out: EdgeOut;
    let shape = corner(vi);
    let a = apply(input.point_a);
    let b = apply(input.point_b);
    let a_clip = frame.view_proj * vec4(a, 1.0);
    let b_clip = frame.view_proj * vec4(b, 1.0);
    let a_ndc = a_clip.xy / max(a_clip.w, 1e-6);
    let b_ndc = b_clip.xy / max(b_clip.w, 1e-6);
    let a_px = vec2((a_ndc.x * 0.5 + 0.5) * frame.viewport.x, (0.5 - a_ndc.y * 0.5) * frame.viewport.y);
    let b_px = vec2((b_ndc.x * 0.5 + 0.5) * frame.viewport.x, (0.5 - b_ndc.y * 0.5) * frame.viewport.y);
    var dir = b_px - a_px;
    let len = length(dir);
    dir = dir / max(len, 0.001);
    let side = vec2(-dir.y, dir.x);
    let px = mix(a_px, b_px, shape.y) + side * shape.x * frame.viewport.z;
    let w = mix(a_clip.w, b_clip.w, shape.y);
    let z = mix(a_clip.z, b_clip.z, shape.y) - frame.viewport.w * w;
    let ndc = vec2(px.x / frame.viewport.x * 2.0 - 1.0, 1.0 - px.y / frame.viewport.y * 2.0);
    out.clip = vec4(ndc * w, z, w);
    out.across = shape.x;
    out.world_rel = mix(a, b, shape.y);
    out.edge_slot = input.edge_slot | 0x80000000u;
    out.instance_id = xform.info.x;
    return out;
}

@fragment
fn fs_edge(input: EdgeOut) -> @location(0) vec4<f32> {
    if (clipped(input.world_rel)) { discard; }
    let alpha = 1.0 - smoothstep(0.45, 1.0, abs(input.across));
    if (alpha < 0.02) { discard; }
    return vec4(vec3<f32>(0.05, 0.07, 0.09), alpha);
}

@fragment
fn fs_pick_edge(input: EdgeOut) -> @location(0) vec4<u32> {
    if (clipped(input.world_rel) || abs(input.across) > 0.55) { discard; }
    let depth_bits = bitcast<u32>(input.clip.z);
    return vec4(input.instance_id, input.edge_slot, 0u, depth_bits);
}
"#;

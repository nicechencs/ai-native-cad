//! 紧凑场景：相同零件只存一份网格，实例只记录位置和转角。
//! 顶点相对原型基准点，避免十万毫米外的小孔被 f32 吃掉精度。

use cad_kernel_occt::{BRepEdge, BRepFace, CurveType, Mat4, SolidShape, SurfaceType};
use cad_syntax::Span;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const CMS2_MAGIC: &[u8; 4] = b"CMS2";
const CMS2_VERSION: u32 = 1;
const HEADER_BYTES: usize = 32;
const DIRECTORY_BYTES: usize = 32;

/// Companion transport envelope magic (`CM2E`) pairing a 16-byte-aligned `CMS2`
/// payload with UTF-8 JSON compile/diagnostics metadata for IPC/HTTP transport.
pub const CMS2_ENVELOPE_MAGIC: &[u8; 4] = b"CM2E";
const CMS2_ENVELOPE_VERSION: u32 = 1;
const CMS2_ENVELOPE_HEADER_BYTES: usize = 32;

const _: () = assert!(std::mem::size_of::<CadVertex>() == 32);
const _: () = assert!(std::mem::size_of::<CadEdgeSegment>() == 32);

/// 32 字节顶点。坐标是「零件局部位置 − 原型基准」，不是世界坐标。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "wgpu-renderer", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct CadVertex {
    pub position: [f32; 3],
    /// 指向 [`PartPrototype::face_table`] 的下标。
    pub face_slot_id: u32,
    pub normal: [f32; 3],
    /// bit0 精确配合面，bit1 精密孔，bit2..7 表面类型。
    pub flags: u32,
}

/// 一条折线段。宽线和虚线以后用 `style_flags`，这里只负责把折线拆开。
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "wgpu-renderer", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct CadEdgeSegment {
    pub point_a: [f32; 3],
    pub edge_slot_id: u32,
    pub point_b: [f32; 3],
    /// bit0 隐藏线，bit1 轮廓线，bit2 精密边。
    pub style_flags: u32,
}

/// 测量用的精确参数，单位毫米，坐标在零件自己的坐标系里。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceRecord {
    pub face_id: String,
    pub surface_type: SurfaceType,
    pub exact_interface: bool,
    pub is_exact_analytic: bool,
    pub plane_origin_mm: [f64; 3],
    pub plane_normal: [f64; 3],
    pub axis_origin_mm: [f64; 3],
    pub axis_dir: [f64; 3],
    pub radius_mm: f64,
    pub provenance_tags: Vec<String>,
    pub ast_span: Span,
    pub vertex_begin: u32,
    pub vertex_count: u32,
    pub index_begin: u32,
    pub index_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub edge_id: String,
    pub curve_type: CurveType,
    pub provenance_tags: Vec<String>,
    pub ast_span: Span,
    pub segment_begin: u32,
    pub segment_count: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PartPrototype {
    pub part_name: String,
    /// 零件局部坐标系里的基准点，通常是包络盒中心。
    pub anchor_mm: [f64; 3],
    pub vertices: Vec<CadVertex>,
    pub indices: Vec<u32>,
    pub edges: Vec<CadEdgeSegment>,
    pub face_table: Vec<FaceRecord>,
    pub edge_table: Vec<EdgeRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneInstance {
    pub prototype_index: u32,
    pub component_id: Option<String>,
    pub origin_mm: [f64; 3],
    pub rotation_deg: [f64; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactScene {
    pub part_name: String,
    pub kind: String,
    pub units: String,
    pub prototypes: Vec<PartPrototype>,
    pub instances: Vec<SceneInstance>,
}

impl CompactScene {
    /// Alias for [`CompactScene::from_solid`].
    pub fn from_solid_shape(shape: &SolidShape) -> Self {
        Self::from_solid(shape)
    }

    /// Alias for [`CompactScene::to_cms2`].
    pub fn encode_cms2(&self) -> Vec<u8> {
        self.to_cms2()
    }

    /// Alias for [`CompactScene::from_cms2`].
    pub fn decode_cms2(bytes: &[u8]) -> Result<Self, String> {
        Self::from_cms2(bytes)
    }

    /// 从求值结果打包。装配里的面是世界坐标，这里会按组件位姿转回零件坐标再去重。
    pub fn from_solid(shape: &SolidShape) -> Self {
        let groups = groups_of(shape);
        let mut prototypes = Vec::new();
        let mut instances = Vec::with_capacity(groups.len());
        let mut seen: HashMap<ProtoKey, u32> = HashMap::new();

        for group in &groups {
            let local = LocalPart::from_group(group);
            let prototype_index = if let Some(index) = seen.get(&local.key) {
                *index
            } else {
                let index = prototypes.len() as u32;
                seen.insert(local.key.clone(), index);
                prototypes.push(local.into_prototype(&group.part_name));
                index
            };
            instances.push(SceneInstance {
                prototype_index,
                component_id: group.component_id.clone(),
                origin_mm: group.origin,
                rotation_deg: group.rotation,
            });
        }

        let kind = if shape.components.is_empty() {
            "part"
        } else {
            "assembly"
        };
        Self {
            part_name: shape.part_name.clone(),
            kind: kind.to_string(),
            units: "mm".to_string(),
            prototypes,
            instances,
        }
    }

    /// 小端 `CMS2`。网格在二进制区，名称和精确参数在末尾 JSON。
    pub fn to_cms2(&self) -> Vec<u8> {
        let blob_start = HEADER_BYTES + self.prototypes.len() * DIRECTORY_BYTES;
        let mut blob = Vec::new();
        let mut directory = Vec::with_capacity(self.prototypes.len() * DIRECTORY_BYTES);
        for prototype in &self.prototypes {
            let vertex_offset = blob_start + blob.len();
            write_vertices(&mut blob, &prototype.vertices);
            let index_offset = blob_start + blob.len();
            for index in &prototype.indices {
                write_u32(&mut blob, *index);
            }
            pad_16(&mut blob);
            let edge_offset = blob_start + blob.len();
            write_edges(&mut blob, &prototype.edges);
            write_u32(&mut directory, prototype.vertices.len() as u32);
            write_u32(&mut directory, prototype.indices.len() as u32);
            write_u32(&mut directory, prototype.edges.len() as u32);
            write_u32(&mut directory, prototype.face_table.len() as u32);
            write_u32(&mut directory, vertex_offset as u32);
            write_u32(&mut directory, index_offset as u32);
            write_u32(&mut directory, edge_offset as u32);
            write_u32(&mut directory, 0);
        }

        let meta = Cms2Meta {
            part_name: self.part_name.clone(),
            kind: self.kind.clone(),
            units: self.units.clone(),
            prototypes: self
                .prototypes
                .iter()
                .map(|prototype| ProtoMeta {
                    part_name: prototype.part_name.clone(),
                    anchor_mm: prototype.anchor_mm,
                    face_table: prototype.face_table.clone(),
                    edge_table: prototype.edge_table.clone(),
                })
                .collect(),
            instances: self.instances.clone(),
        };
        let json = serde_json::to_vec(&meta).expect("CMS2 metadata is plain data");
        let json_offset = blob_start + blob.len();

        let mut out = Vec::with_capacity(json_offset + json.len());
        out.extend_from_slice(CMS2_MAGIC);
        write_u32(&mut out, CMS2_VERSION);
        write_u32(&mut out, 0);
        write_u32(&mut out, self.prototypes.len() as u32);
        write_u32(&mut out, self.instances.len() as u32);
        write_u32(&mut out, json_offset as u32);
        write_u32(&mut out, json.len() as u32);
        write_u32(&mut out, 0);
        debug_assert_eq!(out.len(), HEADER_BYTES);
        out.extend_from_slice(&directory);
        out.extend_from_slice(&blob);
        out.extend_from_slice(&json);
        out
    }

    pub fn from_cms2(bytes: &[u8]) -> Result<Self, String> {
        if bytes.get(0..4) != Some(CMS2_MAGIC.as_slice()) {
            return Err("CMS2 magic mismatch".to_string());
        }
        let version = read_u32(bytes, 4)?;
        if version != CMS2_VERSION {
            return Err(format!("unsupported CMS2 version {version}"));
        }
        if read_u32(bytes, 8)? != 0 || read_u32(bytes, 28)? != 0 {
            return Err("unsupported CMS2 header flags".to_string());
        }
        let prototype_count = read_u32(bytes, 12)? as usize;
        let instance_count = read_u32(bytes, 16)? as usize;
        let json_offset = read_u32(bytes, 20)? as usize;
        let json_len = read_u32(bytes, 24)? as usize;
        let dir_bytes = prototype_count
            .checked_mul(DIRECTORY_BYTES)
            .ok_or_else(|| "CMS2 directory size overflow".to_string())?;
        let blob_start = HEADER_BYTES
            .checked_add(dir_bytes)
            .ok_or_else(|| "CMS2 header size overflow".to_string())?;
        if json_offset < blob_start || json_offset.checked_add(json_len) != Some(bytes.len()) {
            return Err("CMS2 metadata is outside the file".to_string());
        }
        let meta: Cms2Meta = serde_json::from_slice(&bytes[json_offset..json_offset + json_len])
            .map_err(|err| format!("CMS2 metadata: {err}"))?;
        if meta.units != "mm" {
            return Err("CMS2 units must be millimetres".to_string());
        }
        if meta.prototypes.len() != prototype_count || meta.instances.len() != instance_count {
            return Err("CMS2 counts do not match the header".to_string());
        }

        let mut prototypes = Vec::with_capacity(prototype_count);
        for (index, proto_meta) in meta.prototypes.into_iter().enumerate() {
            let entry = HEADER_BYTES + index * DIRECTORY_BYTES;
            let vertex_count = read_u32(bytes, entry)? as usize;
            let index_count = read_u32(bytes, entry + 4)? as usize;
            let edge_count = read_u32(bytes, entry + 8)? as usize;
            let face_count = read_u32(bytes, entry + 12)? as usize;
            let vertex_offset = read_u32(bytes, entry + 16)? as usize;
            let index_offset = read_u32(bytes, entry + 20)? as usize;
            let edge_offset = read_u32(bytes, entry + 24)? as usize;
            if read_u32(bytes, entry + 28)? != 0 {
                return Err("CMS2 directory reserved field must be zero".to_string());
            }
            if face_count != proto_meta.face_table.len() {
                return Err("CMS2 face table length mismatch".to_string());
            }
            let vertex_bytes = vertex_count
                .checked_mul(32)
                .ok_or_else(|| "CMS2 vertex region size overflow".to_string())?;
            let index_bytes = index_count
                .checked_mul(4)
                .ok_or_else(|| "CMS2 index region size overflow".to_string())?;
            let edge_bytes = edge_count
                .checked_mul(32)
                .ok_or_else(|| "CMS2 edge region size overflow".to_string())?;
            check_range(vertex_offset, vertex_bytes, blob_start, json_offset)?;
            check_range(index_offset, index_bytes, blob_start, json_offset)?;
            check_range(edge_offset, edge_bytes, blob_start, json_offset)?;
            let vertex_end = vertex_offset
                .checked_add(vertex_bytes)
                .ok_or_else(|| "CMS2 vertex region overflow".to_string())?;
            let index_end = index_offset
                .checked_add(index_bytes)
                .ok_or_else(|| "CMS2 index region overflow".to_string())?;
            let edge_end = edge_offset
                .checked_add(edge_bytes)
                .ok_or_else(|| "CMS2 edge region overflow".to_string())?;
            if index_offset < vertex_end || edge_offset < index_end || edge_end > json_offset {
                return Err("CMS2 prototype regions overlap".to_string());
            }
            let vertices = read_vertices(bytes, vertex_offset, vertex_count)?;
            let indices = read_indices(bytes, index_offset, index_count)?;
            let edges = read_edges(bytes, edge_offset, edge_count)?;
            check_face_spans(&proto_meta.face_table, vertices.len(), indices.len())?;
            check_edge_spans(&proto_meta.edge_table, edges.len())?;
            prototypes.push(PartPrototype {
                part_name: proto_meta.part_name,
                anchor_mm: proto_meta.anchor_mm,
                vertices,
                indices,
                edges,
                face_table: proto_meta.face_table,
                edge_table: proto_meta.edge_table,
            });
        }

        for instance in &meta.instances {
            if instance.prototype_index as usize >= prototypes.len() {
                return Err("CMS2 instance points at a missing prototype".to_string());
            }
        }

        Ok(Self {
            part_name: meta.part_name,
            kind: meta.kind,
            units: meta.units,
            prototypes,
            instances: meta.instances,
        })
    }
}

#[derive(Clone)]
struct Group {
    component_id: Option<String>,
    part_name: String,
    origin: [f64; 3],
    rotation: [f64; 3],
    faces: Vec<BRepFace>,
    edges: Vec<BRepEdge>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ProtoKey {
    part_name: String,
    words: Vec<i64>,
}

struct LocalFace {
    source: BRepFace,
    points: Vec<[f64; 3]>,
    normals: Vec<[f64; 3]>,
    center: [f64; 3],
    normal: [f64; 3],
    axis_origin: [f64; 3],
    axis_dir: [f64; 3],
}

struct LocalEdge {
    source: BRepEdge,
    points: Vec<[f64; 3]>,
}

struct LocalPart {
    key: ProtoKey,
    anchor: [f64; 3],
    faces: Vec<LocalFace>,
    edges: Vec<LocalEdge>,
}

impl LocalPart {
    fn from_group(group: &Group) -> Self {
        let mut faces = Vec::with_capacity(group.faces.len());
        let mut edges = Vec::with_capacity(group.edges.len());
        let mut cloud = Vec::new();
        for face in &group.faces {
            let local = LocalFace::from_world(face, group.origin, group.rotation);
            cloud.extend_from_slice(&local.points);
            faces.push(local);
        }
        for edge in &group.edges {
            let local = LocalEdge::from_world(edge, group.origin, group.rotation);
            cloud.extend_from_slice(&local.points);
            edges.push(local);
        }
        let anchor = anchor_from(&cloud);
        let mut words = Vec::new();
        for face in &faces {
            face.push_key(&mut words);
        }
        for edge in &edges {
            edge.push_key(&mut words);
        }
        Self {
            key: ProtoKey {
                part_name: group.part_name.clone(),
                words,
            },
            anchor,
            faces,
            edges,
        }
    }

    fn into_prototype(self, part_name: &str) -> PartPrototype {
        let mut prototype = PartPrototype {
            part_name: part_name.to_string(),
            anchor_mm: self.anchor,
            vertices: Vec::new(),
            indices: Vec::new(),
            edges: Vec::new(),
            face_table: Vec::with_capacity(self.faces.len()),
            edge_table: Vec::with_capacity(self.edges.len()),
        };
        for face in &self.faces {
            let slot = prototype.face_table.len() as u32;
            let vertex_begin = prototype.vertices.len() as u32;
            let index_begin = prototype.indices.len() as u32;
            let flags = vertex_flags(&face.source);
            for (point, normal) in face.points.iter().zip(&face.normals) {
                prototype.vertices.push(CadVertex {
                    position: sub_anchor(*point, self.anchor),
                    face_slot_id: slot,
                    normal: [normal[0] as f32, normal[1] as f32, normal[2] as f32],
                    flags,
                });
            }
            let vertex_count = face.points.len() as u32;
            for index in &face.source.indices {
                let local = *index;
                let absolute = if (local as usize) < face.points.len() {
                    vertex_begin.saturating_add(local)
                } else {
                    vertex_begin
                };
                prototype.indices.push(absolute);
            }
            prototype.face_table.push(FaceRecord {
                face_id: face.source.face_id.clone(),
                surface_type: face.source.surface_type,
                exact_interface: face.source.exact_interface,
                is_exact_analytic: matches!(
                    face.source.surface_type,
                    SurfaceType::Plane
                        | SurfaceType::Cylinder
                        | SurfaceType::Cone
                        | SurfaceType::Sphere
                        | SurfaceType::Torus
                ),
                plane_origin_mm: face.center,
                plane_normal: face.normal,
                axis_origin_mm: face.axis_origin,
                axis_dir: face.axis_dir,
                radius_mm: finite_or_zero(face.source.radius_hint.unwrap_or(0.0)),
                provenance_tags: face.source.provenance_tags.clone(),
                ast_span: face.source.ast_span,
                vertex_begin,
                vertex_count,
                index_begin,
                index_count: face.source.indices.len() as u32,
            });
        }
        for edge in &self.edges {
            let slot = prototype.edge_table.len() as u32;
            let segment_begin = prototype.edges.len() as u32;
            let style = edge_style(&edge.source);
            for pair in edge.points.windows(2) {
                prototype.edges.push(CadEdgeSegment {
                    point_a: sub_anchor(pair[0], self.anchor),
                    edge_slot_id: slot,
                    point_b: sub_anchor(pair[1], self.anchor),
                    style_flags: style,
                });
            }
            prototype.edge_table.push(EdgeRecord {
                edge_id: edge.source.edge_id.clone(),
                curve_type: edge.source.curve_type,
                provenance_tags: edge.source.provenance_tags.clone(),
                ast_span: edge.source.ast_span,
                segment_begin,
                segment_count: edge.points.len().saturating_sub(1) as u32,
            });
        }
        prototype
    }
}

impl LocalFace {
    fn from_world(face: &BRepFace, origin: [f64; 3], rotation: [f64; 3]) -> Self {
        let count = face.vertices.len() / 3;
        let mut points = Vec::with_capacity(count);
        let mut normals = Vec::with_capacity(count);
        for index in 0..count {
            points.push(to_local_point(
                read3(&face.vertices, index),
                origin,
                rotation,
            ));
            let raw = if index * 3 + 2 < face.normals.len() {
                read3(&face.normals, index)
            } else {
                face.normal_hint
            };
            normals.push(to_local_dir(raw, rotation));
        }
        let center = to_local_point(face.center_hint, origin, rotation);
        let (axis_origin, axis_dir) =
            axis_from_loops(face, origin, rotation).unwrap_or((center, [0.0; 3]));
        Self {
            source: face.clone(),
            points,
            normals,
            center,
            normal: to_local_dir(face.normal_hint, rotation),
            axis_origin,
            axis_dir,
        }
    }

    fn push_key(&self, words: &mut Vec<i64>) {
        words.push(surface_code(self.source.surface_type) as i64);
        words.push(i64::from(self.source.exact_interface as u8));
        words.push(q(self.source.radius_hint.unwrap_or(0.0)));
        push_point(words, self.center);
        push_point(words, self.normal);
        words.push(self.points.len() as i64);
        for point in &self.points {
            push_point(words, *point);
        }
        for normal in &self.normals {
            push_point(words, *normal);
        }
        words.push(self.source.indices.len() as i64);
        for index in &self.source.indices {
            words.push(*index as i64);
        }
        push_tags(words, &self.source.provenance_tags);
        push_str(words, &self.source.face_id);
    }
}

impl LocalEdge {
    fn from_world(edge: &BRepEdge, origin: [f64; 3], rotation: [f64; 3]) -> Self {
        let mut points = Vec::with_capacity(edge.polyline.len() / 3);
        for index in 0..edge.polyline.len() / 3 {
            points.push(to_local_point(
                read3(&edge.polyline, index),
                origin,
                rotation,
            ));
        }
        Self {
            source: edge.clone(),
            points,
        }
    }

    fn push_key(&self, words: &mut Vec<i64>) {
        words.push(curve_code(self.source.curve_type) as i64);
        words.push(self.points.len() as i64);
        for point in &self.points {
            push_point(words, *point);
        }
        push_tags(words, &self.source.provenance_tags);
        push_str(words, &self.source.edge_id);
    }
}

fn groups_of(shape: &SolidShape) -> Vec<Group> {
    if shape.components.is_empty() {
        return vec![Group {
            component_id: None,
            part_name: shape.part_name.clone(),
            origin: [0.0; 3],
            rotation: [0.0; 3],
            faces: shape.faces.clone(),
            edges: shape.edges.clone(),
        }];
    }

    let mut groups: Vec<Group> = shape
        .components
        .iter()
        .map(|component| Group {
            component_id: Some(component.component_id.clone()),
            part_name: component.part_name.clone(),
            origin: component.origin_mm,
            rotation: component.rotation_deg,
            faces: Vec::new(),
            edges: Vec::new(),
        })
        .collect();
    let index_of: HashMap<String, usize> = groups
        .iter()
        .enumerate()
        .filter_map(|(index, group)| group.component_id.clone().map(|id| (id, index)))
        .collect();
    let mut residual = Group {
        component_id: None,
        part_name: shape.part_name.clone(),
        origin: [0.0; 3],
        rotation: [0.0; 3],
        faces: Vec::new(),
        edges: Vec::new(),
    };
    for face in &shape.faces {
        match face
            .component_id
            .as_ref()
            .and_then(|id| index_of.get(id))
            .copied()
        {
            Some(index) => groups[index].faces.push(face.clone()),
            None => residual.faces.push(face.clone()),
        }
    }
    for edge in &shape.edges {
        match edge
            .component_id
            .as_ref()
            .and_then(|id| index_of.get(id))
            .copied()
        {
            Some(index) => groups[index].edges.push(edge.clone()),
            None => residual.edges.push(edge.clone()),
        }
    }
    if !residual.faces.is_empty() || !residual.edges.is_empty() {
        groups.push(residual);
    }
    groups
}

fn axis_from_loops(
    face: &BRepFace,
    origin: [f64; 3],
    rotation: [f64; 3],
) -> Option<([f64; 3], [f64; 3])> {
    let bottom = centroid(&face.outer_loop)?;
    let top = centroid(face.top_loop.as_deref()?)?;
    let bottom = to_local_point(bottom, origin, rotation);
    let top = to_local_point(top, origin, rotation);
    let delta = [top[0] - bottom[0], top[1] - bottom[1], top[2] - bottom[2]];
    let length = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
    if length < 1e-12 {
        return None;
    }
    Some((
        [
            0.5 * (bottom[0] + top[0]),
            0.5 * (bottom[1] + top[1]),
            0.5 * (bottom[2] + top[2]),
        ],
        [delta[0] / length, delta[1] / length, delta[2] / length],
    ))
}

fn centroid(points: &[[f64; 3]]) -> Option<[f64; 3]> {
    if points.is_empty() {
        return None;
    }
    let scale = points.len() as f64;
    let mut sum = [0.0; 3];
    for point in points {
        sum[0] += point[0];
        sum[1] += point[1];
        sum[2] += point[2];
    }
    Some([sum[0] / scale, sum[1] / scale, sum[2] / scale])
}

fn anchor_from(points: &[[f64; 3]]) -> [f64; 3] {
    let mut finite = points
        .iter()
        .copied()
        .filter(|point| point.iter().all(|v| v.is_finite()));
    let Some(first) = finite.next() else {
        return [0.0; 3];
    };
    let mut min = first;
    let mut max = first;
    for point in finite {
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    [
        0.5 * (min[0] + max[0]),
        0.5 * (min[1] + max[1]),
        0.5 * (min[2] + max[2]),
    ]
}

fn to_local_point(point: [f64; 3], origin: [f64; 3], rotation: [f64; 3]) -> [f64; 3] {
    let placed = Mat4::translate(origin[0], origin[1], origin[2]).mul_mat(Mat4::rotate_euler_deg(
        rotation[0],
        rotation[1],
        rotation[2],
    ));
    finite3(placed.inverse_rigid().transform_point(finite3(point)))
}

fn to_local_dir(direction: [f64; 3], rotation: [f64; 3]) -> [f64; 3] {
    let matrix = Mat4::rotate_euler_deg(rotation[0], rotation[1], rotation[2]);
    let direction = finite3(direction);
    normalize([
        matrix.m[0][0] * direction[0]
            + matrix.m[1][0] * direction[1]
            + matrix.m[2][0] * direction[2],
        matrix.m[0][1] * direction[0]
            + matrix.m[1][1] * direction[1]
            + matrix.m[2][1] * direction[2],
        matrix.m[0][2] * direction[0]
            + matrix.m[1][2] * direction[1]
            + matrix.m[2][2] * direction[2],
    ])
}

fn vertex_flags(face: &BRepFace) -> u32 {
    let mut flags = (surface_code(face.surface_type) & 0x3F) << 2;
    if face.exact_interface {
        flags |= 1;
    }
    if face.provenance_tags.iter().any(|tag| {
        let lower = tag.to_ascii_lowercase();
        lower.contains("precision") || lower.contains("bore")
    }) {
        flags |= 2;
    }
    flags
}

fn edge_style(edge: &BRepEdge) -> u32 {
    let mut flags = 0;
    for tag in &edge.provenance_tags {
        let lower = tag.to_ascii_lowercase();
        if lower.contains("hidden") {
            flags |= 1;
        }
        if lower.contains("silhouette") {
            flags |= 2;
        }
        if lower.contains("precision") {
            flags |= 4;
        }
    }
    flags
}

fn surface_code(kind: SurfaceType) -> u32 {
    match kind {
        SurfaceType::Plane => 0,
        SurfaceType::Cylinder => 1,
        SurfaceType::Cone => 2,
        SurfaceType::Sphere => 3,
        SurfaceType::Torus => 4,
        SurfaceType::Bspline => 5,
        SurfaceType::Mesh => 6,
    }
}

fn curve_code(kind: CurveType) -> u32 {
    match kind {
        CurveType::Line => 0,
        CurveType::Circle => 1,
        CurveType::Arc => 2,
        CurveType::Ellipse => 3,
        CurveType::Bspline => 4,
        CurveType::Polyline => 5,
    }
}

fn sub_anchor(point: [f64; 3], anchor: [f64; 3]) -> [f32; 3] {
    [
        finite_f32(point[0] - anchor[0]),
        finite_f32(point[1] - anchor[1]),
        finite_f32(point[2] - anchor[2]),
    ]
}

fn read3(values: &[f64], index: usize) -> [f64; 3] {
    let start = index * 3;
    [values[start], values[start + 1], values[start + 2]]
}

fn push_point(words: &mut Vec<i64>, point: [f64; 3]) {
    words.push(q(point[0]));
    words.push(q(point[1]));
    words.push(q(point[2]));
}

fn push_tags(words: &mut Vec<i64>, tags: &[String]) {
    words.push(tags.len() as i64);
    for tag in tags {
        push_str(words, tag);
    }
}

fn push_str(words: &mut Vec<i64>, text: &str) {
    words.push(text.len() as i64);
    for byte in text.bytes() {
        words.push(i64::from(byte));
    }
}

fn q(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    (value * 1_000_000.0).round() as i64
}

fn finite3(point: [f64; 3]) -> [f64; 3] {
    if point.iter().all(|value| value.is_finite()) {
        point
    } else {
        [0.0; 3]
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn finite_f32(value: f64) -> f32 {
    if value.is_finite() { value as f32 } else { 0.0 }
}

fn normalize(value: [f64; 3]) -> [f64; 3] {
    let length = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt();
    if length < 1e-12 {
        [0.0; 3]
    } else {
        [value[0] / length, value[1] / length, value[2] / length]
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Cms2Meta {
    part_name: String,
    kind: String,
    units: String,
    prototypes: Vec<ProtoMeta>,
    instances: Vec<SceneInstance>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProtoMeta {
    part_name: String,
    anchor_mm: [f64; 3],
    face_table: Vec<FaceRecord>,
    edge_table: Vec<EdgeRecord>,
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_vertices(out: &mut Vec<u8>, vertices: &[CadVertex]) {
    for vertex in vertices {
        for value in vertex.position {
            write_f32(out, value);
        }
        write_u32(out, vertex.face_slot_id);
        for value in vertex.normal {
            write_f32(out, value);
        }
        write_u32(out, vertex.flags);
    }
}

fn write_edges(out: &mut Vec<u8>, edges: &[CadEdgeSegment]) {
    for edge in edges {
        for value in edge.point_a {
            write_f32(out, value);
        }
        write_u32(out, edge.edge_slot_id);
        for value in edge.point_b {
            write_f32(out, value);
        }
        write_u32(out, edge.style_flags);
    }
}

fn pad_16(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(16) {
        out.push(0);
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated CMS2".to_string())?;
    Ok(u32::from_le_bytes(slice.try_into().expect("4 bytes")))
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated CMS2".to_string())?;
    Ok(f32::from_le_bytes(slice.try_into().expect("4 bytes")))
}

fn read_vertices(bytes: &[u8], offset: usize, count: usize) -> Result<Vec<CadVertex>, String> {
    let mut vertices = Vec::with_capacity(count);
    for index in 0..count {
        let at = offset + index * 32;
        vertices.push(CadVertex {
            position: [
                read_f32(bytes, at)?,
                read_f32(bytes, at + 4)?,
                read_f32(bytes, at + 8)?,
            ],
            face_slot_id: read_u32(bytes, at + 12)?,
            normal: [
                read_f32(bytes, at + 16)?,
                read_f32(bytes, at + 20)?,
                read_f32(bytes, at + 24)?,
            ],
            flags: read_u32(bytes, at + 28)?,
        });
    }
    Ok(vertices)
}

fn read_indices(bytes: &[u8], offset: usize, count: usize) -> Result<Vec<u32>, String> {
    let mut indices = Vec::with_capacity(count);
    for index in 0..count {
        indices.push(read_u32(bytes, offset + index * 4)?);
    }
    Ok(indices)
}

fn read_edges(bytes: &[u8], offset: usize, count: usize) -> Result<Vec<CadEdgeSegment>, String> {
    let mut edges = Vec::with_capacity(count);
    for index in 0..count {
        let at = offset + index * 32;
        edges.push(CadEdgeSegment {
            point_a: [
                read_f32(bytes, at)?,
                read_f32(bytes, at + 4)?,
                read_f32(bytes, at + 8)?,
            ],
            edge_slot_id: read_u32(bytes, at + 12)?,
            point_b: [
                read_f32(bytes, at + 16)?,
                read_f32(bytes, at + 20)?,
                read_f32(bytes, at + 24)?,
            ],
            style_flags: read_u32(bytes, at + 28)?,
        });
    }
    Ok(edges)
}

fn check_range(
    offset: usize,
    length: usize,
    blob_start: usize,
    json_offset: usize,
) -> Result<(), String> {
    if length == 0 {
        return Ok(());
    }
    if offset < blob_start || offset.saturating_add(length) > json_offset {
        return Err("CMS2 binary region is outside the file".to_string());
    }
    Ok(())
}

fn check_face_spans(
    faces: &[FaceRecord],
    vertex_count: usize,
    index_count: usize,
) -> Result<(), String> {
    let mut vertex_cursor = 0u32;
    let mut index_cursor = 0u32;
    for face in faces {
        if face.vertex_begin != vertex_cursor || face.index_begin != index_cursor {
            return Err("CMS2 face ranges are not packed".to_string());
        }
        vertex_cursor = vertex_cursor
            .checked_add(face.vertex_count)
            .ok_or_else(|| "CMS2 face range overflow".to_string())?;
        index_cursor = index_cursor
            .checked_add(face.index_count)
            .ok_or_else(|| "CMS2 index range overflow".to_string())?;
    }
    if vertex_cursor as usize != vertex_count || index_cursor as usize != index_count {
        return Err("CMS2 face ranges do not cover the mesh".to_string());
    }
    Ok(())
}

fn check_edge_spans(edges: &[EdgeRecord], segment_count: usize) -> Result<(), String> {
    let mut cursor = 0u32;
    for edge in edges {
        if edge.segment_begin != cursor {
            return Err("CMS2 edge ranges are not packed".to_string());
        }
        cursor = cursor
            .checked_add(edge.segment_count)
            .ok_or_else(|| "CMS2 edge range overflow".to_string())?;
    }
    if cursor as usize != segment_count {
        return Err("CMS2 edge ranges do not cover the segments".to_string());
    }
    Ok(())
}

/// Packs companion UTF-8 JSON metadata (e.g. diagnostics, elapsed_ms, edit handles)
/// together with a 16-byte-aligned `CMS2` binary payload into a `CM2E` IPC envelope.
///
/// Binary layout (all integers little-endian):
/// - `0..4`:   magic `b"CM2E"`
/// - `4..8`:   version (`1`)
/// - `8..12`:  `cms2_offset` (`32`, 16-byte aligned for zero-copy typed-array views)
/// - `12..16`: `cms2_len`
/// - `16..20`: `metadata_offset` (`cms2_offset + cms2_len`)
/// - `20..24`: `metadata_len`
/// - `24..28`: flags (`0`)
/// - `28..32`: reserved (`0`)
/// - `cms2_offset..cms2_offset+cms2_len`: raw `CMS2` bytes
/// - `metadata_offset..metadata_offset+metadata_len`: UTF-8 JSON bytes
pub fn encode_cms2_envelope(metadata_json: &[u8], cms2_bytes: &[u8]) -> Result<Vec<u8>, String> {
    if cms2_bytes.get(0..4) != Some(CMS2_MAGIC.as_slice()) {
        return Err("CMS2 envelope payload must start with CMS2 magic".to_string());
    }
    let cms2_offset = CMS2_ENVELOPE_HEADER_BYTES;
    let cms2_len =
        u32::try_from(cms2_bytes.len()).map_err(|_| "CMS2 payload exceeds u32 size limit")?;
    let metadata_offset_usize = cms2_offset
        .checked_add(cms2_bytes.len())
        .ok_or_else(|| "CMS2 envelope size overflow".to_string())?;
    let metadata_offset = u32::try_from(metadata_offset_usize)
        .map_err(|_| "CMS2 envelope metadata offset exceeds u32")?;
    let metadata_len =
        u32::try_from(metadata_json.len()).map_err(|_| "CMS2 metadata exceeds u32 size limit")?;
    let total_len = metadata_offset_usize
        .checked_add(metadata_json.len())
        .ok_or_else(|| "CMS2 envelope total size overflow".to_string())?;

    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(CMS2_ENVELOPE_MAGIC);
    write_u32(&mut out, CMS2_ENVELOPE_VERSION);
    write_u32(&mut out, cms2_offset as u32);
    write_u32(&mut out, cms2_len);
    write_u32(&mut out, metadata_offset);
    write_u32(&mut out, metadata_len);
    write_u32(&mut out, 0);
    write_u32(&mut out, 0);
    debug_assert_eq!(out.len(), CMS2_ENVELOPE_HEADER_BYTES);
    out.extend_from_slice(cms2_bytes);
    out.extend_from_slice(metadata_json);
    Ok(out)
}

/// Validates and slices a `CM2E` IPC envelope into `(metadata_json, cms2_bytes)` without copying.
pub fn decode_cms2_envelope(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    if bytes.len() < CMS2_ENVELOPE_HEADER_BYTES {
        return Err("truncated CMS2 envelope header".to_string());
    }
    if bytes.get(0..4) != Some(CMS2_ENVELOPE_MAGIC.as_slice()) {
        return Err("CMS2 envelope magic mismatch".to_string());
    }
    let version = read_u32(bytes, 4)?;
    if version != CMS2_ENVELOPE_VERSION {
        return Err(format!("unsupported CMS2 envelope version {version}"));
    }
    let cms2_offset = read_u32(bytes, 8)? as usize;
    let cms2_len = read_u32(bytes, 12)? as usize;
    let metadata_offset = read_u32(bytes, 16)? as usize;
    let metadata_len = read_u32(bytes, 20)? as usize;
    if read_u32(bytes, 24)? != 0 || read_u32(bytes, 28)? != 0 {
        return Err("unsupported CMS2 envelope flags".to_string());
    }
    if cms2_offset < CMS2_ENVELOPE_HEADER_BYTES || !cms2_offset.is_multiple_of(16) {
        return Err("CMS2 envelope payload offset must be >= 32 and 16-byte aligned".to_string());
    }
    let cms2_end = cms2_offset
        .checked_add(cms2_len)
        .ok_or_else(|| "CMS2 envelope payload range overflow".to_string())?;
    let metadata_end = metadata_offset
        .checked_add(metadata_len)
        .ok_or_else(|| "CMS2 envelope metadata range overflow".to_string())?;
    if cms2_end > metadata_offset || metadata_end != bytes.len() {
        return Err("CMS2 envelope regions are out of bounds".to_string());
    }
    let cms2_slice = &bytes[cms2_offset..cms2_end];
    if cms2_slice.get(0..4) != Some(CMS2_MAGIC.as_slice()) {
        return Err("CMS2 envelope payload magic mismatch".to_string());
    }
    let metadata_slice = &bytes[metadata_offset..metadata_end];
    Ok((metadata_slice, cms2_slice))
}

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, Range};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{
    Deserialize, Deserializer,
    de::{Error, IgnoredAny, SeqAccess, Visitor},
};

use crate::{
    LengthUnit, MAX_INPUT_BYTES, Mesh, MeshError, Vec3, add, check_size, cross, dot, invalid, mul,
};

const MAX_JSON_BYTES: usize = 96 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_BUFFERS: usize = 256;
const MAX_GLB_CHUNKS: usize = 256;
const MAX_NODE_DEPTH: usize = 128;
const JSON_CHUNK: &[u8; 4] = b"JSON";
const BIN_CHUNK: &[u8; 4] = b"BIN\0";
const UNSUPPORTED_BUFFER_URI_MESSAGE: &str = "external or non-Base64 buffer URIs are not supported; only GLB or glTF with embedded Base64 data URIs are supported — convert to .glb before importing";

#[derive(Debug, Clone, PartialEq)]
pub struct GltfDocument {
    pub mesh: Mesh,
    pub primitive_groups: Vec<GltfPrimitiveGroup>,
    /// Labels by source material index; no material or texture assets are loaded.
    pub material_names: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GltfPrimitiveGroup {
    pub node_index: usize,
    pub mesh_index: usize,
    pub primitive_index: usize,
    pub material_index: Option<usize>,
    /// Half-open range indexing [`GltfDocument::mesh`]'s triangles.
    pub triangle_range: Range<usize>,
}

/// Read self-contained glTF 2.0 or GLB static triangle geometry. glTF coordinates
/// are meters; the returned mesh is in millimeters. External buffers and images
/// are never loaded. Use [`Mesh::validate_closed_manifold`] to require closure.
pub fn load_gltf(bytes: &[u8]) -> Result<Mesh, MeshError> {
    load_gltf_with_source_unit(bytes, None)
}

/// Read glTF geometry with an optional override for the source length unit.
pub fn load_gltf_with_source_unit(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
) -> Result<Mesh, MeshError> {
    Ok(read_document(bytes, source_unit, false)?.mesh)
}

/// Read the flattened initial scene and its primitive/material groups. The
/// default scene is used, or the first scene if none is marked as default.
pub fn load_gltf_document(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
) -> Result<GltfDocument, MeshError> {
    read_document(bytes, source_unit, true)
}

fn read_document(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
    collect_groups: bool,
) -> Result<GltfDocument, MeshError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(MeshError::ResourceLimit("glTF input bytes"));
    }
    let (json, bin) = if bytes.starts_with(b"glTF") {
        read_glb(bytes)?
    } else {
        (bytes, None)
    };
    if json.len() > MAX_JSON_BYTES {
        return Err(MeshError::ResourceLimit("glTF JSON bytes"));
    }
    let json: Root = serde_json::from_slice(json.strip_prefix(b"\xef\xbb\xbf").unwrap_or(json))
        .map_err(|error| invalid(format!("glTF JSON: {error}")))?;
    if json.asset.version != "2.0"
        || json
            .asset
            .min_version
            .as_deref()
            .is_some_and(|v| v != "2.0")
    {
        return Err(gltf_error("only version 2.0 is supported"));
    }
    if !json.extensions_required.is_empty() {
        return Err(gltf_error("required extensions are not supported"));
    }
    let buffers = read_buffers(&json.buffers, bin)?;
    let mut output = SceneReader {
        root: &json,
        buffers: &buffers,
        visited: vec![false; json.nodes.len()],
        mesh: Mesh::default(),
        groups: collect_groups.then(Vec::new),
        scale: source_unit
            .unwrap_or(LengthUnit::Meter)
            .millimeters_per_unit(),
    };
    if !json.scenes.is_empty() || json.scene.is_some() {
        let scene = json
            .scenes
            .get(json.scene.unwrap_or(0))
            .ok_or_else(|| gltf_error("default scene index is out of range"))?;
        for &node in scene.nodes.iter() {
            output.node(node, Transform::IDENTITY, 0)?;
        }
    }
    output.mesh.validate_geometry()?;
    Ok(GltfDocument {
        mesh: output.mesh,
        primitive_groups: output.groups.unwrap_or_default(),
        material_names: if collect_groups {
            json.materials.0.into_iter().map(|m| m.name).collect()
        } else {
            Vec::new()
        },
    })
}

fn gltf_error(message: &str) -> MeshError {
    invalid(format!("glTF: {message}"))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_glb(bytes: &[u8]) -> Result<(&[u8], Option<&[u8]>), MeshError> {
    if u32_at(bytes, 4) != Some(2) || u32_at(bytes, 8).map(|len| len as usize) != Some(bytes.len())
    {
        return Err(gltf_error("invalid GLB version or file length"));
    }
    let mut rest = &bytes[12..];
    let mut json = None;
    let mut bin = None;
    let mut index = 0;
    while !rest.is_empty() {
        if index == MAX_GLB_CHUNKS {
            return Err(MeshError::ResourceLimit("GLB chunk count"));
        }
        let len = u32_at(rest, 0).ok_or_else(|| gltf_error("truncated GLB chunk header"))? as usize;
        let kind = rest
            .get(4..8)
            .ok_or_else(|| gltf_error("truncated GLB chunk type"))?;
        let data = 8_usize
            .checked_add(len)
            .and_then(|end| rest.get(8..end))
            .ok_or_else(|| gltf_error("truncated GLB chunk data"))?;
        if !len.is_multiple_of(4) {
            return Err(gltf_error("GLB chunk length is not 4-byte aligned"));
        }
        match kind {
            k if k == JSON_CHUNK && index == 0 => json = Some(data),
            k if k == BIN_CHUNK && index == 1 => bin = Some(data),
            k if k == JSON_CHUNK || k == BIN_CHUNK => {
                return Err(gltf_error("out-of-order or repeated GLB chunk"));
            }
            _ if index == 0 => return Err(gltf_error("first GLB chunk must contain JSON")),
            _ => {} // The GLB spec permits unknown trailing chunks.
        }
        index += 1;
        rest = &rest[8 + len..];
    }
    Ok((
        json.ok_or_else(|| gltf_error("missing GLB JSON chunk"))?,
        bin,
    ))
}

fn read_buffers<'a>(
    buffers: &[Buffer],
    mut bin: Option<&'a [u8]>,
) -> Result<Vec<Cow<'a, [u8]>>, MeshError> {
    let mut bytes = 0_usize;
    let mut data = Vec::with_capacity(buffers.len());
    for (index, buffer) in buffers.iter().enumerate() {
        let len = buffer.byte_length;
        bytes = bytes
            .checked_add(len)
            .filter(|&total| total <= MAX_INPUT_BYTES)
            .ok_or(MeshError::ResourceLimit("glTF total buffer bytes"))?;
        if len == 0 {
            return Err(gltf_error("buffer byteLength is zero"));
        }
        if let Some(uri) = &buffer.uri {
            let payload = [
                "data:application/octet-stream;base64,",
                "data:application/gltf-buffer;base64,",
                "data:;base64,",
            ]
            .into_iter()
            .find_map(|prefix| uri.strip_prefix(prefix))
            .ok_or_else(|| gltf_error(UNSUPPORTED_BUFFER_URI_MESSAGE))?;
            if payload.len() != len.div_ceil(3) * 4 {
                return Err(gltf_error("embedded buffer does not match byteLength"));
            }
            let decoded = STANDARD
                .decode(payload)
                .map_err(|_| gltf_error("invalid Base64 in embedded buffer"))?;
            if decoded.len() != len {
                return Err(gltf_error("embedded buffer does not match byteLength"));
            }
            data.push(Cow::Owned(decoded));
        } else {
            let chunk = bin.take().filter(|_| index == 0).ok_or_else(|| {
                gltf_error("a buffer without a URI requires the first GLB BIN chunk")
            })?;
            if chunk.len() < len || chunk.len() - len > 3 || chunk[len..].iter().any(|&b| b != 0) {
                return Err(gltf_error(
                    "GLB BIN chunk does not match buffer byteLength or padding",
                ));
            }
            data.push(Cow::Borrowed(&chunk[..len]));
        }
    }
    if bin.is_some() {
        return Err(gltf_error("GLB BIN chunk has no matching buffer"));
    }
    Ok(data)
}

// Bound array allocation while decoding JSON, not only after deserialization.
struct Entries<T, const N: usize = MAX_ENTRIES>(Vec<T>);

impl<T, const N: usize> Default for Entries<T, N> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T, const N: usize> Deref for Entries<T, N> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for Entries<T, N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor<T, const N: usize>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for EntryVisitor<T, N> {
            type Value = Entries<T, N>;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                write!(formatter, "an array of at most {N} entries")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while items.len() < N {
                    match seq.next_element()? {
                        Some(item) => items.push(item),
                        None => return Ok(Entries(items)),
                    }
                }
                if seq.next_element::<IgnoredAny>()?.is_some() {
                    return Err(A::Error::invalid_length(N + 1, &self));
                }
                Ok(Entries(items))
            }
        }
        deserializer.deserialize_seq(EntryVisitor::<T, N>(PhantomData))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Root {
    asset: Asset,
    scene: Option<usize>,
    #[serde(default)]
    scenes: Entries<Scene>,
    #[serde(default)]
    nodes: Entries<Node>,
    #[serde(default)]
    meshes: Entries<SourceMesh>,
    #[serde(default)]
    materials: Entries<Material>,
    #[serde(default)]
    buffers: Entries<Buffer, MAX_BUFFERS>,
    #[serde(default)]
    buffer_views: Entries<BufferView>,
    #[serde(default)]
    accessors: Entries<Accessor>,
    #[serde(default)]
    extensions_required: Entries<IgnoredAny, 256>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Asset {
    version: String,
    min_version: Option<String>,
}

#[derive(Deserialize)]
struct Scene {
    #[serde(default)]
    nodes: Entries<usize>,
}

#[derive(Deserialize)]
struct Node {
    mesh: Option<usize>,
    skin: Option<usize>,
    matrix: Option<[f64; 16]>,
    translation: Option<Vec3>,
    rotation: Option<[f64; 4]>,
    scale: Option<Vec3>,
    #[serde(default)]
    children: Entries<usize>,
}

#[derive(Deserialize)]
struct SourceMesh {
    primitives: Entries<Primitive>,
}

#[derive(Deserialize)]
struct Primitive {
    attributes: Attributes,
    indices: Option<usize>,
    mode: Option<u32>,
    material: Option<usize>,
    targets: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct Attributes {
    #[serde(rename = "POSITION")]
    position: usize,
}

#[derive(Deserialize)]
struct Material {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Buffer {
    byte_length: usize,
    uri: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BufferView {
    buffer: usize,
    #[serde(default)]
    byte_offset: usize,
    byte_length: usize,
    byte_stride: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Accessor {
    buffer_view: Option<usize>,
    #[serde(default)]
    byte_offset: usize,
    component_type: u32,
    count: usize,
    #[serde(rename = "type")]
    shape: String,
    #[serde(default)]
    normalized: bool,
    sparse: Option<IgnoredAny>,
}

struct Access<'a> {
    bytes: &'a [u8],
    stride: usize,
    element: usize,
    count: usize,
}

impl Access<'_> {
    fn get(&self, i: usize) -> &[u8] {
        // These bounds were checked over the whole accessor when Access was built.
        let start = self.stride * i;
        &self.bytes[start..start + self.element]
    }
}

struct SceneReader<'a> {
    root: &'a Root,
    buffers: &'a [Cow<'a, [u8]>],
    visited: Vec<bool>,
    mesh: Mesh,
    groups: Option<Vec<GltfPrimitiveGroup>>,
    scale: f64,
}

impl SceneReader<'_> {
    fn node(&mut self, index: usize, parent: Transform, depth: usize) -> Result<(), MeshError> {
        if depth >= MAX_NODE_DEPTH {
            return Err(MeshError::ResourceLimit("glTF node depth"));
        }
        let root = self.root;
        let node = root
            .nodes
            .get(index)
            .ok_or_else(|| gltf_error("node index out of range"))?;
        if std::mem::replace(&mut self.visited[index], true) {
            return Err(gltf_error(
                "node hierarchy contains a cycle or a repeated node",
            ));
        }
        if node.skin.is_some() {
            return Err(gltf_error("skinned geometry is not supported"));
        }
        let transform = parent.compose(node.transform()?)?;
        if let Some(mesh_index) = node.mesh {
            let mesh = root
                .meshes
                .get(mesh_index)
                .ok_or_else(|| gltf_error("mesh index out of range"))?;
            if mesh.primitives.is_empty() {
                return Err(gltf_error("mesh has no primitives"));
            }
            let mut positions = HashMap::new();
            let reflected = transform.reflected();
            for (primitive_index, primitive) in mesh.primitives.iter().enumerate() {
                let start = self.mesh.triangles.len();
                self.primitive(primitive, transform, reflected, &mut positions)?;
                if let Some(groups) = &mut self.groups {
                    if groups.len() == MAX_ENTRIES {
                        return Err(MeshError::ResourceLimit("glTF primitive group count"));
                    }
                    groups.push(GltfPrimitiveGroup {
                        node_index: index,
                        mesh_index,
                        primitive_index,
                        material_index: primitive.material,
                        triangle_range: start..self.mesh.triangles.len(),
                    });
                }
            }
        }
        for &child in node.children.iter() {
            self.node(child, transform, depth + 1)?;
        }
        Ok(())
    }

    fn primitive(
        &mut self,
        primitive: &Primitive,
        transform: Transform,
        reflected: bool,
        positions: &mut HashMap<usize, u32>,
    ) -> Result<(), MeshError> {
        if primitive.mode.unwrap_or(4) != 4 {
            return Err(gltf_error("only TRIANGLES primitives are supported"));
        }
        if primitive.targets.is_some() {
            return Err(gltf_error("morph targets are not supported"));
        }
        if primitive
            .material
            .is_some_and(|index| index >= self.root.materials.len())
        {
            return Err(gltf_error("material index out of range"));
        }
        let position_index = primitive.attributes.position;
        let position = self
            .root
            .accessors
            .get(position_index)
            .ok_or_else(|| gltf_error("position accessor index out of range"))?;
        if position.component_type != 5126 || position.shape != "VEC3" {
            return Err(gltf_error(
                "POSITION accessor must contain float32 VEC3 values",
            ));
        }
        let indices = primitive
            .indices
            .map(|index| {
                let accessor = self
                    .root
                    .accessors
                    .get(index)
                    .ok_or_else(|| gltf_error("index accessor out of range"))?;
                let size = match accessor.component_type {
                    5121 => 1,
                    5123 => 2,
                    5125 => 4,
                    _ => return Err(gltf_error("index accessor must use unsigned integers")),
                };
                if accessor.shape != "SCALAR" {
                    return Err(gltf_error("index accessor must contain SCALAR values"));
                }
                Self::access(self.root, self.buffers, accessor, size, size, false)
            })
            .transpose()?;
        let count = indices.as_ref().map_or(position.count, |i| i.count);
        if count == 0 || !count.is_multiple_of(3) {
            return Err(gltf_error(
                "triangle index count is not a positive multiple of three",
            ));
        }
        let triangles = self.mesh.triangles.len() + count / 3; // count / 3 leaves room for existing triangles.
        let base = if let Some(&first) = positions.get(&position_index) {
            check_size(self.mesh.vertices.len(), triangles)?;
            first
        } else {
            let first = self.mesh.vertices.len();
            let vertices = first
                .checked_add(position.count)
                .ok_or(MeshError::ResourceLimit("glTF vertex count"))?;
            check_size(vertices, triangles)?;
            let access = Self::access(self.root, self.buffers, position, 12, 4, true)?;
            for i in 0..position.count {
                let bytes = access.get(i);
                let point = std::array::from_fn(|axis| {
                    f32::from_le_bytes(bytes[axis * 4..axis * 4 + 4].try_into().unwrap()) as f64
                });
                let point = mul(transform.point(point), self.scale);
                let vertex = self.mesh.vertices.len();
                if !point.iter().all(|v| v.is_finite()) {
                    return Err(MeshError::NonFiniteVertex { vertex });
                }
                self.mesh.vertices.push(point);
            }
            positions.insert(position_index, first as u32);
            first as u32
        };
        for i in (0..count).step_by(3) {
            let mut triangle = [0; 3];
            for (offset, vertex) in triangle.iter_mut().enumerate() {
                let local = if let Some(indices) = &indices {
                    read_index(indices.get(i + offset))
                } else {
                    (i + offset) as u32
                };
                if local as usize >= position.count {
                    return Err(MeshError::InvalidIndex {
                        triangle: self.mesh.triangles.len(),
                        vertex: local,
                    });
                }
                *vertex = base + local;
            }
            if reflected {
                triangle.swap(1, 2);
            }
            self.mesh.triangles.push(triangle);
        }
        Ok(())
    }

    fn access<'a>(
        root: &Root,
        buffers: &'a [Cow<'_, [u8]>],
        accessor: &Accessor,
        element: usize,
        alignment: usize,
        vertex: bool,
    ) -> Result<Access<'a>, MeshError> {
        if accessor.sparse.is_some() || accessor.normalized || accessor.count == 0 {
            return Err(gltf_error(
                "sparse, normalized, or empty accessors are not supported",
            ));
        }
        let view = accessor
            .buffer_view
            .and_then(|i| root.buffer_views.get(i))
            .ok_or_else(|| gltf_error("missing or invalid accessor bufferView"))?;
        let buffer = buffers
            .get(view.buffer)
            .ok_or_else(|| gltf_error("buffer index out of range"))?;
        let view_end = view
            .byte_offset
            .checked_add(view.byte_length)
            .ok_or_else(|| gltf_error("bufferView range overflow"))?;
        let view_bytes = buffer
            .get(view.byte_offset..view_end)
            .ok_or_else(|| gltf_error("bufferView range exceeds its buffer"))?;
        let absolute_offset = view
            .byte_offset
            .checked_add(accessor.byte_offset)
            .ok_or_else(|| gltf_error("accessor offset overflow"))?;
        if !accessor.byte_offset.is_multiple_of(alignment)
            || !absolute_offset.is_multiple_of(alignment)
        {
            return Err(gltf_error("accessor is misaligned"));
        }
        let stride = view.byte_stride.unwrap_or(element);
        if view.byte_stride.is_some()
            && (!vertex || stride < element || stride > 252 || !stride.is_multiple_of(4))
        {
            return Err(gltf_error("invalid bufferView byteStride"));
        }
        let end = (accessor.count - 1)
            .checked_mul(stride)
            .and_then(|offset| offset.checked_add(element))
            .and_then(|len| accessor.byte_offset.checked_add(len))
            .ok_or_else(|| gltf_error("accessor range overflow"))?;
        let bytes = view_bytes
            .get(accessor.byte_offset..end)
            .ok_or_else(|| gltf_error("accessor range exceeds its bufferView"))?;
        Ok(Access {
            bytes,
            stride,
            element,
            count: accessor.count,
        })
    }
}

fn read_index(bytes: &[u8]) -> u32 {
    match bytes {
        [a] => u32::from(*a),
        [a, b] => u32::from(u16::from_le_bytes([*a, *b])),
        [a, b, c, d] => u32::from_le_bytes([*a, *b, *c, *d]),
        _ => unreachable!("validated index component width"),
    }
}

// Column-major affine matrix: 3 basis columns and a translation.
#[derive(Clone, Copy)]
struct Transform([Vec3; 4]);

impl Transform {
    const IDENTITY: Self = Self([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]]);

    fn checked(columns: [Vec3; 4]) -> Result<Self, MeshError> {
        if !columns.iter().flatten().all(|x| x.is_finite()) {
            return Err(gltf_error("node transform is not finite"));
        }
        Ok(Self(columns))
    }

    fn direction(self, vector: Vec3) -> Vec3 {
        add(
            add(mul(self.0[0], vector[0]), mul(self.0[1], vector[1])),
            mul(self.0[2], vector[2]),
        )
    }

    fn point(self, point: Vec3) -> Vec3 {
        add(self.direction(point), self.0[3])
    }

    fn compose(self, local: Self) -> Result<Self, MeshError> {
        let [a, b, c, t] = local.0;
        Self::checked([
            self.direction(a),
            self.direction(b),
            self.direction(c),
            self.point(t),
        ])
    }

    fn reflected(self) -> bool {
        let [a, b, c, _] = self.0.map(|column| {
            let scale = column.iter().fold(0.0_f64, |scale, x| scale.max(x.abs()));
            if scale == 0.0 {
                column
            } else {
                column.map(|x| x / scale)
            }
        });
        dot(cross(a, b), c) < 0.0
    }
}

impl Node {
    fn transform(&self) -> Result<Transform, MeshError> {
        if let Some(matrix) = self.matrix {
            if self.translation.is_some() || self.rotation.is_some() || self.scale.is_some() {
                return Err(gltf_error("node cannot specify both matrix and TRS"));
            }
            if [matrix[3], matrix[7], matrix[11], matrix[15]] != [0.0, 0.0, 0.0, 1.0] {
                return Err(gltf_error("node matrix is not affine"));
            }
            return Transform::checked(std::array::from_fn(|i| {
                [matrix[i * 4], matrix[i * 4 + 1], matrix[i * 4 + 2]]
            }));
        }
        let [x, y, z, w] = self.rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let norm = x.hypot(y).hypot(z).hypot(w);
        if norm == 0.0 || !norm.is_finite() {
            return Err(gltf_error("invalid node rotation quaternion"));
        }
        let [x, y, z, w] = [x / norm, y / norm, z / norm, w / norm];
        let [sx, sy, sz] = self.scale.unwrap_or([1.0; 3]);
        Transform::checked([
            mul(
                [
                    1.0 - 2.0 * (y * y + z * z),
                    2.0 * (x * y + z * w),
                    2.0 * (x * z - y * w),
                ],
                sx,
            ),
            mul(
                [
                    2.0 * (x * y - z * w),
                    1.0 - 2.0 * (x * x + z * z),
                    2.0 * (y * z + x * w),
                ],
                sy,
            ),
            mul(
                [
                    2.0 * (x * z + y * w),
                    2.0 * (y * z - x * w),
                    1.0 - 2.0 * (x * x + y * y),
                ],
                sz,
            ),
            self.translation.unwrap_or([0.0; 3]),
        ])
    }
}

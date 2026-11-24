use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::ops::Range;
use std::rc::Rc;

use roxmltree::{Document, Node, ParsingOptions};
use zip::ZipArchive;

use crate::{
    LengthUnit, MAX_INPUT_BYTES, MAX_TRIANGLES, MAX_VERTICES, Mesh, MeshError, Vec3, add,
    check_size, cross, dot, invalid, mul, normalized,
};

const OPC_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const MODEL_RELATIONSHIP: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const PRODUCTION_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/production/2015/06";
const MAX_ARCHIVE_ENTRIES: usize = 4096;
const MAX_RELATIONSHIPS_BYTES: usize = 256 * 1024;
const MAX_PART_BYTES: usize = 32 * 1024 * 1024;
const MAX_XML_BYTES: usize = 64 * 1024 * 1024;
const MAX_PARTS: usize = 256;
const MAX_PATH_BYTES: usize = 1024;
const MAX_COMPONENT_DEPTH: usize = 64;
const MAX_INSTANCE_VISITS: usize = MAX_TRIANGLES;
const MAX_BUILD_METADATA_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct ThreeMfDocument {
    pub mesh: Mesh,
    /// Original root model unit, independent of the override.
    pub detected_unit: LengthUnit,
    pub build_items: Vec<ThreeMfBuildItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreeMfBuildItem {
    /// Zero-based position in the root build list. Repeated object IDs have distinct occurrences.
    pub occurrence_id: usize,
    /// ZIP-relative path of the model part holding the referenced object.
    pub model_part: String,
    pub object_id: u32,
    pub name: Option<String>,
    /// Half-open range indexing [`ThreeMfDocument::mesh`]'s triangles.
    pub triangle_range: Range<usize>,
    /// World-space AABB `[min, max]` of the referenced triangles, in millimeters.
    pub bounds_mm: [[f64; 3]; 2],
}

/// Read the 3MF build in millimeters, with units inferred from each model part.
/// Call [`Mesh::validate_closed_manifold`] to require a closed shell.
pub fn load_3mf(bytes: &[u8]) -> Result<Mesh, MeshError> {
    load_3mf_with_source_unit(bytes, None)
}

/// Read the 3MF build with an optional source-unit override for all model parts.
pub fn load_3mf_with_source_unit(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
) -> Result<Mesh, MeshError> {
    Ok(load_3mf_with_metadata(bytes, source_unit)?.0)
}

/// Read the 3MF build and return the original root model unit alongside it.
pub fn load_3mf_with_metadata(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
) -> Result<(Mesh, LengthUnit), MeshError> {
    let document = read_document(bytes, source_unit, false, false)?;
    Ok((document.mesh, document.detected_unit))
}

/// Read the flattened mesh and top-level build occurrences with their world geometry bounds.
pub fn load_3mf_document(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
) -> Result<ThreeMfDocument, MeshError> {
    read_document(bytes, source_unit, true, false)
}

/// Read the flattened mesh and top-level build occurrences, stripping degenerate
/// triangles instead of rejecting the archive when real-world 3MF exporters emit
/// zero-area or repeated-index facets.
pub fn load_3mf_document_tolerant(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
) -> Result<ThreeMfDocument, MeshError> {
    read_document(bytes, source_unit, true, true)
}

fn read_document(
    bytes: &[u8],
    source_unit: Option<LengthUnit>,
    collect_build: bool,
    strip_degenerate: bool,
) -> Result<ThreeMfDocument, MeshError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(MeshError::ResourceLimit("3MF archive bytes"));
    }
    let archive = ZipArchive::new(Cursor::new(bytes)).map_err(zip_error)?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(MeshError::ResourceLimit("3MF ZIP entry count"));
    }
    let mut entries = HashMap::new();
    for (index, name) in archive.file_names().enumerate() {
        if name.len() > MAX_PATH_BYTES {
            return Err(MeshError::ResourceLimit("3MF ZIP entry path length"));
        }
        if entries.insert(name.to_ascii_lowercase(), index).is_some() {
            return Err(invalid("duplicate 3MF ZIP entry path"));
        }
    }
    let mut package = Package {
        archive,
        entries,
        override_unit: source_unit,
        strip_degenerate,
        parts: HashMap::new(),
        xml_bytes: 0,
        visits: 0,
        mesh: Mesh::default(),
        active: HashSet::new(),
    };
    let root = package.root_model_index()?;
    let part = package.part(root)?;
    let detected_unit = part.unit;
    let build = Rc::clone(&part.build);
    let mut build_items = Vec::new();
    let mut metadata_bytes = 0;
    for (occurrence_id, reference) in build.iter().enumerate() {
        let start = package.mesh.triangles.len();
        let name = package.instantiate(reference, root, Transform::IDENTITY, 0)?;
        if strip_degenerate {
            let vertices = &package.mesh.vertices;
            let mut write = start;
            for read in start..package.mesh.triangles.len() {
                let tri = package.mesh.triangles[read];
                if !crate::is_degenerate_triangle(vertices, tri) {
                    package.mesh.triangles[write] = tri;
                    write += 1;
                }
            }
            package.mesh.triangles.truncate(write);
        }
        if collect_build {
            let triangle_range = start..package.mesh.triangles.len();
            if strip_degenerate && triangle_range.is_empty() {
                continue;
            }
            let path = package
                .archive
                .name_for_index(reference.part_index.unwrap_or(root))
                .expect("resolved ZIP part");
            metadata_bytes +=
                size_of::<ThreeMfBuildItem>() + path.len() + name.as_deref().map_or(0, str::len);
            if metadata_bytes > MAX_BUILD_METADATA_BYTES {
                return Err(MeshError::ResourceLimit("3MF build metadata bytes"));
            }
            build_items.push(ThreeMfBuildItem {
                occurrence_id,
                model_part: path.to_owned(),
                object_id: reference.id,
                name: name.as_deref().map(str::to_owned),
                bounds_mm: triangle_bounds(&package.mesh, triangle_range.clone()),
                triangle_range,
            });
        }
    }
    package.mesh.validate_geometry()?;
    Ok(ThreeMfDocument {
        mesh: package.mesh,
        detected_unit,
        build_items,
    })
}

fn triangle_bounds(mesh: &Mesh, triangles: Range<usize>) -> [[f64; 3]; 2] {
    let mut bounds = [[f64::INFINITY; 3], [f64::NEG_INFINITY; 3]];
    for &index in mesh.triangles[triangles].iter().flatten() {
        for (axis, value) in mesh.vertices[index as usize].into_iter().enumerate() {
            bounds[0][axis] = bounds[0][axis].min(value);
            bounds[1][axis] = bounds[1][axis].max(value);
        }
    }
    bounds
}

fn zip_error(error: zip::result::ZipError) -> MeshError {
    invalid(format!("3MF ZIP: {error}"))
}

struct Package<'a> {
    archive: ZipArchive<Cursor<&'a [u8]>>,
    entries: HashMap<String, usize>,
    override_unit: Option<LengthUnit>,
    strip_degenerate: bool,
    parts: HashMap<usize, Part>,
    xml_bytes: usize,
    visits: usize,
    mesh: Mesh,
    active: HashSet<(usize, u32)>,
}

impl Package<'_> {
    fn read_part(&mut self, index: usize, limit: usize) -> Result<String, MeshError> {
        let remaining = limit.min(MAX_XML_BYTES - self.xml_bytes);
        let mut file = self.archive.by_index(index).map_err(zip_error)?;
        if !file.is_file() || file.is_symlink() {
            return Err(invalid("3MF XML part is not a regular file"));
        }
        if file.size() > remaining as u64 {
            return Err(MeshError::ResourceLimit("3MF XML bytes"));
        }
        let mut data = String::new();
        // Bound actual decompression as well as the ZIP's declared part size.
        (&mut file)
            .take(remaining as u64 + 1)
            .read_to_string(&mut data)
            .map_err(|error| invalid(format!("3MF XML part could not be read: {error}")))?;
        if data.len() > remaining {
            return Err(MeshError::ResourceLimit("3MF XML bytes"));
        }
        if data.len() as u64 != file.size() {
            return Err(invalid("3MF ZIP part size does not match its contents"));
        }
        self.xml_bytes += data.len();
        Ok(data)
    }

    fn root_model_index(&mut self) -> Result<usize, MeshError> {
        let index = lookup_part_index(&self.entries, "_rels/.rels")?;
        let xml = self.read_part(index, MAX_RELATIONSHIPS_BYTES)?;
        let parsed = document(&xml)?;
        let root = parsed.root_element();
        if !root.has_tag_name((OPC_NS, "Relationships")) {
            return Err(invalid(
                "3MF root relationships part has an invalid root element",
            ));
        }
        let mut model = None;
        for relation in children(root, OPC_NS, "Relationship") {
            if relation.attribute("Type") != Some(MODEL_RELATIONSHIP) {
                continue;
            }
            if model.is_some() {
                return Err(invalid("multiple 3MF start-part relationships"));
            }
            if !matches!(relation.attribute("TargetMode"), None | Some("Internal")) {
                return Err(invalid("3MF start-part relationship must be internal"));
            }
            let path = part_path(None, required(relation, "Target")?)?;
            model = Some(lookup_part_index(&self.entries, &path)?);
        }
        model.ok_or_else(|| invalid("missing 3MF start-part relationship"))
    }

    fn part(&mut self, index: usize) -> Result<&Part, MeshError> {
        if !self.parts.contains_key(&index) {
            if self.parts.len() == MAX_PARTS {
                return Err(MeshError::ResourceLimit("3MF model part count"));
            }
            let data = self.read_part(index, MAX_PART_BYTES)?;
            let path = self
                .archive
                .name_for_index(index)
                .expect("resolved ZIP part");
            let part = parse_part(
                &data,
                path,
                self.override_unit,
                &self.entries,
                self.strip_degenerate,
            )?;
            self.parts.insert(index, part);
        }
        Ok(&self.parts[&index])
    }

    fn instantiate(
        &mut self,
        reference: &Reference,
        from_index: usize,
        parent: Transform,
        depth: usize,
    ) -> Result<Option<Rc<str>>, MeshError> {
        if depth >= MAX_COMPONENT_DEPTH {
            return Err(MeshError::ResourceLimit("3MF component depth"));
        }
        if self.visits == MAX_INSTANCE_VISITS {
            return Err(MeshError::ResourceLimit("3MF component instance count"));
        }
        self.visits += 1;
        let index = reference.part_index.unwrap_or(from_index);
        let key = (index, reference.id);
        if !self.active.insert(key) {
            return Err(invalid("cyclic 3MF component reference"));
        }
        let object = self
            .part(index)?
            .objects
            .get(&reference.id)
            .cloned()
            .ok_or_else(|| {
                let path = self
                    .archive
                    .name_for_index(index)
                    .expect("resolved ZIP part");
                invalid(format!("missing 3MF object {} in {path}", reference.id))
            })?;
        let transform = parent.compose(reference.transform)?;
        match &object.geometry {
            ObjectGeometry::Mesh(mesh) => self.append_mesh(mesh, transform)?,
            ObjectGeometry::Components(components) => {
                for component in components {
                    self.instantiate(component, index, transform, depth + 1)?;
                }
            }
        }
        self.active.remove(&key);
        Ok(object.name.clone())
    }

    fn append_mesh(&mut self, mesh: &Mesh, transform: Transform) -> Result<(), MeshError> {
        check_size(
            self.mesh.vertices.len() + mesh.vertices.len(),
            self.mesh.triangles.len() + mesh.triangles.len(),
        )?;
        let start = self.mesh.vertices.len() as u32;
        self.mesh
            .vertices
            .extend(mesh.vertices.iter().copied().map(|p| transform.apply(p)));
        self.mesh
            .triangles
            .extend(mesh.triangles.iter().map(|triangle| {
                let mut indexed = triangle.map(|index| start + index);
                if transform.mirrored {
                    indexed.swap(1, 2);
                }
                indexed
            }));
        Ok(())
    }
}

fn lookup_part_index(entries: &HashMap<String, usize>, name: &str) -> Result<usize, MeshError> {
    entries
        .get(&name.to_ascii_lowercase())
        .copied()
        .ok_or_else(|| invalid(format!("missing 3MF ZIP part: {name}")))
}

// Resolve OPC URI references within the ZIP namespace only. No files are ever
// extracted, and neither URI schemes/authorities nor encoded separators are accepted.
fn part_path(from: Option<&str>, target: &str) -> Result<String, MeshError> {
    if target.len() > MAX_PATH_BYTES {
        return Err(MeshError::ResourceLimit("3MF part path length"));
    }
    let mut steps = Vec::new();
    let path = if let Some(absolute) = target.strip_prefix('/') {
        absolute
    } else {
        if let Some((dir, _)) = from.and_then(|part| part.rsplit_once('/')) {
            steps.extend(dir.split('/'));
        }
        target
    };
    for segment in path.split('/') {
        match segment {
            "." => {}
            ".." if steps.pop().is_some() => {}
            ".." | "" => return Err(invalid("invalid 3MF part path")),
            _ => {
                validate_segment(segment)?;
                steps.push(segment);
            }
        }
    }
    if matches!(path.rsplit('/').next(), Some("." | "..")) || steps.is_empty() {
        return Err(invalid("3MF part path must identify a file"));
    }
    let resolved = steps.join("/");
    if resolved.len() > MAX_PATH_BYTES {
        return Err(MeshError::ResourceLimit("3MF part path length"));
    }
    Ok(resolved)
}

fn validate_segment(segment: &str) -> Result<(), MeshError> {
    let bytes = segment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            let escaped = segment
                .get(i + 1..i + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| invalid("invalid percent escape in 3MF part path"))?;
            if escaped.is_ascii_control()
                || escaped.is_ascii_alphanumeric()
                || b"-._~/\\".contains(&escaped)
                || !bytes[i + 1..i + 3].iter().all(u8::is_ascii_hexdigit)
            {
                return Err(invalid("invalid percent escape in 3MF part path"));
            }
            i += 3;
        } else if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=@".contains(&b) {
            i += 1;
        } else {
            return Err(invalid("invalid character in 3MF part path"));
        }
    }
    Ok(())
}

fn document(xml: &str) -> Result<Document<'_>, MeshError> {
    Document::parse_with_options(
        xml,
        ParsingOptions {
            nodes_limit: 1_000_000,
            ..ParsingOptions::default()
        },
    )
    .map_err(|e| match e {
        roxmltree::Error::NodesLimitReached => MeshError::ResourceLimit("3MF XML node count"),
        _ => invalid(format!("3MF XML: {e}")),
    })
}

struct Part {
    unit: LengthUnit,
    objects: HashMap<u32, Rc<Object>>,
    build: Rc<Vec<Reference>>,
}

struct Object {
    name: Option<Rc<str>>,
    geometry: ObjectGeometry,
}

enum ObjectGeometry {
    Mesh(Mesh),
    Components(Vec<Reference>),
}

struct Reference {
    id: u32,
    part_index: Option<usize>,
    transform: Transform,
}

fn parse_part(
    xml: &str,
    path: &str,
    override_unit: Option<LengthUnit>,
    entries: &HashMap<String, usize>,
    strip_degenerate: bool,
) -> Result<Part, MeshError> {
    let parsed = document(xml)?;
    let root = parsed.root_element();
    if !root.has_tag_name((CORE_NS, "model")) {
        return Err(invalid("expected a 3MF Core model element"));
    }
    if let Some(extensions) = root.attribute("requiredextensions") {
        for prefix in extensions.split_ascii_whitespace() {
            if root.lookup_namespace_uri(Some(prefix)) != Some(PRODUCTION_NS) {
                return Err(invalid("unsupported required 3MF model extension"));
            }
        }
    }
    let unit = match root.attribute("unit") {
        None => LengthUnit::Millimeter,
        Some("micron" | "millimeter" | "centimeter" | "inch" | "foot" | "meter") => {
            root.attribute("unit").unwrap().parse()?
        }
        Some(value) => return Err(MeshError::InvalidUnit(value.to_owned())),
    };
    let scale = override_unit.unwrap_or(unit).millimeters_per_unit();
    let resources =
        sole_child(root, "resources")?.ok_or_else(|| invalid("missing 3MF resources"))?;
    let mut objects = HashMap::new();
    for object in children(resources, CORE_NS, "object") {
        let id = object_id(object, "id")?;
        let geometry = match (
            sole_child(object, "mesh")?,
            sole_child(object, "components")?,
        ) {
            (Some(mesh), None) => {
                ObjectGeometry::Mesh(parse_mesh(mesh, scale, strip_degenerate)?)
            }
            (None, Some(components)) => {
                let components = parse_references(components, "component", path, scale, entries)?;
                if components.is_empty() {
                    return Err(invalid("3MF object has an empty component list"));
                }
                ObjectGeometry::Components(components)
            }
            _ => return Err(invalid("3MF object must contain either mesh or components")),
        };
        let object = Object {
            geometry,
            name: object.attribute("name").map(Rc::from),
        };
        if objects.insert(id, Rc::new(object)).is_some() {
            return Err(invalid("duplicate 3MF object ID"));
        }
    }
    let build = sole_child(root, "build")?
        .map(|build| parse_references(build, "item", path, scale, entries))
        .transpose()?
        .unwrap_or_default();
    Ok(Part {
        unit,
        objects,
        build: Rc::new(build),
    })
}

fn parse_mesh(
    node: Node<'_, '_>,
    scale: f64,
    strip_degenerate: bool,
) -> Result<Mesh, MeshError> {
    let vertices_node =
        sole_child(node, "vertices")?.ok_or_else(|| invalid("missing 3MF vertices"))?;
    let triangles_node =
        sole_child(node, "triangles")?.ok_or_else(|| invalid("missing 3MF triangles"))?;
    let mut mesh = Mesh::default();
    for vertex in children(vertices_node, CORE_NS, "vertex") {
        if mesh.vertices.len() == MAX_VERTICES {
            return Err(MeshError::ResourceLimit("3MF vertex count"));
        }
        mesh.vertices.push(mul(
            [
                finite(vertex, "x")?,
                finite(vertex, "y")?,
                finite(vertex, "z")?,
            ],
            scale,
        ));
    }
    for triangle in children(triangles_node, CORE_NS, "triangle") {
        if mesh.triangles.len() == MAX_TRIANGLES {
            return Err(MeshError::ResourceLimit("3MF triangle count"));
        }
        mesh.triangles.push([
            unsigned(triangle, "v1")?,
            unsigned(triangle, "v2")?,
            unsigned(triangle, "v3")?,
        ]);
    }
    // Validate local indices before appending; invalid indices must not alias a later instance.
    if strip_degenerate {
        for (vertex, p) in mesh.vertices.iter().enumerate() {
            if p.iter().any(|v| !v.is_finite()) {
                return Err(MeshError::NonFiniteVertex { vertex });
            }
        }
        for (triangle, &indices) in mesh.triangles.iter().enumerate() {
            for vertex in indices {
                if vertex as usize >= mesh.vertices.len() {
                    return Err(MeshError::InvalidIndex { triangle, vertex });
                }
            }
        }
        mesh.strip_degenerate_triangles();
    }
    mesh.validate_geometry()?;
    Ok(mesh)
}

fn parse_references(
    node: Node<'_, '_>,
    name: &'static str,
    path: &str,
    scale: f64,
    entries: &HashMap<String, usize>,
) -> Result<Vec<Reference>, MeshError> {
    children(node, CORE_NS, name)
        .map(|node| {
            let part_index = node
                .attribute((PRODUCTION_NS, "path"))
                .map(|target| {
                    let target = part_path(Some(path), target)?;
                    lookup_part_index(entries, &target)
                })
                .transpose()?;
            Ok(Reference {
                id: object_id(node, "objectid")?,
                part_index,
                transform: Transform::parse(node.attribute("transform"), scale)?,
            })
        })
        .collect()
}

fn children<'a, 'i>(
    node: Node<'a, 'i>,
    ns: &'static str,
    name: &'static str,
) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children()
        .filter(move |child| child.has_tag_name((ns, name)))
}

fn sole_child<'a, 'i>(
    node: Node<'a, 'i>,
    name: &'static str,
) -> Result<Option<Node<'a, 'i>>, MeshError> {
    let mut found = children(node, CORE_NS, name);
    let child = found.next();
    if found.next().is_some() {
        return Err(invalid(format!("duplicate 3MF {name} element")));
    }
    Ok(child)
}

fn required<'a>(node: Node<'a, '_>, attr: &'static str) -> Result<&'a str, MeshError> {
    node.attribute(attr)
        .ok_or_else(|| invalid(format!("missing 3MF {attr} attribute")))
}

fn unsigned(node: Node<'_, '_>, attr: &'static str) -> Result<u32, MeshError> {
    required(node, attr)?
        .trim()
        .parse()
        .map_err(|_| invalid(format!("invalid 3MF {attr} integer")))
}

fn object_id(node: Node<'_, '_>, attr: &'static str) -> Result<u32, MeshError> {
    let id = unsigned(node, attr)?;
    if id == 0 {
        return Err(invalid("3MF object IDs must be positive"));
    }
    Ok(id)
}

fn finite(node: Node<'_, '_>, attr: &'static str) -> Result<f64, MeshError> {
    required(node, attr)?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| invalid(format!("invalid 3MF {attr} coordinate")))
}

#[derive(Clone, Copy)]
struct Transform {
    // The 3MF matrix uses row-vector order; its last row is the translation.
    rows: [Vec3; 3],
    translation: Vec3,
    mirrored: bool,
}

impl Transform {
    const IDENTITY: Self = Self {
        rows: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
        translation: [0.; 3],
        mirrored: false,
    };

    fn parse(raw: Option<&str>, scale: f64) -> Result<Self, MeshError> {
        let Some(raw) = raw else {
            return Ok(Self::IDENTITY);
        };
        let mut values = raw.split_ascii_whitespace();
        let mut matrix = [[0.0; 3]; 4];
        for slot in matrix.iter_mut().flatten() {
            *slot = values
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite())
                .ok_or_else(|| invalid("3MF transform must contain 12 finite numbers"))?;
        }
        if values.next().is_some() {
            return Err(invalid("3MF transform must contain exactly 12 numbers"));
        }
        let [a, b, c, translation] = matrix;
        let det = normalized(a)
            .zip(normalized(b))
            .zip(normalized(c))
            .map_or(0.0, |((a, b), c)| dot(a, cross(b, c)));
        if det == 0.0 {
            return Err(invalid("singular 3MF transform"));
        }
        let result = Self {
            rows: [a, b, c],
            translation: mul(translation, scale),
            mirrored: det < 0.0,
        };
        result.check_finite()?;
        Ok(result)
    }

    fn apply_linear(self, vector: Vec3) -> Vec3 {
        std::array::from_fn(|i| {
            vector[0] * self.rows[0][i] + vector[1] * self.rows[1][i] + vector[2] * self.rows[2][i]
        })
    }

    fn apply(self, point: Vec3) -> Vec3 {
        add(self.apply_linear(point), self.translation)
    }

    fn compose(self, child: Self) -> Result<Self, MeshError> {
        let result = Self {
            rows: child.rows.map(|row| self.apply_linear(row)),
            translation: self.apply(child.translation),
            mirrored: self.mirrored ^ child.mirrored,
        };
        result.check_finite()?;
        Ok(result)
    }

    fn check_finite(self) -> Result<(), MeshError> {
        if self
            .rows
            .iter()
            .flatten()
            .chain(&self.translation)
            .any(|v| !v.is_finite())
        {
            return Err(invalid("non-finite composed 3MF transform"));
        }
        Ok(())
    }
}

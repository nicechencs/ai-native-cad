use std::ops::Range;

use crate::{
    LengthUnit, MAX_INPUT_BYTES, MAX_VERTICES, Mesh, MeshError, Vec3, check_size, cross, invalid,
    sub,
};

const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_FACE_CORNERS: usize = 1024;
const MAX_POLYGON_WORK: usize = 64 * 1024 * 1024;
const MAX_MATERIAL_METADATA_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct ObjDocument {
    pub mesh: Mesh,
    pub material_groups: Vec<ObjMaterialGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjMaterialGroup {
    /// The in-file `usemtl` label, or `None` before the first `usemtl`.
    pub material: Option<String>,
    /// Contiguous half-open range indexing [`ObjDocument::mesh`]'s triangles.
    pub triangle_range: Range<usize>,
}

/// Read polygonal OBJ geometry and convert positions to millimeters. OBJ does
/// not declare units; pass [`LengthUnit::Millimeter`] if none are known.
///
/// Position indices are preserved, without welding or splitting vertices for
/// UV/normal seams. Face indices must refer to already-declared entries.
/// Concave faces are triangulated in a dominant-axis projection. There is no
/// external file access; textures, custom normals and non-polygonal geometry
/// are not imported. Call [`Mesh::validate_closed_manifold`] to require closure.
pub fn load_obj(bytes: &[u8], source_unit: LengthUnit) -> Result<Mesh, MeshError> {
    Ok(read_document(bytes, source_unit, false)?.mesh)
}

/// Read the mesh and contiguous material runs without resolving `mtllib` or
/// reading any material or texture files.
pub fn load_obj_document(bytes: &[u8], source_unit: LengthUnit) -> Result<ObjDocument, MeshError> {
    read_document(bytes, source_unit, true)
}

fn read_document(
    bytes: &[u8],
    unit: LengthUnit,
    collect_materials: bool,
) -> Result<ObjDocument, MeshError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(MeshError::ResourceLimit("OBJ input bytes"));
    }
    let input = std::str::from_utf8(bytes).map_err(|_| invalid("expected UTF-8 OBJ text"))?;
    if input.contains('\0') {
        return Err(invalid("unexpected NUL in OBJ text"));
    }
    let mut parser = Parser {
        mesh: Mesh::default(),
        scale: unit.millimeters_per_unit(),
        texcoords: 0,
        normals: 0,
        corners: Vec::new(),
        polygon_work_left: MAX_POLYGON_WORK,
        materials: collect_materials.then(Materials::default),
    };
    let mut pending = String::new();
    let mut start = None;
    for (line_index, physical) in input.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line_index + 1;
        let content = physical.split('#').next().unwrap_or("").trim_ascii();
        let fragment = content.strip_suffix('\\');
        if let Some(fragment) = fragment {
            start.get_or_insert(line);
            if pending.len() + fragment.len() + 1 > MAX_LINE_BYTES {
                return Err(MeshError::ResourceLimit("OBJ logical line bytes"));
            }
            pending.push_str(fragment);
            pending.push(' ');
        } else if let Some(first) = start.take() {
            if pending.len() + content.len() > MAX_LINE_BYTES {
                return Err(MeshError::ResourceLimit("OBJ logical line bytes"));
            }
            pending.push_str(content);
            parser.record(&pending, first)?;
            pending.clear();
        } else {
            if content.len() > MAX_LINE_BYTES {
                return Err(MeshError::ResourceLimit("OBJ logical line bytes"));
            }
            parser.record(content, line)?;
        }
    }
    if let Some(first) = start {
        return Err(line_error(first, "unterminated line continuation"));
    }
    parser.mesh.validate_geometry()?;
    Ok(ObjDocument {
        mesh: parser.mesh,
        material_groups: parser.materials.map_or_else(Vec::new, |m| m.groups),
    })
}

struct Parser {
    mesh: Mesh,
    scale: f64,
    texcoords: usize,
    normals: usize,
    corners: Vec<u32>,
    polygon_work_left: usize,
    materials: Option<Materials>,
}

impl Parser {
    fn record(&mut self, content: &str, line: usize) -> Result<(), MeshError> {
        let content = content.trim_ascii_start();
        let (keyword, fields) = content
            .split_once(|ch: char| ch.is_ascii_whitespace())
            .unwrap_or((content, ""));
        let fields = fields.trim_ascii();
        match keyword {
            "v" => {
                let vertex = self.mesh.vertices.len();
                check_size(vertex + 1, self.mesh.triangles.len())?;
                let mut tokens = fields.split_ascii_whitespace();
                let mut position = [0.0; 3];
                for value in &mut position {
                    *value = parse_number(tokens.next(), line)? * self.scale;
                }
                if !position.iter().all(|x| x.is_finite()) {
                    return Err(MeshError::NonFiniteVertex { vertex });
                }
                // Ignore optional free-form weight and common vertex color extensions.
                let count = count_finite(tokens, 4, line)?;
                if count == 2 {
                    return Err(line_error(
                        line,
                        "expected XYZ with optional weight or vertex color",
                    ));
                }
                self.mesh.vertices.push(position);
            }
            "vt" => {
                if self.texcoords == MAX_VERTICES {
                    return Err(MeshError::ResourceLimit("OBJ texture coordinate count"));
                }
                if count_finite(fields.split_ascii_whitespace(), 3, line)? == 0 {
                    return Err(line_error(
                        line,
                        "expected one to three texture coordinates",
                    ));
                }
                self.texcoords += 1;
            }
            "vn" => {
                if self.normals == MAX_VERTICES {
                    return Err(MeshError::ResourceLimit("OBJ normal count"));
                }
                if count_finite(fields.split_ascii_whitespace(), 3, line)? != 3 {
                    return Err(line_error(line, "expected three normal coordinates"));
                }
                self.normals += 1;
            }
            "f" => self.face(fields, line)?,
            "usemtl" => {
                if fields.is_empty() {
                    return Err(line_error(line, "expected a material name"));
                }
                if let Some(materials) = &mut self.materials {
                    materials.select(fields);
                }
            }
            // Includes mtllib and legacy call/csh: no external data or commands.
            _ => {}
        }
        Ok(())
    }

    fn face(&mut self, fields: &str, line: usize) -> Result<(), MeshError> {
        self.corners.clear();
        let counts = [self.mesh.vertices.len(), self.texcoords, self.normals];
        for field in fields.split_ascii_whitespace() {
            if self.corners.len() == MAX_FACE_CORNERS {
                return Err(MeshError::ResourceLimit("OBJ polygon corner count"));
            }
            self.corners.push(parse_corner(field, counts, line)?);
        }
        let count = self.corners.len();
        if count < 3 {
            return Err(line_error(line, "face needs at least three corners"));
        }
        let start = self.mesh.triangles.len();
        let end = start + count - 2;
        check_size(self.mesh.vertices.len(), end)?;
        if let [a, b, c] = self.corners.as_slice() {
            self.mesh.triangles.push([*a, *b, *c]);
        } else {
            triangulate_polygon(
                &self.corners,
                &mut self.mesh,
                &mut self.polygon_work_left,
                line,
            )?;
        }
        if let Some(materials) = &mut self.materials {
            materials.record(start..end)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Materials {
    current: String,
    changed: bool,
    groups: Vec<ObjMaterialGroup>,
    bytes: usize,
}

impl Materials {
    fn select(&mut self, label: &str) {
        self.current.clear();
        self.current.push_str(label);
        self.changed = true;
    }

    fn record(&mut self, triangle_range: Range<usize>) -> Result<(), MeshError> {
        let material = (!self.current.is_empty()).then_some(self.current.as_str());
        let changed = std::mem::take(&mut self.changed);
        if let Some(group) = self.groups.last_mut()
            && (!changed || group.material.as_deref() == material)
        {
            group.triangle_range.end = triangle_range.end;
            return Ok(());
        }
        self.bytes += size_of::<ObjMaterialGroup>() + self.current.len();
        if self.bytes > MAX_MATERIAL_METADATA_BYTES {
            return Err(MeshError::ResourceLimit("OBJ material metadata bytes"));
        }
        self.groups.push(ObjMaterialGroup {
            material: material.map(str::to_owned),
            triangle_range,
        });
        Ok(())
    }
}

fn parse_number(token: Option<&str>, line: usize) -> Result<f64, MeshError> {
    token
        .and_then(|token| token.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .ok_or_else(|| line_error(line, "expected a finite number"))
}

fn count_finite<'a>(
    tokens: impl Iterator<Item = &'a str>,
    max: usize,
    line: usize,
) -> Result<usize, MeshError> {
    let mut count = 0;
    for token in tokens {
        count += 1;
        if count > max {
            return Err(line_error(line, "too many coordinates"));
        }
        parse_number(Some(token), line)?;
    }
    Ok(count)
}

fn parse_corner(token: &str, counts: [usize; 3], line: usize) -> Result<u32, MeshError> {
    let mut fields = token.split('/');
    let position = resolve_index(fields.next().unwrap_or(""), counts[0], line, "position")?;
    match (fields.next(), fields.next()) {
        (Some(uv), Some(normal)) => {
            if !uv.is_empty() {
                resolve_index(uv, counts[1], line, "texture coordinate")?;
            }
            resolve_index(normal, counts[2], line, "normal")?;
        }
        (Some(uv), None) => {
            resolve_index(uv, counts[1], line, "texture coordinate")?;
        }
        _ => {}
    }
    if fields.next().is_some() {
        return Err(line_error(line, "face corner has too many index fields"));
    }
    Ok(position)
}

fn resolve_index(token: &str, count: usize, line: usize, what: &str) -> Result<u32, MeshError> {
    let index = token
        .parse::<i64>()
        .map_err(|_| line_error(line, format_args!("invalid {what} index")))?;
    let count = count as i64;
    let resolved = if index > 0 {
        index - 1
    } else if index < 0 {
        count + index
    } else {
        -1
    };
    if !(0..count).contains(&resolved) {
        return Err(line_error(line, format_args!("{what} index out of range")));
    }
    Ok(resolved as u32)
}

fn line_error(line: usize, message: impl std::fmt::Display) -> MeshError {
    invalid(format!("OBJ line {line}: {message}"))
}

fn charge_work(remaining: &mut usize, work: usize) -> Result<(), MeshError> {
    *remaining = remaining
        .checked_sub(work)
        .ok_or(MeshError::ResourceLimit("OBJ polygon triangulation work"))?;
    Ok(())
}

type Point2 = [f64; 2];

fn triangulate_polygon(
    corners: &[u32],
    mesh: &mut Mesh,
    work: &mut usize,
    line: usize,
) -> Result<(), MeshError> {
    let n = corners.len();
    // Reserve the worst-case work of checking polygon edges before comparing them.
    charge_work(work, n * n)?;
    let points = project_polygon(corners, &mesh.vertices)
        .ok_or_else(|| line_error(line, "degenerate face projection"))?;
    for i in 0..n {
        let (a, b, c) = (points[(i + n - 1) % n], points[i], points[(i + 1) % n]);
        if b == c || (orient(a, b, c) == 0.0 && (between(a, b, c) || between(c, a, b))) {
            return Err(line_error(
                line,
                "polygon has coincident corners or overlapping edges",
            ));
        }
        for j in i + 2..n {
            if i == 0 && j == n - 1 {
                continue; // The last and first edges also share an endpoint.
            }
            if segments_intersect(b, c, points[j], points[(j + 1) % n]) {
                return Err(line_error(line, "polygon edges intersect"));
            }
        }
    }
    let area: f64 = (1..n - 1)
        .map(|i| orient(points[0], points[i], points[i + 1]))
        .sum();
    if area == 0.0 {
        return Err(line_error(line, "polygon has zero projected area"));
    }
    let winding = area.signum();
    let mut ring: Vec<usize> = (0..n).collect();
    let mut cursor = 0;
    let mut rejected = 0;
    while ring.len() > 3 {
        let len = ring.len();
        let [a, b, c] = [
            ring[(cursor + len - 1) % len],
            ring[cursor],
            ring[(cursor + 1) % len],
        ];
        charge_work(work, 1)?;
        if winding * orient(points[a], points[b], points[c]) > 0.0 {
            charge_work(work, len)?;
            let contains_corner = ring.iter().copied().any(|i| {
                i != a
                    && i != b
                    && i != c
                    && winding * orient(points[a], points[b], points[i]) >= 0.0
                    && winding * orient(points[b], points[c], points[i]) >= 0.0
                    && winding * orient(points[c], points[a], points[i]) >= 0.0
            });
            if !contains_corner {
                charge_work(work, len - cursor)?;
                mesh.triangles.push([corners[a], corners[b], corners[c]]);
                ring.remove(cursor);
                cursor %= ring.len();
                rejected = 0;
                continue;
            }
        }
        cursor = (cursor + 1) % len;
        rejected += 1;
        if rejected == len {
            return Err(line_error(line, "polygon cannot be triangulated"));
        }
    }
    let [a, b, c] = [ring[0], ring[1], ring[2]];
    if winding * orient(points[a], points[b], points[c]) <= 0.0 {
        return Err(line_error(line, "polygon cannot be triangulated"));
    }
    mesh.triangles.push([corners[a], corners[b], corners[c]]);
    Ok(())
}

fn project_polygon(corners: &[u32], vertices: &[Vec3]) -> Option<Vec<Point2>> {
    let origin = vertices[corners[0] as usize];
    let mut relative: Vec<_> = corners
        .iter()
        .map(|&i| sub(vertices[i as usize], origin))
        .collect();
    let scale = relative
        .iter()
        .flatten()
        .fold(0.0_f64, |scale, x| scale.max(x.abs()));
    if scale == 0.0 || !scale.is_finite() {
        return None;
    }
    // Use a power of two near the largest delta to avoid rounding from division
    // for ordinary coordinates, and over/underflow when computing areas.
    let scale = f64::from_bits(scale.to_bits() & 0x7ff0_0000_0000_0000).max(f64::MIN_POSITIVE);
    for p in &mut relative {
        *p = p.map(|x| x / scale);
    }
    let mut normal = [0.0; 3];
    for pair in relative[1..].windows(2) {
        for (sum, axis) in normal.iter_mut().zip(cross(pair[0], pair[1])) {
            *sum += axis;
        }
    }
    let axis = (0..3).max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs()))?;
    (normal[axis] != 0.0).then(|| {
        relative
            .iter()
            .map(|p| [p[(axis + 1) % 3], p[(axis + 2) % 3]])
            .collect()
    })
}

fn orient(a: Point2, b: Point2, c: Point2) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

// Used only when the three points are collinear.
fn between(p: Point2, a: Point2, b: Point2) -> bool {
    (0..2).all(|axis| (a[axis].min(b[axis])..=a[axis].max(b[axis])).contains(&p[axis]))
}

fn segments_intersect(a: Point2, b: Point2, c: Point2, d: Point2) -> bool {
    let [abc, abd, cda, cdb] = [
        orient(a, b, c),
        orient(a, b, d),
        orient(c, d, a),
        orient(c, d, b),
    ];
    let opposite = |x, y| (x < 0.0 && y > 0.0) || (x > 0.0 && y < 0.0);
    (opposite(abc, abd) && opposite(cda, cdb))
        || (abc == 0.0 && between(c, a, b))
        || (abd == 0.0 && between(d, a, b))
        || (cda == 0.0 && between(a, c, d))
        || (cdb == 0.0 && between(b, c, d))
}

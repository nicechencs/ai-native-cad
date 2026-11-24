use std::collections::HashMap;

use crate::{LengthUnit, MAX_INPUT_BYTES, MAX_TRIANGLES, Mesh, MeshError, Vec3, invalid, mul};

/// Read binary or ASCII STL in the supplied source unit. Exact matching vertex
/// coordinates are welded; triangle winding determines the geometric normals.
/// Call [`Mesh::validate_closed_manifold`] to require a closed shell.
pub fn load_stl(bytes: &[u8], source_unit: LengthUnit) -> Result<Mesh, MeshError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(MeshError::ResourceLimit("STL input bytes"));
    }
    let scale = source_unit.millimeters_per_unit();
    let mut mesh = StlMesh {
        scale,
        ..StlMesh::default()
    };
    // A binary STL header is arbitrary and may start with `solid`; the byte
    // count is stronger evidence than the header text.
    if let Some(raw_count) = bytes.get(80..84) {
        let count = u32::from_le_bytes(raw_count.try_into().unwrap()) as usize;
        if count.checked_mul(50).and_then(|size| size.checked_add(84)) == Some(bytes.len()) {
            if count > MAX_TRIANGLES {
                return Err(MeshError::ResourceLimit("STL triangle count"));
            }
            mesh.mesh.triangles.reserve(count);
            for facet in bytes[84..].as_chunks::<50>().0 {
                let read = |i: usize| {
                    f32::from_le_bytes(facet[4 * i..4 * (i + 1)].try_into().unwrap()) as f64
                };
                if (0..3).any(|i| !read(i).is_finite()) {
                    return Err(invalid("non-finite binary STL facet normal"));
                }
                mesh.push(std::array::from_fn(|i| {
                    std::array::from_fn(|j| read(3 + i * 3 + j))
                }))?;
            }
        } else {
            read_ascii(bytes, &mut mesh)?;
        }
    } else {
        read_ascii(bytes, &mut mesh)?;
    }
    mesh.mesh.validate_geometry()?;
    Ok(mesh.mesh)
}

#[derive(Default)]
struct StlMesh {
    mesh: Mesh,
    indices: HashMap<[u64; 3], u32>,
    scale: f64,
}

impl StlMesh {
    fn push(&mut self, points: [Vec3; 3]) -> Result<(), MeshError> {
        if self.mesh.triangles.len() == MAX_TRIANGLES {
            return Err(MeshError::ResourceLimit("STL triangle count"));
        }
        let mut indices = [0; 3];
        for (point, index) in points.into_iter().zip(&mut indices) {
            let mm = mul(point, self.scale);
            if mm.iter().any(|x| !x.is_finite()) {
                return Err(MeshError::NonFiniteVertex {
                    vertex: self.mesh.vertices.len(),
                });
            }
            let key = point.map(|x| if x == 0.0 { 0 } else { x.to_bits() });
            *index = *self.indices.entry(key).or_insert_with(|| {
                let index = self.mesh.vertices.len() as u32;
                self.mesh.vertices.push(mm);
                index
            });
        }
        self.mesh.triangles.push(indices);
        Ok(())
    }
}

fn read_ascii(bytes: &[u8], mesh: &mut StlMesh) -> Result<(), MeshError> {
    let mut input = std::str::from_utf8(bytes)
        .map_err(|_| invalid("expected ASCII STL or a complete binary STL"))?
        .trim_start_matches('\u{feff}')
        .trim();
    // Solid names can contain spaces; solid markers therefore consume their
    // lines. Within a solid, facet records are whitespace-delimited.
    while !input.is_empty() {
        let (header, body) = input
            .split_once('\n')
            .ok_or_else(|| invalid("missing ASCII STL body"))?;
        if header.split_ascii_whitespace().next() != Some("solid") {
            return Err(invalid("expected ASCII STL solid header"));
        }
        let mut offset = 0;
        let (facets, rest) = loop {
            let line = body[offset..]
                .split_inclusive('\n')
                .next()
                .ok_or_else(|| invalid("missing ASCII STL endsolid"))?;
            if line.split_ascii_whitespace().next() == Some("endsolid") {
                break (&body[..offset], &body[offset + line.len()..]);
            }
            offset += line.len();
        };
        let mut tokens = facets.split_ascii_whitespace().peekable();
        while tokens.peek().is_some() {
            expect(&mut tokens, "facet")?;
            expect(&mut tokens, "normal")?;
            read_point(&mut tokens)?;
            expect(&mut tokens, "outer")?;
            expect(&mut tokens, "loop")?;
            let mut points = [[0.0; 3]; 3];
            for p in &mut points {
                expect(&mut tokens, "vertex")?;
                *p = read_point(&mut tokens)?;
            }
            expect(&mut tokens, "endloop")?;
            expect(&mut tokens, "endfacet")?;
            mesh.push(points)?;
        }
        input = rest.trim_start();
    }
    Ok(())
}

fn expect<'a>(tokens: &mut impl Iterator<Item = &'a str>, expected: &str) -> Result<(), MeshError> {
    if tokens.next() == Some(expected) {
        Ok(())
    } else {
        Err(invalid(format!("expected '{expected}' in ASCII STL")))
    }
}

fn read_point<'a>(tokens: &mut impl Iterator<Item = &'a str>) -> Result<Vec3, MeshError> {
    let mut point = [0.0; 3];
    for x in &mut point {
        *x = tokens
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .ok_or_else(|| invalid("expected finite number in ASCII STL"))?;
    }
    Ok(point)
}

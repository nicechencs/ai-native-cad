//! Deterministic 3MF Core and Beam Lattice writers.

use std::fmt::Write as _;
use std::fs;
use std::io::{Cursor, Write as _};
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::{Mesh, MeshError};

const CORE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/core/2015/02";
const OPC_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const MODEL_RELATIONSHIP: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const BEAM_LATTICE_NS: &str = "http://schemas.microsoft.com/3dmanufacturing/beamlattice/2017/02";
// Keep these in sync with the corresponding reader limits in threemf.rs.
const MAX_MODEL_PART_BYTES: usize = 32 * 1024 * 1024;
const MAX_MODEL_XML_NODES: usize = 1_000_000;

/// A single beam connecting two vertices in a beam lattice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThreeMfBeam {
    pub v1: u32,
    pub v2: u32,
    pub r1_mm: f64,
    pub r2_mm: f64,
}

/// Beam lattice data in millimeters.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreeMfBeamLattice {
    pub min_length_mm: f64,
    pub default_radius_mm: f64,
    pub vertices_mm: Vec<[f64; 3]>,
    pub beams: Vec<ThreeMfBeam>,
}

/// Write a deterministic 3MF Core mesh package to memory.
///
/// Rejects open, non-manifold, or degenerate meshes so 3MF exports always
/// represent closed oriented two-manifold solids.
pub fn write_3mf_mesh_bytes(mesh: &Mesh, model_name: &str) -> Result<Vec<u8>, MeshError> {
    mesh.validate_closed_manifold()?;
    check_model_limits(mesh, None)?;
    let model = model_xml(mesh, model_name, None)?;
    write_archive(model.as_bytes())
}

/// Write a deterministic 3MF Core mesh package to a file.
pub fn write_3mf_mesh_file(path: &Path, mesh: &Mesh, model_name: &str) -> Result<(), MeshError> {
    let bytes = write_3mf_mesh_bytes(mesh, model_name)?;
    fs::write(path, bytes)
        .map_err(|error| MeshError::InvalidFormat(format!("cannot write 3MF file: {error}")))
}

/// Write a 3MF package containing a Core mesh and a Beam Lattice extension.
///
/// The lattice vertices are appended to the optional mesh vertices. Beam vertex
/// indices are adjusted accordingly when a mesh is present.
pub fn write_3mf_with_beam_lattice_bytes(
    mesh: Option<&Mesh>,
    lattice: &ThreeMfBeamLattice,
    model_name: &str,
) -> Result<Vec<u8>, MeshError> {
    if let Some(mesh) = mesh {
        mesh.validate_geometry()?;
    }
    validate_lattice(lattice)?;
    let empty_mesh = Mesh::default();
    let mesh = mesh.unwrap_or(&empty_mesh);
    check_model_limits(mesh, Some(lattice))?;
    let model = model_xml(mesh, model_name, Some(lattice))?;
    write_archive(model.as_bytes())
}

fn check_model_limits(mesh: &Mesh, lattice: Option<&ThreeMfBeamLattice>) -> Result<(), MeshError> {
    let lattice_vertices = lattice.map_or(0, |lattice| lattice.vertices_mm.len());
    let total_vertices = mesh
        .vertices
        .len()
        .checked_add(lattice_vertices)
        .ok_or(MeshError::ResourceLimit("3MF vertex count"))?;
    if total_vertices > u32::MAX as usize {
        return Err(MeshError::ResourceLimit("3MF vertex index range"));
    }
    let mut nodes = 10_usize
        .checked_add(mesh.vertices.len())
        .and_then(|nodes| nodes.checked_add(mesh.triangles.len()))
        .and_then(|nodes| nodes.checked_add(lattice_vertices))
        .ok_or(MeshError::ResourceLimit("3MF XML node count"))?;
    if let Some(lattice) = lattice {
        nodes = nodes
            .checked_add(lattice.beams.len())
            .ok_or(MeshError::ResourceLimit("3MF XML node count"))?;
    }
    if nodes > MAX_MODEL_XML_NODES {
        return Err(MeshError::ResourceLimit("3MF XML node count"));
    }
    Ok(())
}

fn validate_lattice(lattice: &ThreeMfBeamLattice) -> Result<(), MeshError> {
    if !lattice.min_length_mm.is_finite() || lattice.min_length_mm <= 0.0 {
        return Err(MeshError::InvalidFormat(
            "beam lattice minimum length must be finite and positive".to_owned(),
        ));
    }
    if !lattice.default_radius_mm.is_finite() || lattice.default_radius_mm <= 0.0 {
        return Err(MeshError::InvalidFormat(
            "beam lattice default radius must be finite and positive".to_owned(),
        ));
    }
    for (vertex, coordinates) in lattice.vertices_mm.iter().enumerate() {
        if coordinates.iter().any(|coordinate| !coordinate.is_finite()) {
            return Err(MeshError::NonFiniteVertex { vertex });
        }
    }
    for (beam, data) in lattice.beams.iter().enumerate() {
        if data.v1 as usize >= lattice.vertices_mm.len() {
            return Err(MeshError::InvalidFormat(format!(
                "beam {beam} references vertex {} outside the lattice",
                data.v1
            )));
        }
        if data.v2 as usize >= lattice.vertices_mm.len() {
            return Err(MeshError::InvalidFormat(format!(
                "beam {beam} references vertex {} outside the lattice",
                data.v2
            )));
        }
        if data.v1 == data.v2 {
            return Err(MeshError::InvalidFormat(format!(
                "beam {beam} references the same vertex twice"
            )));
        }
        if !data.r1_mm.is_finite() || data.r1_mm <= 0.0 {
            return Err(MeshError::InvalidFormat(format!(
                "beam {beam} first radius must be finite and positive"
            )));
        }
        if !data.r2_mm.is_finite() || data.r2_mm <= 0.0 {
            return Err(MeshError::InvalidFormat(format!(
                "beam {beam} second radius must be finite and positive"
            )));
        }
    }
    Ok(())
}

fn model_xml(
    mesh: &Mesh,
    model_name: &str,
    lattice: Option<&ThreeMfBeamLattice>,
) -> Result<String, MeshError> {
    let mut xml = String::new();
    validate_model_name(model_name)?;
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    write!(xml, "<model xmlns=\"{CORE_NS}\" unit=\"millimeter\"").expect("writing XML to a string");
    if lattice.is_some() {
        write!(
            xml,
            " xmlns:b=\"{BEAM_LATTICE_NS}\" requiredextensions=\"b\""
        )
        .expect("writing XML to a string");
    }
    xml.push_str("><resources><object id=\"1\" name=\"");
    escape_xml(&mut xml, model_name);
    xml.push_str("\"><mesh><vertices>");
    for vertex in &mesh.vertices {
        write!(
            xml,
            "<vertex x=\"{:?}\" y=\"{:?}\" z=\"{:?}\"/>",
            vertex[0], vertex[1], vertex[2]
        )
        .expect("writing XML to a string");
    }
    if let Some(lattice) = lattice {
        for vertex in &lattice.vertices_mm {
            write!(
                xml,
                "<vertex x=\"{:?}\" y=\"{:?}\" z=\"{:?}\"/>",
                vertex[0], vertex[1], vertex[2]
            )
            .expect("writing XML to a string");
        }
    }
    xml.push_str("</vertices><triangles>");
    for triangle in &mesh.triangles {
        write!(
            xml,
            "<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>",
            triangle[0], triangle[1], triangle[2]
        )
        .expect("writing XML to a string");
    }
    xml.push_str("</triangles>");
    if let Some(lattice) = lattice {
        write!(
            xml,
            "<b:beamlattice minlength=\"{:?}\" radius=\"{:?}\" clippingmode=\"none\"><b:beams>",
            lattice.min_length_mm, lattice.default_radius_mm
        )
        .expect("writing XML to a string");
        let offset = mesh.vertices.len() as u32;
        for beam in &lattice.beams {
            write!(
                xml,
                "<b:beam v1=\"{}\" v2=\"{}\" r1=\"{:?}\" r2=\"{:?}\"/>",
                beam.v1 + offset,
                beam.v2 + offset,
                beam.r1_mm,
                beam.r2_mm
            )
            .expect("writing XML to a string");
        }
        xml.push_str("</b:beams></b:beamlattice>");
    }
    xml.push_str("</mesh></object></resources><build><item objectid=\"1\"/></build></model>");
    if xml.len() > MAX_MODEL_PART_BYTES {
        return Err(MeshError::ResourceLimit("3MF model part bytes"));
    }
    Ok(xml)
}

fn escape_xml(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '\t' => output.push_str("&#x9;"),
            '\n' => output.push_str("&#xA;"),
            '\r' => output.push_str("&#xD;"),
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            character => output.push(character),
        }
    }
}

fn validate_model_name(value: &str) -> Result<(), MeshError> {
    if value.chars().any(|character| {
        let code = character as u32;
        (code < 0x20 && !matches!(character, '\t' | '\n' | '\r'))
            || code == 0xFFFE
            || code == 0xFFFF
    }) {
        return Err(MeshError::InvalidFormat(
            "3MF model name contains an XML-invalid character".to_owned(),
        ));
    }
    Ok(())
}

fn write_archive(model: &[u8]) -> Result<Vec<u8>, MeshError> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default());
    write_entry(&mut zip, "[Content_Types].xml", &content_types(), options)?;
    write_entry(&mut zip, "_rels/.rels", &relationships(), options)?;
    write_entry(&mut zip, "3D/3dmodel.model", model, options)?;
    zip.finish()
        .map_err(|error| MeshError::InvalidFormat(format!("cannot finish 3MF ZIP: {error}")))
        .map(|cursor| cursor.into_inner())
}

fn write_entry<W: std::io::Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &str,
    content: &[u8],
    options: SimpleFileOptions,
) -> Result<(), MeshError> {
    zip.start_file(path, options)
        .map_err(|error| MeshError::InvalidFormat(format!("cannot create 3MF ZIP: {error}")))?;
    zip.write_all(content)
        .map_err(|error| MeshError::InvalidFormat(format!("cannot write 3MF ZIP entry: {error}")))
}

fn content_types() -> Vec<u8> {
    br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/3D/3dmodel.model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>"#
        .to_vec()
}

fn relationships() -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{OPC_NS}"><Relationship Id="rel-0" Target="/3D/3dmodel.model" Type="{MODEL_RELATIONSHIP}"/></Relationships>"#
    )
    .into_bytes()
}

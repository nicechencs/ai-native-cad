//! Little-endian `SUBD` blob for `blobs/subd/` content-addressed payloads.
//!
//! ```text
//! offset  size        field
//! 0       4           magic b"SUBD"
//! 4       4           version u32 = 1
//! 8       4           vertex_count u32
//! 12      4           face_count u32
//! 16      4           crease_count u32
//! 20      24 * V      vertex xyz as f64
//! then each face:
//!         4           corner_count u32 (>= 3)
//!         4 * N       corner indices u32
//! then each crease, in ascending (a, b) order:
//!         4 + 4 + 4   a u32, b u32 (a < b), sharpness f32
//! ```
//!
//! Encoding walks the crease map in sorted order, so the same cage always
//! produces the same bytes.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::cage::{SubDCage, SubDError, fit_u32};

/// Four-byte magic at the start of every SubD blob.
pub const SUBD_MAGIC: [u8; 4] = *b"SUBD";

/// Format version written by [`encode_subd_blob`].
pub const SUBD_VERSION: u32 = 1;

const HEADER_LEN: usize = 20;
const VERTEX_STRIDE: usize = 24;
const CREASE_STRIDE: usize = 12;
const MIN_FACE_STRIDE: usize = 16;

/// Encode a validated cage. Creases are written in `BTreeMap` key order.
pub fn encode_subd_blob(cage: &SubDCage) -> Result<Vec<u8>, SubDError> {
    cage.validate()?;
    let vertex_count = fit_u32(cage.vertices.len())?;
    let face_count = fit_u32(cage.faces.len())?;
    let crease_count = fit_u32(cage.edge_creases.len())?;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&SUBD_MAGIC);
    push_u32(&mut bytes, SUBD_VERSION);
    push_u32(&mut bytes, vertex_count);
    push_u32(&mut bytes, face_count);
    push_u32(&mut bytes, crease_count);
    for vertex in &cage.vertices {
        for coordinate in vertex {
            bytes.extend_from_slice(&coordinate.to_le_bytes());
        }
    }
    for face in &cage.faces {
        push_u32(&mut bytes, fit_u32(face.len())?);
        for index in face {
            push_u32(&mut bytes, *index);
        }
    }
    for (&(a, b), &sharpness) in &cage.edge_creases {
        push_u32(&mut bytes, a);
        push_u32(&mut bytes, b);
        bytes.extend_from_slice(&sharpness.to_le_bytes());
    }
    Ok(bytes)
}

/// Decode a version-1 blob. Truncation, bad magic, and non-finite numbers fail.
pub fn decode_subd_blob(bytes: &[u8]) -> Result<SubDCage, SubDError> {
    if bytes.len() < 4 {
        return Err(SubDError::TruncatedBlob);
    }
    if bytes[0..4] != SUBD_MAGIC {
        return Err(SubDError::BadMagic);
    }
    if bytes.len() < HEADER_LEN {
        return Err(SubDError::TruncatedBlob);
    }

    let mut reader = Reader::new(bytes);
    let _magic = reader.take(4)?;
    let version = reader.u32()?;
    if version != SUBD_VERSION {
        return Err(SubDError::UnsupportedVersion { version });
    }
    let vertex_count = reader.u32()?;
    let face_count = reader.u32()?;
    let crease_count = reader.u32()?;

    let vertex_bytes = (vertex_count as usize)
        .checked_mul(VERTEX_STRIDE)
        .ok_or(SubDError::InvalidCount)?;
    if reader.remaining() < vertex_bytes {
        return Err(SubDError::TruncatedBlob);
    }
    let mut vertices = Vec::with_capacity(vertex_count as usize);
    for _ in 0..vertex_count {
        let x = reader.f64()?;
        let y = reader.f64()?;
        let z = reader.f64()?;
        vertices.push([x, y, z]);
    }

    let min_face_bytes = (face_count as usize)
        .checked_mul(MIN_FACE_STRIDE)
        .ok_or(SubDError::InvalidCount)?;
    if reader.remaining() < min_face_bytes {
        return Err(SubDError::TruncatedBlob);
    }
    let mut faces = Vec::with_capacity(face_count as usize);
    for face_index in 0..face_count {
        let corner_count = reader.u32()?;
        if corner_count < 3 {
            return Err(SubDError::DegenerateFace {
                face: face_index as usize,
                count: corner_count as usize,
            });
        }
        let index_bytes = (corner_count as usize)
            .checked_mul(4)
            .ok_or(SubDError::InvalidCount)?;
        if reader.remaining() < index_bytes {
            return Err(SubDError::TruncatedBlob);
        }
        let mut face = Vec::with_capacity(corner_count as usize);
        for _ in 0..corner_count {
            face.push(reader.u32()?);
        }
        faces.push(face);
    }

    let crease_bytes = (crease_count as usize)
        .checked_mul(CREASE_STRIDE)
        .ok_or(SubDError::InvalidCount)?;
    if reader.remaining() < crease_bytes {
        return Err(SubDError::TruncatedBlob);
    }
    if reader.remaining() != crease_bytes {
        return Err(SubDError::TrailingBytes {
            count: reader.remaining() - crease_bytes,
        });
    }

    let mut edge_creases = BTreeMap::new();
    for _ in 0..crease_count {
        let a = reader.u32()?;
        let b = reader.u32()?;
        let sharpness = reader.f32()?;
        if a >= b {
            return Err(SubDError::CreaseKeyNotNormalized { a, b });
        }
        if edge_creases.insert((a, b), sharpness).is_some() {
            return Err(SubDError::DuplicateCrease { a, b });
        }
    }
    if reader.remaining() != 0 {
        return Err(SubDError::TrailingBytes {
            count: reader.remaining(),
        });
    }

    let cage = SubDCage {
        vertices,
        faces,
        edge_creases,
    };
    cage.validate()?;
    Ok(cage)
}

/// Lower-case hex SHA-256 of the raw blob bytes.
pub fn subd_blob_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], SubDError> {
        let end = self.pos.checked_add(len).ok_or(SubDError::InvalidCount)?;
        if end > self.bytes.len() {
            return Err(SubDError::TruncatedBlob);
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, SubDError> {
        let bytes = self.take(4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(buf))
    }

    fn f32(&mut self) -> Result<f32, SubDError> {
        let bytes = self.take(4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(bytes);
        Ok(f32::from_le_bytes(buf))
    }

    fn f64(&mut self) -> Result<f64, SubDError> {
        let bytes = self.take(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(bytes);
        Ok(f64::from_le_bytes(buf))
    }
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

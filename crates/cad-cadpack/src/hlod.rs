//! Binary codecs for the multi-resolution prototype meshes and exact-pick data.
//!
//! The codecs in this module deliberately use explicit little-endian writes rather
//! than a Rust memory representation.  This makes the bytes stable across target
//! architectures and keeps decoding independent of struct padding.  A prototype
//! chunk starts with a 56-byte `HLD1` header followed by the four packed arrays in
//! this order: vertices, normals, face-slot IDs, and indices.  Exact-pick records
//! are fixed-width 64-byte rows.
//!
//! The mesh header stores magic (`0..4`), version (`4..8`), prototype ID
//! (`8..12`), level (`12`), three reserved bytes, the f64 anchor (`16..40`), and
//! four u32 counts (`40..56`) in array order. Each pick row stores a u32 slot
//! ID (`0..4`), a surface code (`4`), three reserved bytes, f64 origin and axis
//! vectors (`8..32`, `32..56`), and the f64 radius (`56..64`).

use std::collections::HashMap;
use std::convert::TryFrom;
use thiserror::Error;

const PROTOTYPE_MAGIC: [u8; 4] = *b"HLD1";
const PROTOTYPE_VERSION: u32 = 1;
const PROTOTYPE_HEADER_LEN: usize = 56;
pub const EXACT_PICK_RECORD_SIZE: usize = 64;

/// Errors returned when a mesh or exact-pick byte stream is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HlodCodecError {
    #[error("truncated HLOD data: need {needed} bytes, have {available}")]
    Truncated { needed: usize, available: usize },
    #[error("invalid HLOD magic: {0:?}")]
    InvalidMagic([u8; 4]),
    #[error("unsupported HLOD codec version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid HLOD level {0}")]
    InvalidLevel(u8),
    #[error("invalid {field} count {value}")]
    InvalidCount { field: &'static str, value: u64 },
    #[error("duplicate exact-pick face slot ID {0}")]
    DuplicateFaceSlotId(u32),
    #[error("HLOD byte length overflow")]
    LengthOverflow,
    #[error("HLOD byte length mismatch: expected {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
}

/// Resolution of a prototype mesh, from the finest representation to the
/// coarse first-frame representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HlodLevel {
    Hlod0Fine,
    Hlod1Medium,
    Hlod2Coarse,
}

impl HlodLevel {
    fn from_wire(value: u8) -> Result<Self, HlodCodecError> {
        match value {
            0 => Ok(Self::Hlod0Fine),
            1 => Ok(Self::Hlod1Medium),
            2 => Ok(Self::Hlod2Coarse),
            other => Err(HlodCodecError::InvalidLevel(other)),
        }
    }

    /// Returns the stable one-byte representation used by the binary codec.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Hlod0Fine => 0,
            Self::Hlod1Medium => 1,
            Self::Hlod2Coarse => 2,
        }
    }
}

/// A prototype mesh at one HLOD resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct PrototypeHlodChunk {
    pub prototype_id: u32,
    pub level: HlodLevel,
    pub anchor_mm: [f64; 3],
    pub vertices_local: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub face_slot_ids: Vec<u32>,
    pub indices: Vec<u32>,
}

impl PrototypeHlodChunk {
    /// Encodes this chunk using the canonical little-endian wire format.
    pub fn encode(&self) -> Result<Vec<u8>, HlodCodecError> {
        encode_prototype_hlod_chunk(self)
    }

    /// Decodes one complete prototype chunk.
    pub fn decode(bytes: &[u8]) -> Result<Self, HlodCodecError> {
        decode_prototype_hlod_chunk(bytes)
    }
}

/// Encodes one prototype HLOD chunk.
fn encode_prototype_hlod_chunk(chunk: &PrototypeHlodChunk) -> Result<Vec<u8>, HlodCodecError> {
    let vertex_count = checked_u32_len(chunk.vertices_local.len(), "vertices")?;
    let normal_count = checked_u32_len(chunk.normals.len(), "normals")?;
    let face_slot_count = checked_u32_len(chunk.face_slot_ids.len(), "face_slot_ids")?;
    let index_count = checked_u32_len(chunk.indices.len(), "indices")?;

    let vertex_bytes = checked_mul(chunk.vertices_local.len(), 12)?;
    let normal_bytes = checked_mul(chunk.normals.len(), 12)?;
    let face_slot_bytes = checked_mul(chunk.face_slot_ids.len(), 4)?;
    let index_bytes = checked_mul(chunk.indices.len(), 4)?;
    let body_len = vertex_bytes
        .checked_add(normal_bytes)
        .and_then(|value| value.checked_add(face_slot_bytes))
        .and_then(|value| value.checked_add(index_bytes))
        .ok_or(HlodCodecError::LengthOverflow)?;
    let total_len = PROTOTYPE_HEADER_LEN
        .checked_add(body_len)
        .ok_or(HlodCodecError::LengthOverflow)?;

    let mut bytes = Vec::with_capacity(total_len);
    bytes.extend_from_slice(&PROTOTYPE_MAGIC);
    push_u32(&mut bytes, PROTOTYPE_VERSION);
    push_u32(&mut bytes, chunk.prototype_id);
    bytes.push(chunk.level.as_u8());
    bytes.extend_from_slice(&[0; 3]);
    for value in chunk.anchor_mm {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    push_u32(&mut bytes, vertex_count);
    push_u32(&mut bytes, normal_count);
    push_u32(&mut bytes, face_slot_count);
    push_u32(&mut bytes, index_count);

    for vertex in &chunk.vertices_local {
        for value in vertex {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    for normal in &chunk.normals {
        for value in normal {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    for value in &chunk.face_slot_ids {
        push_u32(&mut bytes, *value);
    }
    for value in &chunk.indices {
        push_u32(&mut bytes, *value);
    }

    debug_assert_eq!(bytes.len(), total_len);
    Ok(bytes)
}

/// Decodes one complete prototype HLOD chunk and rejects trailing bytes.
fn decode_prototype_hlod_chunk(bytes: &[u8]) -> Result<PrototypeHlodChunk, HlodCodecError> {
    if bytes.len() < PROTOTYPE_HEADER_LEN {
        return Err(HlodCodecError::Truncated {
            needed: PROTOTYPE_HEADER_LEN,
            available: bytes.len(),
        });
    }
    let magic = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if magic != PROTOTYPE_MAGIC {
        return Err(HlodCodecError::InvalidMagic(magic));
    }
    let version = read_u32(bytes, 4);
    if version != PROTOTYPE_VERSION {
        return Err(HlodCodecError::UnsupportedVersion(version));
    }
    let prototype_id = read_u32(bytes, 8);
    let level = HlodLevel::from_wire(bytes[12])?;
    let anchor_mm = [
        read_f64(bytes, 16),
        read_f64(bytes, 24),
        read_f64(bytes, 32),
    ];
    let vertex_count =
        usize::try_from(read_u32(bytes, 40)).map_err(|_| HlodCodecError::InvalidCount {
            field: "vertices",
            value: u64::from(read_u32(bytes, 40)),
        })?;
    let normal_count =
        usize::try_from(read_u32(bytes, 44)).map_err(|_| HlodCodecError::InvalidCount {
            field: "normals",
            value: u64::from(read_u32(bytes, 44)),
        })?;
    let face_slot_count =
        usize::try_from(read_u32(bytes, 48)).map_err(|_| HlodCodecError::InvalidCount {
            field: "face_slot_ids",
            value: u64::from(read_u32(bytes, 48)),
        })?;
    let index_count =
        usize::try_from(read_u32(bytes, 52)).map_err(|_| HlodCodecError::InvalidCount {
            field: "indices",
            value: u64::from(read_u32(bytes, 52)),
        })?;

    let vertex_bytes = checked_mul(vertex_count, 12)?;
    let normal_bytes = checked_mul(normal_count, 12)?;
    let face_slot_bytes = checked_mul(face_slot_count, 4)?;
    let index_bytes = checked_mul(index_count, 4)?;
    let expected_len = PROTOTYPE_HEADER_LEN
        .checked_add(vertex_bytes)
        .and_then(|value| value.checked_add(normal_bytes))
        .and_then(|value| value.checked_add(face_slot_bytes))
        .and_then(|value| value.checked_add(index_bytes))
        .ok_or(HlodCodecError::LengthOverflow)?;
    if bytes.len() != expected_len {
        return Err(HlodCodecError::LengthMismatch {
            expected: expected_len,
            actual: bytes.len(),
        });
    }

    let mut cursor = PROTOTYPE_HEADER_LEN;
    let mut vertices_local = Vec::with_capacity(vertex_count);
    for _ in 0..vertex_count {
        vertices_local.push([
            read_f32(bytes, cursor),
            read_f32(bytes, cursor + 4),
            read_f32(bytes, cursor + 8),
        ]);
        cursor += 12;
    }
    let mut normals = Vec::with_capacity(normal_count);
    for _ in 0..normal_count {
        normals.push([
            read_f32(bytes, cursor),
            read_f32(bytes, cursor + 4),
            read_f32(bytes, cursor + 8),
        ]);
        cursor += 12;
    }
    let mut face_slot_ids = Vec::with_capacity(face_slot_count);
    for _ in 0..face_slot_count {
        face_slot_ids.push(read_u32(bytes, cursor));
        cursor += 4;
    }
    let mut indices = Vec::with_capacity(index_count);
    for _ in 0..index_count {
        indices.push(read_u32(bytes, cursor));
        cursor += 4;
    }

    Ok(PrototypeHlodChunk {
        prototype_id,
        level,
        anchor_mm,
        vertices_local,
        normals,
        face_slot_ids,
        indices,
    })
}

/// Fixed-layout metadata used to resolve an exact face pick without loading B-Rep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExactPickRecord {
    pub face_slot_id: u32,
    pub surface_kind: u8,
    pub origin_mm: [f64; 3],
    pub axis_or_normal: [f64; 3],
    pub radius_mm: f64,
}

impl ExactPickRecord {
    /// Encodes this metadata row as its canonical fixed-width representation.
    pub fn encode(&self) -> [u8; EXACT_PICK_RECORD_SIZE] {
        let mut bytes = [0_u8; EXACT_PICK_RECORD_SIZE];
        bytes[0..4].copy_from_slice(&self.face_slot_id.to_le_bytes());
        bytes[4] = self.surface_kind;
        let mut cursor = 8;
        for value in self.origin_mm {
            bytes[cursor..cursor + 8].copy_from_slice(&value.to_le_bytes());
            cursor += 8;
        }
        for value in self.axis_or_normal {
            bytes[cursor..cursor + 8].copy_from_slice(&value.to_le_bytes());
            cursor += 8;
        }
        bytes[cursor..cursor + 8].copy_from_slice(&self.radius_mm.to_le_bytes());
        bytes
    }

    /// Decodes one complete 64-byte metadata row.
    pub fn decode(bytes: &[u8]) -> Result<Self, HlodCodecError> {
        if bytes.len() != EXACT_PICK_RECORD_SIZE {
            return Err(HlodCodecError::LengthMismatch {
                expected: EXACT_PICK_RECORD_SIZE,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            face_slot_id: read_u32(bytes, 0),
            surface_kind: bytes[4],
            origin_mm: [read_f64(bytes, 8), read_f64(bytes, 16), read_f64(bytes, 24)],
            axis_or_normal: [
                read_f64(bytes, 32),
                read_f64(bytes, 40),
                read_f64(bytes, 48),
            ],
            radius_mm: read_f64(bytes, 56),
        })
    }
}

/// Encodes exact-pick rows as a contiguous fixed-width table.
pub fn encode_exact_pick_records(records: &[ExactPickRecord]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(records.len() * EXACT_PICK_RECORD_SIZE);
    for record in records {
        bytes.extend_from_slice(&record.encode());
    }
    bytes
}

/// Decodes a complete exact-pick table.
pub fn decode_exact_pick_records(bytes: &[u8]) -> Result<Vec<ExactPickRecord>, HlodCodecError> {
    if !bytes.len().is_multiple_of(EXACT_PICK_RECORD_SIZE) {
        return Err(HlodCodecError::LengthMismatch {
            expected: bytes.len() / EXACT_PICK_RECORD_SIZE * EXACT_PICK_RECORD_SIZE,
            actual: bytes.len(),
        });
    }
    let count = bytes.len() / EXACT_PICK_RECORD_SIZE;
    let mut records = Vec::with_capacity(count);
    for index in 0..count {
        records.push(exact_pick_record_at(bytes, index)?);
    }
    Ok(records)
}

/// Reads one exact-pick row by its zero-based row number in O(1) time.
///
/// The table is row-addressed.  `face_slot_id` is carried in each row and may
/// be sparse or non-monotonic, so looking up by that value requires a separate
/// index.  Keeping this primitive row-addressed preserves O(1) access without
/// silently imposing a contiguous face-slot-ID contract on imported CAD data.
pub fn exact_pick_record_at(bytes: &[u8], row: usize) -> Result<ExactPickRecord, HlodCodecError> {
    let offset = row
        .checked_mul(EXACT_PICK_RECORD_SIZE)
        .ok_or(HlodCodecError::LengthOverflow)?;
    let end = offset
        .checked_add(EXACT_PICK_RECORD_SIZE)
        .ok_or(HlodCodecError::LengthOverflow)?;
    if end > bytes.len() {
        return Err(HlodCodecError::Truncated {
            needed: end,
            available: bytes.len(),
        });
    }
    ExactPickRecord::decode(&bytes[offset..end])
}

/// An indexed view over a fixed-layout exact-pick byte table.
///
/// Construction validates the table length and builds an in-memory map from
/// sparse `face_slot_id` values to row numbers.  Duplicate IDs are rejected so
/// [`Self::get`] has an unambiguous result.  The original records are kept in
/// the caller-owned byte slice and each lookup decodes only one 64-byte row.
#[derive(Debug)]
pub struct ExactPickTable<'a> {
    bytes: &'a [u8],
    row_by_face_slot: HashMap<u32, usize>,
}

impl<'a> ExactPickTable<'a> {
    /// Validates and indexes a complete exact-pick table.
    pub fn new(bytes: &'a [u8]) -> Result<Self, HlodCodecError> {
        if !bytes.len().is_multiple_of(EXACT_PICK_RECORD_SIZE) {
            return Err(HlodCodecError::LengthMismatch {
                expected: bytes.len() / EXACT_PICK_RECORD_SIZE * EXACT_PICK_RECORD_SIZE,
                actual: bytes.len(),
            });
        }

        let count = bytes.len() / EXACT_PICK_RECORD_SIZE;
        let mut row_by_face_slot = HashMap::with_capacity(count);
        for row in 0..count {
            let offset = row * EXACT_PICK_RECORD_SIZE;
            let face_slot_id = read_u32(bytes, offset);
            if row_by_face_slot.insert(face_slot_id, row).is_some() {
                return Err(HlodCodecError::DuplicateFaceSlotId(face_slot_id));
            }
        }

        Ok(Self {
            bytes,
            row_by_face_slot,
        })
    }

    /// Returns the number of rows in the validated table.
    pub fn len(&self) -> usize {
        self.bytes.len() / EXACT_PICK_RECORD_SIZE
    }

    /// Returns whether the validated table has no rows.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Looks up a record by sparse face-slot ID in expected O(1) time.
    pub fn get(&self, face_slot_id: u32) -> Option<ExactPickRecord> {
        self.row_by_face_slot
            .get(&face_slot_id)
            .and_then(|row| exact_pick_record_at(self.bytes, *row).ok())
    }

    /// Reads a record by its zero-based row number in O(1) time.
    pub fn get_row(&self, row: usize) -> Result<ExactPickRecord, HlodCodecError> {
        exact_pick_record_at(self.bytes, row)
    }

    /// Returns the encoded table bytes without copying them.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

fn checked_u32_len(length: usize, field: &'static str) -> Result<u32, HlodCodecError> {
    u32::try_from(length).map_err(|_| HlodCodecError::InvalidCount {
        field,
        value: length as u64,
    })
}

fn checked_mul(left: usize, right: usize) -> Result<usize, HlodCodecError> {
    left.checked_mul(right)
        .ok_or(HlodCodecError::LengthOverflow)
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    let mut value = [0_u8; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(value)
}

fn read_f32(bytes: &[u8], offset: usize) -> f32 {
    let mut value = [0_u8; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    f32::from_le_bytes(value)
}

fn read_f64(bytes: &[u8], offset: usize) -> f64 {
    let mut value = [0_u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    f64::from_le_bytes(value)
}

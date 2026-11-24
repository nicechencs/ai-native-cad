//! Voxel scalar fields and the little-endian `FLDS` blob for `blobs/fields/`.
//!
//! A finite-element solve stores one stress or temperature sample on a regular
//! 3D grid. This module reads that grid back as a continuous millimeter field
//! and can turn Von Mises stress into a wall thickness for a TPMS sheet.
//!
//! ```text
//! offset  size        field
//! 0       4           magic b"FLDS"
//! 4       4           version u32 = 1
//! 8       24          bbox_min_mm as 3 × f64, little-endian
//! 32      24          bbox_max_mm as 3 × f64, little-endian
//! 56      12          dims nx, ny, nz as 3 × u32
//! 68      4 × N       values as N × f32, x fastest: x + nx * (y + ny * z)
//! ```

use std::sync::Arc;

use crate::field::ScalarField;

/// Four-byte magic at the start of every field blob.
pub const FLDS_MAGIC: [u8; 4] = *b"FLDS";

/// Format version written by [`encode_voxel_field_blob`].
pub const FLDS_VERSION: u32 = 1;

const HEADER_LEN: usize = 68;

/// Why a voxel field, an `FLDS` blob, or a stress-to-thickness map was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ImplicitError {
    /// The first four bytes are not `FLDS`.
    #[error("FLDS blob magic is not FLDS")]
    BadMagic,

    /// The byte slice ends before a complete field.
    #[error("FLDS blob is truncated")]
    TruncatedBlob,

    /// Bytes remain after the last sample.
    #[error("FLDS blob has {count} trailing bytes")]
    TrailingBytes { count: usize },

    /// The blob header names a format version this crate does not read.
    #[error("FLDS blob version {version} is not supported")]
    UnsupportedVersion { version: u32 },

    /// A dimension is below 2, does not fit `u32`, or does not match `values`.
    #[error("voxel dimensions must be at least 2 and match the sample count")]
    InvalidDims,

    /// A grid sample is NaN or infinite.
    #[error("voxel sample {index} is not finite")]
    NonFiniteValue { index: usize },

    /// A bounding-box coordinate is NaN or infinite.
    #[error("voxel bounds are not finite")]
    NonFiniteBound,

    /// The box has no positive length on at least one axis.
    #[error("each voxel bound max must be greater than min")]
    DegenerateBounds,

    /// A stress or thickness limit is NaN or infinite.
    #[error("stress and thickness limits must be finite")]
    NonFiniteRange,

    /// The two stress limits are the same, so the map has no slope.
    #[error("stress limits must differ")]
    ZeroStressSpan,
}

/// Regular 3D grid of scalar samples in physical millimeters.
///
/// Samples sit on the corners of `bbox_min_mm` and `bbox_max_mm`. Query points
/// outside the box use the nearest face. Points inside use trilinear
/// interpolation. `values` uses x as the fastest index.
#[derive(Debug, Clone, PartialEq)]
pub struct VoxelScalarField {
    /// Minimum corner of the grid, in millimeters.
    pub bbox_min_mm: [f64; 3],
    /// Maximum corner of the grid, in millimeters.
    pub bbox_max_mm: [f64; 3],
    /// Sample counts `(nx, ny, nz)`. Each count is at least 2.
    pub dims: [usize; 3],
    /// Finite samples, length `nx * ny * nz`.
    pub values: Vec<f32>,
}

impl VoxelScalarField {
    /// Builds a grid after checking bounds, dimensions, and samples.
    #[must_use]
    pub fn try_new(
        bbox_min_mm: [f64; 3],
        bbox_max_mm: [f64; 3],
        dims: [usize; 3],
        values: Vec<f32>,
    ) -> Result<Self, ImplicitError> {
        let field = Self {
            bbox_min_mm,
            bbox_max_mm,
            dims,
            values,
        };
        validate_voxel_field(&field)?;
        Ok(field)
    }
}

impl ScalarField for VoxelScalarField {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let [nx, ny, nz] = self.dims;
        let count = nx.checked_mul(ny).and_then(|plane| plane.checked_mul(nz));
        if nx < 2 || ny < 2 || nz < 2 || count != Some(self.values.len()) {
            return 0.0;
        }

        let (ix, tx) = clamped_axis(self.bbox_min_mm[0], self.bbox_max_mm[0], nx, p[0]);
        let (iy, ty) = clamped_axis(self.bbox_min_mm[1], self.bbox_max_mm[1], ny, p[1]);
        let (iz, tz) = clamped_axis(self.bbox_min_mm[2], self.bbox_max_mm[2], nz, p[2]);
        let at = |x, y, z| f64::from(self.values[x + nx * (y + ny * z)]);

        let c00 = lerp(at(ix, iy, iz), at(ix + 1, iy, iz), tx);
        let c10 = lerp(at(ix, iy + 1, iz), at(ix + 1, iy + 1, iz), tx);
        let c01 = lerp(at(ix, iy, iz + 1), at(ix + 1, iy, iz + 1), tx);
        let c11 = lerp(at(ix, iy + 1, iz + 1), at(ix + 1, iy + 1, iz + 1), tx);
        let c0 = lerp(c00, c10, ty);
        let c1 = lerp(c01, c11, ty);
        lerp(c0, c1, tz)
    }
}

/// Writes a version-1 `FLDS` blob. The same field always produces the same bytes.
pub fn encode_voxel_field_blob(field: &VoxelScalarField) -> Result<Vec<u8>, ImplicitError> {
    validate_voxel_field(field)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&FLDS_MAGIC);
    push_u32(&mut bytes, FLDS_VERSION);
    for component in field.bbox_min_mm.into_iter().chain(field.bbox_max_mm) {
        bytes.extend_from_slice(&component.to_le_bytes());
    }
    for dim in field.dims {
        push_u32(
            &mut bytes,
            u32::try_from(dim).map_err(|_| ImplicitError::InvalidDims)?,
        );
    }
    for value in &field.values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

/// Reads a version-1 `FLDS` blob.
///
/// Bad magic, a short buffer, leftover bytes, a dimension below 2, and
/// non-finite numbers are rejected. Sample bits are kept as stored.
pub fn decode_voxel_field_blob(bytes: &[u8]) -> Result<VoxelScalarField, ImplicitError> {
    if bytes.len() < 4 {
        return Err(ImplicitError::TruncatedBlob);
    }
    if bytes[0..4] != FLDS_MAGIC {
        return Err(ImplicitError::BadMagic);
    }
    if bytes.len() < HEADER_LEN {
        return Err(ImplicitError::TruncatedBlob);
    }

    let mut reader = Reader::new(bytes);
    let _magic = reader.take(4)?;
    let version = reader.u32()?;
    if version != FLDS_VERSION {
        return Err(ImplicitError::UnsupportedVersion { version });
    }
    let bbox_min_mm = [reader.f64()?, reader.f64()?, reader.f64()?];
    let bbox_max_mm = [reader.f64()?, reader.f64()?, reader.f64()?];
    let dims = [
        reader.u32()? as usize,
        reader.u32()? as usize,
        reader.u32()? as usize,
    ];
    if dims.iter().any(|dim| *dim < 2) {
        return Err(ImplicitError::InvalidDims);
    }
    let count = dims[0]
        .checked_mul(dims[1])
        .and_then(|plane| plane.checked_mul(dims[2]))
        .ok_or(ImplicitError::InvalidDims)?;
    let payload_bytes = count.checked_mul(4).ok_or(ImplicitError::InvalidDims)?;
    if reader.remaining() < payload_bytes {
        return Err(ImplicitError::TruncatedBlob);
    }
    if reader.remaining() != payload_bytes {
        return Err(ImplicitError::TrailingBytes {
            count: reader.remaining() - payload_bytes,
        });
    }

    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(reader.f32()?);
    }
    let field = VoxelScalarField {
        bbox_min_mm,
        bbox_max_mm,
        dims,
        values,
    };
    validate_voxel_field(&field)?;
    Ok(field)
}

/// Maps a stress field in MPa onto a wall thickness in millimeters.
///
/// Stress is clamped to `[min_stress_mpa, max_stress_mpa]` and then stretched
/// linearly onto `[min_thickness_mm, max_thickness_mm]`. The result implements
/// [`ScalarField`], so it can be boxed straight into [`crate::TpmsSheetField`].
#[derive(Clone)]
pub struct StressDrivenThicknessField {
    stress: Arc<dyn ScalarField>,
    min_stress_mpa: f64,
    max_stress_mpa: f64,
    min_thickness_mm: f64,
    max_thickness_mm: f64,
}

impl StressDrivenThicknessField {
    /// Builds the map. The stress limits must be finite and different.
    /// Thickness limits must be finite. They may be equal or reversed.
    #[must_use]
    pub fn try_new(
        stress: Arc<dyn ScalarField>,
        min_stress_mpa: f64,
        max_stress_mpa: f64,
        min_thickness_mm: f64,
        max_thickness_mm: f64,
    ) -> Result<Self, ImplicitError> {
        if ![
            min_stress_mpa,
            max_stress_mpa,
            min_thickness_mm,
            max_thickness_mm,
        ]
        .into_iter()
        .all(f64::is_finite)
        {
            return Err(ImplicitError::NonFiniteRange);
        }
        let stress_span = max_stress_mpa - min_stress_mpa;
        let thickness_span = max_thickness_mm - min_thickness_mm;
        if !stress_span.is_finite() || !thickness_span.is_finite() {
            return Err(ImplicitError::NonFiniteRange);
        }
        if stress_span == 0.0 {
            return Err(ImplicitError::ZeroStressSpan);
        }
        Ok(Self {
            stress,
            min_stress_mpa,
            max_stress_mpa,
            min_thickness_mm,
            max_thickness_mm,
        })
    }
}

impl std::fmt::Debug for StressDrivenThicknessField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StressDrivenThicknessField")
            .field("min_stress_mpa", &self.min_stress_mpa)
            .field("max_stress_mpa", &self.max_stress_mpa)
            .field("min_thickness_mm", &self.min_thickness_mm)
            .field("max_thickness_mm", &self.max_thickness_mm)
            .finish_non_exhaustive()
    }
}

impl ScalarField for StressDrivenThicknessField {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let stress_mpa = self.stress.eval(p);
        let t = ((stress_mpa - self.min_stress_mpa) / (self.max_stress_mpa - self.min_stress_mpa))
            .clamp(0.0, 1.0);
        self.min_thickness_mm + t * (self.max_thickness_mm - self.min_thickness_mm)
    }
}

fn validate_voxel_field(field: &VoxelScalarField) -> Result<(), ImplicitError> {
    if field
        .dims
        .iter()
        .any(|dim| *dim < 2 || *dim > u32::MAX as usize)
    {
        return Err(ImplicitError::InvalidDims);
    }
    let count = field.dims[0]
        .checked_mul(field.dims[1])
        .and_then(|plane| plane.checked_mul(field.dims[2]))
        .ok_or(ImplicitError::InvalidDims)?;
    if count != field.values.len() {
        return Err(ImplicitError::InvalidDims);
    }
    if !field
        .bbox_min_mm
        .iter()
        .chain(&field.bbox_max_mm)
        .all(|component| component.is_finite())
    {
        return Err(ImplicitError::NonFiniteBound);
    }
    if field
        .bbox_min_mm
        .into_iter()
        .zip(field.bbox_max_mm)
        .any(|(min_mm, max_mm)| max_mm <= min_mm)
    {
        return Err(ImplicitError::DegenerateBounds);
    }
    if let Some(index) = field.values.iter().position(|value| !value.is_finite()) {
        return Err(ImplicitError::NonFiniteValue { index });
    }
    Ok(())
}

/// Fractional cell coordinate clamped to the grid, in units of cell index.
///
/// A query that lands on a node (within a tiny tolerance) snaps to that node so
/// the sample is returned unchanged.
fn clamped_axis(min_mm: f64, max_mm: f64, nodes: usize, point_mm: f64) -> (usize, f64) {
    let last = nodes.saturating_sub(1);
    let span = max_mm - min_mm;
    if nodes < 2 || !span.is_finite() || span <= 0.0 {
        return (0, 0.0);
    }
    let cells = last as f64;
    let mut t = ((point_mm - min_mm) / span * cells).clamp(0.0, cells);
    let nearest = t.round();
    if (t - nearest).abs() <= 1.0e-8 {
        t = nearest.clamp(0.0, cells);
    }
    if t >= cells {
        return (last - 1, 1.0);
    }
    let index = (t.floor() as usize).min(last - 1);
    (index, t - index as f64)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    (1.0 - t).mul_add(a, t * b)
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

    fn take(&mut self, len: usize) -> Result<&'a [u8], ImplicitError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(ImplicitError::TruncatedBlob)?;
        if end > self.bytes.len() {
            return Err(ImplicitError::TruncatedBlob);
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, ImplicitError> {
        let bytes = self.take(4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(buf))
    }

    fn f32(&mut self) -> Result<f32, ImplicitError> {
        let bytes = self.take(4)?;
        let mut buf = [0u8; 4];
        buf.copy_from_slice(bytes);
        Ok(f32::from_le_bytes(buf))
    }

    fn f64(&mut self) -> Result<f64, ImplicitError> {
        let bytes = self.take(8)?;
        let mut buf = [0u8; 8];
        buf.copy_from_slice(bytes);
        Ok(f64::from_le_bytes(buf))
    }
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

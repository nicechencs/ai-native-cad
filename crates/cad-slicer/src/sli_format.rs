//! EOS `SLI` (Slice Layer Interface) binary slice writer and reader.
//!
//! Encodes deterministic layer stacks (including `0.05 mm` and arbitrary layer
//! spacing) with polyline contour records, optional hatch scan-vector records,
//! and a trailing random-access layer index table for lossless round-trip
//! verification.
//!
//! ## Binary Layout (Little-Endian)
//!
//! - **Header (128 bytes)**:
//!   - `0..16`: magic `b"EOS 1993 SLI\r\n\0\0"`
//!   - `16..18`: format version `u16` (`2` = lossless IEEE-754 f64 coordinates)
//!   - `18..20`: flags `u16` (`0x0001` = f64 coordinates)
//!   - `20..24`: header length `u32` (`128`)
//!   - `24..32`: scale unit `f64` (mm per nominal machine unit, e.g. `0.001`)
//!   - `32..56`: `bbox_min_mm` (`[f64; 3]`)
//!   - `56..80`: `bbox_max_mm` (`[f64; 3]`)
//!   - `80..88`: `layer_height_mm` (`f64`)
//!   - `88..96`: `xy_step_mm` (`f64`)
//!   - `96..104`: `stitch_tol_mm` (`f64`)
//!   - `104..108`: `layer_count` (`u32`)
//!   - `108..112`: `polyline_count` (`u32`)
//!   - `112..116`: `hatch_count` (`u32`)
//!   - `116..120`: reserved (`u32` = 0)
//!   - `120..128`: `index_table_offset` (`u64`)
//! - **Geometry Stream (starting at byte 128)**:
//!   - For each layer (strictly increasing `z_mm`):
//!     - Layer record (`0x01`): `layer_index: u32`, `z_mm: f64`, `loop_count: u32`, `hatch_count: u32`
//!     - `loop_count` polyline records (`0x02`): `part_id: u32`, `direction: u8` (`1` = CCW outer, `0` = CW hole), `vertex_count: u32` (explicitly closed, includes closing vertex), followed by `vertex_count` `(x_mm: f64, y_mm: f64)` pairs
//!     - `hatch_count` hatch records (`0x03`): `part_id: u32`, `segment_count: u32`, followed by `segment_count` `(x0, y0, x1, y1)` `f64` tuples
//! - **Layer Index Table (at `index_table_offset`)**:
//!   - `layer_count` entries of `(z_mm: f64, byte_offset: u64)`

use crate::{ContourLoop, SliceConfig, SliceLayer, SliceStack, SlicerError};

/// 16-byte EOS SLI binary file signature.
pub const SLI_MAGIC: [u8; 16] = *b"EOS 1993 SLI\r\n\0\0";

/// Binary format version written by [`export_sli_binary`].
pub const SLI_VERSION: u16 = 2;

const SLI_FLAG_F64_COORDS: u16 = 0x0001;
const SLI_HEADER_LEN: usize = 128;

const TAG_LAYER: u8 = 0x01;
const TAG_POLYLINE: u8 = 0x02;
const TAG_HATCH: u8 = 0x03;

/// A single scan-vector hatch segment on a slice layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliHatchSegment {
    pub start_mm: [f64; 2],
    pub end_mm: [f64; 2],
}

/// Optional hatch vectors associated with a slice layer.
#[derive(Debug, Clone, PartialEq)]
pub struct SliLayerHatches {
    pub layer_index: usize,
    pub z_mm: f64,
    pub segments: Vec<SliHatchSegment>,
}

/// Parsed EOS SLI binary file header.
#[derive(Debug, Clone, PartialEq)]
pub struct SliHeader {
    pub version: u16,
    pub scale_mm: f64,
    pub bbox_min_mm: [f64; 3],
    pub bbox_max_mm: [f64; 3],
    pub layer_height_mm: f64,
    pub xy_step_mm: f64,
    pub stitch_tol_mm: f64,
    pub layer_count: usize,
    pub polyline_count: usize,
    pub hatch_count: usize,
    pub index_table_offset: u64,
}

/// A fully decoded EOS SLI document, preserving layers, contours, and hatches.
#[derive(Debug, Clone, PartialEq)]
pub struct SliDocument {
    pub header: SliHeader,
    pub stack: SliceStack,
    pub hatches: Vec<SliLayerHatches>,
}

/// Exports a [`SliceStack`] to EOS `SLI` binary format without hatch vectors.
pub fn export_sli_binary(stack: &SliceStack, scale_mm: f64) -> Result<Vec<u8>, SlicerError> {
    export_sli_binary_with_hatches(stack, &[], scale_mm)
}

/// Exports a [`SliceStack`] and optional per-layer hatch segments to EOS `SLI`
/// binary format.
pub fn export_sli_binary_with_hatches(
    stack: &SliceStack,
    hatches: &[SliLayerHatches],
    scale_mm: f64,
) -> Result<Vec<u8>, SlicerError> {
    if !scale_mm.is_finite() || scale_mm <= 0.0 {
        return Err(SlicerError::InvalidUnits);
    }
    validate_config(&stack.config)?;

    let layer_count = u32::try_from(stack.layers.len())
        .map_err(|_| SlicerError::InvalidConfig("too many SLI layers"))?;

    let mut out = vec![0u8; SLI_HEADER_LEN];
    let mut index_entries: Vec<(f64, u64)> = Vec::with_capacity(stack.layers.len());
    let mut total_polylines: usize = 0;
    let mut total_hatches: usize = 0;
    let mut previous_z: Option<f64> = None;

    for layer in &stack.layers {
        if !layer.z_mm.is_finite() {
            return Err(SlicerError::InvalidContour {
                layer_index: layer.layer_index,
                reason: "SLI layer Z height is not finite",
            });
        }
        if previous_z.is_some_and(|prev| layer.z_mm <= prev) {
            return Err(SlicerError::InvalidContour {
                layer_index: layer.layer_index,
                reason: "SLI layer heights must be strictly increasing",
            });
        }
        previous_z = Some(layer.z_mm);

        let layer_hatches: Vec<&SliLayerHatches> = hatches
            .iter()
            .filter(|h| h.layer_index == layer.layer_index)
            .collect();

        let layer_offset = out.len() as u64;
        index_entries.push((layer.z_mm, layer_offset));

        let layer_idx_u32 = u32::try_from(layer.layer_index).map_err(|_| {
            SlicerError::InvalidContour {
                layer_index: layer.layer_index,
                reason: "SLI layer index exceeds u32",
            }
        })?;
        let loop_count_u32 = u32::try_from(layer.loops.len()).map_err(|_| {
            SlicerError::InvalidContour {
                layer_index: layer.layer_index,
                reason: "SLI layer polyline count exceeds u32",
            }
        })?;
        let hatch_record_count_u32 = u32::try_from(layer_hatches.len()).map_err(|_| {
            SlicerError::InvalidContour {
                layer_index: layer.layer_index,
                reason: "SLI layer hatch record count exceeds u32",
            }
        })?;

        out.push(TAG_LAYER);
        out.extend_from_slice(&layer_idx_u32.to_le_bytes());
        out.extend_from_slice(&layer.z_mm.to_le_bytes());
        out.extend_from_slice(&loop_count_u32.to_le_bytes());
        out.extend_from_slice(&hatch_record_count_u32.to_le_bytes());

        for contour in &layer.loops {
            write_sli_polyline(&mut out, layer, contour)?;
            total_polylines += 1;
        }

        for hatch in layer_hatches {
            write_sli_hatch(&mut out, layer.layer_index, hatch)?;
            total_hatches += hatch.segments.len();
        }
    }

    let index_table_offset = out.len() as u64;
    for (z_mm, offset) in &index_entries {
        out.extend_from_slice(&z_mm.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }

    let total_polylines_u32 = u32::try_from(total_polylines)
        .map_err(|_| SlicerError::InvalidConfig("SLI polyline count exceeds u32"))?;
    let total_hatches_u32 = u32::try_from(total_hatches)
        .map_err(|_| SlicerError::InvalidConfig("SLI hatch count exceeds u32"))?;

    // Populate 128-byte header in place.
    let mut hdr = Vec::with_capacity(SLI_HEADER_LEN);
    hdr.extend_from_slice(&SLI_MAGIC);
    hdr.extend_from_slice(&SLI_VERSION.to_le_bytes());
    hdr.extend_from_slice(&SLI_FLAG_F64_COORDS.to_le_bytes());
    hdr.extend_from_slice(&(SLI_HEADER_LEN as u32).to_le_bytes());
    hdr.extend_from_slice(&scale_mm.to_le_bytes());
    for v in stack.config.bbox_min_mm {
        hdr.extend_from_slice(&v.to_le_bytes());
    }
    for v in stack.config.bbox_max_mm {
        hdr.extend_from_slice(&v.to_le_bytes());
    }
    hdr.extend_from_slice(&stack.config.layer_height_mm.to_le_bytes());
    hdr.extend_from_slice(&stack.config.xy_step_mm.to_le_bytes());
    hdr.extend_from_slice(&stack.config.stitch_tol_mm.to_le_bytes());
    hdr.extend_from_slice(&layer_count.to_le_bytes());
    hdr.extend_from_slice(&total_polylines_u32.to_le_bytes());
    hdr.extend_from_slice(&total_hatches_u32.to_le_bytes());
    hdr.extend_from_slice(&0u32.to_le_bytes());
    hdr.extend_from_slice(&index_table_offset.to_le_bytes());
    debug_assert_eq!(hdr.len(), SLI_HEADER_LEN);
    out[..SLI_HEADER_LEN].copy_from_slice(&hdr);

    Ok(out)
}

fn write_sli_polyline(
    out: &mut Vec<u8>,
    layer: &SliceLayer,
    contour: &ContourLoop,
) -> Result<(), SlicerError> {
    if !contour.closed {
        return Err(SlicerError::InvalidContour {
            layer_index: layer.layer_index,
            reason: "SLI contour is open",
        });
    }
    let mut points = contour.points_mm.as_slice();
    if points.len() > 1 && points.first() == points.last() {
        points = &points[..points.len() - 1];
    }
    if points.len() < 3 {
        return Err(SlicerError::InvalidContour {
            layer_index: layer.layer_index,
            reason: "SLI contour has fewer than three vertices",
        });
    }
    let vertex_count = u32::try_from(points.len() + 1).map_err(|_| {
        SlicerError::InvalidContour {
            layer_index: layer.layer_index,
            reason: "SLI contour has too many vertices",
        }
    })?;
    let area = signed_area(points);
    if !area.is_finite() || area == 0.0 || (area < 0.0) != contour.is_hole {
        return Err(SlicerError::InvalidContour {
            layer_index: layer.layer_index,
            reason: "SLI contour winding does not match its direction",
        });
    }

    let direction = u8::from(!contour.is_hole);
    out.push(TAG_POLYLINE);
    out.extend_from_slice(&1u32.to_le_bytes()); // part_id = 1
    out.push(direction);
    out.extend_from_slice(&vertex_count.to_le_bytes());

    for &[x, y] in points.iter().chain(&points[..1]) {
        if !x.is_finite() || !y.is_finite() {
            return Err(SlicerError::InvalidContour {
                layer_index: layer.layer_index,
                reason: "SLI contour contains non-finite vertex",
            });
        }
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
    }
    Ok(())
}

fn write_sli_hatch(
    out: &mut Vec<u8>,
    layer_index: usize,
    hatch: &SliLayerHatches,
) -> Result<(), SlicerError> {
    let seg_count = u32::try_from(hatch.segments.len()).map_err(|_| {
        SlicerError::InvalidContour {
            layer_index,
            reason: "SLI hatch segment count exceeds u32",
        }
    })?;
    out.push(TAG_HATCH);
    out.extend_from_slice(&1u32.to_le_bytes()); // part_id = 1
    out.extend_from_slice(&seg_count.to_le_bytes());
    for seg in &hatch.segments {
        for coord in [seg.start_mm[0], seg.start_mm[1], seg.end_mm[0], seg.end_mm[1]] {
            if !coord.is_finite() {
                return Err(SlicerError::InvalidContour {
                    layer_index,
                    reason: "SLI hatch contains non-finite coordinate",
                });
            }
            out.extend_from_slice(&coord.to_le_bytes());
        }
    }
    Ok(())
}

/// Parses only the 128-byte EOS SLI binary header.
pub fn parse_sli_binary_summary(bytes: &[u8]) -> Result<SliHeader, SlicerError> {
    if bytes.len() < SLI_HEADER_LEN {
        return Err(sli_error(0, "SLI buffer shorter than 128-byte header"));
    }
    if bytes[..16] != SLI_MAGIC {
        return Err(sli_error(0, "invalid SLI magic signature"));
    }
    let mut cursor = Cursor::new(bytes);
    cursor.pos = 16;
    let version = cursor.read_u16()?;
    if version != SLI_VERSION {
        return Err(sli_error(16, format!("unsupported SLI version {version}")));
    }
    let flags = cursor.read_u16()?;
    if flags != SLI_FLAG_F64_COORDS {
        return Err(sli_error(18, format!("unsupported SLI flags 0x{flags:04x}")));
    }
    let header_len = cursor.read_u32()? as usize;
    if header_len != SLI_HEADER_LEN {
        return Err(sli_error(20, format!("unexpected SLI header length {header_len}")));
    }
    let scale_mm = cursor.read_f64()?;
    if !scale_mm.is_finite() || scale_mm <= 0.0 {
        return Err(sli_error(24, "SLI scale must be finite and positive"));
    }
    let bbox_min_mm = [cursor.read_f64()?, cursor.read_f64()?, cursor.read_f64()?];
    let bbox_max_mm = [cursor.read_f64()?, cursor.read_f64()?, cursor.read_f64()?];
    let layer_height_mm = cursor.read_f64()?;
    let xy_step_mm = cursor.read_f64()?;
    let stitch_tol_mm = cursor.read_f64()?;
    let layer_count = cursor.read_u32()? as usize;
    let polyline_count = cursor.read_u32()? as usize;
    let hatch_count = cursor.read_u32()? as usize;
    let _reserved = cursor.read_u32()?;
    let index_table_offset = cursor.read_u64()?;

    let config = SliceConfig {
        bbox_min_mm,
        bbox_max_mm,
        layer_height_mm,
        xy_step_mm,
        stitch_tol_mm,
    };
    validate_config(&config).map_err(|_| sli_error(32, "invalid SLI bounding box or spacing"))?;

    Ok(SliHeader {
        version,
        scale_mm,
        bbox_min_mm,
        bbox_max_mm,
        layer_height_mm,
        xy_step_mm,
        stitch_tol_mm,
        layer_count,
        polyline_count,
        hatch_count,
        index_table_offset,
    })
}

/// Parses a complete EOS SLI binary buffer, validating the header, every layer
/// record, contour closure & winding, hatch records, and the trailing layer
/// index table.
pub fn parse_sli_binary(bytes: &[u8]) -> Result<SliDocument, SlicerError> {
    let header = parse_sli_binary_summary(bytes)?;
    let index_offset = usize::try_from(header.index_table_offset)
        .map_err(|_| sli_error(120, "SLI index table offset out of range"))?;
    if index_offset < SLI_HEADER_LEN || index_offset > bytes.len() {
        return Err(sli_error(120, "SLI index table offset out of bounds"));
    }

    let mut cursor = Cursor::new(bytes);
    cursor.pos = SLI_HEADER_LEN;

    let mut layers = Vec::with_capacity(header.layer_count);
    let mut hatches = Vec::new();
    let mut observed_offsets = Vec::with_capacity(header.layer_count);
    let mut observed_polylines = 0usize;
    let mut observed_hatches = 0usize;
    let mut previous_z: Option<f64> = None;

    for expected_idx in 0..header.layer_count {
        let layer_start = cursor.pos;
        let tag = cursor.read_u8()?;
        if tag != TAG_LAYER {
            return Err(sli_error(
                layer_start,
                format!("expected SLI layer tag 0x01, got 0x{tag:02x}"),
            ));
        }
        let layer_index = cursor.read_u32()? as usize;
        if layer_index != expected_idx {
            return Err(sli_error(
                layer_start + 1,
                format!("expected layer index {expected_idx}, got {layer_index}"),
            ));
        }
        let z_mm = cursor.read_f64()?;
        if !z_mm.is_finite() {
            return Err(sli_error(layer_start + 5, "non-finite SLI layer Z height"));
        }
        if previous_z.is_some_and(|prev| z_mm <= prev) {
            return Err(sli_error(
                layer_start + 5,
                "SLI layer heights must be strictly increasing",
            ));
        }
        previous_z = Some(z_mm);
        observed_offsets.push((z_mm, layer_start as u64));

        let loop_count = cursor.read_u32()? as usize;
        let hatch_record_count = cursor.read_u32()? as usize;
        let mut loops = Vec::with_capacity(loop_count);

        for _ in 0..loop_count {
            let poly_offset = cursor.pos;
            let ptag = cursor.read_u8()?;
            if ptag != TAG_POLYLINE {
                return Err(sli_error(
                    poly_offset,
                    format!("expected SLI polyline tag 0x02, got 0x{ptag:02x}"),
                ));
            }
            let _part_id = cursor.read_u32()?;
            let direction = cursor.read_u8()?;
            if direction > 1 {
                return Err(sli_error(poly_offset + 5, "invalid SLI polyline direction"));
            }
            let vertex_count = cursor.read_u32()? as usize;
            if vertex_count < 4 {
                return Err(sli_error(
                    poly_offset + 6,
                    "SLI closed polyline requires at least 4 coordinates (3 unique + closing)",
                ));
            }
            let mut raw_points = Vec::with_capacity(vertex_count);
            for _ in 0..vertex_count {
                let x = cursor.read_f64()?;
                let y = cursor.read_f64()?;
                if !x.is_finite() || !y.is_finite() {
                    return Err(sli_error(cursor.pos, "non-finite SLI polyline coordinate"));
                }
                raw_points.push([x, y]);
            }
            if raw_points.first() != raw_points.last() {
                return Err(sli_error(
                    poly_offset,
                    "SLI polyline is not explicitly closed",
                ));
            }
            raw_points.pop();
            let area = signed_area(&raw_points);
            let is_hole = direction == 0;
            if area == 0.0 || (area < 0.0) != is_hole {
                return Err(sli_error(
                    poly_offset,
                    "SLI polyline winding does not match declared direction",
                ));
            }
            loops.push(ContourLoop {
                points_mm: raw_points,
                signed_area_mm2: area,
                is_hole,
                closed: true,
            });
            observed_polylines += 1;
        }

        for _ in 0..hatch_record_count {
            let hatch_offset = cursor.pos;
            let htag = cursor.read_u8()?;
            if htag != TAG_HATCH {
                return Err(sli_error(
                    hatch_offset,
                    format!("expected SLI hatch tag 0x03, got 0x{htag:02x}"),
                ));
            }
            let _part_id = cursor.read_u32()?;
            let seg_count = cursor.read_u32()? as usize;
            let mut segments = Vec::with_capacity(seg_count);
            for _ in 0..seg_count {
                let x0 = cursor.read_f64()?;
                let y0 = cursor.read_f64()?;
                let x1 = cursor.read_f64()?;
                let y1 = cursor.read_f64()?;
                if ![x0, y0, x1, y1].into_iter().all(f64::is_finite) {
                    return Err(sli_error(cursor.pos, "non-finite SLI hatch coordinate"));
                }
                segments.push(SliHatchSegment {
                    start_mm: [x0, y0],
                    end_mm: [x1, y1],
                });
            }
            observed_hatches += segments.len();
            hatches.push(SliLayerHatches {
                layer_index,
                z_mm,
                segments,
            });
        }

        let net_area_mm2: f64 = loops.iter().map(|c| c.signed_area_mm2).sum();
        let total_perimeter_mm: f64 = loops.iter().map(perimeter).sum();
        layers.push(SliceLayer {
            layer_index,
            z_mm,
            loops,
            net_area_mm2,
            total_perimeter_mm,
        });
    }

    if cursor.pos != index_offset {
        return Err(sli_error(
            cursor.pos,
            "SLI geometry stream does not end at declared index_table_offset",
        ));
    }

    for (expected_z, expected_offset) in &observed_offsets {
        let entry_pos = cursor.pos;
        let idx_z = cursor.read_f64()?;
        let idx_off = cursor.read_u64()?;
        if idx_z.to_bits() != expected_z.to_bits() || idx_off != *expected_offset {
            return Err(sli_error(
                entry_pos,
                "SLI layer index table entry does not match geometry stream",
            ));
        }
    }

    if cursor.pos != bytes.len() {
        return Err(sli_error(
            cursor.pos,
            format!("{} trailing bytes after SLI index table", bytes.len() - cursor.pos),
        ));
    }
    if observed_polylines != header.polyline_count {
        return Err(sli_error(
            108,
            "SLI header polyline_count does not match geometry stream",
        ));
    }
    if observed_hatches != header.hatch_count {
        return Err(sli_error(
            112,
            "SLI header hatch_count does not match geometry stream",
        ));
    }

    let config = SliceConfig {
        bbox_min_mm: header.bbox_min_mm,
        bbox_max_mm: header.bbox_max_mm,
        layer_height_mm: header.layer_height_mm,
        xy_step_mm: header.xy_step_mm,
        stitch_tol_mm: header.stitch_tol_mm,
    };

    Ok(SliDocument {
        header,
        stack: SliceStack { config, layers },
        hatches,
    })
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn read_bytes<const N: usize>(&mut self) -> Result<[u8; N], SlicerError> {
        let end = self
            .pos
            .checked_add(N)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| sli_error(self.pos, "unexpected EOF while reading SLI"))?;
        let mut out = [0u8; N];
        out.copy_from_slice(&self.bytes[self.pos..end]);
        self.pos = end;
        Ok(out)
    }

    fn read_u8(&mut self) -> Result<u8, SlicerError> {
        Ok(self.read_bytes::<1>()?[0])
    }

    fn read_u16(&mut self) -> Result<u16, SlicerError> {
        Ok(u16::from_le_bytes(self.read_bytes::<2>()?))
    }

    fn read_u32(&mut self) -> Result<u32, SlicerError> {
        Ok(u32::from_le_bytes(self.read_bytes::<4>()?))
    }

    fn read_u64(&mut self) -> Result<u64, SlicerError> {
        Ok(u64::from_le_bytes(self.read_bytes::<8>()?))
    }

    fn read_f64(&mut self) -> Result<f64, SlicerError> {
        Ok(f64::from_le_bytes(self.read_bytes::<8>()?))
    }
}

fn validate_config(config: &SliceConfig) -> Result<(), SlicerError> {
    let lengths = [
        config.layer_height_mm,
        config.xy_step_mm,
        config.stitch_tol_mm,
    ];
    if lengths
        .into_iter()
        .any(|length| !length.is_finite() || length <= 0.0)
    {
        return Err(SlicerError::InvalidConfig(
            "layer height, XY step and stitch tolerance must be finite and positive",
        ));
    }
    for (min, max) in config.bbox_min_mm.into_iter().zip(config.bbox_max_mm) {
        if !min.is_finite() || !max.is_finite() || !(max - min).is_finite() || max <= min {
            return Err(SlicerError::InvalidConfig(
                "bounding-box extents must be finite and positive",
            ));
        }
    }
    Ok(())
}

fn signed_area(points: &[[f64; 2]]) -> f64 {
    let [x, y] = points[0];
    let twice_area: f64 = points
        .windows(2)
        .map(|pair| (pair[0][0] - x) * (pair[1][1] - y) - (pair[1][0] - x) * (pair[0][1] - y))
        .sum();
    twice_area * 0.5
}

fn perimeter(contour: &ContourLoop) -> f64 {
    let points = &contour.points_mm;
    let length: f64 = points
        .windows(2)
        .map(|pair| (pair[0][0] - pair[1][0]).hypot(pair[0][1] - pair[1][1]))
        .sum();
    if contour.closed && points.len() >= 2 {
        let first = points[0];
        let last = points[points.len() - 1];
        length + (first[0] - last[0]).hypot(first[1] - last[1])
    } else {
        length
    }
}

fn sli_error(offset: usize, reason: impl Into<String>) -> SlicerError {
    SlicerError::InvalidSli {
        offset,
        reason: reason.into(),
    }
}

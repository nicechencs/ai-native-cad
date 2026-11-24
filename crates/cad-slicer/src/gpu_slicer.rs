//! Bounded TPMS layer sampling and incremental CLI geometry export.

use std::f64::consts::TAU;

use crate::{SliceConfig, SliceField, SliceStack, export_cli_ascii, slice_field};
pub use crate::{SliceLayer as LayerSlice, SlicerError as SliceError};

const MAX_GRID_BYTES: usize = 64 * 1024 * 1024;
const MAX_GRID_NODES: usize = (MAX_GRID_BYTES - 1) / size_of::<f64>();
const MAX_LAYERS: usize = 100_000;
const GEOMETRY_START: &str = "$$GEOMETRYSTART\n";
const GEOMETRY_END: &str = "$$GEOMETRYEND\n";

/// WebGPU compute shader for one XY layer, with `main` as the entry point.
///
/// Group 0 binding 0 is an 80-byte uniform. Its fields are laid out as declared,
/// with 12 bytes of trailing padding. The grid spans both XY endpoints and has
/// at least two nodes per axis. Domain 0 is a box. Domain 1 is a Z-aligned capped
/// cylinder inscribed in the box, with radius half the smaller XY extent.
/// Variant values correspond to [`TpmsVariant`].
///
/// Group 0 binding 1 is a writable row-major array of `grid_size.x * grid_size.y`
/// f32 samples. Dispatch ceil(width / 8) by ceil(height / 8) by 1 workgroups.
pub const WGSL_TPMS_LAYER_SLICE_SHADER: &str = r#"
struct SliceParams {
    grid_min_mm: vec2f,
    grid_max_mm: vec2f,
    bbox_min_mm: vec3f,
    z_mm: f32,
    bbox_max_mm: vec3f,
    period_mm: f32,
    grid_size: vec2u,
    wall_thickness_mm: f32,
    tpms_variant: u32,
    domain: u32,
}

@group(0) @binding(0) var<uniform> params: SliceParams;
@group(0) @binding(1) var<storage, read_write> samples: array<f32>;

const TAU: f32 = 6.283185307179586;

// Each function returns (phase gradient, level-set value).
fn tpms_gyroid(s: vec3f, c: vec3f) -> vec4f {
    let gradient = c * c.yzx - s.zxy * s;
    return vec4f(gradient, dot(s, c.yzx));
}

fn tpms_schwarz_p(s: vec3f, c: vec3f) -> vec4f {
    return vec4f(-s, c.x + c.y + c.z);
}

fn tpms_diamond(s: vec3f, c: vec3f) -> vec4f {
    let a = s.yzx * s.zxy + c.yzx * c.zxy;
    let b = s.yzx * c.zxy + c.yzx * s.zxy;
    return vec4f(c * a - s * b, s.x * a.x + c.x * b.x);
}

fn tpms_sheet_sdf(p: vec3f) -> f32 {
    let phase = (p % vec3f(params.period_mm)) / params.period_mm * TAU;
    let s = sin(phase);
    let c = cos(phase);
    var surface: vec4f;
    switch params.tpms_variant {
        case 1u: { surface = tpms_schwarz_p(s, c); }
        case 2u: { surface = tpms_diamond(s, c); }
        default: { surface = tpms_gyroid(s, c); }
    }
    let distance_mm = (abs(surface.w) / max(length(surface.xyz), 1e-6) / TAU)
        * params.period_mm;
    return distance_mm - 0.5 * params.wall_thickness_mm;
}

fn box_sdf(p: vec3f) -> f32 {
    let q = max(params.bbox_min_mm - p, p - params.bbox_max_mm);
    return length(max(q, vec3f(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}

fn cylinder_sdf(p: vec3f) -> f32 {
    let half_extent = (params.bbox_max_mm.xy - params.bbox_min_mm.xy) * 0.5;
    let center = params.bbox_min_mm.xy + half_extent;
    let radial = length(p.xy - center) - min(half_extent.x, half_extent.y);
    let axial = max(params.bbox_min_mm.z - p.z, p.z - params.bbox_max_mm.z);
    let q = vec2f(radial, axial);
    return length(max(q, vec2f(0.0))) + min(max(radial, axial), 0.0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3u) {
    if any(id.xy >= params.grid_size) {
        return;
    }
    let t = vec2f(id.xy) / vec2f(params.grid_size - vec2u(1));
    let p = vec3f(mix(params.grid_min_mm, params.grid_max_mm, t), params.z_mm);
    var domain_sdf = box_sdf(p);
    if params.domain == 1u {
        domain_sdf = cylinder_sdf(p);
    }
    samples[id.y * params.grid_size.x + id.x] = max(domain_sdf, tpms_sheet_sdf(p));
}
"#;

/// TPMS sheet families, using the same discriminants as the WGSL uniform.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TpmsVariant {
    Gyroid = 0,
    SchwarzP = 1,
    Diamond = 2,
}

/// Periodic sheet infill clipped to an axis-aligned box in millimeters.
///
/// The wall thickness is the full thickness. Distances to the TPMS sheet use
/// a first-order approximation based on the analytic gradient of the level set.
/// Evaluation returns NaN for invalid bounds, a non-finite sample position, or
/// a period or wall thickness that is non-finite or non-positive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TpmsLayerFieldSpec {
    pub kind: TpmsVariant,
    pub bbox_min_mm: [f64; 3],
    pub bbox_max_mm: [f64; 3],
    pub period_mm: f64,
    pub wall_thickness_mm: f64,
}

impl SliceField for TpmsLayerFieldSpec {
    fn eval(&self, x_mm: f64, y_mm: f64, z_mm: f64) -> f64 {
        let point = [x_mm, y_mm, z_mm];
        if !positive_finite(self.period_mm)
            || !positive_finite(self.wall_thickness_mm)
            || !point.into_iter().all(f64::is_finite)
            || !valid_bounds(self.bbox_min_mm, self.bbox_max_mm)
        {
            return f64::NAN;
        }

        let [(sx, cx), (sy, cy), (sz, cz)] =
            point.map(|p| (((p % self.period_mm) / self.period_mm) * TAU).sin_cos());
        let (value, [dx, dy, dz]) = match self.kind {
            TpmsVariant::Gyroid => (
                sx * cy + sy * cz + sz * cx,
                [cx * cy - sz * sx, cy * cz - sx * sy, cz * cx - sy * sz],
            ),
            TpmsVariant::SchwarzP => (cx + cy + cz, [-sx, -sy, -sz]),
            TpmsVariant::Diamond => {
                let a = [sy * sz + cy * cz, sz * sx + cz * cx, sx * sy + cx * cy];
                let b = [sy * cz + cy * sz, sz * cx + cz * sx, sx * cy + cx * sy];
                (
                    sx * a[0] + cx * b[0],
                    [
                        cx * a[0] - sx * b[0],
                        cy * a[1] - sy * b[1],
                        cz * a[2] - sz * b[2],
                    ],
                )
            }
        };
        let gradient = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);
        let sheet = (value.abs() / gradient / TAU) * self.period_mm - 0.5 * self.wall_thickness_mm;

        let [x, y, z] = std::array::from_fn(|axis| {
            (self.bbox_min_mm[axis] - point[axis]).max(point[axis] - self.bbox_max_mm[axis])
        });
        let outside = x.max(0.0).hypot(y.max(0.0)).hypot(z.max(0.0));
        let domain = outside + x.max(y).max(z).min(0.0);
        domain.max(sheet)
    }
}

/// Measurements from the finished CLI stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamingSliceStats {
    pub layer_count: usize,
    pub total_contours: usize,
    /// Number of polyline edges, including each closing edge.
    pub total_segments: usize,
    /// Peak bytes of the f64 scalar grid for one layer.
    pub peak_layer_grid_bytes: usize,
    pub cli_byte_len: usize,
}

/// Samples and writes one slab at a time, using 1 mm CLI units.
///
/// The callback runs after the current layer has been encoded. The scalar grid
/// must fit below 64 MiB, and only the geometry of the current slab is kept in
/// memory during extraction. The returned String holds all encoded records.
pub fn stream_slice_to_cli<F: SliceField>(
    field: &F,
    config: &SliceConfig,
    mut on_layer: impl FnMut(&LayerSlice),
) -> Result<(String, StreamingSliceStats), SliceError> {
    if ![
        config.layer_height_mm,
        config.xy_step_mm,
        config.stitch_tol_mm,
    ]
    .into_iter()
    .all(positive_finite)
    {
        return Err(SliceError::InvalidConfig(
            "layer height, XY step and stitch tolerance must be finite and positive",
        ));
    }
    if !valid_bounds(config.bbox_min_mm, config.bbox_max_mm) {
        return Err(SliceError::InvalidConfig(
            "bounding-box extents must be finite and positive",
        ));
    }
    let [min_x, min_y, min_z] = config.bbox_min_mm;
    let [max_x, max_y, max_z] = config.bbox_max_mm;
    let columns = step_count(max_x - min_x, config.xy_step_mm, MAX_GRID_NODES)? + 1;
    let rows = step_count(max_y - min_y, config.xy_step_mm, MAX_GRID_NODES)? + 1;
    if columns > MAX_GRID_NODES / rows {
        return Err(SliceError::InvalidConfig(
            "XY scalar grid must be smaller than 64 MiB",
        ));
    }
    let planned_layers = step_count(max_z - min_z, config.layer_height_mm, MAX_LAYERS)?;
    let layer_tops = (1..=planned_layers).map(|number| {
        if number == planned_layers {
            max_z
        } else {
            (number as f64)
                .mul_add(config.layer_height_mm, min_z)
                .min(max_z)
        }
    });
    // Rounding can reach max_z before the nominal last layer.
    let layer_count = layer_tops.clone().take_while(|&z| z < max_z).count() + 1;
    let mut stack = SliceStack {
        config: *config,
        layers: Vec::new(),
    };
    let header = export_cli_ascii(&stack, 1.0)?;
    let mut output = header
        .strip_suffix(GEOMETRY_END)
        .expect("CLI exporter writes a geometry footer")
        .replacen("$$LAYERS/0\n", &format!("$$LAYERS/{layer_count}\n"), 1);
    let mut stats = StreamingSliceStats {
        peak_layer_grid_bytes: columns * rows * size_of::<f64>(),
        ..Default::default()
    };
    let mut bottom = min_z;
    let mut previous_cli_z = None;
    for (layer_index, top) in layer_tops.take(layer_count).enumerate() {
        if top <= bottom {
            return Err(SliceError::InvalidConfig(
                "layer height is below coordinate precision",
            ));
        }
        let mut slab = *config;
        slab.bbox_min_mm[2] = bottom;
        slab.bbox_max_mm[2] = top;
        slab.layer_height_mm = top - bottom;

        // The existing extractor releases its scalar grid before returning the layer.
        stack.layers = slice_field(field, &slab)?.layers;
        let layer = &mut stack.layers[0];
        layer.layer_index = layer_index;
        let record = export_cli_ascii(&stack, 1.0)?;
        let geometry = record
            .split_once(GEOMETRY_START)
            .and_then(|(_, tail)| tail.strip_suffix(GEOMETRY_END))
            .expect("CLI exporter brackets the layer records");
        let cli_z = geometry
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("$$LAYER/"))
            .and_then(|z| z.parse::<f64>().ok())
            .expect("CLI exporter writes the layer height first");
        if previous_cli_z.is_some_and(|previous| cli_z <= previous) {
            return Err(SliceError::InvalidContour {
                layer_index,
                reason: "CLI layer heights must be strictly increasing",
            });
        }
        previous_cli_z = Some(cli_z);
        output.push_str(geometry);

        let layer = &stack.layers[0];
        stats.layer_count += 1;
        stats.total_contours += layer.loops.len();
        stats.total_segments += layer
            .loops
            .iter()
            .map(|contour| contour.points_mm.len())
            .sum::<usize>();
        on_layer(layer);
        stack.layers.clear();
        bottom = top;
    }
    output.push_str(GEOMETRY_END);
    stats.cli_byte_len = output.len();
    Ok((output, stats))
}

fn positive_finite(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn valid_bounds(min: [f64; 3], max: [f64; 3]) -> bool {
    min.into_iter()
        .zip(max)
        .all(|(min, max)| positive_finite(max - min))
}

// Mirrors the marching-squares step count, including the whole-step roundoff allowance.
fn step_count(extent: f64, step: f64, limit: usize) -> Result<usize, SliceError> {
    let ratio = extent / step;
    let nearest = ratio.round();
    let count = if nearest >= 1.0 && (ratio - nearest).abs() <= 4.0 * f64::EPSILON * nearest {
        nearest
    } else {
        ratio.ceil().max(1.0)
    };
    if !count.is_finite() || count > limit as f64 {
        return Err(SliceError::InvalidConfig(
            "grid or layer count is too large",
        ));
    }
    Ok(count as usize)
}

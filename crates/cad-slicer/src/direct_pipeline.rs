//! End-to-end direct layer-by-layer TPMS contour slicing pipeline (zero STL intermediate).
//!
//! Connects any implicit [`SliceField`] (including `cad-implicit`'s
//! `HybridTpmsField` via [`crate::FnSliceField`] or [`HybridTpmsSliceField`])
//! directly to [`crate::slice_field`] -> [`crate::verify_stack_contours`] ->
//! [`crate::export_cli_ascii`] & [`crate::export_sli_binary`].

use std::f64::consts::TAU;

use crate::{
    SliceConfig, SliceField, SliceStack, SlicerError, StackVerificationReport, TpmsVariant,
    export_cli_ascii, export_sli_binary, slice_field, verify_stack_contours,
};

/// Protected exact mechanical interface zone for [`HybridTpmsSliceField`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProtectedZone {
    /// Axis-aligned solid box (e.g. mounting flange or pad).
    Box {
        center_mm: [f64; 3],
        half_extents_mm: [f64; 3],
        margin_mm: f64,
    },
    /// Z-aligned solid cylinder (e.g. bolt boss or bearing seat).
    Cylinder {
        center_mm: [f64; 3],
        radius_mm: f64,
        half_height_mm: f64,
        margin_mm: f64,
    },
    /// Perimeter sealing rim along the XY walls of an outer box.
    RectangularRim {
        bbox_min_mm: [f64; 3],
        bbox_max_mm: [f64; 3],
        rim_width_mm: f64,
    },
}

impl ProtectedZone {
    /// Signed distance to this protected zone (negative inside).
    #[must_use]
    pub fn eval(&self, p: [f64; 3]) -> f64 {
        match *self {
            Self::Box {
                center_mm,
                half_extents_mm,
                margin_mm,
            } => box_sdf(p, center_mm, half_extents_mm) - margin_mm.max(0.0),
            Self::Cylinder {
                center_mm,
                radius_mm,
                half_height_mm,
                margin_mm,
            } => cylinder_sdf(p, center_mm, radius_mm, half_height_mm) - margin_mm.max(0.0),
            Self::RectangularRim {
                bbox_min_mm,
                bbox_max_mm,
                rim_width_mm,
            } => {
                let center = [
                    0.5 * (bbox_min_mm[0] + bbox_max_mm[0]),
                    0.5 * (bbox_min_mm[1] + bbox_max_mm[1]),
                    0.5 * (bbox_min_mm[2] + bbox_max_mm[2]),
                ];
                let half = [
                    0.5 * (bbox_max_mm[0] - bbox_min_mm[0]),
                    0.5 * (bbox_max_mm[1] - bbox_min_mm[1]),
                    0.5 * (bbox_max_mm[2] - bbox_min_mm[2]),
                ];
                let outer = box_sdf(p, center, half);
                let inner_half = [
                    (half[0] - rim_width_mm.max(0.0)).max(0.0),
                    (half[1] - rim_width_mm.max(0.0)).max(0.0),
                    half[2] + rim_width_mm.max(0.0) + 1.0,
                ];
                let inner = box_sdf(p, center, inner_half);
                outer.max(-inner)
            }
        }
    }
}

/// Self-contained hybrid TPMS field specification implementing [`SliceField`],
/// combining an exact outer box envelope, internal TPMS lattice, smooth skin,
/// and protected exact interface zones.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridTpmsSliceField {
    pub kind: TpmsVariant,
    pub bbox_min_mm: [f64; 3],
    pub bbox_max_mm: [f64; 3],
    pub period_mm: f64,
    pub wall_thickness_mm: f64,
    pub skin_thickness_mm: f64,
    pub transition_width_mm: f64,
    pub preserve_exact_interfaces: bool,
    pub protected_zones: Vec<ProtectedZone>,
}

impl SliceField for HybridTpmsSliceField {
    fn eval(&self, x_mm: f64, y_mm: f64, z_mm: f64) -> f64 {
        let p = [x_mm, y_mm, z_mm];
        if !self.period_mm.is_finite()
            || self.period_mm <= 0.0
            || !self.wall_thickness_mm.is_finite()
            || self.wall_thickness_mm <= 0.0
            || !p.into_iter().all(f64::is_finite)
        {
            return f64::NAN;
        }
        let center = [
            0.5 * (self.bbox_min_mm[0] + self.bbox_max_mm[0]),
            0.5 * (self.bbox_min_mm[1] + self.bbox_max_mm[1]),
            0.5 * (self.bbox_min_mm[2] + self.bbox_max_mm[2]),
        ];
        let half = [
            0.5 * (self.bbox_max_mm[0] - self.bbox_min_mm[0]),
            0.5 * (self.bbox_max_mm[1] - self.bbox_min_mm[1]),
            0.5 * (self.bbox_max_mm[2] - self.bbox_min_mm[2]),
        ];
        if half.into_iter().any(|h| !h.is_finite() || h <= 0.0) {
            return f64::NAN;
        }

        let domain = box_sdf(p, center, half);
        let sheet = eval_tpms_sheet(self.kind, p, self.period_mm, self.wall_thickness_mm);

        let mut solid_feature = f64::INFINITY;
        if self.skin_thickness_mm > 0.0 {
            solid_feature = solid_feature.min(-domain - self.skin_thickness_mm);
        }
        if self.preserve_exact_interfaces {
            for zone in &self.protected_zones {
                solid_feature = solid_feature.min(zone.eval(p));
            }
        }

        let material = if solid_feature.is_finite() {
            if self.transition_width_mm > 0.0 {
                smooth_min(sheet, solid_feature, self.transition_width_mm)
            } else {
                sheet.min(solid_feature)
            }
        } else {
            sheet
        };

        material.max(domain)
    }
}

/// Verified direct-sliced output bundle containing the contour stack,
/// topology verification report, ASCII `CLI`, and binary `SLI` bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectSliceBundle {
    pub stack: SliceStack,
    pub verification: StackVerificationReport,
    pub cli_ascii: String,
    pub sli_binary: Vec<u8>,
    /// Peak scalar grid memory in bytes for a single XY layer (`O(W * H)`).
    pub peak_layer_grid_bytes: usize,
}

impl DirectSliceBundle {
    /// Returns `true` when 100% of layers and contours pass closure and
    /// zero-self-intersection validation.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.verification.is_valid()
    }
}

/// Directly slices an implicit field over `bbox` (`(min_mm, max_mm)`) using
/// `config` without any STL tessellation, verifies 100% contour closure and
/// zero self-intersections, and emits both `CLI` ASCII and `SLI` binary stacks.
pub fn slice_hybrid_tpms_to_stack(
    field: &(impl SliceField + ?Sized),
    bbox: ([f64; 3], [f64; 3]),
    mut config: SliceConfig,
) -> Result<DirectSliceBundle, SlicerError> {
    config.bbox_min_mm = bbox.0;
    config.bbox_max_mm = bbox.1;

    let stack = slice_field(field, &config)?;
    let verification = verify_stack_contours(&stack, config.stitch_tol_mm);
    if !verification.is_valid() {
        let first_layer = verification
            .layer_reports
            .iter()
            .find(|r| !r.is_valid())
            .map_or(0, |r| r.layer_index);
        return Err(SlicerError::InvalidContour {
            layer_index: first_layer,
            reason: "sliced layer failed contour closure or self-intersection verification",
        });
    }

    let cli_ascii = export_cli_ascii(&stack, 1.0)?;
    let sli_binary = export_sli_binary(&stack, 0.001)?;

    let nx = ((config.bbox_max_mm[0] - config.bbox_min_mm[0]) / config.xy_step_mm)
        .ceil()
        .max(1.0) as usize
        + 1;
    let ny = ((config.bbox_max_mm[1] - config.bbox_min_mm[1]) / config.xy_step_mm)
        .ceil()
        .max(1.0) as usize
        + 1;
    let peak_layer_grid_bytes = nx * ny * size_of::<f64>();

    Ok(DirectSliceBundle {
        stack,
        verification,
        cli_ascii,
        sli_binary,
        peak_layer_grid_bytes,
    })
}

fn eval_tpms_sheet(kind: TpmsVariant, point: [f64; 3], period_mm: f64, wall_thickness_mm: f64) -> f64 {
    let [(sx, cx), (sy, cy), (sz, cz)] =
        point.map(|p| (((p % period_mm) / period_mm) * TAU).sin_cos());
    let (value, [dx, dy, dz]) = match kind {
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
    (value.abs() / gradient / TAU) * period_mm - 0.5 * wall_thickness_mm
}

fn box_sdf(p: [f64; 3], center_mm: [f64; 3], half_extents_mm: [f64; 3]) -> f64 {
    let q = [
        (p[0] - center_mm[0]).abs() - half_extents_mm[0].max(0.0),
        (p[1] - center_mm[1]).abs() - half_extents_mm[1].max(0.0),
        (p[2] - center_mm[2]).abs() - half_extents_mm[2].max(0.0),
    ];
    let outside = q[0].max(0.0).hypot(q[1].max(0.0)).hypot(q[2].max(0.0));
    outside + q[0].max(q[1]).max(q[2]).min(0.0)
}

fn cylinder_sdf(p: [f64; 3], center_mm: [f64; 3], radius_mm: f64, half_height_mm: f64) -> f64 {
    let x = p[0] - center_mm[0];
    let y = p[1] - center_mm[1];
    let z = (p[2] - center_mm[2]).abs();
    let q = [x.hypot(y) - radius_mm.max(0.0), z - half_height_mm.max(0.0)];
    let outside = q[0].max(0.0).hypot(q[1].max(0.0));
    outside + q[0].max(q[1]).min(0.0)
}

fn smooth_min(a: f64, b: f64, k: f64) -> f64 {
    let min = a.min(b);
    let diff = (a - b).abs();
    if k > 0.0 && diff < k {
        let h = 1.0 - diff / k;
        min - (0.25 * k) * h * h
    } else {
        min
    }
}

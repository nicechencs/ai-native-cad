//! Protected exact-interface hybrid TPMS fields and streaming layer evaluation.
//!
//! [`HybridTpmsField`] composes an outer exact envelope SDF, an internal TPMS
//! sheet lattice (`Gyroid`, `Diamond`, `SchwarzP`, `Lidinoid`), an optional
//! solid outer skin with smooth transition blending, and a set of
//! [`ExactInterfaceZone`]s (such as mounting flanges, bolt bosses, sealing
//! lands, and bearing seats).
//!
//! Whenever `preserve_exact_interfaces` is enabled, every point inside a
//! declared [`ExactInterfaceZone`] (within the outer envelope) is guaranteed
//! to evaluate to solid (`< 0.0`) and is never perforated by TPMS voids.
//!
//! [`LayerFieldEvaluator`] streams one Z-layer at a time into a single reused
//! `O(W * H)` scalar buffer so downstream slicers can emit closed contours
//! directly without ever building a 3D STL triangle mesh.

use crate::fea_field::ImplicitError;
use crate::field::{ConstantField, ScalarField};
use crate::primitives::{BoxSdf, CylinderSdf, SphereSdf};
use crate::tpms::{TpmsFamily, TpmsField, TpmsSheetField};

/// Approximate surface area per unit cell ($A / L^2$) for each TPMS family,
/// used to convert a target sheet relative density into a physical wall thickness.
fn tpms_area_factor(family: TpmsFamily) -> f64 {
    match family {
        TpmsFamily::Gyroid => 3.091,
        TpmsFamily::SchwarzP => 2.345,
        TpmsFamily::Diamond => 3.838,
        TpmsFamily::Lidinoid => 3.40,
    }
}

/// Estimates the full TPMS sheet wall thickness in millimetres for a target
/// relative density in `(0.0, 1.0)` given per-axis cell periods.
///
/// Panics if any period is non-finite or non-positive, or if `relative_density`
/// is not strictly between `0.0` and `1.0`.
#[must_use]
pub fn wall_thickness_for_relative_density(
    family: TpmsFamily,
    period_mm: [f64; 3],
    relative_density: f64,
) -> f64 {
    assert!(
        period_mm.into_iter().all(|p| p.is_finite() && p > 0.0),
        "TPMS period must be finite and positive"
    );
    assert!(
        relative_density.is_finite() && relative_density > 0.0 && relative_density < 1.0,
        "relative density must lie strictly in (0.0, 1.0)"
    );
    let mean_period = (period_mm[0] * period_mm[1] * period_mm[2]).cbrt();
    (relative_density * mean_period) / tpms_area_factor(family)
}

/// An exact mechanical interface zone (bolt boss, mounting flange, sealing rim,
/// bearing seat) that must remain solid and free of TPMS void perforations.
pub struct ExactInterfaceZone {
    name: String,
    region: Box<dyn ScalarField>,
    margin_mm: f64,
}

impl ExactInterfaceZone {
    /// Creates a protected interface zone from an arbitrary SDF (negative inside)
    /// expanded outward by `margin_mm`.
    #[must_use]
    pub fn custom(
        name: impl Into<String>,
        region: Box<dyn ScalarField>,
        margin_mm: f64,
    ) -> Self {
        Self {
            name: name.into(),
            region,
            margin_mm: margin_mm.max(0.0),
        }
    }

    /// Creates an axis-aligned box interface zone (e.g. mounting flange or pad).
    #[must_use]
    pub fn box_zone(
        name: impl Into<String>,
        center_mm: [f64; 3],
        half_extents_mm: [f64; 3],
        margin_mm: f64,
    ) -> Self {
        Self::custom(name, Box::new(BoxSdf::new(center_mm, half_extents_mm)), margin_mm)
    }

    /// Creates a Z-aligned cylindrical interface zone (e.g. bolt boss or bearing seat).
    #[must_use]
    pub fn cylinder_zone(
        name: impl Into<String>,
        center_mm: [f64; 3],
        radius_mm: f64,
        half_height_mm: f64,
        margin_mm: f64,
    ) -> Self {
        Self::custom(
            name,
            Box::new(CylinderSdf::new(center_mm, radius_mm, half_height_mm)),
            margin_mm,
        )
    }

    /// Creates a spherical interface zone.
    #[must_use]
    pub fn sphere_zone(
        name: impl Into<String>,
        center_mm: [f64; 3],
        radius_mm: f64,
        margin_mm: f64,
    ) -> Self {
        Self::custom(name, Box::new(SphereSdf::new(center_mm, radius_mm)), margin_mm)
    }

    /// Creates a rectangular perimeter sealing rim around a box envelope.
    ///
    /// Points inside the outer box that lie within `rim_width_mm` of any XY side
    /// wall (within the specified Z range) are protected as solid sealing land.
    #[must_use]
    pub fn rectangular_sealing_rim(
        name: impl Into<String>,
        bbox_min_mm: [f64; 3],
        bbox_max_mm: [f64; 3],
        rim_width_mm: f64,
    ) -> Self {
        let rim = RectangularRimSdf {
            bbox_min_mm,
            bbox_max_mm,
            rim_width_mm: rim_width_mm.max(0.0),
        };
        Self::custom(name, Box::new(rim), 0.0)
    }

    /// Human-readable identifier for this interface zone.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Extra solid margin around the zone in millimetres.
    #[must_use]
    pub fn margin_mm(&self) -> f64 {
        self.margin_mm
    }

    /// Evaluates the expanded signed distance to this interface zone
    /// (negative inside the protected solid region).
    #[must_use]
    pub fn eval(&self, p: [f64; 3]) -> f64 {
        self.region.eval(p) - self.margin_mm
    }

    /// Returns `true` when `p` lies strictly inside this protected zone.
    #[must_use]
    pub fn contains(&self, p: [f64; 3]) -> bool {
        self.eval(p) <= 0.0
    }
}

struct RectangularRimSdf {
    bbox_min_mm: [f64; 3],
    bbox_max_mm: [f64; 3],
    rim_width_mm: f64,
}

impl ScalarField for RectangularRimSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
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
        let outer = BoxSdf::new(center, half).eval(p);
        let inner_half = [
            (half[0] - self.rim_width_mm).max(0.0),
            (half[1] - self.rim_width_mm).max(0.0),
            half[2] + self.rim_width_mm + 1.0,
        ];
        let inner = BoxSdf::new(center, inner_half).eval(p);
        outer.max(-inner)
    }
}

/// Hybrid implicit TPMS field with an exact outer envelope, smooth outer skin,
/// and protected exact mechanical interface zones.
pub struct HybridTpmsField {
    domain: Box<dyn ScalarField>,
    family: TpmsFamily,
    period_mm: [f64; 3],
    sheet: TpmsSheetField,
    skin_thickness_mm: f64,
    transition_width_mm: f64,
    preserve_exact_interfaces: bool,
    interface_zones: Vec<ExactInterfaceZone>,
}

impl HybridTpmsField {
    /// Builds a hybrid TPMS field with a constant full wall thickness in millimetres.
    ///
    /// Panics if any period is non-finite/non-positive or `wall_thickness_mm` is
    /// non-finite/non-positive.
    #[must_use]
    pub fn new(
        domain: Box<dyn ScalarField>,
        family: TpmsFamily,
        period_mm: [f64; 3],
        wall_thickness_mm: f64,
    ) -> Self {
        assert!(
            wall_thickness_mm.is_finite() && wall_thickness_mm > 0.0,
            "wall thickness must be finite and positive"
        );
        let base = TpmsField::new(family, period_mm);
        let sheet = TpmsSheetField::new(base, Box::new(ConstantField(0.5 * wall_thickness_mm)));
        Self {
            domain,
            family,
            period_mm,
            sheet,
            skin_thickness_mm: 0.0,
            transition_width_mm: 0.0,
            preserve_exact_interfaces: true,
            interface_zones: Vec::new(),
        }
    }

    /// Builds a hybrid TPMS field whose wall thickness is derived from a target
    /// sheet relative density in `(0.0, 1.0)`.
    #[must_use]
    pub fn with_relative_density(
        domain: Box<dyn ScalarField>,
        family: TpmsFamily,
        period_mm: [f64; 3],
        relative_density: f64,
    ) -> Self {
        let wall = wall_thickness_for_relative_density(family, period_mm, relative_density);
        Self::new(domain, family, period_mm, wall)
    }

    /// Builds a hybrid TPMS field with a spatially varying half-thickness scalar field.
    #[must_use]
    pub fn with_thickness_field(
        domain: Box<dyn ScalarField>,
        family: TpmsFamily,
        period_mm: [f64; 3],
        half_thickness_mm: Box<dyn ScalarField>,
    ) -> Self {
        let base = TpmsField::new(family, period_mm);
        let sheet = TpmsSheetField::new(base, half_thickness_mm);
        Self {
            domain,
            family,
            period_mm,
            sheet,
            skin_thickness_mm: 0.0,
            transition_width_mm: 0.0,
            preserve_exact_interfaces: true,
            interface_zones: Vec::new(),
        }
    }

    /// Sets the solid outer skin thickness and smooth blend transition width in millimetres.
    #[must_use]
    pub fn with_skin(mut self, skin_thickness_mm: f64, transition_width_mm: f64) -> Self {
        assert!(
            skin_thickness_mm.is_finite() && skin_thickness_mm >= 0.0,
            "skin thickness must be finite and non-negative"
        );
        assert!(
            transition_width_mm.is_finite() && transition_width_mm >= 0.0,
            "transition width must be finite and non-negative"
        );
        self.skin_thickness_mm = skin_thickness_mm;
        self.transition_width_mm = transition_width_mm;
        self
    }

    /// Enables or disables `preserve_exact_interfaces` protection.
    #[must_use]
    pub fn with_preserve_exact_interfaces(mut self, preserve: bool) -> Self {
        self.preserve_exact_interfaces = preserve;
        self
    }

    /// Appends a single protected [`ExactInterfaceZone`].
    #[must_use]
    pub fn with_interface_zone(mut self, zone: ExactInterfaceZone) -> Self {
        self.interface_zones.push(zone);
        self
    }

    /// Appends multiple protected [`ExactInterfaceZone`]s.
    #[must_use]
    pub fn with_interface_zones(
        mut self,
        zones: impl IntoIterator<Item = ExactInterfaceZone>,
    ) -> Self {
        self.interface_zones.extend(zones);
        self
    }

    /// Returns the TPMS family used in the core.
    #[must_use]
    pub fn family(&self) -> TpmsFamily {
        self.family
    }

    /// Returns the per-axis TPMS cell period in millimetres.
    #[must_use]
    pub fn period_mm(&self) -> [f64; 3] {
        self.period_mm
    }

    /// Returns the configured outer skin thickness in millimetres.
    #[must_use]
    pub fn skin_thickness_mm(&self) -> f64 {
        self.skin_thickness_mm
    }

    /// Returns the smooth blend transition width in millimetres.
    #[must_use]
    pub fn transition_width_mm(&self) -> f64 {
        self.transition_width_mm
    }

    /// Returns whether protected exact interfaces are enforced.
    #[must_use]
    pub fn preserve_exact_interfaces(&self) -> bool {
        self.preserve_exact_interfaces
    }

    /// Returns the registered exact interface zones.
    #[must_use]
    pub fn interface_zones(&self) -> &[ExactInterfaceZone] {
        &self.interface_zones
    }

    /// Evaluates the outer envelope SDF alone.
    #[must_use]
    pub fn eval_domain(&self, p: [f64; 3]) -> f64 {
        self.domain.eval(p)
    }

    /// Evaluates the raw internal TPMS sheet SDF alone.
    #[must_use]
    pub fn eval_tpms_core(&self, p: [f64; 3]) -> f64 {
        self.sheet.eval(p)
    }

    /// Returns `true` if `p` lies inside the outer envelope and inside at least
    /// one active [`ExactInterfaceZone`].
    #[must_use]
    pub fn is_in_protected_zone(&self, p: [f64; 3]) -> bool {
        if !self.preserve_exact_interfaces || self.domain.eval(p) > 0.0 {
            return false;
        }
        self.interface_zones.iter().any(|zone| zone.contains(p))
    }
}

impl ScalarField for HybridTpmsField {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let domain = self.domain.eval(p);
        let sheet = self.sheet.eval(p);

        let mut solid_feature = f64::INFINITY;

        if self.skin_thickness_mm > 0.0 {
            // Inside the domain (domain <= 0), depth from surface is -domain.
            // Skin is solid when depth <= skin_thickness_mm, i.e. -domain - skin_thickness_mm <= 0.
            let skin = -domain - self.skin_thickness_mm;
            solid_feature = solid_feature.min(skin);
        }

        if self.preserve_exact_interfaces {
            for zone in &self.interface_zones {
                solid_feature = solid_feature.min(zone.eval(p));
            }
        }

        let material = if solid_feature.is_finite() {
            if self.transition_width_mm > 0.0 {
                // smooth_min(a, b, k) <= min(a, b), so solid_feature < 0 ALWAYS stays < 0!
                smooth_min(sheet, solid_feature, self.transition_width_mm)
            } else {
                sheet.min(solid_feature)
            }
        } else {
            sheet
        };

        // Clip all internal material strictly to the exact outer envelope.
        material.max(domain)
    }
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

/// Grid geometry for a single XY slice layer evaluated by [`LayerFieldEvaluator`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerGridSpec {
    pub min_xy_mm: [f64; 2],
    pub max_xy_mm: [f64; 2],
    pub nx: usize,
    pub ny: usize,
}

/// Streaming single-layer evaluator that samples any [`ScalarField`] onto a
/// reused `O(nx * ny)` row-major buffer, avoiding 3D voxel or STL allocations.
pub struct LayerFieldEvaluator<'a> {
    field: &'a dyn ScalarField,
    spec: LayerGridSpec,
    xs: Vec<f64>,
    ys: Vec<f64>,
    buffer: Vec<f64>,
}

impl<'a> LayerFieldEvaluator<'a> {
    /// Creates a streaming layer evaluator and pre-allocates the single `O(nx * ny)` buffer.
    pub fn new(field: &'a dyn ScalarField, spec: LayerGridSpec) -> Result<Self, ImplicitError> {
        if spec.nx < 2 || spec.ny < 2 {
            return Err(ImplicitError::InvalidDims);
        }
        let cell_count = spec
            .nx
            .checked_mul(spec.ny)
            .ok_or(ImplicitError::InvalidDims)?;
        for axis in 0..2 {
            let min = spec.min_xy_mm[axis];
            let max = spec.max_xy_mm[axis];
            if !min.is_finite() || !max.is_finite() || !(max - min).is_finite() || max <= min {
                return Err(ImplicitError::NonFiniteBound);
            }
        }
        let xs = linspace(spec.min_xy_mm[0], spec.max_xy_mm[0], spec.nx);
        let ys = linspace(spec.min_xy_mm[1], spec.max_xy_mm[1], spec.ny);
        Ok(Self {
            field,
            spec,
            xs,
            ys,
            buffer: vec![0.0; cell_count],
        })
    }

    /// Evaluates the field on the XY grid at `z_mm`, overwriting the reused
    /// row-major buffer (`row * nx + col`) with zero heap allocation.
    pub fn evaluate_layer(&mut self, z_mm: f64) -> Result<&[f64], ImplicitError> {
        if !z_mm.is_finite() {
            return Err(ImplicitError::NonFiniteBound);
        }
        let mut idx = 0;
        for &y in &self.ys {
            for &x in &self.xs {
                let value = self.field.eval([x, y, z_mm]);
                if !value.is_finite() {
                    return Err(ImplicitError::NonFiniteValue { index: idx });
                }
                self.buffer[idx] = value;
                idx += 1;
            }
        }
        Ok(&self.buffer)
    }

    /// Streams over a sequence of Z heights, invoking `on_layer` for each layer
    /// while keeping memory strictly bounded to the single `O(nx * ny)` buffer.
    pub fn stream_layers(
        &mut self,
        z_levels_mm: &[f64],
        mut on_layer: impl FnMut(usize, f64, &[f64]),
    ) -> Result<usize, ImplicitError> {
        for (layer_idx, &z_mm) in z_levels_mm.iter().enumerate() {
            let samples = self.evaluate_layer(z_mm)?;
            on_layer(layer_idx, z_mm, samples);
        }
        Ok(z_levels_mm.len())
    }

    /// Evaluates the underlying field at a single arbitrary 3D point.
    #[must_use]
    pub fn eval_point(&self, x_mm: f64, y_mm: f64, z_mm: f64) -> f64 {
        self.field.eval([x_mm, y_mm, z_mm])
    }

    /// Returns the grid specification.
    #[must_use]
    pub fn spec(&self) -> LayerGridSpec {
        self.spec
    }

    /// Returns the X grid coordinates in millimetres.
    #[must_use]
    pub fn xs(&self) -> &[f64] {
        &self.xs
    }

    /// Returns the Y grid coordinates in millimetres.
    #[must_use]
    pub fn ys(&self) -> &[f64] {
        &self.ys
    }

    /// Returns the allocated byte size of the reused layer buffer (`nx * ny * 8`).
    #[must_use]
    pub fn buffer_bytes(&self) -> usize {
        self.buffer.capacity() * size_of::<f64>()
    }
}

fn linspace(min: f64, max: f64, count: usize) -> Vec<f64> {
    let denom = (count - 1) as f64;
    let mut coords = Vec::with_capacity(count);
    for i in 0..count - 1 {
        coords.push((i as f64 / denom).mul_add(max - min, min));
    }
    coords.push(max);
    coords
}

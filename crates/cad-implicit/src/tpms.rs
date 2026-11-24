//! Triply periodic minimal surfaces and thickness-modulated sheet lattices.

use std::f64::consts::TAU;

pub use cad_ir::TpmsFamily;

use crate::field::ScalarField;

/// The dimensionless level-set function of a TPMS family, with per-axis periods in millimeters.
#[derive(Debug, Clone, Copy)]
pub struct TpmsField {
    family: TpmsFamily,
    period_mm: [f64; 3],
    wave_numbers: [f64; 3],
}

impl TpmsField {
    /// Panics if any period or resulting wave number is non-finite or non-positive.
    pub fn new(family: TpmsFamily, period_mm: [f64; 3]) -> Self {
        assert!(
            period_mm
                .into_iter()
                .all(|period| period.is_finite() && period > 0.0)
        );
        let wave_numbers = period_mm.map(|period| TAU / period);
        assert!(wave_numbers.into_iter().all(f64::is_finite));
        Self {
            family,
            period_mm,
            wave_numbers,
        }
    }

    fn phase(&self, p: [f64; 3]) -> [f64; 3] {
        // Keep the angles small enough for the sheet's phase differences to remain distinct.
        std::array::from_fn(|axis| (p[axis] % self.period_mm[axis]) * self.wave_numbers[axis])
    }

    fn eval_phase(&self, [x, y, z]: [f64; 3]) -> f64 {
        let (sx, cx) = x.sin_cos();
        let (sy, cy) = y.sin_cos();
        let (sz, cz) = z.sin_cos();
        match self.family {
            TpmsFamily::Gyroid => sx * cy + sy * cz + sz * cx,
            TpmsFamily::SchwarzP => cx + cy + cz,
            TpmsFamily::Diamond => sx * sy * sz + sx * cy * cz + cx * sy * cz + cx * cy * sz,
            TpmsFamily::Lidinoid => {
                let (s2x, c2x) = (2.0 * x).sin_cos();
                let (s2y, c2y) = (2.0 * y).sin_cos();
                let (s2z, c2z) = (2.0 * z).sin_cos();
                0.5 * (s2x * cy * sz + s2y * cz * sx + s2z * cx * sy)
                    - 0.5 * (c2x * c2y + c2y * c2z + c2z * c2x)
                    + 0.15
            }
        }
    }
}

impl ScalarField for TpmsField {
    fn eval(&self, p: [f64; 3]) -> f64 {
        self.eval_phase(self.phase(p))
    }
}

/// A sheet surrounding the zero level set of a TPMS field.
///
/// Dividing the level-set magnitude by the spatial gradient magnitude gives a first-order
/// distance in millimeters near the surface. This approximation is best for sheets thin
/// relative to their periods. The thickness field controls the distance on each side.
pub struct TpmsSheetField {
    base: TpmsField,
    half_thickness_mm: Box<dyn ScalarField>,
}

impl TpmsSheetField {
    pub fn new(base: TpmsField, half_thickness_mm: Box<dyn ScalarField>) -> Self {
        Self {
            base,
            half_thickness_mm,
        }
    }
}

impl ScalarField for TpmsSheetField {
    fn eval(&self, p: [f64; 3]) -> f64 {
        const PHASE_STEP: f64 = 1e-4;

        let phase = self.base.phase(p);
        let partials: [f64; 3] = std::array::from_fn(|axis| {
            let mut before = phase;
            let mut after = phase;
            before[axis] -= PHASE_STEP;
            after[axis] += PHASE_STEP;
            let derivative =
                (self.base.eval_phase(after) - self.base.eval_phase(before)) / (2.0 * PHASE_STEP);
            derivative * self.base.wave_numbers[axis]
        });
        let [dx, dy, dz] = partials;
        // Regularize stationary points using the longest period's wave number.
        let min_wave_number = self
            .base
            .wave_numbers
            .into_iter()
            .fold(f64::INFINITY, f64::min);
        let magnitude = dx.hypot(dy).hypot(dz).max(min_wave_number * 1e-6);
        self.base.eval_phase(phase).abs() / magnitude - self.half_thickness_mm.eval(p)
    }
}

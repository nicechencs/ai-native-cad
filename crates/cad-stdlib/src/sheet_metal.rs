//! Sheet-metal bend allowance.
//!
//! Bend allowance is the neutral-axis arc length:
//! `BA = θ × (R + K × T)`, with `θ` in radians.
//! Bend deduction for a bend angle below 180° is
//! `BD = 2 × (R + T) × tan(θ / 2) − BA`.
//! `default_k_factor` is an air-bend starting value, not a measured shop factor.

use serde::{Deserialize, Serialize};

use crate::StdLibError;

/// Neutral-axis length and the flat-pattern deduction for one bend.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BendAllowanceResult {
    /// K-factor used for this result.
    pub k_factor: f64,
    /// Neutral-axis arc length, millimetres.
    pub bend_allowance_mm: f64,
    /// Amount removed from the sum of the outside flanges, millimetres.
    pub bend_deduction_mm: f64,
}

/// Bend allowance and deduction.
///
/// `angle_deg` is the angle the sheet turns through (90 for an L).
/// `k_factor` is the neutral-axis position as a fraction of thickness,
/// measured from the inside face. It must be strictly between 0 and 1.
pub fn compute_bend_allowance(
    thickness_mm: f64,
    inner_radius_mm: f64,
    angle_deg: f64,
    k_factor: f64,
) -> Result<BendAllowanceResult, StdLibError> {
    if !thickness_mm.is_finite()
        || !inner_radius_mm.is_finite()
        || !angle_deg.is_finite()
        || !k_factor.is_finite()
    {
        return Err(StdLibError::InvalidBend("inputs must be finite"));
    }
    if thickness_mm <= 0.0 {
        return Err(StdLibError::InvalidBend("thickness must be positive"));
    }
    if inner_radius_mm < 0.0 {
        return Err(StdLibError::InvalidBend(
            "inner radius must be zero or positive",
        ));
    }
    if angle_deg <= 0.0 || angle_deg >= 180.0 {
        return Err(StdLibError::InvalidBend(
            "bend angle must be within (0, 180) degrees",
        ));
    }
    if k_factor <= 0.0 || k_factor >= 1.0 {
        return Err(StdLibError::InvalidBend("k-factor must be within (0, 1)"));
    }

    let angle_rad = angle_deg.to_radians();
    let bend_allowance_mm = angle_rad * (inner_radius_mm + k_factor * thickness_mm);
    let outside_setback_mm =
        (inner_radius_mm + thickness_mm) * (angle_deg * 0.5).to_radians().tan();
    let bend_deduction_mm = 2.0 * outside_setback_mm - bend_allowance_mm;
    if !bend_allowance_mm.is_finite() || !bend_deduction_mm.is_finite() {
        return Err(StdLibError::InvalidBend("bend result is not finite"));
    }
    Ok(BendAllowanceResult {
        k_factor,
        bend_allowance_mm,
        bend_deduction_mm,
    })
}

#[derive(Clone, Copy)]
enum KFamily {
    Aluminum,
    Steel,
    Stainless,
    Titanium,
    Brass,
    Polymer,
}

fn family(material_id: &str) -> KFamily {
    let canonical = crate::materials::lookup_material(material_id)
        .map(|spec| spec.id)
        .unwrap_or_else(|_| material_id.trim().to_owned());
    match canonical.as_str() {
        "Al-6061-T6" | "Al-7075-T651" | "Al-7075-T6" => KFamily::Aluminum,
        "SUS304" | "SS-304" | "SS-316L" => KFamily::Stainless,
        "Ti-6Al-4V" => KFamily::Titanium,
        "Brass-C360" => KFamily::Brass,
        "POM-Delrin" | "PA12-SLS" | "PEEK" => KFamily::Polymer,
        // 45# Steel, Steel-4140, and unknown ids share the mild-steel row.
        _ => KFamily::Steel,
    }
}

/// Air-bend K-factor for `material_id` and inside-radius / thickness.
///
/// Rows are sharp (`R/T < 1`), ordinary (`1 <= R/T < 2`), wide
/// (`2 <= R/T < 4`), and generous (`R/T >= 4`). A non-finite ratio uses the
/// ordinary column. Unknown material ids use the mild-steel row.
pub fn default_k_factor(material_id: &str, radius_to_thickness_ratio: f64) -> f64 {
    let column = if !radius_to_thickness_ratio.is_finite() {
        1
    } else if radius_to_thickness_ratio < 1.0 {
        0
    } else if radius_to_thickness_ratio < 2.0 {
        1
    } else if radius_to_thickness_ratio < 4.0 {
        2
    } else {
        3
    };
    const TABLE: [[f64; 4]; 6] = [
        [0.36, 0.40, 0.44, 0.48], // aluminum
        [0.38, 0.42, 0.46, 0.50], // steel and unknown
        [0.34, 0.38, 0.42, 0.46], // stainless
        [0.32, 0.36, 0.40, 0.44], // titanium
        [0.37, 0.41, 0.45, 0.49], // brass
        [0.40, 0.44, 0.47, 0.50], // polymer
    ];
    let row = match family(material_id) {
        KFamily::Aluminum => 0,
        KFamily::Steel => 1,
        KFamily::Stainless => 2,
        KFamily::Titanium => 3,
        KFamily::Brass => 4,
        KFamily::Polymer => 5,
    };
    TABLE[row][column]
}

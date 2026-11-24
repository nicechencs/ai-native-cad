//! Rolling-bearing seats for common deep-groove ball bearings.
//!
//! Boundary sizes follow ISO 15. `608ZZ` uses the same boundary as `608`.
//! Shaft fits follow the SKF recommendation for a solid steel shaft, rotating
//! inner ring, and normal load: `js5` up to 10 mm, `j5` above 10 mm through
//! 17 mm, and `k5` above 17 mm. Housing fits are `H7` on the light 60/68
//! series and `J7` on the 62 series. Shoulder columns are the minimum shaft
//! abutment and the maximum housing abutment.

use serde::{Deserialize, Serialize};

use crate::StdLibError;

/// Installation data for one deep-groove ball bearing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BearingSpec {
    /// Catalog designation, for example `608ZZ` or `6204`.
    pub designation: String,
    /// Bore `d`, millimetres.
    pub bore_d_mm: f64,
    /// Outside diameter `D`, millimetres.
    pub outer_d_mm: f64,
    /// Width `B`, millimetres.
    pub width_mm: f64,
    /// Recommended shaft tolerance band, for example `k5`.
    pub recommended_shaft_fit: String,
    /// Recommended housing tolerance band, for example `H7`.
    pub recommended_housing_fit: String,
    /// Minimum shaft shoulder diameter, millimetres.
    pub shoulder_d_min_mm: f64,
    /// Maximum housing shoulder diameter, millimetres.
    pub housing_shoulder_d_max_mm: f64,
}

struct Bearing {
    designation: &'static str,
    /// Extra spellings that share this boundary (`608`, `608-2Z`, …).
    aliases: &'static [&'static str],
    bore_d_mm: f64,
    outer_d_mm: f64,
    width_mm: f64,
    shaft_fit: &'static str,
    housing_fit: &'static str,
    shoulder_d_min_mm: f64,
    housing_shoulder_d_max_mm: f64,
}

const BEARINGS: &[Bearing] = &[
    Bearing {
        designation: "608ZZ",
        aliases: &["608", "608-2Z", "608-ZZ", "6082Z"],
        bore_d_mm: 8.0,
        outer_d_mm: 22.0,
        width_mm: 7.0,
        shaft_fit: "js5",
        housing_fit: "H7",
        shoulder_d_min_mm: 10.0,
        housing_shoulder_d_max_mm: 20.0,
    },
    Bearing {
        designation: "6000",
        aliases: &[],
        bore_d_mm: 10.0,
        outer_d_mm: 26.0,
        width_mm: 8.0,
        shaft_fit: "js5",
        housing_fit: "H7",
        shoulder_d_min_mm: 12.0,
        housing_shoulder_d_max_mm: 24.0,
    },
    Bearing {
        designation: "6001",
        aliases: &[],
        bore_d_mm: 12.0,
        outer_d_mm: 28.0,
        width_mm: 8.0,
        shaft_fit: "j5",
        housing_fit: "H7",
        shoulder_d_min_mm: 14.0,
        housing_shoulder_d_max_mm: 26.0,
    },
    Bearing {
        designation: "6002",
        aliases: &[],
        bore_d_mm: 15.0,
        outer_d_mm: 32.0,
        width_mm: 9.0,
        shaft_fit: "j5",
        housing_fit: "H7",
        shoulder_d_min_mm: 17.0,
        housing_shoulder_d_max_mm: 30.0,
    },
    Bearing {
        designation: "6004",
        aliases: &[],
        bore_d_mm: 20.0,
        outer_d_mm: 42.0,
        width_mm: 12.0,
        shaft_fit: "k5",
        housing_fit: "H7",
        shoulder_d_min_mm: 25.0,
        housing_shoulder_d_max_mm: 38.0,
    },
    // 62-series shoulders are SKF abutment inches converted to millimetres.
    Bearing {
        designation: "6200",
        aliases: &[],
        bore_d_mm: 10.0,
        outer_d_mm: 30.0,
        width_mm: 9.0,
        shaft_fit: "js5",
        housing_fit: "J7",
        shoulder_d_min_mm: 14.2,
        housing_shoulder_d_max_mm: 25.8,
    },
    Bearing {
        designation: "6201",
        aliases: &[],
        bore_d_mm: 12.0,
        outer_d_mm: 32.0,
        width_mm: 10.0,
        shaft_fit: "j5",
        housing_fit: "J7",
        shoulder_d_min_mm: 16.2,
        housing_shoulder_d_max_mm: 27.8,
    },
    Bearing {
        designation: "6202",
        aliases: &[],
        bore_d_mm: 15.0,
        outer_d_mm: 35.0,
        width_mm: 11.0,
        shaft_fit: "j5",
        housing_fit: "J7",
        shoulder_d_min_mm: 19.2,
        housing_shoulder_d_max_mm: 30.8,
    },
    Bearing {
        designation: "6204",
        aliases: &[],
        bore_d_mm: 20.0,
        outer_d_mm: 47.0,
        width_mm: 14.0,
        shaft_fit: "k5",
        housing_fit: "J7",
        shoulder_d_min_mm: 25.6,
        housing_shoulder_d_max_mm: 41.4,
    },
    Bearing {
        designation: "6804",
        aliases: &[],
        bore_d_mm: 20.0,
        outer_d_mm: 32.0,
        width_mm: 7.0,
        shaft_fit: "k5",
        housing_fit: "H7",
        shoulder_d_min_mm: 22.0,
        housing_shoulder_d_max_mm: 30.0,
    },
    Bearing {
        designation: "6805",
        aliases: &[],
        bore_d_mm: 25.0,
        outer_d_mm: 37.0,
        width_mm: 7.0,
        shaft_fit: "k5",
        housing_fit: "H7",
        shoulder_d_min_mm: 27.0,
        housing_shoulder_d_max_mm: 35.0,
    },
];

fn normalize_designation(designation: &str) -> String {
    designation
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-' && *ch != '_')
        .flat_map(|ch| ch.to_uppercase())
        .collect()
}

fn same_designation(left: &str, right: &str) -> bool {
    normalize_designation(left) == normalize_designation(right)
}

/// Look up a bearing seat by designation (`608ZZ`, `6204`, …).
pub fn lookup_bearing(designation: &str) -> Result<BearingSpec, StdLibError> {
    let row = BEARINGS
        .iter()
        .find(|row| {
            same_designation(row.designation, designation)
                || row
                    .aliases
                    .iter()
                    .any(|alias| same_designation(alias, designation))
        })
        .ok_or_else(|| StdLibError::UnknownBearing(designation.trim().to_string()))?;
    Ok(BearingSpec {
        designation: row.designation.to_string(),
        bore_d_mm: row.bore_d_mm,
        outer_d_mm: row.outer_d_mm,
        width_mm: row.width_mm,
        recommended_shaft_fit: row.shaft_fit.to_string(),
        recommended_housing_fit: row.housing_fit.to_string(),
        shoulder_d_min_mm: row.shoulder_d_min_mm,
        housing_shoulder_d_max_mm: row.housing_shoulder_d_max_mm,
    })
}

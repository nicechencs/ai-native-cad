//! ISO 286-2 limit deviations for the hole and shaft bands used on fits.
//!
//! Steps match ISO 286: above the previous edge, up to and including the
//! listed edge. The first step is closed at 1 mm because sizes below 1 mm are
//! outside this catalog. `JS` / `js` are symmetric about the zero line
//! (`±IT/2`, half-microns kept). `H` / `h` sit on the zero line. `J7`, `g6`,
//! `f7`, `j5`, `k5`, `m6`, and `p6` are the published limit deviations for that
//! grade. Unknown bands and sizes outside 1 mm..=250 mm return an error.

use serde::{Deserialize, Serialize};

use crate::StdLibError;

/// Upper and lower deviations for one nominal size and one tolerance band.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToleranceDeviation {
    /// Requested nominal size, millimetres.
    pub nominal_mm: f64,
    /// Canonical band, for example `H7` or `g6`.
    pub band: String,
    /// Upper deviation, micrometres.
    pub upper_um: f64,
    /// Lower deviation, micrometres.
    pub lower_um: f64,
    /// Upper deviation, millimetres.
    pub upper_mm: f64,
    /// Lower deviation, millimetres.
    pub lower_mm: f64,
    /// `nominal_mm + upper_mm`.
    pub max_limit_mm: f64,
    /// `nominal_mm + lower_mm`.
    pub min_limit_mm: f64,
}

/// Inclusive upper edge of each ISO step, millimetres.
/// Index 0 covers `1..=3`. Later indexes cover `(previous, edge]`.
const STEP_EDGE_MM: [f64; 10] = [3.0, 6.0, 10.0, 18.0, 30.0, 50.0, 80.0, 120.0, 180.0, 250.0];

struct Limits {
    band: &'static str,
    upper_um: [f64; 10],
    lower_um: [f64; 10],
}

const BANDS: &[Limits] = &[
    Limits {
        band: "H6",
        upper_um: [6.0, 8.0, 9.0, 11.0, 13.0, 16.0, 19.0, 22.0, 25.0, 29.0],
        lower_um: [0.0; 10],
    },
    Limits {
        band: "H7",
        upper_um: [10.0, 12.0, 15.0, 18.0, 21.0, 25.0, 30.0, 35.0, 40.0, 46.0],
        lower_um: [0.0; 10],
    },
    Limits {
        band: "H8",
        upper_um: [14.0, 18.0, 22.0, 27.0, 33.0, 39.0, 46.0, 54.0, 63.0, 72.0],
        lower_um: [0.0; 10],
    },
    Limits {
        band: "H9",
        upper_um: [25.0, 30.0, 36.0, 43.0, 52.0, 62.0, 74.0, 87.0, 100.0, 115.0],
        lower_um: [0.0; 10],
    },
    Limits {
        band: "H11",
        upper_um: [
            60.0, 75.0, 90.0, 110.0, 130.0, 160.0, 190.0, 220.0, 250.0, 290.0,
        ],
        lower_um: [0.0; 10],
    },
    Limits {
        band: "J7",
        upper_um: [4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 18.0, 22.0, 26.0, 30.0],
        lower_um: [
            -6.0, -6.0, -7.0, -8.0, -9.0, -11.0, -12.0, -13.0, -14.0, -16.0,
        ],
    },
    Limits {
        band: "JS7",
        upper_um: [5.0, 6.0, 7.5, 9.0, 10.5, 12.5, 15.0, 17.5, 20.0, 23.0],
        lower_um: [
            -5.0, -6.0, -7.5, -9.0, -10.5, -12.5, -15.0, -17.5, -20.0, -23.0,
        ],
    },
    Limits {
        band: "g6",
        upper_um: [
            -2.0, -4.0, -5.0, -6.0, -7.0, -9.0, -10.0, -12.0, -14.0, -15.0,
        ],
        lower_um: [
            -8.0, -12.0, -14.0, -17.0, -20.0, -25.0, -29.0, -34.0, -39.0, -44.0,
        ],
    },
    Limits {
        band: "f7",
        upper_um: [
            -6.0, -10.0, -13.0, -16.0, -20.0, -25.0, -30.0, -36.0, -43.0, -50.0,
        ],
        lower_um: [
            -16.0, -22.0, -28.0, -34.0, -41.0, -50.0, -60.0, -71.0, -83.0, -96.0,
        ],
    },
    Limits {
        band: "h6",
        upper_um: [0.0; 10],
        lower_um: [
            -6.0, -8.0, -9.0, -11.0, -13.0, -16.0, -19.0, -22.0, -25.0, -29.0,
        ],
    },
    Limits {
        band: "h7",
        upper_um: [0.0; 10],
        lower_um: [
            -10.0, -12.0, -15.0, -18.0, -21.0, -25.0, -30.0, -35.0, -40.0, -46.0,
        ],
    },
    Limits {
        band: "j5",
        upper_um: [2.0, 3.0, 4.0, 5.0, 5.0, 6.0, 6.0, 6.0, 7.0, 7.0],
        lower_um: [
            -2.0, -2.0, -2.0, -3.0, -4.0, -5.0, -7.0, -9.0, -11.0, -13.0,
        ],
    },
    Limits {
        band: "js5",
        upper_um: [2.0, 2.5, 3.0, 4.0, 4.5, 5.5, 6.5, 7.5, 9.0, 10.0],
        lower_um: [-2.0, -2.5, -3.0, -4.0, -4.5, -5.5, -6.5, -7.5, -9.0, -10.0],
    },
    // k, grades IT4 to IT7: lower deviation is fundamental, upper = lower + IT5.
    Limits {
        band: "k5",
        upper_um: [4.0, 6.0, 7.0, 9.0, 11.0, 13.0, 15.0, 18.0, 21.0, 24.0],
        lower_um: [0.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 4.0],
    },
    Limits {
        band: "m6",
        upper_um: [8.0, 12.0, 15.0, 18.0, 21.0, 25.0, 30.0, 35.0, 40.0, 46.0],
        lower_um: [2.0, 4.0, 6.0, 7.0, 8.0, 9.0, 11.0, 13.0, 15.0, 17.0],
    },
    Limits {
        band: "p6",
        upper_um: [12.0, 20.0, 24.0, 29.0, 35.0, 42.0, 51.0, 59.0, 68.0, 79.0],
        lower_um: [6.0, 12.0, 15.0, 18.0, 22.0, 26.0, 32.0, 37.0, 43.0, 50.0],
    },
];

fn step_index(nominal_mm: f64) -> Result<usize, StdLibError> {
    if !nominal_mm.is_finite() || nominal_mm < 1.0 || nominal_mm > STEP_EDGE_MM[9] {
        return Err(StdLibError::OutOfRange(nominal_mm));
    }
    STEP_EDGE_MM
        .iter()
        .position(|edge| nominal_mm <= *edge)
        .ok_or(StdLibError::OutOfRange(nominal_mm))
}

/// ISO 286-2 deviations for `nominal_mm` and `band`.
///
/// `band` is case-sensitive: `H7` is a hole and `h7` is a shaft.
pub fn lookup_iso286(nominal_mm: f64, band: &str) -> Result<ToleranceDeviation, StdLibError> {
    let index = step_index(nominal_mm)?;
    let band = band.trim();
    let limits = BANDS
        .iter()
        .find(|row| row.band == band)
        .ok_or_else(|| StdLibError::UnknownToleranceBand(band.to_string()))?;
    let upper_um = limits.upper_um[index];
    let lower_um = limits.lower_um[index];
    let upper_mm = upper_um * 1.0e-3;
    let lower_mm = lower_um * 1.0e-3;
    Ok(ToleranceDeviation {
        nominal_mm,
        band: limits.band.to_string(),
        upper_um,
        lower_um,
        upper_mm,
        lower_mm,
        max_limit_mm: nominal_mm + upper_mm,
        min_limit_mm: nominal_mm + lower_mm,
    })
}

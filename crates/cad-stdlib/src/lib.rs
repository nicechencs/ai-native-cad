//! Read-only engineering standard library.
//!
//! Fastener holes, bearing seats, ISO 286 deviations, material constants, and
//! sheet-metal bend factors. A missing or out-of-range key returns
//! [`StdLibError`] instead of an invented number.

#![forbid(unsafe_code)]

pub mod bearings;
pub mod fasteners;
pub mod materials;
pub mod sheet_metal;
pub mod tolerances;

pub use bearings::{BearingSpec, lookup_bearing};
pub use fasteners::{
    ClearanceClass, CounterboreSpec, CountersinkSpec, lookup_iso4762_cbore, lookup_iso10642_csink,
    lookup_metric_tap_drill,
};
pub use materials::{MaterialSpec, lookup_material, mass_kg};
pub use sheet_metal::{BendAllowanceResult, compute_bend_allowance, default_k_factor};
pub use tolerances::{ToleranceDeviation, lookup_iso286};

/// Failure from a catalog lookup or a bend calculation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StdLibError {
    /// Fastener size or pitch is not in the metric catalog.
    #[error("unknown fastener specification `{0}`")]
    UnknownFastener(String),

    /// Bearing designation is not in the seat catalog.
    #[error("unknown bearing designation `{0}`")]
    UnknownBearing(String),

    /// Nominal size is outside the tabulated ISO 286 steps (1 mm through 250 mm).
    #[error("nominal size {0} mm is outside the ISO 286 table (1 mm through 250 mm)")]
    OutOfRange(f64),

    /// Tolerance band is not one of the tabulated hole or shaft bands.
    #[error("unknown ISO 286 tolerance band `{0}`")]
    UnknownToleranceBand(String),

    /// Material id is not in the catalog.
    #[error("unknown material `{0}`")]
    UnknownMaterial(String),

    /// Thickness, radius, angle, or K-factor cannot form a bend allowance.
    #[error("invalid sheet-metal bend: {0}")]
    InvalidBend(&'static str),
}

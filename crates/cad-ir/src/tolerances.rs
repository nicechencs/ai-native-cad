//! Canonical geometric and numerical tolerances shared across the CAD IR and
//! downstream geometry, topology, and verification layers.
//!
//! All linear tolerances are expressed in millimetres (`mm`), areas in `mm²`,
//! volumes in `mm³`, and angles in degrees (`deg`) or radians (`rad`) as noted
//! by each constant's suffix.

/// Canonical linear coincidence tolerance in millimetres (`1e-6 mm`, 1 nm).
pub const LINEAR_TOLERANCE_MM: f64 = 1.0e-6;

/// Canonical angular coincidence tolerance in degrees (`1e-4 deg`).
pub const ANGULAR_TOLERANCE_DEG: f64 = 1.0e-4;

/// Canonical angular coincidence tolerance in radians (`1e-6 rad`).
pub const ANGULAR_TOLERANCE_RAD: f64 = 1.0e-6;

/// B-Rep modeling and sewing precision tolerance in millimetres (`1e-5 mm`).
pub const KERNEL_BREP_TOLERANCE_MM: f64 = 1.0e-5;

/// 2D sketch endpoint closure and loop continuity tolerance in millimetres (`1e-4 mm`).
pub const SKETCH_CLOSURE_TOLERANCE_MM: f64 = 1.0e-4;

/// Tessellated mesh vertex welding and watertightness tolerance in millimetres (`1e-5 mm`).
pub const MESH_WATERTIGHT_TOLERANCE_MM: f64 = 1.0e-5;

/// Degenerate edge / segment zero-length guard in millimetres (`1e-9 mm`).
pub const ZERO_LENGTH_EPSILON_MM: f64 = 1.0e-9;

/// Degenerate face / loop zero-area guard in square millimetres (`1e-9 mm²`).
pub const ZERO_AREA_EPSILON_MM2: f64 = 1.0e-9;

/// Degenerate solid zero-volume guard in cubic millimetres (`1e-9 mm³`).
pub const ZERO_VOLUME_EPSILON_MM3: f64 = 1.0e-9;

/// Topological fingerprint centroid matching tolerance in millimetres (`1e-3 mm`).
pub const TOPO_CENTROID_TOLERANCE_MM: f64 = 1.0e-3;

/// Topological fingerprint normal/axis matching tolerance in degrees (`1.0 deg`).
pub const TOPO_NORMAL_ANGLE_TOLERANCE_DEG: f64 = 1.0;

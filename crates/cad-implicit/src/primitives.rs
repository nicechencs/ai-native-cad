//! Analytic signed-distance fields for common manufacturing primitives.
//!
//! All distances returned by this module are in millimetres.  A negative
//! value denotes the interior of a solid, zero is the boundary, and a
//! positive value denotes the exterior.

use crate::field::ScalarField;

/// Signed distance to an axis-aligned box.
#[derive(Debug, Clone, Copy)]
pub struct BoxSdf {
    /// Centre of the box in millimetres.
    pub center_mm: [f64; 3],
    /// Positive half extents along the x, y, and z axes.
    pub half_extents_mm: [f64; 3],
}

impl BoxSdf {
    /// Creates a box from its centre and half extents.
    #[must_use]
    pub fn new(center_mm: [f64; 3], half_extents_mm: [f64; 3]) -> Self {
        Self {
            center_mm,
            half_extents_mm,
        }
    }
}

impl ScalarField for BoxSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let q = [
            (p[0] - self.center_mm[0]).abs() - self.half_extents_mm[0].max(0.0),
            (p[1] - self.center_mm[1]).abs() - self.half_extents_mm[1].max(0.0),
            (p[2] - self.center_mm[2]).abs() - self.half_extents_mm[2].max(0.0),
        ];
        let [x, y, z] = q.map(|component| component.max(0.0));
        let outside_distance = x.hypot(y).hypot(z);
        outside_distance + q[0].max(q[1]).max(q[2]).min(0.0)
    }
}

/// Signed distance to a finite cylinder aligned with the z axis.
#[derive(Debug, Clone, Copy)]
pub struct CylinderSdf {
    /// Centre of the cylinder in millimetres.
    pub center_mm: [f64; 3],
    /// Radius in millimetres.
    pub radius_mm: f64,
    /// Half of the cylinder height in millimetres.
    pub half_height_mm: f64,
}

impl CylinderSdf {
    /// Creates a finite z-aligned cylinder.
    #[must_use]
    pub fn new(center_mm: [f64; 3], radius_mm: f64, half_height_mm: f64) -> Self {
        Self {
            center_mm,
            radius_mm,
            half_height_mm,
        }
    }
}

impl ScalarField for CylinderSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let x = p[0] - self.center_mm[0];
        let y = p[1] - self.center_mm[1];
        let z = (p[2] - self.center_mm[2]).abs();
        let q = [
            x.hypot(y) - self.radius_mm.max(0.0),
            z - self.half_height_mm.max(0.0),
        ];
        let outside_distance = q[0].max(0.0).hypot(q[1].max(0.0));
        outside_distance + q[0].max(q[1]).min(0.0)
    }
}

/// Signed distance to a sphere.
#[derive(Debug, Clone, Copy)]
pub struct SphereSdf {
    /// Centre of the sphere in millimetres.
    pub center_mm: [f64; 3],
    /// Radius in millimetres.
    pub radius_mm: f64,
}

impl SphereSdf {
    /// Creates a sphere from its centre and radius.
    #[must_use]
    pub fn new(center_mm: [f64; 3], radius_mm: f64) -> Self {
        Self {
            center_mm,
            radius_mm,
        }
    }
}

impl ScalarField for SphereSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let dx = p[0] - self.center_mm[0];
        let dy = p[1] - self.center_mm[1];
        let dz = p[2] - self.center_mm[2];
        dx.hypot(dy).hypot(dz) - self.radius_mm.max(0.0)
    }
}

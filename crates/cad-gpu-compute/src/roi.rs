//! Region-of-interest (ROI) centroid rebasing and double-float (`df32`) precision helpers.
//!
//! Subtracting an `f64` ROI anchor before running LBVH, ray-beam, interference,
//! or `f32` GPU passes eliminates catastrophic cancellation when parts are placed
//! far from the world origin (for example `[100_000mm, -50_000mm, 25_000mm]`).

use serde::{Deserialize, Serialize};

use crate::lbvh::{Aabb3, TriRef};

/// Double-float (`hi + lo`) Dekker split for high-precision `f32` GPU emulation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Df32 {
    pub hi: f32,
    pub lo: f32,
}

impl Df32 {
    /// Splits a finite `f64` into two `f32` limbs whose sum approximates `value`
    /// to ~48 bits of mantissa precision.
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        let hi = value as f32;
        let lo = (value - f64::from(hi)) as f32;
        Self { hi, lo }
    }

    /// Reconstructs the `f64` value from the two `f32` limbs.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from(self.hi) + f64::from(self.lo)
    }

    /// Splits a 3D `f64` vector into `Df32` components.
    #[must_use]
    pub fn from_vec3(point: [f64; 3]) -> [Self; 3] {
        [
            Self::from_f64(point[0]),
            Self::from_f64(point[1]),
            Self::from_f64(point[2]),
        ]
    }

    /// Restores a 3D `f64` vector from `Df32` components.
    #[must_use]
    pub fn to_vec3(point: [Self; 3]) -> [f64; 3] {
        [point[0].to_f64(), point[1].to_f64(), point[2].to_f64()]
    }
}

/// Centroid-anchored local coordinate frame in millimeters (`f64`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RoiAnchor {
    pub origin_mm: [f64; 3],
}

impl RoiAnchor {
    /// Creates an anchor at the specified world-space origin (in millimeters).
    #[must_use]
    pub const fn new(origin_mm: [f64; 3]) -> Self {
        Self { origin_mm }
    }

    /// Computes an ROI anchor at the bounding-box midpoint of `bounds`.
    #[must_use]
    pub fn from_bounds(bounds: Aabb3) -> Self {
        let origin_mm = std::array::from_fn(|axis| {
            let lo = bounds.min[axis];
            let hi = bounds.max[axis];
            if lo.is_finite() && hi.is_finite() {
                lo.midpoint(hi)
            } else {
                0.0
            }
        });
        Self { origin_mm }
    }

    /// Computes an ROI anchor at the bounding-box midpoint of all finite triangle vertices.
    #[must_use]
    pub fn from_triangles(triangles: &[TriRef]) -> Self {
        Self::from_triangles_and_aabbs(triangles, &[])
    }

    /// Computes a joint ROI anchor covering both triangles and auxiliary AABBs (such as keep-out zones).
    #[must_use]
    pub fn from_triangles_and_aabbs(triangles: &[TriRef], aabbs: &[Aabb3]) -> Self {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        let mut saw_finite = false;

        let mut include = |point: [f64; 3]| {
            if point.into_iter().all(f64::is_finite) {
                saw_finite = true;
                for axis in 0..3 {
                    lo[axis] = lo[axis].min(point[axis]);
                    hi[axis] = hi[axis].max(point[axis]);
                }
            }
        };

        for tri in triangles {
            include(tri.a);
            include(tri.b);
            include(tri.c);
        }
        for bounds in aabbs {
            include(bounds.min);
            include(bounds.max);
        }

        if !saw_finite {
            return Self::default();
        }
        Self::from_bounds(Aabb3 { min: lo, max: hi })
    }

    /// Converts a world-space point into ROI-local `f64` coordinates.
    #[must_use]
    pub fn to_local_f64(&self, world: [f64; 3]) -> [f64; 3] {
        [
            world[0] - self.origin_mm[0],
            world[1] - self.origin_mm[1],
            world[2] - self.origin_mm[2],
        ]
    }

    /// Restores an ROI-local point back to world-space `f64` coordinates.
    #[must_use]
    pub fn to_world_f64(&self, local: [f64; 3]) -> [f64; 3] {
        [
            local[0] + self.origin_mm[0],
            local[1] + self.origin_mm[1],
            local[2] + self.origin_mm[2],
        ]
    }

    /// Subtracts the `f64` ROI anchor and casts the local coordinate to finite `f32`.
    #[must_use]
    pub fn to_local_f32(&self, world: [f64; 3]) -> Option<[f32; 3]> {
        let local = self.to_local_f64(world);
        let cast = [local[0] as f32, local[1] as f32, local[2] as f32];
        cast.into_iter().all(f32::is_finite).then_some(cast)
    }

    /// Rebases a single triangle into ROI-local `f64` coordinates.
    #[must_use]
    pub fn rebase_triangle(&self, tri: &TriRef) -> TriRef {
        TriRef {
            instance_id: tri.instance_id,
            face_slot_id: tri.face_slot_id,
            a: self.to_local_f64(tri.a),
            b: self.to_local_f64(tri.b),
            c: self.to_local_f64(tri.c),
        }
    }

    /// Rebases a slice of triangles into ROI-local `f64` coordinates.
    #[must_use]
    pub fn rebase_triangles(&self, triangles: &[TriRef]) -> Vec<TriRef> {
        triangles
            .iter()
            .map(|tri| self.rebase_triangle(tri))
            .collect()
    }

    /// Rebases an AABB into ROI-local `f64` coordinates.
    #[must_use]
    pub fn rebase_aabb(&self, bounds: &Aabb3) -> Aabb3 {
        Aabb3 {
            min: self.to_local_f64(bounds.min),
            max: self.to_local_f64(bounds.max),
        }
    }

    /// Restores an ROI-local AABB back into world `f64` coordinates.
    #[must_use]
    pub fn restore_aabb(&self, local_bounds: &Aabb3) -> Aabb3 {
        Aabb3 {
            min: self.to_world_f64(local_bounds.min),
            max: self.to_world_f64(local_bounds.max),
        }
    }
}

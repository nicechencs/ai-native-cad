//! 6-DOF rigid pose and assembly component state.
//!
//! `rotation_deg` is `[rx, ry, rz]` in degrees. The rotation matrix is the
//! ZYX Tait-Bryan product `Rz(rz) * Ry(ry) * Rx(rx)`.

use serde::{Deserialize, Serialize};

/// Rigid transform of one assembly component, in millimetres and degrees.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RigidPose {
    /// Component origin in the assembly frame, in millimetres.
    pub translation_mm: [f64; 3],
    /// ZYX Euler angles `[rx, ry, rz]` in degrees.
    pub rotation_deg: [f64; 3],
}

impl RigidPose {
    /// Identity pose: origin, with no rotation.
    pub fn identity() -> Self {
        Self {
            translation_mm: [0.0, 0.0, 0.0],
            rotation_deg: [0.0, 0.0, 0.0],
        }
    }

    /// `Rz(rz) * Ry(ry) * Rx(rx)`, row-major.
    ///
    /// Column `i` is the image of the corresponding local basis vector.
    pub fn rotation_matrix(&self) -> [[f64; 3]; 3] {
        let (sx, cx) = self.rotation_deg[0].to_radians().sin_cos();
        let (sy, cy) = self.rotation_deg[1].to_radians().sin_cos();
        let (sz, cz) = self.rotation_deg[2].to_radians().sin_cos();
        [
            [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
            [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
            [-sy, cy * sx, cy * cx],
        ]
    }

    /// Map a point from the component frame into the assembly frame.
    pub fn transform_point(&self, local: [f64; 3]) -> [f64; 3] {
        let rotated = self.transform_dir(local);
        add(rotated, self.translation_mm)
    }

    /// Map a free vector by the rotation only. Length is preserved.
    pub fn transform_dir(&self, local: [f64; 3]) -> [f64; 3] {
        let r = self.rotation_matrix();
        [
            r[0][0] * local[0] + r[0][1] * local[1] + r[0][2] * local[2],
            r[1][0] * local[0] + r[1][1] * local[1] + r[1][2] * local[2],
            r[2][0] * local[0] + r[2][1] * local[1] + r[2][2] * local[2],
        ]
    }
}

impl Default for RigidPose {
    fn default() -> Self {
        Self::identity()
    }
}

/// One rigid body in an assembly.
///
/// A grounded component keeps its pose. Its six degrees of freedom are left
/// out of the solver variable vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssemblyComponent {
    pub id: String,
    pub pose: RigidPose,
    /// When true, the pose is fixed and excluded from the free variables.
    pub grounded: bool,
}

pub(crate) fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// Unit vector. A near-zero input returns zero so callers can detect it.
pub(crate) fn unit(a: [f64; 3]) -> [f64; 3] {
    let len = length(a);
    if len < 1e-15 {
        [0.0, 0.0, 0.0]
    } else {
        scale(a, 1.0 / len)
    }
}

pub(crate) fn finite3(a: [f64; 3]) -> bool {
    a.iter().all(|v| v.is_finite())
}

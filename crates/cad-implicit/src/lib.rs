//! Implicit signed-distance fields (SDF), TPMS lattices, and isosurface extraction.

#![forbid(unsafe_code)]

pub mod csg;
pub mod fea_field;
pub mod field;
pub mod hybrid;
pub mod isosurface;
pub mod primitives;
pub mod tpms;

pub use csg::{
    IntersectSdf, LatticeInfillSdf, ShellSdf, SmoothSubtractSdf, SmoothUnionSdf, SubtractSdf,
    UnionSdf,
};
pub use fea_field::{
    FLDS_MAGIC, FLDS_VERSION, ImplicitError, StressDrivenThicknessField, VoxelScalarField,
    decode_voxel_field_blob, encode_voxel_field_blob,
};
pub use field::{ConstantField, FieldRamp, ScalarField, gradient, normal};
pub use hybrid::{
    ExactInterfaceZone, HybridTpmsField, LayerFieldEvaluator, LayerGridSpec,
    wall_thickness_for_relative_density,
};
pub use isosurface::{ImplicitMesh, estimate_volume_mm3, extract_isosurface, relative_density};
pub use primitives::{BoxSdf, CylinderSdf, SphereSdf};
pub use tpms::{TpmsFamily, TpmsField, TpmsSheetField};

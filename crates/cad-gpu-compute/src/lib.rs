//! LBVH interference, ray-beam DFM gate, exact Keep-Out intersection, and WGSL compute kernel modules.

#![forbid(unsafe_code)]

pub mod backend;
pub mod dfm_gate;
pub mod dfm_raybeam;
pub mod interference;
pub mod keepout;
pub mod lbvh;
pub mod morton;
pub mod roi;
pub mod shaders;
pub mod voxel_fea;
#[cfg(feature = "wgpu-compute")]
pub mod wgpu_runner;

pub use backend::{GpuComputeBackend, GpuComputeError};
pub use dfm_gate::{
    DfmAnalysisRequest, DfmGateSummary, DfmRepairHint, DfmRepairKind, run_dfm_gate,
    run_dfm_gate_with_backend,
};
pub use dfm_raybeam::{
    DfmConfig, DfmReport, DfmViolationKind, FaceDfmMetrics, evaluate_raybeam_dfm,
};
pub use interference::{InterferencePair, InterferenceReport, check_assembly_interference};
pub use keepout::{
    KeepOutReport, KeepOutShape, KeepOutViolation, KeepOutZone, check_keepout_zones,
    detect_mesh_interference_pairs,
};
pub use lbvh::{Aabb3, GpuLbvhNode, LbvhTree, RayHit, TriRef};
pub use morton::morton_3d;
pub use roi::{Df32, RoiAnchor};
pub use shaders::{LBVH_BROADPHASE_WGSL, RAYBEAM_DFM_WGSL};
pub use voxel_fea::{
    FeaGateVerdict, FeaMaterial, FeaRegion, FixedConstraint, ForceLoad, VoxelFeaConfig,
    VoxelFeaReport, VoxelGrid, solve_mesh_voxel_fea, solve_mesh_voxel_fea_auto, solve_voxel_fea,
    voxelize_mesh, voxelize_mesh_with_max_voxels,
};

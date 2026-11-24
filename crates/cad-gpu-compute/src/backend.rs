//! Pluggable GPU / CPU LBVH compute backend selection.
//!
//! [`GpuComputeBackend::auto`] selects a WebGPU compute context when built with
//! the `wgpu-compute` feature and an adapter is available, and falls back
//! deterministically to the CPU LBVH reference implementation in headless CI.

use std::sync::OnceLock;

use crate::dfm_gate::{DfmAnalysisRequest, DfmGateSummary, run_dfm_gate_with_backend};
use crate::dfm_raybeam::{DfmConfig, DfmReport, evaluate_raybeam_dfm};
use crate::interference::InterferenceReport;
use crate::keepout::{KeepOutReport, KeepOutZone, check_keepout_zones, detect_mesh_interference_pairs};
use crate::lbvh::TriRef;
use crate::morton::morton_3d;

#[derive(Debug, thiserror::Error)]
pub enum GpuComputeError {
    #[error("no compatible WebGPU adapter")]
    NoAdapter,
    #[cfg(feature = "wgpu-compute")]
    #[error("could not create WebGPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    #[cfg(feature = "wgpu-compute")]
    #[error("could not compile WebGPU compute pipelines: {0}")]
    Pipeline(#[source] wgpu::Error),
    #[cfg(feature = "wgpu-compute")]
    #[error("could not map the WebGPU readback buffer: {0}")]
    Map(#[from] wgpu::BufferAsyncError),
    #[error("WebGPU readback callback dropped")]
    ReadbackDropped,
    #[error("input exceeds WebGPU buffer or dispatch limits")]
    WorkloadLimit,
    #[error("DFM geometry is outside the GPU kernel's numeric range")]
    DfmGeometryRange,
}

/// Active compute backend for Morton LBVH and ray-beam DFM workloads.
#[derive(Debug)]
pub enum GpuComputeBackend {
    #[cfg(feature = "wgpu-compute")]
    Wgpu(crate::wgpu_runner::GpuComputeContext),
    CpuFallback,
}

impl GpuComputeBackend {
    /// Auto-selects a WebGPU adapter when `wgpu-compute` is compiled in and an
    /// adapter is available; otherwise returns [`Self::CpuFallback`].
    #[must_use]
    pub fn auto() -> Self {
        #[cfg(feature = "wgpu-compute")]
        {
            Self::from_context_result(crate::wgpu_runner::GpuComputeContext::try_new())
        }
        #[cfg(not(feature = "wgpu-compute"))]
        {
            Self::CpuFallback
        }
    }

    /// Process-wide cached auto-selected backend to avoid repeated adapter probing.
    #[must_use]
    pub fn shared_auto() -> &'static Self {
        static SHARED: OnceLock<GpuComputeBackend> = OnceLock::new();
        SHARED.get_or_init(Self::auto)
    }

    /// Explicit deterministic CPU LBVH backend.
    #[must_use]
    pub const fn cpu_fallback() -> Self {
        Self::CpuFallback
    }

    /// Machine-readable identifier of the active backend (`"wgpu"` or `"cpu-lbvh"`).
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        match self {
            #[cfg(feature = "wgpu-compute")]
            Self::Wgpu(_) => "wgpu",
            Self::CpuFallback => "cpu-lbvh",
        }
    }

    /// Returns true when backed by a live WebGPU compute context.
    #[must_use]
    pub fn is_gpu(&self) -> bool {
        match self {
            #[cfg(feature = "wgpu-compute")]
            Self::Wgpu(_) => true,
            Self::CpuFallback => false,
        }
    }

    pub fn compute_morton_codes(
        &self,
        centroids: &[[f32; 3]],
        scene_min: [f32; 3],
        scene_max: [f32; 3],
    ) -> Result<Vec<u32>, GpuComputeError> {
        match self {
            #[cfg(feature = "wgpu-compute")]
            Self::Wgpu(context) => context.compute_morton_codes(centroids, scene_min, scene_max),
            Self::CpuFallback => {
                let min = scene_min.map(f64::from);
                let max = scene_max.map(f64::from);
                Ok(centroids
                    .iter()
                    .map(|point| {
                        let [x, y, z] = point.map(f64::from);
                        morton_3d(x, y, z, min, max)
                    })
                    .collect())
            }
        }
    }

    /// Reports DFM metrics using the available backend. Oversized geometry or
    /// geometry outside the GPU kernel's numeric range falls back to the CPU reference.
    pub fn analyze_dfm_raybeams(
        &self,
        triangles: &[TriRef],
        config: &DfmConfig,
    ) -> Result<DfmReport, GpuComputeError> {
        #[cfg(feature = "wgpu-compute")]
        if let Self::Wgpu(context) = self {
            match context.analyze_dfm_raybeams(triangles, config) {
                Err(
                    error @ (GpuComputeError::WorkloadLimit | GpuComputeError::DfmGeometryRange),
                ) => {
                    crate::wgpu_runner::warn_gpu_fallback(
                        "analyze_dfm_raybeams",
                        crate::wgpu_runner::gpu_error_type(&error),
                        format!(
                            "GPU DFM dispatch fell back to CPU reference ({error}; triangles={})",
                            triangles.len()
                        ),
                    );
                }
                result => return result,
            }
        }
        Ok(evaluate_raybeam_dfm(triangles, config))
    }

    /// Runs exact ROI-rebased Keep-Out zone intersection checks.
    #[must_use]
    pub fn check_keepout(&self, triangles: &[TriRef], zones: &[KeepOutZone]) -> KeepOutReport {
        check_keepout_zones(triangles, zones)
    }

    /// Runs exact ROI-rebased multi-body interference and containment checks.
    #[must_use]
    pub fn check_interference(
        &self,
        triangles: &[TriRef],
        min_clearance_mm: f64,
    ) -> InterferenceReport {
        detect_mesh_interference_pairs(triangles, min_clearance_mm)
    }

    /// Evaluates the full L3 DFM + Keep-Out + Interference gate and produces
    /// structured machine-readable repair hints.
    #[must_use]
    pub fn evaluate_dfm_gate(&self, request: &DfmAnalysisRequest) -> DfmGateSummary {
        run_dfm_gate_with_backend(self, request)
    }
}

//! Last-known-good geometry cache and drift detection.

use serde::{Deserialize, Serialize};

use crate::{AicadProject, KernelFingerprint, compute_sha256_hex};

/// A kernel-bound B-Rep snapshot that can be opened while an evaluator drifts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastKnownGoodCache {
    pub kernel: KernelFingerprint,
    pub graph_sha256: String,
    pub brep_bytes: Vec<u8>,
}

/// Result of comparing the saved cache with the current graph and kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CacheStatus {
    FreshExactMatch,
    KernelVersionDrift {
        cached: KernelFingerprint,
        current: KernelFingerprint,
    },
    GraphModified,
    Missing,
}

/// Compute the cache key from the canonical graph JSON representation.
pub(crate) fn graph_sha256(graph: &cad_ir::CanonicalGraph) -> Option<String> {
    let canonical_json = graph.to_canonical_json().ok()?;
    Some(compute_sha256_hex(canonical_json.as_bytes()))
}

pub(crate) fn evaluate_cache_status(
    graph: &cad_ir::CanonicalGraph,
    cached: Option<(&KernelFingerprint, &str)>,
    current_kernel: &KernelFingerprint,
) -> CacheStatus {
    let Some((cached_kernel, cached_graph_sha256)) = cached else {
        return CacheStatus::Missing;
    };

    let Some(current_graph_sha256) = graph_sha256(graph) else {
        return CacheStatus::GraphModified;
    };

    if current_graph_sha256 != cached_graph_sha256 {
        return CacheStatus::GraphModified;
    }
    if cached_kernel != current_kernel {
        return CacheStatus::KernelVersionDrift {
            cached: cached_kernel.clone(),
            current: current_kernel.clone(),
        };
    }
    CacheStatus::FreshExactMatch
}

impl AicadProject {
    /// Compare a proposed rebuild with the saved cache.
    /// Opening a project preserves its cache independently of this check.
    pub fn check_cache_status(&self, current_kernel: &KernelFingerprint) -> CacheStatus {
        evaluate_cache_status(
            &self.graph,
            self.last_known_good
                .as_ref()
                .map(|cache| (&cache.kernel, cache.graph_sha256.as_str())),
            current_kernel,
        )
    }
}

//! Stateful CAD document runtime.
//!
//! One process hosts a [`DocumentSession`] over a SQLite `.aicad` file.
//! Desktop and headless hosts share this crate. Geometry, patches, and history
//! stay in `cad-api`, `cad-container`, `cad-evaluator`, and `cad-kcl-bridge`.

#![forbid(unsafe_code)]

mod atomic_write;
mod discovery;
mod error;
mod l3_dfm;
mod l3_fea;
mod sandbox;
mod session;
mod types;

pub use atomic_write::write_file_atomic;

pub use discovery::{
    DISCOVERY_PROTOCOL_VERSION, RUNTIME_DISCOVERY_FILE, RuntimeDiscoveryInfo, PublishedDiscovery,
    cleanup_runtime_discovery, default_ipc_endpoint, discovery_pid_is_stale,
    publish_runtime_discovery, read_runtime_discovery, sha256_hex, write_runtime_discovery,
};
pub use error::RuntimeError;
pub use l3_fea::{
    FeaGateSummaryPayload, FeaRepairHint, L3FeaEvaluation, StructuralFeaContract, evaluate_l3_fea,
    evaluate_l3_fea_triangles, resolve_fea_material,
};
pub use sandbox::resolve_in_workspace;
pub use session::{DocumentRuntime, DocumentSession, encode_solid_3mf, fast_verification_passes};
pub use types::{
    Actor, BranchDiff, BranchTarget, BBoxDelta, CameraPose, CancelToken, CommitIfPassOutcome,
    ExecOutcome, ExportArtifactReport, HistoryDirection, HistoryOutcome, MetricDelta, RuntimeMode,
    SessionContext, SolidDigest, VerificationProfile, ViewportContext,
};

pub use cad_api::{CanonicalGraph, ChangeSummary, NodeId, NodeRecoveryHint, SemanticCommand};
pub use cad_container::{BranchInfo, BranchPurpose, BranchStatus, KernelFingerprint};
pub use cad_ir::KernelIdentity;
pub use cad_kernel_occt::GeometryKernel;

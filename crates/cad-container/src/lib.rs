//! Open hybrid `.aicad` project container (SQLite working container & Git-friendly directory/sharded-directory exchange).

#![forbid(unsafe_code)]

pub mod blobs;
pub mod cache;
pub mod detect;
mod directory_store;
pub mod drawing;
pub mod engineering;
pub mod lazy;
pub mod lifecycle;
pub mod manifest;
pub mod sharded;
pub mod sqlite;

use std::collections::BTreeMap;

use cad_ir::{CanonicalGraph, VerificationReport};

pub use blobs::compute_sha256_hex;
pub use cache::{CacheStatus, LastKnownGoodCache};
pub use detect::{
    JournalPolicy, SQLITE_APPLICATION_ID, SQLITE_MAGIC, StorageKind, detect_storage_kind,
    is_network_or_unc_path, is_reparse_or_symlink, resolve_journal_mode_for_path,
};
pub use drawing::{DrawingDimension, DrawingSheet, DrawingViewKind, DrawingViewSpec};
pub use engineering::{
    AppearanceOverride, BomItem, EngineeringMetadata, PmiAnnotation, PmiAnnotationKind, PmiKind,
};
pub use lazy::{LazyProjectReader, ProjectInspectionSummary};
pub use lifecycle::{PackAndGoReport, VacuumReport, pack_and_go, vacuum_container};
pub use manifest::{
    CONTAINER_FORMAT_VERSION, ContainerManifest, KclProjectionPolicy, KernelFingerprint,
    SHARDED_ENTRY_GRAPH_PATH,
};
pub use sharded::{GraphNodeShard, ShardedGraphHeader, merge_sharded_graph, split_graph_into_shards};
pub use sqlite::{
    BranchInfo, BranchPurpose, BranchStatus, BundledRefEntry, NodeBrepCacheEntry, OplogActor,
    OplogEntry, SqliteContainer, apply_patch_in_place,
};

/// A project shared across SQLite and unpacked directory (single-graph or sharded) formats.
#[derive(Debug, Clone, PartialEq)]
pub struct AicadProject {
    pub manifest: ContainerManifest,
    pub graph: CanonicalGraph,
    pub kcl_view: Option<String>,
    pub engineering: EngineeringMetadata,
    pub drawings: BTreeMap<String, DrawingSheet>,
    /// Payloads indexed by the relative paths in `graph.blobs`.
    pub blobs: BTreeMap<String, Vec<u8>>,
    pub last_known_good: Option<LastKnownGoodCache>,
    pub cadpack_bytes: Option<Vec<u8>>,
    /// Latest persisted five-layer tri-state engineering verification report.
    pub verification_report: Option<VerificationReport>,
}

impl AicadProject {
    /// Create a project from its manifest and graph, without optional payloads.
    pub fn new(manifest: ContainerManifest, graph: CanonicalGraph) -> Self {
        Self {
            manifest,
            graph,
            kcl_view: None,
            engineering: EngineeringMetadata::default(),
            drawings: BTreeMap::new(),
            blobs: BTreeMap::new(),
            last_known_good: None,
            cadpack_bytes: None,
            verification_report: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContainerError {
    #[error("unsupported {component} version {found} (supported version: {supported})")]
    UnsupportedVersion {
        component: &'static str,
        found: u64,
        supported: u32,
    },
    #[error("unsupported or unrecognized container format at `{0}`")]
    UnsupportedFormat(String),
    #[error("unsafe container path `{0}`")]
    UnsafePath(String),
    #[error("container entry `{0}` is missing")]
    MissingEntry(String),
    #[error("blob `{0}` is missing")]
    MissingBlob(String),
    #[error("blob `{path}` does not match its recorded SHA-256 or byte length")]
    BlobHashMismatch { path: String },
    #[error("drawing `{sheet_id}` does not match its path `{path}`")]
    DrawingIdMismatch { path: String, sheet_id: String },
    #[error("branch `{0}` already exists")]
    DuplicateBranch(String),
    #[error("unknown branch `{0}`")]
    UnknownBranch(String),
    #[error("cannot delete active branch `{0}`")]
    CannotDeleteActiveBranch(String),
    #[error(
        "invalid rollback target sequence {target_seq} on branch `{branch}` (head: {head_seq})"
    )]
    InvalidRollbackTarget {
        branch: String,
        target_seq: i64,
        head_seq: i64,
    },
    #[error("external reference cycle detected: {0}")]
    ExternalRefCycle(String),
    #[error(
        "external reference `{ref_id}` (`{path}`) SHA-256 mismatch: expected {expected}, got {actual}"
    )]
    ExternalRefHashMismatch {
        ref_id: String,
        path: String,
        expected: String,
        actual: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Ir(#[from] cad_ir::IrError),
}

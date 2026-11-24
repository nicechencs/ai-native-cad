//! Session identity, viewport state, and command results.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use cad_api::{ChangeSummary, NodeId, NodeRecoveryHint};
use cad_ir::VerificationReport;

use crate::error::RuntimeError;

/// Who submitted a document change. Stored on the SQLite `oplog` actor column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    HumanUi,
    BuiltinAgent,
    ExternalAgent { client: String, session_id: String },
    PythonSdk,
    KclView,
}

impl Default for Actor {
    fn default() -> Self {
        Self::HumanUi
    }
}

/// Where a batch is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchTarget {
    /// The branch the session currently has checked out.
    Active,
    /// The durable `main` branch.
    Main,
    /// An explicit branch, including a preview sandbox.
    Named(String),
}

/// How this process is hosting the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    /// CLI, CI, or a git worktree. The runtime lives in the calling process.
    Headless,
    /// A desktop host owns the session and publishes `.aicad-runtime.json`.
    LiveAttached,
}

/// Camera placement in millimetres. Axes follow the document frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraPose {
    pub eye: [f64; 3],
    pub target: [f64; 3],
    pub up: [f64; 3],
}

/// Live viewport selection. This is session memory, not an oplog edit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewportContext {
    pub selected_topo_refs: Vec<String>,
    pub selected_node_ids: Vec<String>,
    pub camera_pose: Option<CameraPose>,
}

/// Read-only picture of the open session for a status query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContext {
    pub revision: u64,
    pub active_branch: String,
    pub project_name: String,
    pub actor: Actor,
    pub mode: RuntimeMode,
    pub viewport: ViewportContext,
}

/// Cooperative cancel flag shared with the caller.
///
/// The runtime checks it before evaluation and again before the SQLite commit.
#[derive(Clone, Debug)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// Volumes and bounds of the solids produced by the latest evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SolidDigest {
    pub output_count: usize,
    pub valid_nodes: usize,
    pub total_volume_mm3: f64,
    pub bbox_min: [f64; 3],
    pub bbox_max: [f64; 3],
    pub cache_hits: usize,
    pub cache_misses: usize,
    pub recomputed_nodes: usize,
}

impl SolidDigest {
    pub fn matches_geometry(&self, other: &Self) -> bool {
        self.output_count == other.output_count
            && self.valid_nodes == other.valid_nodes
            && (self.total_volume_mm3 - other.total_volume_mm3).abs() < 1e-6
            && vec_close(self.bbox_min, other.bbox_min)
            && vec_close(self.bbox_max, other.bbox_max)
    }
}

fn vec_close(left: [f64; 3], right: [f64; 3]) -> bool {
    left.iter()
        .zip(right)
        .all(|(a, b)| (a - b).abs() < 1e-6)
}

/// Scalar before/after pair. `before` is `main` when the diff is a branch preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricDelta {
    pub before: f64,
    pub after: f64,
}

/// Axis-aligned bounds of the evaluated output solids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BBoxDelta {
    pub before_min: [f64; 3],
    pub before_max: [f64; 3],
    pub after_min: [f64; 3],
    pub after_max: [f64; 3],
}

/// Parameter, node, volume, and bounding-box differences against `main`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BranchDiff {
    pub branch: String,
    pub base: String,
    pub added_parameters: Vec<String>,
    pub removed_parameters: Vec<String>,
    pub changed_parameters: Vec<String>,
    pub added_nodes: Vec<NodeId>,
    pub removed_nodes: Vec<NodeId>,
    pub changed_nodes: Vec<NodeId>,
    pub volume_mm3: MetricDelta,
    pub bbox: BBoxDelta,
}

/// Result of one committed semantic batch, KCL sync, or preview merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecOutcome {
    pub revision: u64,
    pub branch: String,
    pub summary: ChangeSummary,
    pub created_nodes: Vec<NodeId>,
    pub modified_nodes: Vec<NodeId>,
    pub deleted_nodes: Vec<NodeId>,
    pub solids: SolidDigest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_hints: Vec<NodeRecoveryHint>,
}

/// Result of [`crate::DocumentSession::exec_semantic_cmds_commit_if_pass`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitIfPassOutcome {
    pub committed: bool,
    pub outcome: ExecOutcome,
    pub report: VerificationReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryDirection {
    Undo,
    Redo,
}

/// How much of the five-layer report a caller asked to run.
///
/// `Fast` judges structure, solids, and explicit interface checks.
/// `Release` also runs manufacturability and a STEP round-trip when the
/// authoritative kernel is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationProfile {
    Fast,
    Release,
}

impl VerificationProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Release => "release",
        }
    }
}

/// One geometry file written inside the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportArtifactReport {
    pub format: String,
    pub requested_path: String,
    pub resolved_path: String,
    pub project_path: String,
    pub revision: u64,
    pub branch: String,
    pub bytes_written: u64,
    pub sha256: String,
    pub kernel_backend: String,
    /// True when the session geometry kernel supports authoritative B-Rep delivery (OCCT).
    #[serde(default)]
    pub kernel_authoritative: bool,
    /// `EXACT` for authoritative STEP/DXF/SVG, `APPROXIMATE` for non-authoritative DXF/SVG, `MESH` for 3MF.
    pub geometry_bytes: String,
    pub preserved: Vec<String>,
    pub not_preserved: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_label: Option<String>,
}

/// Result of moving the oplog cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryOutcome {
    pub revision: u64,
    pub branch: String,
    pub direction: HistoryDirection,
}

pub(crate) fn ensure_running(
    session_token: &CancelToken,
    extra: Option<&CancelToken>,
) -> Result<(), RuntimeError> {
    if session_token.is_cancelled() || extra.is_some_and(CancelToken::is_cancelled) {
        Err(RuntimeError::Cancelled)
    } else {
        Ok(())
    }
}

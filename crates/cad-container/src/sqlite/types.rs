use cad_ir::GraphPatch;
use serde::{Deserialize, Serialize};

use crate::KernelFingerprint;

/// Purpose classification for lightweight container branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchPurpose {
    #[default]
    Main,
    User,
    AiCandidate,
}

impl BranchPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::User => "user",
            Self::AiCandidate => "ai_candidate",
        }
    }

    pub(super) fn parse(s: &str) -> Self {
        match s {
            "main" => Self::Main,
            "ai_candidate" => Self::AiCandidate,
            _ => Self::User,
        }
    }
}

/// Lifecycle state of a container branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchStatus {
    #[default]
    Active,
    Promoted,
    Discarded,
    Archived,
}

impl BranchStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Promoted => "promoted",
            Self::Discarded => "discarded",
            Self::Archived => "archived",
        }
    }

    pub(super) fn parse(s: &str) -> Self {
        match s {
            "promoted" => Self::Promoted,
            "discarded" => Self::Discarded,
            "archived" => Self::Archived,
            _ => Self::Active,
        }
    }
}

/// Summary of a lightweight branch inside a SQLite `.aicad` container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BranchInfo {
    pub branch_name: String,
    pub head_seq: i64,
    pub base_branch: Option<String>,
    pub base_seq: Option<i64>,
    pub purpose: BranchPurpose,
    pub status: BranchStatus,
    pub gate_score: Option<f64>,
    pub is_active: bool,
    pub created_at_ms: i64,
}

/// Provenance actor recorded in `oplog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OplogActor {
    #[default]
    HumanGui,
    KclSync,
    PythonSdk,
    AiAgent,
    System,
}

impl OplogActor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HumanGui => "human_gui",
            Self::KclSync => "kcl_sync",
            Self::PythonSdk => "python_sdk",
            Self::AiAgent => "ai_agent",
            Self::System => "system",
        }
    }

    pub(super) fn parse(s: &str) -> Self {
        match s {
            "kcl_sync" => Self::KclSync,
            "python_sdk" => Self::PythonSdk,
            "ai_agent" => Self::AiAgent,
            "system" => Self::System,
            _ => Self::HumanGui,
        }
    }
}

/// Atomic operation log entry persisted in `oplog`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OplogEntry {
    pub seq: i64,
    pub branch_name: String,
    pub parent_seq: Option<i64>,
    pub patch_id: String,
    pub actor: OplogActor,
    pub description: String,
    pub patch: GraphPatch,
    pub outputs_merkle: String,
    pub created_at_ms: i64,
}

/// Node-level Merkle B-Rep cache record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeBrepCacheEntry {
    pub merkle_hash: String,
    pub kernel: KernelFingerprint,
    pub uncompressed_len: u64,
    pub is_output_pinned: bool,
    pub brep_bytes: Vec<u8>,
}

/// Metadata record for an inline external container stored in `bundled_refs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundledRefEntry {
    pub ref_id: String,
    pub original_container_path: String,
    pub container_sha256: String,
    pub source_format: String,
    pub uncompressed_len: u64,
    pub bundled_at_ms: i64,
}

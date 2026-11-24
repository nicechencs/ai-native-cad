//! Failures returned by the document runtime.
//!
//! A failed command leaves the in-memory graph and the SQLite document unchanged.

use cad_api::SemanticApiError;
use cad_container::ContainerError;
use cad_kcl_bridge::BridgeError;

/// Structured runtime failure. [`RuntimeError::code`] is the stable machine code.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// `expected_revision` does not match the target branch. Nothing was written.
    #[error(
        "E409_REVISION_CONFLICT: expected revision {expected}, actual {actual}: {latest_summary}"
    )]
    RevisionConflict {
        expected: u64,
        actual: u64,
        latest_summary: String,
    },

    /// The path walks out of `workspace_root` or contains a `..` segment.
    #[error("E_PATH_ESCAPE: `{path}` escapes workspace `{workspace_root}`")]
    PathEscape {
        path: String,
        workspace_root: String,
    },

    /// The caller cancelled the command before it was committed.
    #[error("E_CANCELLED: operation cancelled before commit")]
    Cancelled,

    /// The active branch has no oplog entry to invert.
    #[error("E_NOTHING_TO_UNDO")]
    NothingToUndo,

    /// A new edit, or a fresh session, cleared the redo stack.
    #[error("E_NOTHING_TO_REDO")]
    NothingToRedo,

    /// The branch name is not in the container.
    #[error("E_UNKNOWN_BRANCH: {0}")]
    UnknownBranch(String),

    /// A branch with this name already exists.
    #[error("E_BRANCH_EXISTS: {0}")]
    BranchExists(String),

    /// `main` cannot be discarded, and the active branch cannot be deleted in place.
    #[error("E_PROTECTED_BRANCH: {0}")]
    ProtectedBranch(String),

    /// `main` moved after the preview was forked, so commit would overwrite it.
    #[error(
        "E409_PREVIEW_DIVERGED: main revision {main_revision} moved past preview base {base_revision}"
    )]
    PreviewDiverged {
        main_revision: u64,
        base_revision: u64,
    },

    /// In-memory state does not match the SQLite head.
    #[error("E500_PERSIST_MISMATCH: in-memory session does not match the SQLite head")]
    PersistMismatch,

    /// The caller passed an empty name or another rejected argument.
    #[error("E400_INVALID_ARGUMENT: {0}")]
    Invalid(String),

    /// The document or the requested solid cannot support this operation.
    #[error("E422_VALIDATION_FAILED: {0}")]
    ValidationFailed(String),

    /// Semantic CAD API rejected the batch. The document was not modified.
    #[error(transparent)]
    Semantic(#[from] SemanticApiError),

    /// `.kcl` text could not be parsed.
    #[error(transparent)]
    Kcl(#[from] BridgeError),

    /// SQLite container read or write failed.
    #[error(transparent)]
    Container(ContainerError),

    /// Graph evaluation failed before a durable result was recorded.
    #[error("E_EVAL: {0}")]
    Eval(String),

    /// `.aicad-runtime.json` could not be written or parsed.
    #[error("E_DISCOVERY: {0}")]
    Discovery(String),

    /// Filesystem failure outside the path sandbox.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl RuntimeError {
    /// Stable code string, for example `E409_REVISION_CONFLICT`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::RevisionConflict { .. } => "E409_REVISION_CONFLICT",
            Self::PathEscape { .. } => "E_PATH_ESCAPE",
            Self::Cancelled => "E_CANCELLED",
            Self::NothingToUndo => "E_NOTHING_TO_UNDO",
            Self::NothingToRedo => "E_NOTHING_TO_REDO",
            Self::UnknownBranch(_) => "E_UNKNOWN_BRANCH",
            Self::BranchExists(_) => "E_BRANCH_EXISTS",
            Self::ProtectedBranch(_) => "E_PROTECTED_BRANCH",
            Self::PreviewDiverged { .. } => "E409_PREVIEW_DIVERGED",
            Self::PersistMismatch => "E500_PERSIST_MISMATCH",
            Self::Invalid(_) => "E400_INVALID_ARGUMENT",
            Self::ValidationFailed(_) => "E422_VALIDATION_FAILED",
            Self::Semantic(err) => err.code.as_str(),
            Self::Kcl(_) => "E_KCL",
            Self::Container(_) => "E_CONTAINER",
            Self::Eval(_) => "E_EVAL",
            Self::Discovery(_) => "E_DISCOVERY",
            Self::Io(_) => "E_IO",
        }
    }
}

impl From<ContainerError> for RuntimeError {
    fn from(err: ContainerError) -> Self {
        match err {
            ContainerError::UnknownBranch(name) => Self::UnknownBranch(name),
            ContainerError::DuplicateBranch(name) => Self::BranchExists(name),
            ContainerError::CannotDeleteActiveBranch(name) => Self::ProtectedBranch(name),
            other => Self::Container(other),
        }
    }
}

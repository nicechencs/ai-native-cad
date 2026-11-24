//! Structured failures for semantic command compilation and read APIs.
//!
//! A batch that fails leaves the caller's [`cad_ir::CanonicalGraph`] unchanged.
//! `command_index` is the zero-based position inside that batch when the
//! failure is tied to one command.

use serde::{Deserialize, Serialize};

use cad_ir::IrError;

/// Machine-readable failure codes for the semantic CAD API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SemanticErrorCode {
    /// A literal, parameter, or formula mixes incompatible physical dimensions.
    #[serde(rename = "E102_UNIT_MISMATCH")]
    E102UnitMismatch,
    /// The command names a node, parameter, or other id that is not in the graph.
    #[serde(rename = "E404_NODE_NOT_FOUND")]
    E404NodeNotFound,
    /// The command is incomplete, contradictory, or uses an unknown catalog key.
    #[serde(rename = "E400_INVALID_COMMAND")]
    E400InvalidCommand,
    /// The compiled patch does not satisfy graph, formula-DAG, or reference rules.
    #[serde(rename = "E422_VALIDATION_FAILED")]
    E422ValidationFailed,
}

impl SemanticErrorCode {
    /// Stable code string, for example `E404_NODE_NOT_FOUND`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::E102UnitMismatch => "E102_UNIT_MISMATCH",
            Self::E404NodeNotFound => "E404_NODE_NOT_FOUND",
            Self::E400InvalidCommand => "E400_INVALID_COMMAND",
            Self::E422ValidationFailed => "E422_VALIDATION_FAILED",
        }
    }
}

impl std::fmt::Display for SemanticErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One semantic API failure. The original graph is never partially updated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticApiError {
    pub code: SemanticErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_hints: Vec<serde_json::Value>,
}

impl std::fmt::Display for SemanticApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)?;
        if let Some(index) = self.command_index {
            write!(formatter, " (command {index})")?;
        }
        Ok(())
    }
}

impl std::error::Error for SemanticApiError {}

impl SemanticApiError {
    pub fn new(code: SemanticErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            command_index: None,
            recovery_hints: Vec::new(),
        }
    }

    pub fn with_recovery_hints(mut self, hints: Vec<serde_json::Value>) -> Self {
        self.recovery_hints = hints;
        self
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(SemanticErrorCode::E400InvalidCommand, message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(SemanticErrorCode::E404NodeNotFound, message)
    }

    pub(crate) fn unit_mismatch(message: impl Into<String>) -> Self {
        Self::new(SemanticErrorCode::E102UnitMismatch, message)
    }

    pub(crate) fn validation(message: impl Into<String>) -> Self {
        Self::new(SemanticErrorCode::E422ValidationFailed, message)
    }

    pub(crate) fn at(mut self, index: usize) -> Self {
        self.command_index = Some(index);
        self
    }

    /// Map a kernel-independent IR error onto a semantic API code.
    pub fn from_ir(err: IrError) -> Self {
        let code = match &err {
            IrError::UnitMismatch | IrError::FormulaDimensionMismatch { .. } => {
                SemanticErrorCode::E102UnitMismatch
            }
            IrError::InvalidFeatureSpec { message, .. } if is_dimension_conflict(message) => {
                SemanticErrorCode::E102UnitMismatch
            }
            IrError::DanglingNode(_) | IrError::DanglingParameter(_) => {
                SemanticErrorCode::E404NodeNotFound
            }
            IrError::DuplicateId { .. }
            | IrError::EmptyId { .. }
            | IrError::NonFiniteNumber
            | IrError::FormulaParseError { .. }
            | IrError::NonLiteralParameter(_)
            | IrError::InvalidFeatureSpec { .. }
            | IrError::DivisionByZero => SemanticErrorCode::E400InvalidCommand,
            _ => SemanticErrorCode::E422ValidationFailed,
        };
        Self::new(code, err.to_string())
    }
}

fn is_dimension_conflict(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("expects") && lower.contains("got")
}

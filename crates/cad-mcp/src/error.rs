//! Protocol-level failures. Business failures keep the runtime or API code.

use cad_api::SemanticApiError;
use cad_runtime::RuntimeError;
use cad_stdlib::StdLibError;
use cad_topo::{MeasurementError, TopoError};

/// Stable machine code plus the text an agent should read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct GatewayError {
    pub code: &'static str,
    pub message: String,
    pub recovery_hints: Vec<serde_json::Value>,
}

impl GatewayError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recovery_hints: Vec::new(),
        }
    }

    pub fn with_recovery_hints(mut self, hints: Vec<serde_json::Value>) -> Self {
        self.recovery_hints = hints;
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("E400_INVALID_ARGUMENT", message)
    }

    pub fn to_payload(&self) -> serde_json::Value {
        let mut payload = serde_json::json!({
            "code": self.code,
            "message": self.message,
        });
        if !self.recovery_hints.is_empty() {
            payload["recovery_hints"] = serde_json::Value::Array(self.recovery_hints.clone());
        }
        payload
    }
}

impl From<RuntimeError> for GatewayError {
    fn from(err: RuntimeError) -> Self {
        let recovery_hints = match &err {
            RuntimeError::Semantic(sem) => sem.recovery_hints.clone(),
            _ => Vec::new(),
        };
        let code = match err.code() {
            "E409_REVISION_CONFLICT" => "E409_REVISION_CONFLICT",
            "E_PATH_ESCAPE" => "E_PATH_ESCAPE",
            "E_CANCELLED" => "E_CANCELLED",
            "E_NOTHING_TO_UNDO" => "E_NOTHING_TO_UNDO",
            "E_NOTHING_TO_REDO" => "E_NOTHING_TO_REDO",
            "E_UNKNOWN_BRANCH" => "E_UNKNOWN_BRANCH",
            "E_BRANCH_EXISTS" => "E_BRANCH_EXISTS",
            "E_PROTECTED_BRANCH" => "E_PROTECTED_BRANCH",
            "E409_PREVIEW_DIVERGED" => "E409_PREVIEW_DIVERGED",
            "E500_PERSIST_MISMATCH" => "E500_PERSIST_MISMATCH",
            "E400_INVALID_ARGUMENT" => "E400_INVALID_ARGUMENT",
            "E102_UNIT_MISMATCH" => "E102_UNIT_MISMATCH",
            "E404_NODE_NOT_FOUND" => "E404_NODE_NOT_FOUND",
            "E400_INVALID_COMMAND" => "E400_INVALID_COMMAND",
            "E422_VALIDATION_FAILED" => "E422_VALIDATION_FAILED",
            "E_KCL" => "E_KCL",
            "E_CONTAINER" => "E_CONTAINER",
            "E_EVAL" => "E_EVAL",
            "E_DISCOVERY" => "E_DISCOVERY",
            "E_IO" => "E_IO",
            _ => "E_RUNTIME",
        };
        Self::new(code, err.to_string()).with_recovery_hints(recovery_hints)
    }
}

impl From<SemanticApiError> for GatewayError {
    fn from(err: SemanticApiError) -> Self {
        let code = match err.code.as_str() {
            "E102_UNIT_MISMATCH" => "E102_UNIT_MISMATCH",
            "E404_NODE_NOT_FOUND" => "E404_NODE_NOT_FOUND",
            "E400_INVALID_COMMAND" => "E400_INVALID_COMMAND",
            "E422_VALIDATION_FAILED" => "E422_VALIDATION_FAILED",
            _ => "E400_INVALID_COMMAND",
        };
        let message = err.to_string();
        Self::new(code, message).with_recovery_hints(err.recovery_hints)
    }
}

impl From<TopoError> for GatewayError {
    fn from(err: TopoError) -> Self {
        let code = match &err {
            TopoError::AmbiguousMatch { .. } => "E201_AMBIGUOUS_TOPOLOGY",
            TopoError::MissingMatch { .. } => "E202_EMPTY_SELECTOR",
            TopoError::InvalidQuery(_) => "E400_INVALID_ARGUMENT",
        };
        Self::new(code, err.to_string())
    }
}

impl From<MeasurementError> for GatewayError {
    fn from(err: MeasurementError) -> Self {
        let code = match &err {
            MeasurementError::AmbiguousReference { .. } => "E201_AMBIGUOUS_TOPOLOGY",
            MeasurementError::Unresolved { .. } => "E202_EMPTY_SELECTOR",
            MeasurementError::Invalid(_) => "E400_INVALID_ARGUMENT",
            _ => "E400_INVALID_ARGUMENT",
        };
        Self::new(code, err.to_string())
    }
}

impl From<std::io::Error> for GatewayError {
    fn from(err: std::io::Error) -> Self {
        Self::new("E_IO", err.to_string())
    }
}

impl From<StdLibError> for GatewayError {
    fn from(err: StdLibError) -> Self {
        Self::new("E400_INVALID_COMMAND", err.to_string())
    }
}

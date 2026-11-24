//! Kernel-independent CAD graph, dimensional values, formula DAG, structured
//! sketches, strongly-typed feature specs, tri-state verification, and atomic graph edits.

#![forbid(unsafe_code)]

pub mod drawing;
pub mod feature_spec;
pub mod formula;
pub mod graph;
pub mod merkle;
pub mod patch;
pub mod sketch;
pub mod slice;
pub mod tolerances;
pub mod types;
pub mod units;
pub mod validation;
pub mod verification;

mod ports;
mod wire;

pub use drawing::{DrawingDimension, DrawingSheet, DrawingViewKind, DrawingViewSpec};
pub use feature_spec::*;
pub use formula::*;
pub use graph::*;
pub use merkle::compute_node_merkle_hashes;
pub use patch::{GraphPatch, PatchOp};
pub use sketch::*;
pub use slice::{ContextSliceOptions, extract_context_slice};
pub use types::IrType;
pub use units::*;
pub use validation::validate_graph;
pub use verification::*;

/// Arithmetic, graph validation, and persistence errors.
#[derive(Debug, thiserror::Error)]
pub enum IrError {
    #[error("quantity dimensions do not match")]
    UnitMismatch,
    #[error("IR numbers must be finite")]
    NonFiniteNumber,
    #[error("division by zero")]
    DivisionByZero,
    #[error("the graph contains a dependency cycle")]
    CycleDetected,
    #[error("empty {entity} identifier")]
    EmptyId { entity: &'static str },
    #[error("duplicate {entity} identifier `{id}`")]
    DuplicateId { entity: &'static str, id: String },
    #[error("{entity} map key `{key}` does not match identifier `{id}`")]
    IdMismatch {
        entity: &'static str,
        key: String,
        id: String,
    },
    #[error("unknown node `{0}`")]
    DanglingNode(NodeId),
    #[error("unknown blob `{0}`")]
    DanglingBlob(BlobHandle),
    #[error("unknown parameter `{0}`")]
    DanglingParameter(ParamId),
    #[error("unknown external reference `{0}`")]
    DanglingExternalRef(ExternalRefId),
    #[error("invalid SHA-256 hex digest `{sha256}` on {entity} `{id}`")]
    InvalidSha256 {
        entity: &'static str,
        id: String,
        sha256: String,
    },
    #[error("node `{node_id}` has no output port `{port}`")]
    UnknownOutputPort { node_id: NodeId, port: String },
    #[error("node `{node_id}` has no typed input port `{port}`")]
    UnknownInputPort { node_id: NodeId, port: String },
    #[error("node `{node_id}` is missing input `{port}`")]
    MissingInput { node_id: NodeId, port: &'static str },
    #[error("{location} expects {expected:?}, got {actual:?}")]
    TypeMismatch {
        location: String,
        expected: Vec<IrType>,
        actual: Option<IrType>,
    },
    #[error("parameter `{0}` requires a literal value")]
    NonLiteralParameter(ParamId),
    #[error("invalid formula on parameter `{param}`: {message}")]
    FormulaParseError { param: ParamId, message: String },
    #[error("dimensional mismatch in formula on parameter `{param}`: {message}")]
    FormulaDimensionMismatch { param: ParamId, message: String },
    #[error("formula evaluation error on parameter `{param}`: {message}")]
    FormulaEvalError { param: ParamId, message: String },
    #[error("parameter formula cycle detected involving {params:?}")]
    ParameterCycle { params: Vec<ParamId> },
    #[error("invalid feature_spec on node `{node_id}`: {message}")]
    InvalidFeatureSpec { node_id: NodeId, message: String },
    #[error("invalid graph JSON: {0}")]
    Json(#[from] serde_json::Error),
}

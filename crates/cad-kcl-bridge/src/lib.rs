//! Lossless 1:1 bidirectional projection between `CanonicalGraph` (`cad-ir`) and `.kcl 2.0`.
//!
//! `.kcl 2.0` is a formatted view of the canonical graph. Editing that text, parsing it,
//! and diffing against the live graph yields a `GraphPatch` that `apply_patch` can commit.
//! CAD-DSL v0.2 parts, and one assembly of those parts, can also be lowered to the same `CanonicalGraph`.

#![forbid(unsafe_code)]

mod diff;
mod emit;
pub mod eval_bridge;
mod feature_kcl;
mod lower_dsl;
mod model;
mod parse;

pub use diff::diff_graphs;
pub use emit::emit_kcl;
pub use eval_bridge::{
    EVAL_ENGINE_ENV, EvalEngineMode, EvalPath, GraphReadinessReport, KclSourceFlavor,
    bridge_error_diagnostic, detect_source_flavor, diagnostic_error, inspect_graph_eval_readiness,
};
pub use lower_dsl::{lower_dsl_to_graph, parse_any_kcl_to_graph};
pub use parse::parse_kcl_to_graph;

/// Failure while converting between KCL source and a canonical graph.
///
/// `line` and `column` are 1-based. A graph that fails validation before any text
/// exists is reported at line 1, column 1.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BridgeError {
    /// Syntax or graph-validation failure at a source location.
    #[error("{message} at {line}:{column}")]
    At {
        /// 1-based line number.
        line: usize,
        /// 1-based column number.
        column: usize,
        /// What went wrong, without the location suffix.
        message: String,
    },
}

impl BridgeError {
    pub(crate) fn at(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self::At {
            line,
            column,
            message: message.into(),
        }
    }
}

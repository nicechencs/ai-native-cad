//! Read models and the result of an atomic semantic edit.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use cad_ir::{CanonicalGraph, GraphPatch, IrType, IrValue, NodeId, UnitSystem};

use crate::command::DesignContract;

/// Machine-readable recovery hint attached to a node during evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRecoveryHint {
    pub node_id: NodeId,
    pub hint: serde_json::Value,
}

/// Human-readable and machine-structured description of one semantic edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredChange {
    pub action: String,
    pub target: String,
    pub detail: String,
}

/// Summary returned with every successful [`crate::apply_commands`] call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSummary {
    pub title: String,
    pub lines: Vec<String>,
    pub changes: Vec<StructuredChange>,
}

impl ChangeSummary {
    pub(crate) fn from_changes(title: impl Into<String>, changes: Vec<StructuredChange>) -> Self {
        let lines = changes
            .iter()
            .map(|change| format!("{} {}: {}", change.action, change.target, change.detail))
            .collect();
        Self {
            title: title.into(),
            lines,
            changes,
        }
    }
}

/// Graph produced by applying a semantic batch, plus the patch that built it.
///
/// `created_nodes`, `modified_nodes`, and `deleted_nodes` are the net node-id
/// effect of the batch. A node that is removed and re-added (the design
/// contract sentinel) is reported as modified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyCommandsResult {
    pub graph: CanonicalGraph,
    pub patch: GraphPatch,
    pub created_nodes: Vec<NodeId>,
    pub modified_nodes: Vec<NodeId>,
    pub deleted_nodes: Vec<NodeId>,
    pub summary: ChangeSummary,
}

/// One parameter after formula evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterSummary {
    pub name: String,
    pub ir_type: IrType,
    pub value: IrValue,
    pub formula: Option<String>,
}

/// One feature in tree order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureOutlineEntry {
    pub node_id: NodeId,
    pub name: String,
    pub feature: String,
    pub suppressed: bool,
    #[serde(default)]
    pub merkle_hash: String,
}

/// L0 structural check captured by [`crate::inspect_document_summary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationSummary {
    pub valid: bool,
    pub execution_order: Vec<NodeId>,
    pub message: Option<String>,
}

/// Read-only picture of a document: parameters, feature tree, parts, and contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentSummary {
    pub graph_id: String,
    pub units: UnitSystem,
    pub parameters: Vec<ParameterSummary>,
    pub feature_outline: Vec<FeatureOutlineEntry>,
    pub components: Vec<FeatureOutlineEntry>,
    pub outputs: Vec<NodeId>,
    pub validation: ValidationSummary,
    pub design_contract: Option<DesignContract>,
    #[serde(default)]
    pub node_merkle: BTreeMap<String, String>,
}

/// What [`crate::explain_entity`] should describe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExplainTarget {
    Node { node_id: NodeId },
    Parameter { name: String },
}

/// One step in a parameter formula dependency chain. Dependencies come first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormulaLink {
    pub name: String,
    pub ir_type: IrType,
    pub formula: Option<String>,
    pub value: IrValue,
    pub depends_on: Vec<String>,
}

/// Why a node or parameter exists, and which topology refs it carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityExplanation {
    pub id: String,
    pub display_name: String,
    pub feature_node: Option<NodeId>,
    pub parameter_chain: Vec<FormulaLink>,
    pub upstream_inputs: Vec<NodeId>,
    pub topo_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdlib_resolution: Option<String>,
}

/// Focus and depth for [`crate::slice_document_context`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSliceRequest {
    pub focus_nodes: Vec<NodeId>,
    #[serde(default)]
    pub upstream_depth: usize,
    #[serde(default)]
    pub downstream_depth: usize,
    #[serde(default = "default_true")]
    pub ensure_valid_closure: bool,
}

const fn default_true() -> bool {
    true
}

/// Subgraph returned by [`crate::slice_document_context`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSliceResult {
    pub graph: CanonicalGraph,
    pub kept_nodes: Vec<NodeId>,
    pub kept_parameters: Vec<String>,
}

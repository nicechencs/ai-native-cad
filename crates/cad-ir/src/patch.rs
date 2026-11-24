use std::collections::{BTreeMap, btree_map::Entry};

use serde::{Deserialize, Serialize};

use crate::{
    BlobHandle, BlobManifestEntry, CanonicalGraph, ExternalRefDef, ExternalRefId, IrError, IrNode,
    IrValue, NodeId, NodeKind, NodeState, ParamId, ParameterDef, validate_graph,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPatch {
    pub patch_id: String,
    pub description: String,
    pub ops: Vec<PatchOp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PatchOp {
    UpsertParameter(ParameterDef),
    RemoveParameter {
        name: String,
    },
    UpsertBlob(BlobManifestEntry),
    RemoveBlob {
        handle: String,
    },
    UpsertExternalRef(ExternalRefDef),
    RemoveExternalRef {
        ref_id: String,
    },
    AddNode(IrNode),
    UpdateNode {
        id: NodeId,
        name: Option<String>,
        inputs: Option<BTreeMap<String, IrValue>>,
        kind: Option<NodeKind>,
    },
    RemoveNode {
        id: NodeId,
    },
    SetNodeState {
        id: NodeId,
        state: NodeState,
    },
    SetOutputs(Vec<NodeId>),
}

impl CanonicalGraph {
    /// Apply operations in order, recompute derived parameter formulas, and
    /// commit only if the final graph is valid.
    pub fn apply_patch(&mut self, patch: &GraphPatch) -> Result<(), IrError> {
        let mut staged = self.clone();
        for operation in &patch.ops {
            staged.apply_operation(operation)?;
        }
        staged.evaluate_formulas()?;
        validate_graph(&staged)?;
        *self = staged;
        Ok(())
    }

    fn apply_operation(&mut self, operation: &PatchOp) -> Result<(), IrError> {
        match operation {
            PatchOp::UpsertParameter(parameter) => {
                self.parameters
                    .insert(parameter.name.0.clone(), parameter.clone());
            }
            PatchOp::RemoveParameter { name } => {
                self.parameters
                    .remove(name)
                    .ok_or_else(|| IrError::DanglingParameter(ParamId(name.clone())))?;
            }
            PatchOp::UpsertBlob(blob) => {
                self.blobs.insert(blob.handle.0.clone(), blob.clone());
            }
            PatchOp::RemoveBlob { handle } => {
                self.blobs
                    .remove(handle)
                    .ok_or_else(|| IrError::DanglingBlob(BlobHandle(handle.clone())))?;
            }
            PatchOp::UpsertExternalRef(external_ref) => {
                self.external_refs
                    .insert(external_ref.ref_id.0.clone(), external_ref.clone());
            }
            PatchOp::RemoveExternalRef { ref_id } => {
                self.external_refs
                    .remove(ref_id)
                    .ok_or_else(|| IrError::DanglingExternalRef(ExternalRefId(ref_id.clone())))?;
            }
            PatchOp::AddNode(node) => match self.nodes.entry(node.id.0.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(node.clone());
                }
                Entry::Occupied(entry) => {
                    return Err(IrError::DuplicateId {
                        entity: "node",
                        id: entry.key().clone(),
                    });
                }
            },
            PatchOp::UpdateNode {
                id,
                name,
                inputs,
                kind,
            } => {
                let node = self.node_mut(id)?;
                if let Some(name) = name {
                    node.name.clone_from(name);
                }
                if let Some(inputs) = inputs {
                    node.inputs.clone_from(inputs);
                }
                if let Some(kind) = kind {
                    node.kind.clone_from(kind);
                }
            }
            PatchOp::RemoveNode { id } => {
                self.nodes
                    .remove(&id.0)
                    .ok_or_else(|| IrError::DanglingNode(id.clone()))?;
            }
            PatchOp::SetNodeState { id, state } => self.node_mut(id)?.state.clone_from(state),
            PatchOp::SetOutputs(outputs) => self.outputs.clone_from(outputs),
        }
        Ok(())
    }

    fn node_mut(&mut self, id: &NodeId) -> Result<&mut IrNode, IrError> {
        self.nodes
            .get_mut(&id.0)
            .ok_or_else(|| IrError::DanglingNode(id.clone()))
    }
}

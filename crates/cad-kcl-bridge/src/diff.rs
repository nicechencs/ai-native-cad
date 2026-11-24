//! Minimal deterministic `GraphPatch` between two canonical graphs.
//!
//! `GraphPatch` can change parameters, blobs, external references, nodes, node
//! state, and outputs. It cannot change `graph_id`, `units`, or
//! `ir_schema_version`. Those header fields stay on the graph the patch is
//! applied to.

use std::collections::BTreeSet;

use cad_ir::{CanonicalGraph, GraphPatch, IrNode, NodeId, PatchOp};

/// Build a patch whose application turns `before` into `after`.
///
/// Operations are ordered so replacements stay valid: removals come first,
/// additions next, in-place edits after that, and `SetOutputs` last.
/// Two equal graphs produce an empty operation list. Map keys are visited in
/// sorted order, so equal pairs always yield the same patch.
pub fn diff_graphs(before: &CanonicalGraph, after: &CanonicalGraph, patch_id: &str) -> GraphPatch {
    let mut ops = Vec::new();
    diff_parameters(before, after, &mut ops);
    diff_blobs(before, after, &mut ops);
    diff_external_refs(before, after, &mut ops);
    diff_nodes(before, after, &mut ops);
    if before.outputs != after.outputs {
        ops.push(PatchOp::SetOutputs(after.outputs.clone()));
    }
    GraphPatch {
        patch_id: patch_id.to_owned(),
        description: "kcl 2.0 projection diff".to_owned(),
        ops,
    }
}

fn diff_parameters(before: &CanonicalGraph, after: &CanonicalGraph, ops: &mut Vec<PatchOp>) {
    for name in before.parameters.keys() {
        if !after.parameters.contains_key(name) {
            ops.push(PatchOp::RemoveParameter { name: name.clone() });
        }
    }
    for (name, parameter) in &after.parameters {
        if before.parameters.get(name) != Some(parameter) {
            ops.push(PatchOp::UpsertParameter(parameter.clone()));
        }
    }
}

fn diff_blobs(before: &CanonicalGraph, after: &CanonicalGraph, ops: &mut Vec<PatchOp>) {
    for handle in before.blobs.keys() {
        if !after.blobs.contains_key(handle) {
            ops.push(PatchOp::RemoveBlob {
                handle: handle.clone(),
            });
        }
    }
    for (handle, blob) in &after.blobs {
        if before.blobs.get(handle) != Some(blob) {
            ops.push(PatchOp::UpsertBlob(blob.clone()));
        }
    }
}

fn diff_external_refs(before: &CanonicalGraph, after: &CanonicalGraph, ops: &mut Vec<PatchOp>) {
    for ref_id in before.external_refs.keys() {
        if !after.external_refs.contains_key(ref_id) {
            ops.push(PatchOp::RemoveExternalRef {
                ref_id: ref_id.clone(),
            });
        }
    }
    for (ref_id, external_ref) in &after.external_refs {
        if before.external_refs.get(ref_id) != Some(external_ref) {
            ops.push(PatchOp::UpsertExternalRef(external_ref.clone()));
        }
    }
}

fn diff_nodes(before: &CanonicalGraph, after: &CanonicalGraph, ops: &mut Vec<PatchOp>) {
    let mut remove: BTreeSet<String> = BTreeSet::new();
    let mut add: Vec<&IrNode> = Vec::new();
    let mut updates = Vec::new();
    let mut states = Vec::new();

    for id in before.nodes.keys() {
        if !after.nodes.contains_key(id) {
            remove.insert(id.clone());
        }
    }

    for (id, after_node) in &after.nodes {
        match before.nodes.get(id) {
            None => add.push(after_node),
            Some(before_node) if replaces(before_node, after_node) => {
                remove.insert(id.clone());
                add.push(after_node);
            }
            Some(before_node) => {
                let name = (before_node.name != after_node.name).then(|| after_node.name.clone());
                let inputs =
                    (before_node.inputs != after_node.inputs).then(|| after_node.inputs.clone());
                let kind = (before_node.kind != after_node.kind).then(|| after_node.kind.clone());
                if name.is_some() || inputs.is_some() || kind.is_some() {
                    updates.push(PatchOp::UpdateNode {
                        id: after_node.id.clone(),
                        name,
                        inputs,
                        kind,
                    });
                }
                if before_node.state != after_node.state {
                    states.push(PatchOp::SetNodeState {
                        id: after_node.id.clone(),
                        state: after_node.state.clone(),
                    });
                }
            }
        }
    }

    for id in remove {
        ops.push(PatchOp::RemoveNode {
            id: NodeId::from(id),
        });
    }
    for node in add {
        ops.push(PatchOp::AddNode(node.clone()));
    }
    ops.extend(updates);
    ops.extend(states);
}

/// `UpdateNode` cannot change output type, kernel version, or metadata.
fn replaces(before: &IrNode, after: &IrNode) -> bool {
    before.output_type != after.output_type
        || before.kernel_op_version != after.kernel_op_version
        || before.metadata != after.metadata
}

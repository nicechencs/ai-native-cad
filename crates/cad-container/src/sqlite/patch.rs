use std::{collections::BTreeMap, path::Path};

use cad_ir::{
    BlobHandle, CanonicalGraph, ExternalRefId, GraphPatch, IrError, NodeId, ParamId, PatchOp,
    compute_node_merkle_hashes, validate_graph,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    ContainerError,
    blobs::{checked_blob_path, compute_sha256_hex},
};

use super::{
    OplogActor, OplogEntry, SqliteContainer,
    blobs::blob_kind_str,
    branches::ensure_branch_exists,
    project_io::{
        load_branch_graph_from_conn, upsert_external_ref_row, upsert_node_row,
        upsert_parameter_row, verification_reports_table_exists,
    },
    unix_now_ms,
};

/// Apply an incremental `GraphPatch` directly in-place on a SQLite `.aicad` container inside one ACID transaction.
pub fn apply_patch_in_place(
    path: &Path,
    patch: &GraphPatch,
    branch_opt: Option<&str>,
) -> Result<OplogEntry, ContainerError> {
    let mut container = SqliteContainer::open(path)?;
    container.apply_patch(patch, branch_opt)
}

impl SqliteContainer {
    /// Apply an atomic `GraphPatch` incrementally inside a single SQLite transaction,
    /// appending an `oplog` record with exact inverse patch and advancing `head_seq`.
    pub fn apply_patch(
        &mut self,
        patch: &GraphPatch,
        branch_opt: Option<&str>,
    ) -> Result<OplogEntry, ContainerError> {
        self.apply_patch_with_actor(patch, branch_opt, OplogActor::HumanGui)
    }

    /// Apply an atomic `GraphPatch` incrementally with an explicit provenance actor.
    pub fn apply_patch_with_actor(
        &mut self,
        patch: &GraphPatch,
        branch_opt: Option<&str>,
        actor: OplogActor,
    ) -> Result<OplogEntry, ContainerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let branch_name = match branch_opt {
            Some(name) => {
                ensure_branch_exists(&tx, name)?;
                name.to_owned()
            }
            None => tx.query_row(
                "SELECT active_branch FROM container_meta WHERE singleton_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )?,
        };

        let old_head_seq: i64 = tx.query_row(
            "SELECT head_seq FROM branches WHERE branch_name = ?1",
            params![branch_name],
            |row| row.get(0),
        )?;

        let current_graph = load_branch_graph_from_conn(&tx, &branch_name)?;
        let old_merkle_hashes = load_branch_merkle_map(&tx, &branch_name)?;

        let (staged_graph, inverse_patch) = stage_patch_with_inverse(&current_graph, patch)?;
        let new_merkle_hashes = compute_node_merkle_hashes(&staged_graph)?;

        apply_incremental_sql_delta(
            &tx,
            &branch_name,
            patch,
            &current_graph,
            &staged_graph,
            &old_merkle_hashes,
            &new_merkle_hashes,
        )?;

        let outputs_merkle = compute_outputs_merkle(&staged_graph, &new_merkle_hashes);
        let now_ms = unix_now_ms();
        let patch_json = serde_json::to_string(patch)?;
        let inverse_patch_json = serde_json::to_string(&inverse_patch)?;

        let parent_seq = (old_head_seq > 0).then_some(old_head_seq);
        tx.execute(
            "INSERT INTO oplog (
                branch_name, parent_seq, patch_id, actor, description,
                patch_json, inverse_patch_json, outputs_merkle, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                branch_name,
                parent_seq,
                patch.patch_id,
                actor.as_str(),
                patch.description,
                patch_json,
                inverse_patch_json,
                outputs_merkle,
                now_ms,
            ],
        )?;
        let new_seq = tx.last_insert_rowid();

        tx.execute(
            "UPDATE branches SET head_seq = ?2 WHERE branch_name = ?1",
            params![branch_name, new_seq],
        )?;
        // Invalidate any stale persisted verification report on this branch once its graph changes.
        if verification_reports_table_exists(&tx)? {
            tx.execute(
                "DELETE FROM verification_reports WHERE branch_name = ?1",
                params![branch_name],
            )?;
        }
        tx.execute(
            "UPDATE container_meta SET updated_at_ms = ?1 WHERE singleton_id = 1",
            params![now_ms],
        )?;

        tx.commit()?;

        Ok(OplogEntry {
            seq: new_seq,
            branch_name,
            parent_seq,
            patch_id: patch.patch_id.clone(),
            actor,
            description: patch.description.clone(),
            patch: patch.clone(),
            outputs_merkle,
            created_at_ms: now_ms,
        })
    }

    /// Roll the active branch back to `target_seq` by applying stored inverse patches atomically.
    pub fn rollback_to_seq(&mut self, target_seq: i64) -> Result<CanonicalGraph, ContainerError> {
        let active = self.active_branch()?;
        self.rollback_branch_to_seq(&active, target_seq)
    }

    /// Roll `branch_name` back to `target_seq` by applying stored inverse patches atomically.
    pub fn rollback_branch_to_seq(
        &mut self,
        branch_name: &str,
        target_seq: i64,
    ) -> Result<CanonicalGraph, ContainerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (head_seq, base_seq): (i64, Option<i64>) = tx
            .query_row(
                "SELECT head_seq, base_seq FROM branches WHERE branch_name = ?1",
                params![branch_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| ContainerError::UnknownBranch(branch_name.to_owned()))?;

        let floor_seq = base_seq.unwrap_or(0);
        if target_seq < floor_seq || target_seq > head_seq {
            return Err(ContainerError::InvalidRollbackTarget {
                branch: branch_name.to_owned(),
                target_seq,
                head_seq,
            });
        }
        if target_seq == head_seq {
            let graph = load_branch_graph_from_conn(&tx, branch_name)?;
            tx.commit()?;
            return Ok(graph);
        }

        if target_seq > floor_seq {
            let target_exists: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM oplog WHERE branch_name = ?1 AND seq = ?2",
                    params![branch_name, target_seq],
                    |row| row.get(0),
                )
                .optional()?;
            if target_exists.is_none() {
                return Err(ContainerError::InvalidRollbackTarget {
                    branch: branch_name.to_owned(),
                    target_seq,
                    head_seq,
                });
            }
        }

        let entries_to_undo: Vec<(i64, Option<i64>, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT seq, parent_seq, inverse_patch_json
                 FROM oplog
                 WHERE branch_name = ?1 AND seq > ?2
                 ORDER BY seq DESC",
            )?;
            let rows =
                stmt.query_map(params![branch_name, target_seq], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        if entries_to_undo.is_empty() {
            return Err(ContainerError::InvalidRollbackTarget {
                branch: branch_name.to_owned(),
                target_seq,
                head_seq,
            });
        }

        // Verify the undo chain reaches `target_seq` without any vacuum-pruned gap.
        let mut expected_seq = head_seq;
        for (seq, parent_seq, inv_opt) in &entries_to_undo {
            if *seq != expected_seq || inv_opt.is_none() {
                return Err(ContainerError::InvalidRollbackTarget {
                    branch: branch_name.to_owned(),
                    target_seq,
                    head_seq,
                });
            }
            expected_seq = parent_seq.unwrap_or(0);
        }
        if expected_seq != target_seq {
            return Err(ContainerError::InvalidRollbackTarget {
                branch: branch_name.to_owned(),
                target_seq,
                head_seq,
            });
        }

        let mut graph = load_branch_graph_from_conn(&tx, branch_name)?;
        let mut merkle_hashes = load_branch_merkle_map(&tx, branch_name)?;

        for (_, _, inv_opt) in &entries_to_undo {
            let inv_json = inv_opt.as_deref().expect("checked non-empty inverse_patch_json");
            let inverse_patch: GraphPatch = serde_json::from_str(inv_json)?;
            let prev_graph = graph.clone();
            graph.apply_patch(&inverse_patch)?;
            let next_merkle = compute_node_merkle_hashes(&graph)?;
            apply_incremental_sql_delta(
                &tx,
                branch_name,
                &inverse_patch,
                &prev_graph,
                &graph,
                &merkle_hashes,
                &next_merkle,
            )?;
            merkle_hashes = next_merkle;
        }

        tx.execute(
            "DELETE FROM oplog WHERE branch_name = ?1 AND seq > ?2",
            params![branch_name, target_seq],
        )?;
        tx.execute(
            "UPDATE branches SET head_seq = ?2 WHERE branch_name = ?1",
            params![branch_name, target_seq],
        )?;
        // Invalidate any stale persisted verification report on this branch after rollback.
        if verification_reports_table_exists(&tx)? {
            tx.execute(
                "DELETE FROM verification_reports WHERE branch_name = ?1",
                params![branch_name],
            )?;
        }
        tx.execute(
            "UPDATE container_meta SET updated_at_ms = ?1 WHERE singleton_id = 1",
            params![unix_now_ms()],
        )?;

        tx.commit()?;
        Ok(graph)
    }
}

fn stage_patch_with_inverse(
    original: &CanonicalGraph,
    patch: &GraphPatch,
) -> Result<(CanonicalGraph, GraphPatch), ContainerError> {
    let mut staged = original.clone();
    let mut undo_ops_rev = Vec::with_capacity(patch.ops.len());

    for op in &patch.ops {
        let inverse_op = match op {
            PatchOp::UpsertParameter(param) => match staged.parameters.get(&param.name.0) {
                Some(prev) => PatchOp::UpsertParameter(prev.clone()),
                None => PatchOp::RemoveParameter {
                    name: param.name.0.clone(),
                },
            },
            PatchOp::RemoveParameter { name } => {
                let prev = staged
                    .parameters
                    .get(name)
                    .ok_or_else(|| IrError::DanglingParameter(ParamId(name.clone())))?
                    .clone();
                PatchOp::UpsertParameter(prev)
            }
            PatchOp::UpsertBlob(blob) => match staged.blobs.get(&blob.handle.0) {
                Some(prev) => PatchOp::UpsertBlob(prev.clone()),
                None => PatchOp::RemoveBlob {
                    handle: blob.handle.0.clone(),
                },
            },
            PatchOp::RemoveBlob { handle } => {
                let prev = staged
                    .blobs
                    .get(handle)
                    .ok_or_else(|| IrError::DanglingBlob(BlobHandle(handle.clone())))?
                    .clone();
                PatchOp::UpsertBlob(prev)
            }
            PatchOp::UpsertExternalRef(ext) => match staged.external_refs.get(&ext.ref_id.0) {
                Some(prev) => PatchOp::UpsertExternalRef(prev.clone()),
                None => PatchOp::RemoveExternalRef {
                    ref_id: ext.ref_id.0.clone(),
                },
            },
            PatchOp::RemoveExternalRef { ref_id } => {
                let prev = staged
                    .external_refs
                    .get(ref_id)
                    .ok_or_else(|| IrError::DanglingExternalRef(ExternalRefId(ref_id.clone())))?
                    .clone();
                PatchOp::UpsertExternalRef(prev)
            }
            PatchOp::AddNode(node) => {
                if staged.nodes.contains_key(&node.id.0) {
                    return Err(ContainerError::Ir(IrError::DuplicateId {
                        entity: "node",
                        id: node.id.0.clone(),
                    }));
                }
                PatchOp::RemoveNode {
                    id: node.id.clone(),
                }
            }
            PatchOp::UpdateNode { id, .. } => {
                let prev = staged
                    .nodes
                    .get(&id.0)
                    .ok_or_else(|| IrError::DanglingNode(id.clone()))?;
                PatchOp::UpdateNode {
                    id: id.clone(),
                    name: Some(prev.name.clone()),
                    inputs: Some(prev.inputs.clone()),
                    kind: Some(prev.kind.clone()),
                }
            }
            PatchOp::RemoveNode { id } => {
                let prev = staged
                    .nodes
                    .get(&id.0)
                    .ok_or_else(|| IrError::DanglingNode(id.clone()))?
                    .clone();
                PatchOp::AddNode(prev)
            }
            PatchOp::SetNodeState { id, .. } => {
                let prev = staged
                    .nodes
                    .get(&id.0)
                    .ok_or_else(|| IrError::DanglingNode(id.clone()))?;
                PatchOp::SetNodeState {
                    id: id.clone(),
                    state: prev.state.clone(),
                }
            }
            PatchOp::SetOutputs(_) => PatchOp::SetOutputs(staged.outputs.clone()),
        };

        apply_single_op_unchecked(&mut staged, op)?;
        undo_ops_rev.push(inverse_op);
    }

    staged.evaluate_formulas()?;
    validate_graph(&staged)?;
    undo_ops_rev.reverse();

    let inverse_patch = GraphPatch {
        patch_id: format!("inverse:{}", patch.patch_id),
        description: format!("Undo {}", patch.description),
        ops: undo_ops_rev,
    };
    Ok((staged, inverse_patch))
}

fn apply_single_op_unchecked(staged: &mut CanonicalGraph, op: &PatchOp) -> Result<(), IrError> {
    match op {
        PatchOp::UpsertParameter(param) => {
            staged
                .parameters
                .insert(param.name.0.clone(), param.clone());
        }
        PatchOp::RemoveParameter { name } => {
            staged
                .parameters
                .remove(name)
                .ok_or_else(|| IrError::DanglingParameter(ParamId(name.clone())))?;
        }
        PatchOp::UpsertBlob(blob) => {
            staged.blobs.insert(blob.handle.0.clone(), blob.clone());
        }
        PatchOp::RemoveBlob { handle } => {
            staged
                .blobs
                .remove(handle)
                .ok_or_else(|| IrError::DanglingBlob(BlobHandle(handle.clone())))?;
        }
        PatchOp::UpsertExternalRef(ext) => {
            staged
                .external_refs
                .insert(ext.ref_id.0.clone(), ext.clone());
        }
        PatchOp::RemoveExternalRef { ref_id } => {
            staged
                .external_refs
                .remove(ref_id)
                .ok_or_else(|| IrError::DanglingExternalRef(ExternalRefId(ref_id.clone())))?;
        }
        PatchOp::AddNode(node) => {
            if staged.nodes.contains_key(&node.id.0) {
                return Err(IrError::DuplicateId {
                    entity: "node",
                    id: node.id.0.clone(),
                });
            }
            staged.nodes.insert(node.id.0.clone(), node.clone());
        }
        PatchOp::UpdateNode {
            id,
            name,
            inputs,
            kind,
        } => {
            let node = staged
                .nodes
                .get_mut(&id.0)
                .ok_or_else(|| IrError::DanglingNode(id.clone()))?;
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
            staged
                .nodes
                .remove(&id.0)
                .ok_or_else(|| IrError::DanglingNode(id.clone()))?;
        }
        PatchOp::SetNodeState { id, state } => {
            staged
                .nodes
                .get_mut(&id.0)
                .ok_or_else(|| IrError::DanglingNode(id.clone()))?
                .state
                .clone_from(state);
        }
        PatchOp::SetOutputs(outputs) => staged.outputs.clone_from(outputs),
    }
    Ok(())
}

fn apply_incremental_sql_delta(
    conn: &Connection,
    branch_name: &str,
    patch: &GraphPatch,
    previous_graph: &CanonicalGraph,
    staged_graph: &CanonicalGraph,
    old_merkle_hashes: &BTreeMap<NodeId, String>,
    new_merkle_hashes: &BTreeMap<NodeId, String>,
) -> Result<(), ContainerError> {
    let mut touched_params = BTreeMap::<String, bool>::new();
    let mut touched_blobs = BTreeMap::<String, bool>::new();
    let mut touched_ext_refs = BTreeMap::<String, bool>::new();
    let mut touched_nodes = BTreeMap::<String, bool>::new();
    let mut outputs_changed = false;

    for op in &patch.ops {
        match op {
            PatchOp::UpsertParameter(p) => {
                touched_params.insert(p.name.0.clone(), true);
            }
            PatchOp::RemoveParameter { name } => {
                touched_params.insert(name.clone(), false);
            }
            PatchOp::UpsertBlob(b) => {
                touched_blobs.insert(b.handle.0.clone(), true);
            }
            PatchOp::RemoveBlob { handle } => {
                touched_blobs.insert(handle.clone(), false);
            }
            PatchOp::UpsertExternalRef(r) => {
                touched_ext_refs.insert(r.ref_id.0.clone(), true);
            }
            PatchOp::RemoveExternalRef { ref_id } => {
                touched_ext_refs.insert(ref_id.clone(), false);
            }
            PatchOp::AddNode(n) => {
                touched_nodes.insert(n.id.0.clone(), true);
            }
            PatchOp::UpdateNode { id, .. } | PatchOp::SetNodeState { id, .. } => {
                touched_nodes.insert(id.0.clone(), true);
            }
            PatchOp::RemoveNode { id } => {
                touched_nodes.insert(id.0.clone(), false);
            }
            PatchOp::SetOutputs(_) => {
                outputs_changed = true;
            }
        }
    }

    // Persist any formula-derived parameters whose recomputed values changed.
    for (name, param) in &staged_graph.parameters {
        if previous_graph.parameters.get(name) != Some(param) {
            touched_params.insert(name.clone(), true);
        }
    }

    for (name, present) in touched_params {
        if present {
            let param = &staged_graph.parameters[&name];
            upsert_parameter_row(conn, branch_name, param)?;
        } else {
            conn.execute(
                "DELETE FROM graph_parameters WHERE branch_name = ?1 AND param_name = ?2",
                params![branch_name, name],
            )?;
        }
    }

    for (handle, present) in touched_blobs {
        if present {
            let entry = &staged_graph.blobs[&handle];
            checked_blob_path(&entry.relative_path)?;
            let exists_in_store: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM blobs WHERE sha256 = ?1",
                    params![entry.sha256],
                    |r| r.get(0),
                )
                .optional()?;
            if exists_in_store.is_none() {
                return Err(ContainerError::MissingBlob(entry.relative_path.clone()));
            }
            conn.execute(
                "INSERT INTO graph_blobs (branch_name, handle, blob_kind, sha256, byte_len, relative_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(branch_name, handle) DO UPDATE SET
                     blob_kind = excluded.blob_kind,
                     sha256 = excluded.sha256,
                     byte_len = excluded.byte_len,
                     relative_path = excluded.relative_path",
                params![
                    branch_name,
                    entry.handle.0,
                    blob_kind_str(entry.kind),
                    entry.sha256,
                    entry.byte_len as i64,
                    entry.relative_path,
                ],
            )?;
        } else {
            conn.execute(
                "DELETE FROM graph_blobs WHERE branch_name = ?1 AND handle = ?2",
                params![branch_name, handle],
            )?;
        }
    }

    for (ref_id, present) in touched_ext_refs {
        if present {
            let ext = &staged_graph.external_refs[&ref_id];
            upsert_external_ref_row(conn, branch_name, ext)?;
        } else {
            conn.execute(
                "DELETE FROM graph_external_refs WHERE branch_name = ?1 AND ref_id = ?2",
                params![branch_name, ref_id],
            )?;
        }
    }

    // 1. Upsert added/updated nodes first so `SetOutputs` foreign keys succeed.
    for (node_id, present) in &touched_nodes {
        if *present {
            let node = &staged_graph.nodes[node_id];
            let merkle = &new_merkle_hashes[&node.id];
            upsert_node_row(conn, branch_name, node, merkle)?;
        }
    }

    // 2. Refresh outputs before deleting removed nodes so `graph_outputs` never dangles.
    if outputs_changed {
        conn.execute(
            "DELETE FROM graph_outputs WHERE branch_name = ?1",
            params![branch_name],
        )?;
        for (ordinal, out_id) in staged_graph.outputs.iter().enumerate() {
            conn.execute(
                "INSERT INTO graph_outputs (branch_name, ordinal, node_id) VALUES (?1, ?2, ?3)",
                params![branch_name, ordinal as i64, out_id.0],
            )?;
        }
    }

    // 3. Delete removed nodes.
    for (node_id, present) in &touched_nodes {
        if !*present {
            conn.execute(
                "DELETE FROM graph_nodes WHERE branch_name = ?1 AND node_id = ?2",
                params![branch_name, node_id],
            )?;
        }
    }

    // 4. Update `merkle_hash` only on downstream surviving nodes whose Merkle hash actually changed.
    for (node_id, new_hash) in new_merkle_hashes {
        if touched_nodes.get(&node_id.0) == Some(&true) {
            continue;
        }
        if old_merkle_hashes.get(node_id) != Some(new_hash) {
            conn.execute(
                "UPDATE graph_nodes SET merkle_hash = ?3 WHERE branch_name = ?1 AND node_id = ?2",
                params![branch_name, node_id.0, new_hash],
            )?;
        }
    }

    Ok(())
}

fn load_branch_merkle_map(
    conn: &Connection,
    branch_name: &str,
) -> Result<BTreeMap<NodeId, String>, ContainerError> {
    let mut map = BTreeMap::new();
    let mut stmt = conn
        .prepare("SELECT node_id, merkle_hash FROM graph_nodes WHERE branch_name = ?1")?;
    let rows = stmt.query_map(params![branch_name], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, hash) = row?;
        map.insert(NodeId(id), hash);
    }
    Ok(map)
}

fn compute_outputs_merkle(
    graph: &CanonicalGraph,
    merkle_hashes: &BTreeMap<NodeId, String>,
) -> String {
    let mut joined = String::new();
    for (idx, out_id) in graph.outputs.iter().enumerate() {
        if idx > 0 {
            joined.push(':');
        }
        if let Some(h) = merkle_hashes.get(out_id) {
            joined.push_str(h);
        }
    }
    compute_sha256_hex(joined.as_bytes())
}

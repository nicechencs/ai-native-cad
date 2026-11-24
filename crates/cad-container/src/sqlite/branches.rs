use cad_ir::{CanonicalGraph, IrError};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::ContainerError;

use super::{
    BranchInfo, BranchPurpose, BranchStatus, SqliteContainer,
    project_io::{load_branch_graph_from_conn, verification_reports_table_exists},
    unix_now_ms,
};

impl SqliteContainer {
    /// Return the currently checked-out active branch name.
    pub fn active_branch(&self) -> Result<String, ContainerError> {
        self.conn
            .query_row(
                "SELECT active_branch FROM container_meta WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| ContainerError::MissingEntry("container_meta".to_owned()))
    }

    /// List all lightweight branches in the container.
    pub fn list_branches(&self) -> Result<Vec<BranchInfo>, ContainerError> {
        let active = self.active_branch()?;
        let mut stmt = self.conn.prepare(
            "SELECT branch_name, head_seq, base_branch, base_seq, purpose, status, gate_score, created_at_ms
             FROM branches
             ORDER BY created_at_ms ASC, branch_name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let name: String = row.get(0)?;
            let purpose_str: String = row.get(4)?;
            let status_str: String = row.get(5)?;
            Ok(BranchInfo {
                is_active: name == active,
                branch_name: name,
                head_seq: row.get(1)?,
                base_branch: row.get(2)?,
                base_seq: row.get(3)?,
                purpose: BranchPurpose::parse(&purpose_str),
                status: BranchStatus::parse(&status_str),
                gate_score: row.get(6)?,
                created_at_ms: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Create a lightweight branch forked from `from_branch_opt` (or the active branch) without copying blob bytes.
    pub fn create_branch(
        &mut self,
        branch_name: &str,
        from_branch_opt: Option<&str>,
    ) -> Result<BranchInfo, ContainerError> {
        let purpose = if branch_name.starts_with("ai/") || branch_name.starts_with("cand") {
            BranchPurpose::AiCandidate
        } else {
            BranchPurpose::User
        };
        self.create_branch_with_purpose(branch_name, from_branch_opt, purpose)
    }

    /// Create a lightweight branch with an explicit `BranchPurpose`.
    pub fn create_branch_with_purpose(
        &mut self,
        branch_name: &str,
        from_branch_opt: Option<&str>,
        purpose: BranchPurpose,
    ) -> Result<BranchInfo, ContainerError> {
        if branch_name.trim().is_empty() {
            return Err(ContainerError::Ir(IrError::EmptyId { entity: "branch" }));
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: String = tx.query_row(
            "SELECT active_branch FROM container_meta WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )?;
        let source_branch = from_branch_opt.unwrap_or(active.as_str());
        ensure_branch_exists(&tx, source_branch)?;

        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM branches WHERE branch_name = ?1",
                params![branch_name],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(ContainerError::DuplicateBranch(branch_name.to_owned()));
        }

        let (src_head_seq, graph_id, length_unit, angle_unit, units_json): (
            i64,
            String,
            String,
            String,
            String,
        ) = tx.query_row(
            "SELECT head_seq, graph_id, length_unit, angle_unit, units_json
             FROM branches WHERE branch_name = ?1",
            params![source_branch],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let now_ms = unix_now_ms();

        tx.execute(
            "INSERT INTO branches (
                branch_name, head_seq, base_branch, base_seq, purpose, status,
                gate_score, graph_id, length_unit, angle_unit, units_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'active', NULL, ?6, ?7, ?8, ?9, ?10)",
            params![
                branch_name,
                src_head_seq,
                source_branch,
                src_head_seq,
                purpose.as_str(),
                graph_id,
                length_unit,
                angle_unit,
                units_json,
                now_ms,
            ],
        )?;

        tx.execute(
            "INSERT INTO graph_parameters (branch_name, param_name, ir_type, value_json, formula)
             SELECT ?1, param_name, ir_type, value_json, formula
             FROM graph_parameters WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        tx.execute(
            "INSERT INTO graph_nodes (
                branch_name, node_id, name, kind_json, inputs_json, output_type,
                state_json, kernel_op_version, metadata_json, merkle_hash
             )
             SELECT ?1, node_id, name, kind_json, inputs_json, output_type,
                    state_json, kernel_op_version, metadata_json, merkle_hash
             FROM graph_nodes WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        tx.execute(
            "INSERT INTO graph_outputs (branch_name, ordinal, node_id)
             SELECT ?1, ordinal, node_id FROM graph_outputs WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        tx.execute(
            "INSERT INTO graph_external_refs (
                branch_name, ref_id, container_path, expected_sha256,
                target_graph_id, payload_policy, overrides_json
             )
             SELECT ?1, ref_id, container_path, expected_sha256,
                    target_graph_id, payload_policy, overrides_json
             FROM graph_external_refs WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        tx.execute(
            "INSERT INTO graph_blobs (branch_name, handle, blob_kind, sha256, byte_len, relative_path)
             SELECT ?1, handle, blob_kind, sha256, byte_len, relative_path
             FROM graph_blobs WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        tx.execute(
            "INSERT INTO engineering_meta (branch_name, category, ordinal, record_key, topo_ref_json, record_json)
             SELECT ?1, category, ordinal, record_key, topo_ref_json, record_json
             FROM engineering_meta WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        tx.execute(
            "INSERT INTO drawing_sheets (branch_name, sheet_id, title, paper_size, views_json, dimensions_json)
             SELECT ?1, sheet_id, title, paper_size, views_json, dimensions_json
             FROM drawing_sheets WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        tx.execute(
            "INSERT INTO whole_graph_brep_cache (
                branch_name, kernel_id, kernel_version, graph_sha256,
                compression, uncompressed_len, brep_payload
             )
             SELECT ?1, kernel_id, kernel_version, graph_sha256,
                    compression, uncompressed_len, brep_payload
             FROM whole_graph_brep_cache WHERE branch_name = ?2",
            params![branch_name, source_branch],
        )?;
        if verification_reports_table_exists(&tx)? {
            tx.execute(
                "INSERT INTO verification_reports (
                    branch_name, schema_version, overall_status, kernel_provider,
                    kernel_version, authoritative, report_json, updated_at_ms
                 )
                 SELECT ?1, schema_version, overall_status, kernel_provider,
                        kernel_version, authoritative, report_json, updated_at_ms
                 FROM verification_reports WHERE branch_name = ?2",
                params![branch_name, source_branch],
            )?;
        }

        tx.commit()?;

        Ok(BranchInfo {
            branch_name: branch_name.to_owned(),
            head_seq: src_head_seq,
            base_branch: Some(source_branch.to_owned()),
            base_seq: Some(src_head_seq),
            purpose,
            status: BranchStatus::Active,
            gate_score: None,
            is_active: false,
            created_at_ms: now_ms,
        })
    }

    /// Switch the active container branch to `branch_name` and return its `CanonicalGraph`.
    pub fn checkout_branch(&mut self, branch_name: &str) -> Result<CanonicalGraph, ContainerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (graph_id, length_unit, angle_unit): (String, String, String) = tx
            .query_row(
                "SELECT graph_id, length_unit, angle_unit FROM branches WHERE branch_name = ?1",
                params![branch_name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| ContainerError::UnknownBranch(branch_name.to_owned()))?;

        let now_ms = unix_now_ms();
        tx.execute(
            "UPDATE container_meta
             SET active_branch = ?1, graph_id = ?2, length_unit = ?3, angle_unit = ?4, updated_at_ms = ?5
             WHERE singleton_id = 1",
            params![branch_name, graph_id, length_unit, angle_unit, now_ms],
        )?;
        let graph = load_branch_graph_from_conn(&tx, branch_name)?;
        tx.commit()?;
        Ok(graph)
    }

    /// Update a branch's lifecycle status (e.g., `BranchStatus::Discarded` or `Promoted`) and optional gate score.
    pub fn set_branch_status(
        &mut self,
        branch_name: &str,
        status: BranchStatus,
        gate_score: Option<f64>,
    ) -> Result<(), ContainerError> {
        ensure_branch_exists(&self.conn, branch_name)?;
        self.conn.execute(
            "UPDATE branches SET status = ?2, gate_score = ?3 WHERE branch_name = ?1",
            params![branch_name, status.as_str(), gate_score],
        )?;
        Ok(())
    }

    /// Delete a non-active branch and cascade-remove its branch-local rows.
    pub fn delete_branch(&mut self, branch_name: &str) -> Result<(), ContainerError> {
        let active = self.active_branch()?;
        if active == branch_name {
            return Err(ContainerError::CannotDeleteActiveBranch(
                branch_name.to_owned(),
            ));
        }
        ensure_branch_exists(&self.conn, branch_name)?;
        self.conn.execute(
            "DELETE FROM branches WHERE branch_name = ?1",
            params![branch_name],
        )?;
        Ok(())
    }
}

pub(super) fn ensure_branch_exists(conn: &Connection, branch_name: &str) -> Result<(), ContainerError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM branches WHERE branch_name = ?1",
            params![branch_name],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(ContainerError::UnknownBranch(branch_name.to_owned()));
    }
    Ok(())
}

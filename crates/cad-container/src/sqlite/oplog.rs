use rusqlite::params;

use crate::ContainerError;

use super::{OplogActor, OplogEntry, SqliteContainer, branches::ensure_branch_exists};

impl SqliteContainer {
    /// Return ordered `oplog` entries for `branch_opt` (or all container oplog entries if `None`).
    pub fn oplog_entries(&self, branch_opt: Option<&str>) -> Result<Vec<OplogEntry>, ContainerError> {
        if let Some(branch) = branch_opt {
            ensure_branch_exists(&self.conn, branch)?;
            let mut stmt = self.conn.prepare(
                "SELECT seq, branch_name, parent_seq, patch_id, actor, description,
                        patch_json, outputs_merkle, created_at_ms
                 FROM oplog
                 WHERE branch_name = ?1
                 ORDER BY seq ASC",
            )?;
            collect_oplog_rows(stmt.query(params![branch])?)
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT seq, branch_name, parent_seq, patch_id, actor, description,
                        patch_json, outputs_merkle, created_at_ms
                 FROM oplog
                 ORDER BY seq ASC",
            )?;
            collect_oplog_rows(stmt.query([])?)
        }
    }
}

fn collect_oplog_rows(mut rows: rusqlite::Rows<'_>) -> Result<Vec<OplogEntry>, ContainerError> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let actor_str: String = row.get(4)?;
        let patch_json: String = row.get(6)?;
        out.push(OplogEntry {
            seq: row.get(0)?,
            branch_name: row.get(1)?,
            parent_seq: row.get(2)?,
            patch_id: row.get(3)?,
            actor: OplogActor::parse(&actor_str),
            description: row.get(5)?,
            patch: serde_json::from_str(&patch_json)?,
            outputs_merkle: row.get(7)?,
            created_at_ms: row.get(8)?,
        });
    }
    Ok(out)
}

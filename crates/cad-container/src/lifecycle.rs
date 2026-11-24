//! Engineering container lifecycle operations: `pack_and_go` (recursive self-contained bundling)
//! and `vacuum_container` (mark-and-sweep garbage collection + SQLite page compaction).

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use cad_ir::{GraphPatch, PatchOp};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    AicadProject, BundledRefEntry, ContainerError, SqliteContainer, StorageKind,
    blobs::compute_sha256_hex,
    detect::{detect_storage_kind, is_reparse_or_symlink},
};

/// Summary returned by `pack_and_go` after assembling a self-contained `.aicad` SQLite bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackAndGoReport {
    pub output_path: PathBuf,
    pub bundled_count: usize,
    pub locked_hashes: BTreeMap<String, String>,
    pub bundled_refs: Vec<BundledRefEntry>,
}

/// Summary returned by `vacuum_container` after sweeping orphan payloads and compacting SQLite pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VacuumReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub reclaimed_bytes: u64,
    pub removed_blobs: usize,
    pub removed_node_caches: usize,
    pub pruned_oplog_entries: usize,
}

struct PendingBundle {
    ref_id: String,
    original_container_path: String,
    container_sha256: String,
    source_format: &'static str,
    raw_bytes: Vec<u8>,
}

/// Recursively resolve, validate, hash-lock, and inline all `graph.external_refs` into a standalone
/// single-file SQLite `.aicad` bundle (`bundled_refs` table) for zero-broken-link offline delivery.
pub fn pack_and_go(
    src_path: &Path,
    workspace_root: &Path,
    out_sqlite_path: &Path,
) -> Result<PackAndGoReport, ContainerError> {
    let ws_meta = fs::symlink_metadata(workspace_root)?;
    if is_reparse_or_symlink(&ws_meta) || !ws_meta.is_dir() {
        return Err(ContainerError::UnsafePath(
            workspace_root.display().to_string(),
        ));
    }
    let canon_workspace = fs::canonicalize(workspace_root)?;

    let src_meta = fs::symlink_metadata(src_path)?;
    if is_reparse_or_symlink(&src_meta) {
        return Err(ContainerError::UnsafePath(src_path.display().to_string()));
    }
    let canon_src = fs::canonicalize(src_path)?;
    if !canon_src.starts_with(&canon_workspace) {
        return Err(ContainerError::UnsafePath(src_path.display().to_string()));
    }

    let mut root_project = AicadProject::load_any(src_path)?;

    let mut visiting = BTreeSet::new();
    visiting.insert(canon_src.clone());
    let mut chain = vec![canon_src.display().to_string()];

    let mut pending_bundles = BTreeMap::<String, PendingBundle>::new();
    let mut locked_hashes = BTreeMap::<String, String>::new();

    collect_external_refs_recursive(
        &canon_src,
        &canon_workspace,
        &mut root_project,
        true,
        &mut visiting,
        &mut chain,
        &mut pending_bundles,
        &mut locked_hashes,
    )?;

    let mut out_container = SqliteContainer::create(out_sqlite_path, &root_project)?;
    let mut bundled_refs = Vec::with_capacity(pending_bundles.len());
    for (_, item) in pending_bundles {
        let entry = out_container.insert_bundled_ref(
            &item.ref_id,
            &item.original_container_path,
            &item.container_sha256,
            item.source_format,
            &item.raw_bytes,
        )?;
        bundled_refs.push(entry);
    }

    Ok(PackAndGoReport {
        output_path: out_sqlite_path.to_path_buf(),
        bundled_count: bundled_refs.len(),
        locked_hashes,
        bundled_refs,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_external_refs_recursive(
    current_container_canon: &Path,
    canon_workspace: &Path,
    project: &mut AicadProject,
    is_root: bool,
    visiting: &mut BTreeSet<PathBuf>,
    chain: &mut Vec<String>,
    pending_bundles: &mut BTreeMap<String, PendingBundle>,
    locked_hashes: &mut BTreeMap<String, String>,
) -> Result<(), ContainerError> {
    let ref_keys: Vec<String> = project.graph.external_refs.keys().cloned().collect();

    for key in ref_keys {
        let (ref_id, container_path_str, expected_sha256) = {
            let def = &project.graph.external_refs[&key];
            (
                def.ref_id.0.clone(),
                def.container_path.clone(),
                def.expected_sha256.clone(),
            )
        };

        let resolved_child = resolve_safe_external_path(
            current_container_canon,
            canon_workspace,
            &container_path_str,
        )?;

        if !visiting.insert(resolved_child.clone()) {
            let mut cycle_desc = chain.join(" -> ");
            cycle_desc.push_str(" -> ");
            cycle_desc.push_str(&resolved_child.display().to_string());
            return Err(ContainerError::ExternalRefCycle(cycle_desc));
        }
        chain.push(resolved_child.display().to_string());

        let child_kind = detect_storage_kind(&resolved_child)?;
        let mut child_project = AicadProject::load_any(&resolved_child)?;

        // Recursively validate and bundle transitive external references first.
        collect_external_refs_recursive(
            &resolved_child,
            canon_workspace,
            &mut child_project,
            false,
            visiting,
            chain,
            pending_bundles,
            locked_hashes,
        )?;

        let raw_bytes = match child_kind {
            StorageKind::SqliteContainer => fs::read(&resolved_child)?,
            StorageKind::UnpackedSingleGraphDir | StorageKind::UnpackedShardedDir => {
                child_project.to_sqlite_bytes()?
            }
        };
        let actual_sha256 = compute_sha256_hex(&raw_bytes);

        if let Some(expected) = &expected_sha256
            && !expected.eq_ignore_ascii_case(&actual_sha256)
        {
            return Err(ContainerError::ExternalRefHashMismatch {
                ref_id,
                path: container_path_str,
                expected: expected.clone(),
                actual: actual_sha256,
            });
        }

        if let Some(def) = project.graph.external_refs.get_mut(&key) {
            def.expected_sha256 = Some(actual_sha256.clone());
        }
        if is_root {
            locked_hashes.insert(ref_id.clone(), actual_sha256.clone());
        }

        let bundle_key = if let Some(existing) = pending_bundles.get(&ref_id) {
            if existing.container_sha256 == actual_sha256 {
                ref_id.clone()
            } else {
                format!("{ref_id}@{}", &actual_sha256[..12])
            }
        } else {
            ref_id.clone()
        };

        pending_bundles.entry(bundle_key.clone()).or_insert(PendingBundle {
            ref_id: bundle_key,
            original_container_path: container_path_str,
            container_sha256: actual_sha256,
            source_format: child_kind.as_str(),
            raw_bytes,
        });

        chain.pop();
        visiting.remove(&resolved_child);
    }

    Ok(())
}

fn resolve_safe_external_path(
    current_container_canon: &Path,
    canon_workspace: &Path,
    raw_rel: &str,
) -> Result<PathBuf, ContainerError> {
    let trimmed = raw_rel.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains('\\')
        || trimmed.contains(':')
        || trimmed.chars().any(|c| c.is_control())
    {
        return Err(ContainerError::UnsafePath(raw_rel.to_owned()));
    }

    let primary_base = if current_container_canon == canon_workspace {
        canon_workspace.to_path_buf()
    } else {
        current_container_canon
            .parent()
            .unwrap_or(canon_workspace)
            .to_path_buf()
    };

    if let Some(found) = try_walk_within_workspace(&primary_base, canon_workspace, trimmed)? {
        return Ok(found);
    }
    if primary_base != canon_workspace
        && let Some(found) = try_walk_within_workspace(canon_workspace, canon_workspace, trimmed)?
    {
        return Ok(found);
    }

    Err(ContainerError::MissingEntry(raw_rel.to_owned()))
}

fn try_walk_within_workspace(
    base_dir: &Path,
    canon_workspace: &Path,
    rel: &str,
) -> Result<Option<PathBuf>, ContainerError> {
    let mut cursor = base_dir.to_path_buf();
    if !cursor.starts_with(canon_workspace) {
        return Err(ContainerError::UnsafePath(rel.to_owned()));
    }

    for component in Path::new(rel).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !cursor.pop() || !cursor.starts_with(canon_workspace) {
                    return Err(ContainerError::UnsafePath(rel.to_owned()));
                }
            }
            Component::Normal(part) => {
                let Some(part_str) = part.to_str() else {
                    return Err(ContainerError::UnsafePath(rel.to_owned()));
                };
                if part_str.is_empty() || part_str.ends_with(['.', ' ']) {
                    return Err(ContainerError::UnsafePath(rel.to_owned()));
                }
                cursor.push(part);
                match fs::symlink_metadata(&cursor) {
                    Ok(meta) => {
                        if is_reparse_or_symlink(&meta) {
                            return Err(ContainerError::UnsafePath(rel.to_owned()));
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e.into()),
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(ContainerError::UnsafePath(rel.to_owned()));
            }
        }
    }

    let canon = fs::canonicalize(&cursor)?;
    if !canon.starts_with(canon_workspace) {
        return Err(ContainerError::UnsafePath(rel.to_owned()));
    }
    Ok(Some(canon))
}

/// Sweep unreferenced orphan `blobs`, stale non-pinned `node_brep_cache` entries, discarded branches,
/// and old `oplog` records beyond `keep_recent_oplog`, then run SQLite `VACUUM` to reclaim physical disk pages.
pub fn vacuum_container(
    sqlite_path: &Path,
    keep_recent_oplog: Option<usize>,
) -> Result<VacuumReport, ContainerError> {
    let kind = detect_storage_kind(sqlite_path)?;
    if kind != StorageKind::SqliteContainer {
        return Err(ContainerError::UnsupportedFormat(
            sqlite_path.display().to_string(),
        ));
    }

    let bytes_before = total_sqlite_footprint_bytes(sqlite_path)?;

    let (removed_blobs, removed_node_caches, pruned_oplog_entries) = {
        let mut container = SqliteContainer::open(sqlite_path)?;
        let conn = container.connection_mut();

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let active_branch: String = tx.query_row(
            "SELECT active_branch FROM container_meta WHERE singleton_id = 1",
            [],
            |r| r.get(0),
        )?;

        // 1. Sweep explicitly discarded non-active branches (cascades their graph_nodes, graph_blobs, oplog, etc.).
        tx.execute(
            "DELETE FROM branches WHERE status = 'discarded' AND branch_name != ?1",
            params![active_branch],
        )?;

        // 2. Prune older `oplog` entries per retained branch according to `keep_recent_oplog`.
        let mut pruned_oplog_entries = 0_usize;
        if let Some(keep) = keep_recent_oplog {
            let branch_names: Vec<String> = {
                let mut stmt = tx.prepare("SELECT branch_name FROM branches")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<Result<Vec<_>, _>>()?
            };

            for branch in branch_names {
                let deleted = if keep == 0 {
                    tx.execute("DELETE FROM oplog WHERE branch_name = ?1", params![branch])?
                } else {
                    tx.execute(
                        "DELETE FROM oplog
                         WHERE branch_name = ?1
                           AND seq NOT IN (
                               SELECT seq FROM oplog
                               WHERE branch_name = ?1
                               ORDER BY seq DESC
                               LIMIT ?2
                           )",
                        params![branch, keep as i64],
                    )?
                };
                pruned_oplog_entries += deleted;
            }
        }

        // 3. Sweep orphan `blobs` not referenced by any retained branch's `graph_blobs`,
        //    `container_meta.cadpack_sha256`, or any retained `oplog` entry (`patch_json` / `inverse_patch_json`).
        let live_blob_hashes = collect_live_blob_hashes(&tx)?;
        let stored_blob_hashes: Vec<String> = {
            let mut stmt = tx.prepare("SELECT sha256 FROM blobs")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut removed_blobs = 0_usize;
        {
            let mut del_stmt = tx.prepare("DELETE FROM blobs WHERE sha256 = ?1")?;
            for sha in stored_blob_hashes {
                if !live_blob_hashes.contains(&sha.to_ascii_lowercase()) {
                    removed_blobs += del_stmt.execute(params![sha])?;
                }
            }
        }

        // 4. Sweep non-pinned `node_brep_cache` entries whose `merkle_hash` no longer matches any retained branch node.
        let removed_node_caches = tx.execute(
            "DELETE FROM node_brep_cache
             WHERE is_output_pinned = 0
               AND merkle_hash NOT IN (SELECT merkle_hash FROM graph_nodes)",
            [],
        )?;

        tx.commit()?;

        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        conn.execute_batch("VACUUM;")?;
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");

        (removed_blobs, removed_node_caches, pruned_oplog_entries)
    };

    let bytes_after = total_sqlite_footprint_bytes(sqlite_path)?;
    let reclaimed_bytes = bytes_before.saturating_sub(bytes_after);

    Ok(VacuumReport {
        bytes_before,
        bytes_after,
        reclaimed_bytes,
        removed_blobs,
        removed_node_caches,
        pruned_oplog_entries,
    })
}

fn collect_live_blob_hashes(conn: &Connection) -> Result<BTreeSet<String>, ContainerError> {
    let mut live = BTreeSet::new();

    {
        let mut stmt = conn.prepare("SELECT DISTINCT sha256 FROM graph_blobs")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            live.insert(row?.to_ascii_lowercase());
        }
    }

    {
        let mut stmt = conn.prepare(
            "SELECT cadpack_sha256 FROM container_meta WHERE cadpack_sha256 IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            live.insert(row?.to_ascii_lowercase());
        }
    }

    {
        let mut stmt = conn.prepare("SELECT patch_json, inverse_patch_json FROM oplog")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (patch_json, inverse_patch_json) = row?;
            extract_patch_blob_hashes(&patch_json, &mut live);
            if let Some(inv_json) = inverse_patch_json.as_deref() {
                extract_patch_blob_hashes(inv_json, &mut live);
            }
        }
    }

    Ok(live)
}

fn extract_patch_blob_hashes(json_str: &str, live: &mut BTreeSet<String>) {
    if let Ok(patch) = serde_json::from_str::<GraphPatch>(json_str) {
        for op in patch.ops {
            if let PatchOp::UpsertBlob(entry) = op
                && is_sha256_hex(&entry.sha256)
            {
                live.insert(entry.sha256.to_ascii_lowercase());
            }
        }
        return;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
        collect_value_sha256_fields(&value, live);
    }
}

fn collect_value_sha256_fields(value: &serde_json::Value, live: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "sha256"
                    && let Some(hex) = child.as_str()
                    && is_sha256_hex(hex)
                {
                    live.insert(hex.to_ascii_lowercase());
                }
                collect_value_sha256_fields(child, live);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_value_sha256_fields(item, live);
            }
        }
        _ => {}
    }
}

fn is_sha256_hex(candidate: &str) -> bool {
    candidate.len() == 64 && candidate.bytes().all(|b| b.is_ascii_hexdigit())
}

fn total_sqlite_footprint_bytes(sqlite_path: &Path) -> Result<u64, ContainerError> {
    let mut total = fs::metadata(sqlite_path)?.len();
    let mut wal_os = sqlite_path.as_os_str().to_os_string();
    wal_os.push("-wal");
    if let Ok(meta) = fs::metadata(PathBuf::from(wal_os)) {
        total += meta.len();
    }
    Ok(total)
}

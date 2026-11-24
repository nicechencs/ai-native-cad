use std::{
    collections::{BTreeMap, btree_map::Entry},
    fs,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use cad_ir::{
    AngleUnit, BlobHandle, BlobManifestEntry, CanonicalGraph, ExternalRefDef, ExternalRefId,
    IrNode, IrType, IrValue, LengthUnit, NodeId, NodeKind, NodeState, ParamId, ParameterDef,
    PayloadPolicy, UnitSystem, VerificationReport, compute_node_merkle_hashes, validate_graph,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::{
    AicadProject, AppearanceOverride, BomItem, ContainerError, ContainerManifest, DrawingSheet,
    EngineeringMetadata, KclProjectionPolicy, KernelFingerprint, PmiAnnotation,
    blobs::{checked_blob_path, compute_sha256_hex},
    detect::JournalPolicy,
};

use super::{
    SqliteContainer,
    blobs::{
        blob_kind_str, compress_payload, parse_blob_kind, read_raw_blob_by_sha256,
        stream_blob_by_sha256, upsert_content_addressed_blob_at,
    },
    branches::ensure_branch_exists,
    cache::load_whole_graph_cache_from_conn,
    remove_wal_sidecars,
    schema::VERIFICATION_REPORTS_DDL,
    unix_now_ms,
};

impl AicadProject {
    /// Save this project as a single-file SQLite `.aicad` container using adaptive journal policy.
    pub fn save_sqlite(&self, path: &Path) -> Result<(), ContainerError> {
        self.save_sqlite_with_policy(path, JournalPolicy::Auto)
    }

    /// Save this project as a single-file SQLite `.aicad` container with an explicit `JournalPolicy`.
    pub fn save_sqlite_with_policy(
        &self,
        path: &Path,
        policy: JournalPolicy,
    ) -> Result<(), ContainerError> {
        let _container = SqliteContainer::create_with_policy(path, self, policy)?;
        Ok(())
    }

    /// Serialize this project as deterministic standalone single-file SQLite container bytes.
    pub fn to_sqlite_bytes(&self) -> Result<Vec<u8>, ContainerError> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temp_path = std::env::temp_dir().join(format!(
            "aicad-serialize-{}-{id}.sqlite",
            std::process::id()
        ));
        let write_res = SqliteContainer::create_with_policy_at(
            &temp_path,
            self,
            JournalPolicy::SafeRollback,
            0,
        );
        let result = match write_res {
            Ok(container) => {
                drop(container);
                fs::read(&temp_path).map_err(ContainerError::from)
            }
            Err(err) => Err(err),
        };
        let _ = fs::remove_file(&temp_path);
        remove_wal_sidecars(&temp_path);
        result
    }

    /// Load an `AicadProject` from its active branch in a SQLite `.aicad` container.
    pub fn load_sqlite(path: &Path) -> Result<Self, ContainerError> {
        let container = SqliteContainer::open(path)?;
        container.load_project()
    }
}

impl SqliteContainer {
    /// Reconstruct the full `AicadProject` for the currently active branch (streaming & verifying all blobs).
    pub fn load_project(&self) -> Result<AicadProject, ContainerError> {
        let meta = read_meta_row(&self.conn)?;
        let graph = load_branch_graph_from_conn(&self.conn, &meta.active_branch)?;
        let engineering = load_engineering_from_conn(&self.conn, &meta.active_branch)?;
        let drawings = load_drawings_from_conn(&self.conn, &meta.active_branch)?;

        let mut manifest = ContainerManifest {
            format_version: meta.format_version,
            ir_schema_version: meta.ir_schema_version,
            project_name: meta.project_name,
            created_with_kernel: meta.kernel,
            entry_graph: meta.entry_graph,
            kcl_view: meta.kcl_view_path,
            kcl_projection_policy: meta.kcl_projection_policy,
        };
        if manifest.kcl_projection_policy == KclProjectionPolicy::OnDemandDynamic {
            manifest.kcl_view = None;
        }

        let mut project = AicadProject::new(manifest, graph);
        if project.manifest.kcl_projection_policy == KclProjectionPolicy::MaterializedSynced {
            project.kcl_view = meta.kcl_view_text;
        }
        project.engineering = engineering;
        project.drawings = drawings;

        for entry in project.graph.blobs.values() {
            let rel_path = checked_blob_path(&entry.relative_path)?;
            if let Entry::Vacant(slot) = project.blobs.entry(rel_path) {
                let mut bytes = Vec::with_capacity(entry.byte_len as usize);
                stream_blob_by_sha256(
                    &self.conn,
                    &entry.sha256,
                    entry.byte_len,
                    &entry.relative_path,
                    &mut bytes,
                )?;
                slot.insert(bytes);
            }
        }
        project.verify_all_blobs()?;

        project.last_known_good =
            load_whole_graph_cache_from_conn(&self.conn, &meta.active_branch)?;
        if let Some(cadpack_sha) = meta.cadpack_sha256 {
            project.cadpack_bytes = Some(read_raw_blob_by_sha256(&self.conn, &cadpack_sha)?);
        }
        project.verification_report =
            load_verification_report_from_conn(&self.conn, &meta.active_branch)?;

        Ok(project)
    }

    /// Load the `CanonicalGraph` of the active branch.
    pub fn load_active_graph(&self) -> Result<CanonicalGraph, ContainerError> {
        let branch = self.active_branch()?;
        load_branch_graph_from_conn(&self.conn, &branch)
    }

    /// Load the `CanonicalGraph` of any named branch.
    pub fn load_branch_graph(&self, branch_name: &str) -> Result<CanonicalGraph, ContainerError> {
        ensure_branch_exists(&self.conn, branch_name)?;
        load_branch_graph_from_conn(&self.conn, branch_name)
    }

    /// Persist or replace the latest tri-state [`VerificationReport`] on `branch_opt`
    /// (defaulting to the active branch) inside an ACID transaction.
    pub fn save_verification_report(
        &mut self,
        report: &VerificationReport,
        branch_opt: Option<&str>,
    ) -> Result<(), ContainerError> {
        self.conn.execute_batch(VERIFICATION_REPORTS_DDL)?;
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
        let now_ms = unix_now_ms();
        upsert_verification_report_row(&tx, &branch_name, report, now_ms)?;
        tx.execute(
            "UPDATE container_meta SET updated_at_ms = ?1 WHERE singleton_id = 1",
            params![now_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Load the latest persisted [`VerificationReport`] for `branch_opt`
    /// (defaulting to the active branch), or `None` if no report has been stored.
    pub fn load_verification_report(
        &self,
        branch_opt: Option<&str>,
    ) -> Result<Option<VerificationReport>, ContainerError> {
        let branch_name = match branch_opt {
            Some(name) => {
                ensure_branch_exists(&self.conn, name)?;
                name.to_owned()
            }
            None => self.active_branch()?,
        };
        load_verification_report_from_conn(&self.conn, &branch_name)
    }

    /// Delete any persisted [`VerificationReport`] on `branch_opt` (defaulting to active branch).
    pub fn clear_verification_report(
        &mut self,
        branch_opt: Option<&str>,
    ) -> Result<(), ContainerError> {
        if !verification_reports_table_exists(&self.conn)? {
            return Ok(());
        }
        let branch_name = match branch_opt {
            Some(name) => {
                ensure_branch_exists(&self.conn, name)?;
                name.to_owned()
            }
            None => self.active_branch()?,
        };
        self.conn.execute(
            "DELETE FROM verification_reports WHERE branch_name = ?1",
            params![branch_name],
        )?;
        Ok(())
    }
}

pub(crate) struct MetaRow {
    pub(crate) format_version: u32,
    pub(crate) ir_schema_version: u32,
    pub(crate) project_name: String,
    pub(crate) kernel: KernelFingerprint,
    pub(crate) entry_graph: String,
    pub(crate) kcl_view_path: Option<String>,
    pub(crate) active_branch: String,
    pub(crate) kcl_projection_policy: KclProjectionPolicy,
    pub(crate) kcl_view_text: Option<String>,
    pub(crate) cadpack_sha256: Option<String>,
}

pub(crate) fn read_meta_row(conn: &Connection) -> Result<MetaRow, ContainerError> {
    conn.query_row(
        "SELECT format_version, ir_schema_version, project_name, kernel_id, kernel_version,
                entry_graph, kcl_view_path, active_branch, kcl_projection_policy,
                kcl_view_text, cadpack_sha256
         FROM container_meta WHERE singleton_id = 1",
        [],
        |row| {
            let policy_str: String = row.get(8)?;
            Ok(MetaRow {
                format_version: row.get::<_, i64>(0)? as u32,
                ir_schema_version: row.get::<_, i64>(1)? as u32,
                project_name: row.get(2)?,
                kernel: KernelFingerprint {
                    kernel_id: row.get(3)?,
                    kernel_version: row.get(4)?,
                },
                entry_graph: row.get(5)?,
                kcl_view_path: row.get(6)?,
                active_branch: row.get(7)?,
                kcl_projection_policy: KclProjectionPolicy::parse_str(&policy_str)
                    .unwrap_or_default(),
                kcl_view_text: row.get(9)?,
                cadpack_sha256: row.get(10)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| ContainerError::MissingEntry("container_meta".to_owned()))
}

pub(super) fn populate_initial_project(
    conn: &mut Connection,
    manifest: &ContainerManifest,
    project: &AicadProject,
    now_ms: i64,
) -> Result<(), ContainerError> {
    let merkle_hashes = compute_node_merkle_hashes(&project.graph)?;
    let branch_name = "main";
    let length_unit = length_unit_str(project.graph.units.length);
    let angle_unit = angle_unit_str(project.graph.units.angle);
    let units_json = serde_json::to_string(&project.graph.units)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let cadpack_sha256 = if let Some(cadpack_bytes) = &project.cadpack_bytes {
        let sha = compute_sha256_hex(cadpack_bytes);
        upsert_content_addressed_blob_at(
            &tx,
            &sha,
            "__cadpack__",
            branch_name,
            "cadpack",
            "cache/scene.cadpack",
            cadpack_bytes,
            now_ms,
        )?;
        Some(sha)
    } else {
        None
    };

    let kcl_view_text = if manifest.kcl_projection_policy == KclProjectionPolicy::MaterializedSynced
    {
        project.kcl_view.as_deref()
    } else {
        None
    };

    tx.execute(
        "INSERT INTO container_meta (
            singleton_id, format_version, ir_schema_version, project_name,
            kernel_id, kernel_version, entry_graph, kcl_view_path, graph_id,
            length_unit, angle_unit, active_branch, kcl_projection_policy,
            kcl_view_text, cadpack_sha256, created_at_ms, updated_at_ms
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
        params![
            i64::from(manifest.format_version),
            i64::from(manifest.ir_schema_version),
            manifest.project_name,
            manifest.created_with_kernel.kernel_id,
            manifest.created_with_kernel.kernel_version,
            manifest.entry_graph,
            manifest.kcl_view,
            project.graph.graph_id,
            length_unit,
            angle_unit,
            branch_name,
            manifest.kcl_projection_policy.as_str(),
            kcl_view_text,
            cadpack_sha256,
            now_ms,
        ],
    )?;

    tx.execute(
        "INSERT INTO branches (
            branch_name, head_seq, base_branch, base_seq, purpose, status,
            gate_score, graph_id, length_unit, angle_unit, units_json, created_at_ms
         ) VALUES (?1, 0, NULL, NULL, 'main', 'active', NULL, ?2, ?3, ?4, ?5, ?6)",
        params![
            branch_name,
            project.graph.graph_id,
            length_unit,
            angle_unit,
            units_json,
            now_ms,
        ],
    )?;

    for param in project.graph.parameters.values() {
        upsert_parameter_row(&tx, branch_name, param)?;
    }

    for node in project.graph.nodes.values() {
        let merkle = &merkle_hashes[&node.id];
        insert_node_row(&tx, branch_name, node, merkle)?;
    }

    for (ordinal, output_id) in project.graph.outputs.iter().enumerate() {
        tx.execute(
            "INSERT INTO graph_outputs (branch_name, ordinal, node_id) VALUES (?1, ?2, ?3)",
            params![branch_name, ordinal as i64, output_id.0],
        )?;
    }

    for ext_ref in project.graph.external_refs.values() {
        upsert_external_ref_row(&tx, branch_name, ext_ref)?;
    }

    for blob_entry in project.graph.blobs.values() {
        let bytes = &project.blobs[&blob_entry.relative_path];
        upsert_content_addressed_blob_at(
            &tx,
            &blob_entry.sha256,
            &blob_entry.handle.0,
            branch_name,
            blob_kind_str(blob_entry.kind),
            &blob_entry.relative_path,
            bytes,
            now_ms,
        )?;
        tx.execute(
            "INSERT INTO graph_blobs (branch_name, handle, blob_kind, sha256, byte_len, relative_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                branch_name,
                blob_entry.handle.0,
                blob_kind_str(blob_entry.kind),
                blob_entry.sha256,
                blob_entry.byte_len as i64,
                blob_entry.relative_path,
            ],
        )?;
    }

    for (ordinal, pmi) in project.engineering.pmi.iter().enumerate() {
        tx.execute(
            "INSERT INTO engineering_meta (branch_name, category, ordinal, record_key, topo_ref_json, record_json)
             VALUES (?1, 'pmi', ?2, ?3, ?4, ?5)",
            params![
                branch_name,
                ordinal as i64,
                pmi.id,
                serde_json::to_string(&pmi.target)?,
                serde_json::to_string(pmi)?,
            ],
        )?;
    }
    for (ordinal, appearance) in project.engineering.appearances.iter().enumerate() {
        tx.execute(
            "INSERT INTO engineering_meta (branch_name, category, ordinal, record_key, topo_ref_json, record_json)
             VALUES (?1, 'appearance', ?2, ?3, ?4, ?5)",
            params![
                branch_name,
                ordinal as i64,
                format!("appearance_{ordinal}"),
                serde_json::to_string(&appearance.target)?,
                serde_json::to_string(appearance)?,
            ],
        )?;
    }
    for (ordinal, bom) in project.engineering.bom.iter().enumerate() {
        tx.execute(
            "INSERT INTO engineering_meta (branch_name, category, ordinal, record_key, topo_ref_json, record_json)
             VALUES (?1, 'bom', ?2, ?3, NULL, ?4)",
            params![
                branch_name,
                ordinal as i64,
                bom.part_number,
                serde_json::to_string(bom)?,
            ],
        )?;
    }

    for (id, sheet) in &project.drawings {
        let expected_path = crate::directory_store::drawing_path(id)?;
        if sheet.sheet_id != *id {
            return Err(ContainerError::DrawingIdMismatch {
                path: expected_path,
                sheet_id: sheet.sheet_id.clone(),
            });
        }
        tx.execute(
            "INSERT INTO drawing_sheets (branch_name, sheet_id, title, paper_size, views_json, dimensions_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                branch_name,
                sheet.sheet_id,
                sheet.title,
                sheet.paper_size,
                serde_json::to_string(&sheet.views)?,
                serde_json::to_string(&sheet.dimensions)?,
            ],
        )?;
    }

    if let Some(cache) = &project.last_known_good {
        let (compression, stored) = compress_payload(&cache.brep_bytes)?;
        tx.execute(
            "INSERT INTO whole_graph_brep_cache (
                branch_name, kernel_id, kernel_version, graph_sha256,
                compression, uncompressed_len, brep_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                branch_name,
                cache.kernel.kernel_id,
                cache.kernel.kernel_version,
                cache.graph_sha256,
                compression,
                cache.brep_bytes.len() as i64,
                stored,
            ],
        )?;
    }

    if let Some(report) = &project.verification_report {
        upsert_verification_report_row(&tx, branch_name, report, now_ms)?;
    }

    tx.commit()?;
    Ok(())
}

pub(crate) fn load_branch_graph_from_conn(
    conn: &Connection,
    branch_name: &str,
) -> Result<CanonicalGraph, ContainerError> {
    let ir_schema_version: u32 = conn.query_row(
        "SELECT ir_schema_version FROM container_meta WHERE singleton_id = 1",
        [],
        |row| Ok(row.get::<_, i64>(0)? as u32),
    )?;
    let (graph_id, units_json): (String, String) = conn
        .query_row(
            "SELECT graph_id, units_json FROM branches WHERE branch_name = ?1",
            params![branch_name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| ContainerError::UnknownBranch(branch_name.to_owned()))?;
    let units: UnitSystem = serde_json::from_str(&units_json)?;

    let mut parameters = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT param_name, ir_type, value_json, formula
             FROM graph_parameters WHERE branch_name = ?1 ORDER BY param_name ASC",
        )?;
        let rows = stmt.query_map(params![branch_name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (param_name, ir_type_str, value_json, formula) = row?;
            let ir_type: IrType = serde_json::from_str(&ir_type_str)?;
            let value: IrValue = serde_json::from_str(&value_json)?;
            parameters.insert(
                param_name.clone(),
                ParameterDef {
                    name: ParamId(param_name),
                    ir_type,
                    value,
                    formula,
                },
            );
        }
    }

    let mut blobs = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT handle, blob_kind, sha256, byte_len, relative_path
             FROM graph_blobs WHERE branch_name = ?1 ORDER BY handle ASC",
        )?;
        let rows = stmt.query_map(params![branch_name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (handle, kind_str, sha256, byte_len, relative_path) = row?;
            blobs.insert(
                handle.clone(),
                BlobManifestEntry {
                    handle: BlobHandle(handle),
                    kind: parse_blob_kind(&kind_str)?,
                    sha256,
                    byte_len: byte_len as u64,
                    relative_path,
                },
            );
        }
    }

    let mut external_refs = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT ref_id, container_path, expected_sha256, target_graph_id, payload_policy, overrides_json
             FROM graph_external_refs WHERE branch_name = ?1 ORDER BY ref_id ASC",
        )?;
        let rows = stmt.query_map(params![branch_name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (ref_id, container_path, expected_sha256, target_graph_id, policy_str, overrides_json) =
                row?;
            let payload_policy = match policy_str.as_str() {
                "deferred_payload" => PayloadPolicy::DeferredPayload,
                _ => PayloadPolicy::Eager,
            };
            let overrides: BTreeMap<String, IrValue> = serde_json::from_str(&overrides_json)?;
            external_refs.insert(
                ref_id.clone(),
                ExternalRefDef {
                    ref_id: ExternalRefId(ref_id),
                    container_path,
                    expected_sha256,
                    target_graph_id,
                    payload_policy,
                    overrides,
                },
            );
        }
    }

    let mut nodes = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT node_id, name, kind_json, inputs_json, output_type,
                    state_json, kernel_op_version, metadata_json
             FROM graph_nodes WHERE branch_name = ?1 ORDER BY node_id ASC",
        )?;
        let rows = stmt.query_map(params![branch_name], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        for row in rows {
            let (
                node_id,
                name,
                kind_json,
                inputs_json,
                output_type_json,
                state_json,
                kernel_op_version,
                metadata_json,
            ) = row?;
            let kind: NodeKind = serde_json::from_str(&kind_json)?;
            let inputs: BTreeMap<String, IrValue> = serde_json::from_str(&inputs_json)?;
            let output_type: IrType = serde_json::from_str(&output_type_json)?;
            let state: NodeState = serde_json::from_str(&state_json)?;
            let metadata: BTreeMap<String, String> = serde_json::from_str(&metadata_json)?;
            nodes.insert(
                node_id.clone(),
                IrNode {
                    id: NodeId(node_id),
                    name,
                    kind,
                    inputs,
                    output_type,
                    state,
                    kernel_op_version,
                    metadata,
                },
            );
        }
    }

    let mut outputs = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT node_id FROM graph_outputs WHERE branch_name = ?1 ORDER BY ordinal ASC",
        )?;
        let rows = stmt.query_map(params![branch_name], |row| row.get::<_, String>(0))?;
        for row in rows {
            outputs.push(NodeId(row?));
        }
    }

    let mut graph = CanonicalGraph {
        ir_schema_version,
        graph_id,
        units,
        parameters,
        blobs,
        external_refs,
        nodes,
        outputs,
    };
    validate_graph(&graph)?;
    graph.evaluate_formulas()?;
    Ok(graph)
}

pub(crate) fn load_engineering_from_conn(
    conn: &Connection,
    branch_name: &str,
) -> Result<EngineeringMetadata, ContainerError> {
    let mut engineering = EngineeringMetadata::default();
    let mut stmt = conn.prepare(
        "SELECT category, record_json FROM engineering_meta
         WHERE branch_name = ?1 ORDER BY category ASC, ordinal ASC",
    )?;
    let rows = stmt.query_map(params![branch_name], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (category, record_json) = row?;
        match category.as_str() {
            "pmi" => {
                let item: PmiAnnotation = serde_json::from_str(&record_json)?;
                engineering.pmi.push(item);
            }
            "appearance" => {
                let item: AppearanceOverride = serde_json::from_str(&record_json)?;
                engineering.appearances.push(item);
            }
            "bom" => {
                let item: BomItem = serde_json::from_str(&record_json)?;
                engineering.bom.push(item);
            }
            _ => {}
        }
    }
    Ok(engineering)
}

pub(crate) fn load_drawings_from_conn(
    conn: &Connection,
    branch_name: &str,
) -> Result<BTreeMap<String, DrawingSheet>, ContainerError> {
    let mut drawings = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT sheet_id, title, paper_size, views_json, dimensions_json
         FROM drawing_sheets WHERE branch_name = ?1 ORDER BY sheet_id ASC",
    )?;
    let rows = stmt.query_map(params![branch_name], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (sheet_id, title, paper_size, views_json, dimensions_json) = row?;
        drawings.insert(
            sheet_id.clone(),
            DrawingSheet {
                sheet_id,
                title,
                paper_size,
                views: serde_json::from_str(&views_json)?,
                dimensions: serde_json::from_str(&dimensions_json)?,
            },
        );
    }
    Ok(drawings)
}

pub(crate) fn load_verification_report_from_conn(
    conn: &Connection,
    branch_name: &str,
) -> Result<Option<VerificationReport>, ContainerError> {
    if !verification_reports_table_exists(conn)? {
        return Ok(None);
    }
    let report_json: Option<String> = conn
        .query_row(
            "SELECT report_json FROM verification_reports WHERE branch_name = ?1",
            params![branch_name],
            |row| row.get(0),
        )
        .optional()?;
    match report_json {
        Some(json) => Ok(Some(serde_json::from_str(&json)?)),
        None => Ok(None),
    }
}

pub(super) fn verification_reports_table_exists(conn: &Connection) -> Result<bool, ContainerError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'verification_reports'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

pub(super) fn upsert_verification_report_row(
    conn: &Connection,
    branch_name: &str,
    report: &VerificationReport,
    updated_at_ms: i64,
) -> Result<(), ContainerError> {
    let report_json = serde_json::to_string(report)?;
    conn.execute(
        "INSERT INTO verification_reports (
            branch_name, schema_version, overall_status, kernel_provider,
            kernel_version, authoritative, report_json, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(branch_name) DO UPDATE SET
             schema_version = excluded.schema_version,
             overall_status = excluded.overall_status,
             kernel_provider = excluded.kernel_provider,
             kernel_version = excluded.kernel_version,
             authoritative = excluded.authoritative,
             report_json = excluded.report_json,
             updated_at_ms = excluded.updated_at_ms",
        params![
            branch_name,
            i64::from(report.schema_version),
            report.overall_status.as_str(),
            report.kernel_identity.provider,
            report.kernel_identity.version,
            i64::from(report.kernel_identity.authoritative),
            report_json,
            updated_at_ms,
        ],
    )?;
    Ok(())
}

pub(super) fn upsert_parameter_row(
    conn: &Connection,
    branch_name: &str,
    param: &ParameterDef,
) -> Result<(), ContainerError> {
    conn.execute(
        "INSERT INTO graph_parameters (branch_name, param_name, ir_type, value_json, formula)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(branch_name, param_name) DO UPDATE SET
             ir_type = excluded.ir_type,
             value_json = excluded.value_json,
             formula = excluded.formula",
        params![
            branch_name,
            param.name.0,
            serde_json::to_string(&param.ir_type)?,
            serde_json::to_string(&param.value)?,
            param.formula,
        ],
    )?;
    Ok(())
}

fn insert_node_row(
    conn: &Connection,
    branch_name: &str,
    node: &IrNode,
    merkle_hash: &str,
) -> Result<(), ContainerError> {
    upsert_node_row(conn, branch_name, node, merkle_hash)
}

pub(super) fn upsert_node_row(
    conn: &Connection,
    branch_name: &str,
    node: &IrNode,
    merkle_hash: &str,
) -> Result<(), ContainerError> {
    conn.execute(
        "INSERT INTO graph_nodes (
            branch_name, node_id, name, kind_json, inputs_json, output_type,
            state_json, kernel_op_version, metadata_json, merkle_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(branch_name, node_id) DO UPDATE SET
             name = excluded.name,
             kind_json = excluded.kind_json,
             inputs_json = excluded.inputs_json,
             output_type = excluded.output_type,
             state_json = excluded.state_json,
             kernel_op_version = excluded.kernel_op_version,
             metadata_json = excluded.metadata_json,
             merkle_hash = excluded.merkle_hash",
        params![
            branch_name,
            node.id.0,
            node.name,
            serde_json::to_string(&node.kind)?,
            serde_json::to_string(&node.inputs)?,
            serde_json::to_string(&node.output_type)?,
            serde_json::to_string(&node.state)?,
            node.kernel_op_version,
            serde_json::to_string(&node.metadata)?,
            merkle_hash,
        ],
    )?;
    Ok(())
}

pub(super) fn upsert_external_ref_row(
    conn: &Connection,
    branch_name: &str,
    ext: &ExternalRefDef,
) -> Result<(), ContainerError> {
    let policy_str = match ext.payload_policy {
        PayloadPolicy::Eager => "eager",
        PayloadPolicy::DeferredPayload => "deferred_payload",
    };
    conn.execute(
        "INSERT INTO graph_external_refs (
            branch_name, ref_id, container_path, expected_sha256,
            target_graph_id, payload_policy, overrides_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(branch_name, ref_id) DO UPDATE SET
             container_path = excluded.container_path,
             expected_sha256 = excluded.expected_sha256,
             target_graph_id = excluded.target_graph_id,
             payload_policy = excluded.payload_policy,
             overrides_json = excluded.overrides_json",
        params![
            branch_name,
            ext.ref_id.0,
            ext.container_path,
            ext.expected_sha256,
            ext.target_graph_id,
            policy_str,
            serde_json::to_string(&ext.overrides)?,
        ],
    )?;
    Ok(())
}

const fn length_unit_str(unit: LengthUnit) -> &'static str {
    match unit {
        LengthUnit::Mm => "mm",
        LengthUnit::M => "m",
        LengthUnit::In => "in",
        LengthUnit::Um => "um",
        LengthUnit::Cm => "cm",
        LengthUnit::Ft => "ft",
    }
}

const fn angle_unit_str(unit: AngleUnit) -> &'static str {
    match unit {
        AngleUnit::Deg => "deg",
        AngleUnit::Rad => "rad",
    }
}

//! Relational single-file `.aicad` SQLite storage engine with ACID incremental patches,
//! lightweight multi-branch exploration, streaming Zstd blobs, and node-level Merkle B-Rep caching.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use cad_ir::validate_graph;
use rusqlite::{Connection, OptionalExtension};

use crate::{
    AicadProject, ContainerError, KclProjectionPolicy,
    detect::{JournalPolicy, SQLITE_APPLICATION_ID, is_reparse_or_symlink},
    directory_store::{DEFAULT_ENTRY_GRAPH, validate_manifest_paths},
    manifest::{self, DEFAULT_KCL_VIEW_PATH, SHARDED_ENTRY_GRAPH_PATH},
};

mod blobs;
mod branches;
mod cache;
mod oplog;
mod patch;
mod project_io;
mod schema;
mod types;

pub use patch::apply_patch_in_place;
pub use types::{
    BranchInfo, BranchPurpose, BranchStatus, BundledRefEntry, NodeBrepCacheEntry, OplogActor,
    OplogEntry,
};

pub(crate) use blobs::{read_raw_blob_by_sha256, stream_blob_by_sha256};
pub(crate) use cache::{load_whole_graph_cache_from_conn, load_whole_graph_cache_metadata_from_conn};
pub(crate) use project_io::{
    load_branch_graph_from_conn, load_drawings_from_conn, load_engineering_from_conn,
    load_verification_report_from_conn, read_meta_row,
};

use project_io::populate_initial_project;
use schema::{SCHEMA_DDL, SQLITE_USER_VERSION, configure_journal_pragmas};

/// High-performance single-file SQLite `.aicad` container engine.
pub struct SqliteContainer {
    path: PathBuf,
    conn: Connection,
}

impl SqliteContainer {
    /// Create or overwrite a SQLite `.aicad` container from `project` with adaptive journal policy.
    pub fn create(path: &Path, project: &AicadProject) -> Result<Self, ContainerError> {
        Self::create_with_policy(path, project, JournalPolicy::Auto)
    }

    /// Create or overwrite a SQLite `.aicad` container from `project` with an explicit `JournalPolicy`.
    pub fn create_with_policy(
        path: &Path,
        project: &AicadProject,
        policy: JournalPolicy,
    ) -> Result<Self, ContainerError> {
        Self::create_with_policy_at(path, project, policy, unix_now_ms())
    }

    pub(crate) fn create_with_policy_at(
        path: &Path,
        project: &AicadProject,
        policy: JournalPolicy,
        now_ms: i64,
    ) -> Result<Self, ContainerError> {
        if let Some(meta) = optional_symlink_meta(path)?
            && (is_reparse_or_symlink(&meta) || meta.is_dir())
        {
            return Err(ContainerError::UnsafePath(path.display().to_string()));
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        manifest::validate_versions(&project.manifest, project.graph.ir_schema_version)?;
        validate_graph(&project.graph)?;
        project.verify_all_blobs()?;

        let mut manifest = project.manifest.clone();
        if manifest
            .entry_graph
            .eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH)
        {
            manifest.entry_graph = DEFAULT_ENTRY_GRAPH.to_owned();
        }
        match manifest.kcl_projection_policy {
            KclProjectionPolicy::OnDemandDynamic => {
                manifest.kcl_view = None;
            }
            KclProjectionPolicy::MaterializedSynced => {
                if project.kcl_view.is_some() {
                    manifest
                        .kcl_view
                        .get_or_insert_with(|| DEFAULT_KCL_VIEW_PATH.to_owned());
                } else {
                    manifest.kcl_view = None;
                }
            }
        }
        validate_manifest_paths(&manifest, false)?;

        // Remove existing file and any lingering WAL/SHM sidecars before initializing fresh.
        if path.exists() {
            fs::remove_file(path)?;
        }
        remove_wal_sidecars(path);

        let mut conn = Connection::open(path)?;
        conn.execute_batch(&format!(
            "PRAGMA page_size = 4096;
             PRAGMA application_id = {SQLITE_APPLICATION_ID};
             PRAGMA user_version = {SQLITE_USER_VERSION};
             PRAGMA foreign_keys = ON;"
        ))?;
        configure_journal_pragmas(&conn, path, policy)?;
        conn.execute_batch(SCHEMA_DDL)?;

        populate_initial_project(&mut conn, &manifest, project, now_ms)?;

        Ok(Self {
            path: path.to_path_buf(),
            conn,
        })
    }

    /// Open an existing SQLite `.aicad` container with adaptive local `WAL` / network `DELETE` journal policy.
    pub fn open(path: &Path) -> Result<Self, ContainerError> {
        Self::open_with_policy(path, JournalPolicy::Auto)
    }

    /// Open an existing SQLite `.aicad` container with an explicit `JournalPolicy`.
    pub fn open_with_policy(path: &Path, policy: JournalPolicy) -> Result<Self, ContainerError> {
        let meta = fs::symlink_metadata(path)?;
        if is_reparse_or_symlink(&meta) || !meta.is_file() {
            return Err(ContainerError::UnsafePath(path.display().to_string()));
        }

        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        configure_journal_pragmas(&conn, path, policy)?;

        let app_id: i32 = conn.query_row("PRAGMA application_id;", [], |row| row.get(0))?;
        if app_id != SQLITE_APPLICATION_ID {
            return Err(ContainerError::UnsupportedFormat(
                path.display().to_string(),
            ));
        }

        let user_version: i32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;
        if !(0..=SQLITE_USER_VERSION).contains(&user_version) {
            return Err(ContainerError::UnsupportedVersion {
                component: "sqlite container schema",
                found: u64::try_from(user_version).unwrap_or(u64::MAX),
                supported: SQLITE_USER_VERSION as u32,
            });
        }

        // Validate container_meta format and IR versions on open.
        let (format_version, ir_schema_version): (i64, i64) = conn
            .query_row(
                "SELECT format_version, ir_schema_version FROM container_meta WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| ContainerError::MissingEntry("container_meta".to_owned()))?;
        manifest::validate_manifest_versions(format_version as u64, ir_schema_version as u64)?;

        Ok(Self {
            path: path.to_path_buf(),
            conn,
        })
    }

    /// Path to the underlying SQLite file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Query the live SQLite `PRAGMA journal_mode` (`"wal"`, `"delete"`, etc.).
    pub fn effective_journal_mode(&self) -> Result<String, ContainerError> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))?;
        Ok(mode.to_ascii_lowercase())
    }
}

pub(crate) fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn optional_symlink_meta(path: &Path) -> Result<Option<fs::Metadata>, ContainerError> {
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn remove_wal_sidecars(path: &Path) {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let _ = fs::remove_file(PathBuf::from(wal));

    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    let _ = fs::remove_file(PathBuf::from(shm));
}

use std::path::Path;

use rusqlite::Connection;

use crate::{
    ContainerError,
    detect::{JournalPolicy, resolve_journal_mode_for_path},
};

pub(super) const SQLITE_USER_VERSION: i32 = 2;

pub(super) const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS container_meta (
    singleton_id          INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    format_version        INTEGER NOT NULL,
    ir_schema_version     INTEGER NOT NULL,
    project_name          TEXT    NOT NULL,
    kernel_id             TEXT    NOT NULL,
    kernel_version        TEXT    NOT NULL,
    entry_graph           TEXT    NOT NULL DEFAULT 'graph.ir.json',
    kcl_view_path         TEXT,
    graph_id              TEXT    NOT NULL,
    length_unit           TEXT    NOT NULL DEFAULT 'mm',
    angle_unit            TEXT    NOT NULL DEFAULT 'deg',
    active_branch         TEXT    NOT NULL DEFAULT 'main',
    kcl_projection_policy TEXT    NOT NULL DEFAULT 'on_demand_dynamic'
                          CHECK (kcl_projection_policy IN ('on_demand_dynamic', 'materialized_synced')),
    kcl_view_text         TEXT,
    cadpack_sha256        TEXT    CHECK (cadpack_sha256 IS NULL OR length(cadpack_sha256) = 64),
    created_at_ms         INTEGER NOT NULL,
    updated_at_ms         INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS branches (
    branch_name   TEXT    PRIMARY KEY CHECK (length(branch_name) > 0),
    head_seq      INTEGER NOT NULL DEFAULT 0,
    base_branch   TEXT,
    base_seq      INTEGER,
    purpose       TEXT    NOT NULL DEFAULT 'main'
                  CHECK (purpose IN ('main', 'user', 'ai_candidate')),
    status        TEXT    NOT NULL DEFAULT 'active'
                  CHECK (status IN ('active', 'promoted', 'discarded', 'archived')),
    gate_score    REAL,
    graph_id      TEXT    NOT NULL,
    length_unit   TEXT    NOT NULL DEFAULT 'mm',
    angle_unit    TEXT    NOT NULL DEFAULT 'deg',
    units_json    TEXT    NOT NULL,
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY (base_branch) REFERENCES branches(branch_name) ON DELETE SET NULL
) STRICT;

CREATE TABLE IF NOT EXISTS graph_parameters (
    branch_name TEXT NOT NULL,
    param_name  TEXT NOT NULL CHECK (length(param_name) > 0),
    ir_type     TEXT NOT NULL,
    value_json  TEXT NOT NULL,
    formula     TEXT,
    PRIMARY KEY (branch_name, param_name),
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS graph_nodes (
    branch_name       TEXT NOT NULL,
    node_id           TEXT NOT NULL CHECK (length(node_id) > 0),
    name              TEXT NOT NULL,
    kind_json         TEXT NOT NULL,
    inputs_json       TEXT NOT NULL,
    output_type       TEXT NOT NULL,
    state_json        TEXT NOT NULL,
    kernel_op_version TEXT,
    metadata_json     TEXT NOT NULL DEFAULT '{}',
    merkle_hash       TEXT NOT NULL CHECK (length(merkle_hash) = 64),
    PRIMARY KEY (branch_name, node_id),
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS idx_graph_nodes_merkle
    ON graph_nodes(merkle_hash);

CREATE TABLE IF NOT EXISTS graph_outputs (
    branch_name TEXT    NOT NULL,
    ordinal     INTEGER NOT NULL CHECK (ordinal >= 0),
    node_id     TEXT    NOT NULL,
    PRIMARY KEY (branch_name, ordinal),
    FOREIGN KEY (branch_name, node_id)
        REFERENCES graph_nodes(branch_name, node_id) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS graph_external_refs (
    branch_name     TEXT NOT NULL,
    ref_id          TEXT NOT NULL CHECK (length(ref_id) > 0),
    container_path  TEXT NOT NULL CHECK (length(trim(container_path)) > 0),
    expected_sha256 TEXT CHECK (expected_sha256 IS NULL OR length(expected_sha256) = 64),
    target_graph_id TEXT,
    payload_policy  TEXT NOT NULL DEFAULT 'eager'
                    CHECK (payload_policy IN ('eager', 'deferred_payload')),
    overrides_json  TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (branch_name, ref_id),
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS graph_blobs (
    branch_name   TEXT    NOT NULL,
    handle        TEXT    NOT NULL CHECK (length(handle) > 0),
    blob_kind     TEXT    NOT NULL CHECK (blob_kind IN ('spline', 'subd', 'field', 'import')),
    sha256        TEXT    NOT NULL CHECK (length(sha256) = 64),
    byte_len      INTEGER NOT NULL CHECK (byte_len >= 0),
    relative_path TEXT    NOT NULL,
    PRIMARY KEY (branch_name, handle),
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS engineering_meta (
    branch_name   TEXT    NOT NULL,
    category      TEXT    NOT NULL CHECK (category IN ('pmi', 'appearance', 'bom')),
    ordinal       INTEGER NOT NULL CHECK (ordinal >= 0),
    record_key    TEXT    NOT NULL,
    topo_ref_json TEXT,
    record_json   TEXT    NOT NULL,
    PRIMARY KEY (branch_name, category, ordinal),
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS idx_engineering_meta_lookup
    ON engineering_meta(branch_name, category, record_key);

CREATE TABLE IF NOT EXISTS drawing_sheets (
    branch_name     TEXT NOT NULL,
    sheet_id        TEXT NOT NULL CHECK (length(sheet_id) > 0),
    title           TEXT NOT NULL,
    paper_size      TEXT NOT NULL,
    views_json      TEXT NOT NULL DEFAULT '[]',
    dimensions_json TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (branch_name, sheet_id),
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS blobs (
    sha256           TEXT    PRIMARY KEY CHECK (length(sha256) = 64),
    handle           TEXT    NOT NULL,
    branch_name      TEXT    NOT NULL DEFAULT 'main',
    blob_kind        TEXT    NOT NULL
                     CHECK (blob_kind IN ('spline', 'subd', 'field', 'import', 'cadpack')),
    uncompressed_len INTEGER NOT NULL CHECK (uncompressed_len >= 0),
    stored_len       INTEGER NOT NULL CHECK (stored_len >= 0),
    compression      TEXT    NOT NULL DEFAULT 'zstd'
                     CHECK (compression IN ('none', 'zstd')),
    relative_path    TEXT    NOT NULL,
    created_at_ms    INTEGER NOT NULL,
    payload          BLOB    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS idx_blobs_sha256
    ON blobs(sha256);

CREATE TABLE IF NOT EXISTS node_brep_cache (
    merkle_hash      TEXT    NOT NULL CHECK (length(merkle_hash) = 64),
    kernel_id        TEXT    NOT NULL,
    kernel_version   TEXT    NOT NULL,
    compression      TEXT    NOT NULL DEFAULT 'zstd'
                     CHECK (compression IN ('none', 'zstd')),
    uncompressed_len INTEGER NOT NULL CHECK (uncompressed_len >= 0),
    is_output_pinned INTEGER NOT NULL DEFAULT 0 CHECK (is_output_pinned IN (0, 1)),
    created_at_ms    INTEGER NOT NULL,
    last_hit_at_ms   INTEGER NOT NULL,
    brep_payload     BLOB    NOT NULL,
    PRIMARY KEY (merkle_hash, kernel_id, kernel_version)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_node_brep_cache_lru
    ON node_brep_cache(is_output_pinned, last_hit_at_ms);

CREATE TABLE IF NOT EXISTS oplog (
    seq                INTEGER PRIMARY KEY AUTOINCREMENT,
    branch_name        TEXT    NOT NULL,
    parent_seq         INTEGER,
    patch_id           TEXT    NOT NULL,
    actor              TEXT    NOT NULL DEFAULT 'human_gui'
                       CHECK (actor IN ('human_gui', 'kcl_sync', 'python_sdk', 'ai_agent', 'system')),
    description        TEXT    NOT NULL,
    patch_json         TEXT    NOT NULL,
    inverse_patch_json TEXT,
    outputs_merkle     TEXT    NOT NULL,
    created_at_ms      INTEGER NOT NULL,
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS idx_oplog_branch_seq
    ON oplog(branch_name, seq);

CREATE TABLE IF NOT EXISTS bundled_refs (
    ref_id                  TEXT    PRIMARY KEY CHECK (length(ref_id) > 0),
    original_container_path TEXT    NOT NULL,
    container_sha256        TEXT    NOT NULL CHECK (length(container_sha256) = 64),
    source_format           TEXT    NOT NULL
                            CHECK (source_format IN ('sqlite', 'zip', 'sharded_dir', 'single_dir')),
    uncompressed_len        INTEGER NOT NULL CHECK (uncompressed_len >= 0),
    compression             TEXT    NOT NULL DEFAULT 'zstd'
                            CHECK (compression IN ('none', 'zstd')),
    bundled_at_ms           INTEGER NOT NULL,
    embedded_payload        BLOB    NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS whole_graph_brep_cache (
    branch_name      TEXT    PRIMARY KEY,
    kernel_id        TEXT    NOT NULL,
    kernel_version   TEXT    NOT NULL,
    graph_sha256     TEXT    NOT NULL CHECK (length(graph_sha256) = 64),
    compression      TEXT    NOT NULL DEFAULT 'zstd'
                     CHECK (compression IN ('none', 'zstd')),
    uncompressed_len INTEGER NOT NULL CHECK (uncompressed_len >= 0),
    brep_payload     BLOB    NOT NULL,
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS verification_reports (
    branch_name      TEXT    PRIMARY KEY,
    schema_version   INTEGER NOT NULL,
    overall_status   TEXT    NOT NULL
                     CHECK (overall_status IN ('PASS', 'FAIL', 'UNKNOWN')),
    kernel_provider  TEXT    NOT NULL,
    kernel_version   TEXT    NOT NULL,
    authoritative    INTEGER NOT NULL CHECK (authoritative IN (0, 1)),
    report_json      TEXT    NOT NULL,
    updated_at_ms    INTEGER NOT NULL,
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;
";

pub(super) const VERIFICATION_REPORTS_DDL: &str = "
CREATE TABLE IF NOT EXISTS verification_reports (
    branch_name      TEXT    PRIMARY KEY,
    schema_version   INTEGER NOT NULL,
    overall_status   TEXT    NOT NULL
                     CHECK (overall_status IN ('PASS', 'FAIL', 'UNKNOWN')),
    kernel_provider  TEXT    NOT NULL,
    kernel_version   TEXT    NOT NULL,
    authoritative    INTEGER NOT NULL CHECK (authoritative IN (0, 1)),
    report_json      TEXT    NOT NULL,
    updated_at_ms    INTEGER NOT NULL,
    FOREIGN KEY (branch_name) REFERENCES branches(branch_name) ON DELETE CASCADE
) STRICT;
";

pub(super) fn configure_journal_pragmas(
    conn: &Connection,
    path: &Path,
    policy: JournalPolicy,
) -> Result<(), ContainerError> {
    let target_mode = resolve_journal_mode_for_path(path, policy);
    if target_mode == "DELETE" {
        conn.execute_batch(
            "PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = FULL;
             PRAGMA busy_timeout = 10000;",
        )?;
    } else {
        conn.execute_batch(
            "PRAGMA locking_mode = NORMAL;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA wal_autocheckpoint = 1000;
             PRAGMA busy_timeout = 5000;",
        )?;
    }
    Ok(())
}

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use cad_ir::IrError;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    AicadProject, ContainerError, KernelFingerprint, LastKnownGoodCache, SQLITE_MAGIC,
    blobs::compute_sha256_hex,
};

use super::{
    BundledRefEntry, NodeBrepCacheEntry, SqliteContainer,
    blobs::{compress_payload, stream_table_blob},
    remove_wal_sidecars, unix_now_ms,
};

impl SqliteContainer {
    /// Insert or update a node-level Merkle B-Rep cache entry (compressed with Zstd).
    pub fn put_node_brep_cache(
        &mut self,
        merkle_hash: &str,
        kernel: &KernelFingerprint,
        brep_bytes: &[u8],
    ) -> Result<(), ContainerError> {
        self.put_node_brep_cache_pinned(merkle_hash, kernel, brep_bytes, false)
    }

    /// Insert or update a node-level Merkle B-Rep cache entry with optional output pinning (`is_output_pinned`).
    pub fn put_node_brep_cache_pinned(
        &mut self,
        merkle_hash: &str,
        kernel: &KernelFingerprint,
        brep_bytes: &[u8],
        is_output_pinned: bool,
    ) -> Result<(), ContainerError> {
        let normalized_hash = checked_hex_digest("node_brep_cache", merkle_hash)?;
        let (compression, stored_bytes) = compress_payload(brep_bytes)?;
        let now_ms = unix_now_ms();
        self.conn.execute(
            "INSERT INTO node_brep_cache (
                merkle_hash, kernel_id, kernel_version, compression,
                uncompressed_len, is_output_pinned, created_at_ms, last_hit_at_ms, brep_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(merkle_hash, kernel_id, kernel_version) DO UPDATE SET
                 compression = excluded.compression,
                 uncompressed_len = excluded.uncompressed_len,
                 is_output_pinned = MAX(node_brep_cache.is_output_pinned, excluded.is_output_pinned),
                 last_hit_at_ms = excluded.last_hit_at_ms,
                 brep_payload = excluded.brep_payload",
            params![
                normalized_hash,
                kernel.kernel_id,
                kernel.kernel_version,
                compression,
                brep_bytes.len() as i64,
                i32::from(is_output_pinned),
                now_ms,
                now_ms,
                stored_bytes,
            ],
        )?;
        Ok(())
    }

    /// Look up and stream-decompress a cached B-Rep by `(merkle_hash, kernel)`, updating `last_hit_at_ms` on hit.
    pub fn get_node_brep_cache(
        &self,
        merkle_hash: &str,
        kernel: &KernelFingerprint,
    ) -> Result<Option<Vec<u8>>, ContainerError> {
        Ok(self
            .get_node_brep_cache_entry(merkle_hash, kernel)?
            .map(|entry| entry.brep_bytes))
    }

    /// Look up the full `NodeBrepCacheEntry` by `(merkle_hash, kernel)`.
    pub fn get_node_brep_cache_entry(
        &self,
        merkle_hash: &str,
        kernel: &KernelFingerprint,
    ) -> Result<Option<NodeBrepCacheEntry>, ContainerError> {
        let normalized_hash = checked_hex_digest("node_brep_cache", merkle_hash)?;
        let row: Option<(i64, String, i64, i32)> = self
            .conn
            .query_row(
                "SELECT rowid, compression, uncompressed_len, is_output_pinned
                 FROM node_brep_cache
                 WHERE merkle_hash = ?1 AND kernel_id = ?2 AND kernel_version = ?3",
                params![normalized_hash, kernel.kernel_id, kernel.kernel_version],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;

        let Some((rowid, compression, uncompressed_len, pinned)) = row else {
            return Ok(None);
        };

        let mut brep_bytes = Vec::with_capacity(uncompressed_len as usize);
        stream_table_blob(
            &self.conn,
            "node_brep_cache",
            "brep_payload",
            rowid,
            &compression,
            uncompressed_len as u64,
            &mut brep_bytes,
        )?;

        let _ = self.conn.execute(
            "UPDATE node_brep_cache SET last_hit_at_ms = ?4
             WHERE merkle_hash = ?1 AND kernel_id = ?2 AND kernel_version = ?3",
            params![
                normalized_hash,
                kernel.kernel_id,
                kernel.kernel_version,
                unix_now_ms(),
            ],
        );

        Ok(Some(NodeBrepCacheEntry {
            merkle_hash: normalized_hash,
            kernel: kernel.clone(),
            uncompressed_len: uncompressed_len as u64,
            is_output_pinned: pinned != 0,
            brep_bytes,
        }))
    }

    /// Return the total number of rows currently stored in `node_brep_cache`.
    pub fn count_node_brep_cache(&self) -> Result<usize, ContainerError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM node_brep_cache", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// List metadata for all self-contained external sub-containers bundled in `bundled_refs`.
    pub fn list_bundled_refs(&self) -> Result<Vec<BundledRefEntry>, ContainerError> {
        let mut stmt = self.conn.prepare(
            "SELECT ref_id, original_container_path, container_sha256, source_format,
                    uncompressed_len, bundled_at_ms
             FROM bundled_refs
             ORDER BY ref_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(BundledRefEntry {
                ref_id: row.get(0)?,
                original_container_path: row.get(1)?,
                container_sha256: row.get(2)?,
                source_format: row.get(3)?,
                uncompressed_len: row.get::<_, i64>(4)? as u64,
                bundled_at_ms: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Stream, decompress, and SHA-256 verify the raw bytes of an inline `bundled_refs` entry.
    pub fn get_bundled_ref_bytes(&self, ref_id: &str) -> Result<Option<Vec<u8>>, ContainerError> {
        let row: Option<(i64, String, String, i64, String)> = self
            .conn
            .query_row(
                "SELECT rowid, original_container_path, container_sha256, uncompressed_len, compression
                 FROM bundled_refs WHERE ref_id = ?1",
                params![ref_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;

        let Some((rowid, orig_path, expected_sha, uncompressed_len, compression)) = row else {
            return Ok(None);
        };

        let mut bytes = Vec::with_capacity(uncompressed_len as usize);
        stream_table_blob(
            &self.conn,
            "bundled_refs",
            "embedded_payload",
            rowid,
            &compression,
            uncompressed_len as u64,
            &mut bytes,
        )?;
        let actual_sha = compute_sha256_hex(&bytes);
        if actual_sha != expected_sha {
            return Err(ContainerError::ExternalRefHashMismatch {
                ref_id: ref_id.to_owned(),
                path: orig_path,
                expected: expected_sha,
                actual: actual_sha,
            });
        }
        Ok(Some(bytes))
    }

    /// Reconstruct an embedded child `AicadProject` directly from `bundled_refs` (offline zero-broken-link access).
    pub fn load_bundled_ref_project(
        &self,
        ref_id: &str,
    ) -> Result<Option<AicadProject>, ContainerError> {
        let source_format: Option<String> = self
            .conn
            .query_row(
                "SELECT source_format FROM bundled_refs WHERE ref_id = ?1",
                params![ref_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(source_format) = source_format else {
            return Ok(None);
        };
        let bytes = self
            .get_bundled_ref_bytes(ref_id)?
            .expect("bundled_refs row existed");

        if bytes.len() >= SQLITE_MAGIC.len() && &bytes[..SQLITE_MAGIC.len()] == SQLITE_MAGIC {
            let temp_path = temp_sqlite_extract_path(ref_id);
            fs::write(&temp_path, &bytes)?;
            let loaded = AicadProject::load_sqlite(&temp_path);
            let _ = fs::remove_file(&temp_path);
            remove_wal_sidecars(&temp_path);
            Ok(Some(loaded?))
        } else {
            Err(ContainerError::UnsupportedFormat(format!(
                "bundled ref `{ref_id}` (source_format `{source_format}`) is not a SQLite container; legacy ZIP archives are retired"
            )))
        }
    }

    pub(crate) fn insert_bundled_ref(
        &mut self,
        ref_id: &str,
        original_container_path: &str,
        container_sha256: &str,
        source_format: &str,
        raw_bytes: &[u8],
    ) -> Result<BundledRefEntry, ContainerError> {
        let (compression, stored) = compress_payload(raw_bytes)?;
        let now_ms = unix_now_ms();
        self.conn.execute(
            "INSERT INTO bundled_refs (
                ref_id, original_container_path, container_sha256, source_format,
                uncompressed_len, compression, bundled_at_ms, embedded_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(ref_id) DO UPDATE SET
                 original_container_path = excluded.original_container_path,
                 container_sha256 = excluded.container_sha256,
                 source_format = excluded.source_format,
                 uncompressed_len = excluded.uncompressed_len,
                 compression = excluded.compression,
                 bundled_at_ms = excluded.bundled_at_ms,
                 embedded_payload = excluded.embedded_payload",
            params![
                ref_id,
                original_container_path,
                container_sha256,
                source_format,
                raw_bytes.len() as i64,
                compression,
                now_ms,
                stored,
            ],
        )?;
        Ok(BundledRefEntry {
            ref_id: ref_id.to_owned(),
            original_container_path: original_container_path.to_owned(),
            container_sha256: container_sha256.to_owned(),
            source_format: source_format.to_owned(),
            uncompressed_len: raw_bytes.len() as u64,
            bundled_at_ms: now_ms,
        })
    }
}

pub(crate) fn load_whole_graph_cache_metadata_from_conn(
    conn: &Connection,
    branch_name: &str,
) -> Result<Option<(KernelFingerprint, String)>, ContainerError> {
    conn.query_row(
        "SELECT kernel_id, kernel_version, graph_sha256
         FROM whole_graph_brep_cache WHERE branch_name = ?1",
        params![branch_name],
        |row| {
            Ok((
                KernelFingerprint {
                    kernel_id: row.get(0)?,
                    kernel_version: row.get(1)?,
                },
                row.get(2)?,
            ))
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn load_whole_graph_cache_from_conn(
    conn: &Connection,
    branch_name: &str,
) -> Result<Option<LastKnownGoodCache>, ContainerError> {
    let row: Option<(i64, String, String, String, String, i64)> = conn
        .query_row(
            "SELECT rowid, kernel_id, kernel_version, graph_sha256, compression, uncompressed_len
             FROM whole_graph_brep_cache WHERE branch_name = ?1",
            params![branch_name],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;

    let Some((rowid, kernel_id, kernel_version, graph_sha256, compression, uncompressed_len)) = row
    else {
        return Ok(None);
    };

    let mut brep_bytes = Vec::with_capacity(uncompressed_len as usize);
    stream_table_blob(
        conn,
        "whole_graph_brep_cache",
        "brep_payload",
        rowid,
        &compression,
        uncompressed_len as u64,
        &mut brep_bytes,
    )?;

    Ok(Some(LastKnownGoodCache {
        kernel: KernelFingerprint {
            kernel_id,
            kernel_version,
        },
        graph_sha256,
        brep_bytes,
    }))
}

fn checked_hex_digest(entity: &'static str, digest: &str) -> Result<String, ContainerError> {
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ContainerError::Ir(IrError::InvalidSha256 {
            entity,
            id: entity.to_owned(),
            sha256: digest.to_owned(),
        }));
    }
    Ok(digest.to_ascii_lowercase())
}

fn temp_sqlite_extract_path(ref_id: &str) -> PathBuf {
    static NEXT_EXTRACT: AtomicU64 = AtomicU64::new(0);
    let seq = NEXT_EXTRACT.fetch_add(1, Ordering::Relaxed);
    let sanitized: String = ref_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!(
        "aicad-bundled-{sanitized}-{}-{}-{seq}.sqlite",
        std::process::id(),
        unix_now_ms()
    ))
}

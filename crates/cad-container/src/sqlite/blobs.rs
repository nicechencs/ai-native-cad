use std::io::{self, Cursor, Read, Write};

use cad_ir::{BlobHandle, BlobKind, BlobManifestEntry};
use rusqlite::{Connection, DatabaseName, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{ContainerError, blobs::compute_sha256_hex};

use super::{SqliteContainer, branches::ensure_branch_exists, unix_now_ms};

const STREAM_CHUNK_BYTES: usize = 64 * 1024;
const ZSTD_LEVEL: i32 = 3;

impl SqliteContainer {
    /// Insert a content-addressed binary blob into `blobs` and register it on `branch_opt` (or active branch).
    pub fn put_blob(
        &mut self,
        handle: BlobHandle,
        kind: BlobKind,
        bytes: &[u8],
        branch_opt: Option<&str>,
    ) -> Result<BlobManifestEntry, ContainerError> {
        let target_branch = match branch_opt {
            Some(name) => name.to_owned(),
            None => self.active_branch()?,
        };
        let digest = compute_sha256_hex(bytes);
        let directory = match kind {
            BlobKind::Spline => "splines",
            BlobKind::SubD => "subd",
            BlobKind::Field => "fields",
            BlobKind::Import => "imports",
        };
        let relative_path = format!("blobs/{directory}/{digest}.bin");
        let entry = BlobManifestEntry {
            handle: handle.clone(),
            kind,
            sha256: digest.clone(),
            byte_len: bytes.len() as u64,
            relative_path: relative_path.clone(),
        };

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_branch_exists(&tx, &target_branch)?;
        upsert_content_addressed_blob(
            &tx,
            &digest,
            &handle.0,
            &target_branch,
            blob_kind_str(kind),
            &relative_path,
            bytes,
        )?;
        tx.execute(
            "INSERT INTO graph_blobs (branch_name, handle, blob_kind, sha256, byte_len, relative_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(branch_name, handle) DO UPDATE SET
                 blob_kind = excluded.blob_kind,
                 sha256 = excluded.sha256,
                 byte_len = excluded.byte_len,
                 relative_path = excluded.relative_path",
            params![
                target_branch,
                handle.0,
                blob_kind_str(kind),
                digest,
                bytes.len() as i64,
                relative_path,
            ],
        )?;
        tx.commit()?;
        Ok(entry)
    }

    /// Return the number of unique content-addressed payloads currently stored in `blobs`.
    pub fn count_stored_blobs(&self) -> Result<usize, ContainerError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))?;
        Ok(count as usize)
    }
}

pub(crate) fn stream_blob_by_sha256<W: Write>(
    conn: &Connection,
    expected_sha256: &str,
    expected_len: u64,
    display_path: &str,
    writer: &mut W,
) -> Result<u64, ContainerError> {
    let row: Option<(i64, String, i64, String)> = conn
        .query_row(
            "SELECT rowid, compression, uncompressed_len, sha256 FROM blobs WHERE sha256 = ?1",
            params![expected_sha256],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;

    let Some((rowid, compression, uncompressed_len, recorded_sha)) = row else {
        return Err(ContainerError::MissingBlob(display_path.to_owned()));
    };
    if uncompressed_len as u64 != expected_len || recorded_sha != expected_sha256 {
        return Err(ContainerError::BlobHashMismatch {
            path: display_path.to_owned(),
        });
    }

    let mut hasher = HashingWriter {
        inner: writer,
        hasher: Sha256::new(),
        written: 0,
    };
    stream_table_blob(
        conn,
        "blobs",
        "payload",
        rowid,
        &compression,
        expected_len,
        &mut hasher,
    )?;
    let actual_sha = format!("{:x}", hasher.hasher.finalize());
    if hasher.written != expected_len || actual_sha != expected_sha256 {
        return Err(ContainerError::BlobHashMismatch {
            path: display_path.to_owned(),
        });
    }
    Ok(hasher.written)
}

pub(crate) fn read_raw_blob_by_sha256(
    conn: &Connection,
    sha256: &str,
) -> Result<Vec<u8>, ContainerError> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT rowid, uncompressed_len FROM blobs WHERE sha256 = ?1",
            params![sha256],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((_, uncompressed_len)) = row else {
        return Err(ContainerError::MissingBlob(sha256.to_owned()));
    };
    let mut out = Vec::with_capacity(uncompressed_len as usize);
    stream_blob_by_sha256(conn, sha256, uncompressed_len as u64, sha256, &mut out)?;
    Ok(out)
}

pub(super) fn stream_table_blob<W: Write>(
    conn: &Connection,
    table: &str,
    column: &str,
    rowid: i64,
    compression: &str,
    expected_uncompressed_len: u64,
    writer: &mut W,
) -> Result<u64, ContainerError> {
    if expected_uncompressed_len == 0 && compression == "none" {
        return Ok(0);
    }

    let blob = conn.blob_open(DatabaseName::Main, table, column, rowid, true)?;
    let mut written = 0_u64;
    let mut buf = [0_u8; STREAM_CHUNK_BYTES];

    match compression {
        "none" => {
            let mut reader = blob;
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n])?;
                written += n as u64;
            }
        }
        "zstd" => {
            let mut decoder = zstd::stream::read::Decoder::new(blob)?;
            loop {
                let n = decoder.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                writer.write_all(&buf[..n])?;
                written += n as u64;
            }
        }
        other => {
            return Err(ContainerError::UnsupportedFormat(format!(
                "unsupported SQLite blob compression `{other}`"
            )));
        }
    }

    Ok(written)
}

struct HashingWriter<'a, W> {
    inner: &'a mut W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.hasher.update(buf);
        self.written += buf.len() as u64;
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(super) fn upsert_content_addressed_blob(
    conn: &Connection,
    sha256: &str,
    handle: &str,
    branch_name: &str,
    blob_kind: &str,
    relative_path: &str,
    raw_bytes: &[u8],
) -> Result<(), ContainerError> {
    upsert_content_addressed_blob_at(
        conn,
        sha256,
        handle,
        branch_name,
        blob_kind,
        relative_path,
        raw_bytes,
        unix_now_ms(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn upsert_content_addressed_blob_at(
    conn: &Connection,
    sha256: &str,
    handle: &str,
    branch_name: &str,
    blob_kind: &str,
    relative_path: &str,
    raw_bytes: &[u8],
    created_at_ms: i64,
) -> Result<(), ContainerError> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM blobs WHERE sha256 = ?1",
            params![sha256],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(());
    }

    let (compression, stored) = compress_payload(raw_bytes)?;
    conn.execute(
        "INSERT INTO blobs (
            sha256, handle, branch_name, blob_kind, uncompressed_len,
            stored_len, compression, relative_path, created_at_ms, payload
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            sha256,
            handle,
            branch_name,
            blob_kind,
            raw_bytes.len() as i64,
            stored.len() as i64,
            compression,
            relative_path,
            created_at_ms,
            stored,
        ],
    )?;
    Ok(())
}

pub(super) fn compress_payload(bytes: &[u8]) -> Result<(&'static str, Vec<u8>), ContainerError> {
    if bytes.len() < 64 {
        return Ok(("none", bytes.to_vec()));
    }
    let compressed = zstd::stream::encode_all(Cursor::new(bytes), ZSTD_LEVEL)?;
    if compressed.len() < bytes.len() {
        Ok(("zstd", compressed))
    } else {
        Ok(("none", bytes.to_vec()))
    }
}

pub(super) const fn blob_kind_str(kind: BlobKind) -> &'static str {
    match kind {
        BlobKind::Spline => "spline",
        BlobKind::SubD => "subd",
        BlobKind::Field => "field",
        BlobKind::Import => "import",
    }
}

pub(super) fn parse_blob_kind(s: &str) -> Result<BlobKind, ContainerError> {
    match s {
        "spline" => Ok(BlobKind::Spline),
        "subd" => Ok(BlobKind::SubD),
        "field" => Ok(BlobKind::Field),
        "import" => Ok(BlobKind::Import),
        other => Err(ContainerError::UnsupportedFormat(format!(
            "unknown blob kind `{other}`"
        ))),
    }
}

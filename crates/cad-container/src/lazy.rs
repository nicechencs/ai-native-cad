//! Lightweight metadata-first lazy reader (`LazyProjectReader`) with on-demand streaming SHA-256 verification.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use cad_ir::{BlobManifestEntry, CanonicalGraph, VerificationReport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CacheStatus, ContainerError, ContainerManifest, DrawingSheet, EngineeringMetadata,
    KclProjectionPolicy, KernelFingerprint, LastKnownGoodCache, SqliteContainer, StorageKind,
    blobs::checked_blob_path,
    cache::evaluate_cache_status,
    detect::detect_storage_kind,
    directory_store::{
        CACHE_BREP, CACHE_METADATA, CADPACK, CacheMetadata, DEFAULT_ENTRY_GRAPH, ENGINEERING,
        MANIFEST, VERIFICATION_REPORT, commit, directory_drawings, directory_path,
        directory_shards, drawing_path, optional_io, required, validate_file_tree,
        validate_manifest_paths,
    },
    manifest::{self, SHARDED_ENTRY_GRAPH_PATH},
    sharded,
    sqlite::{
        load_branch_graph_from_conn, load_drawings_from_conn, load_engineering_from_conn,
        load_verification_report_from_conn, load_whole_graph_cache_from_conn,
        load_whole_graph_cache_metadata_from_conn, read_meta_row, read_raw_blob_by_sha256,
        stream_blob_by_sha256,
    },
};

const STREAM_BUF_SIZE: usize = 64 * 1024;

/// Summary produced by `LazyProjectReader::inspect_summary` without reading binary blob payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInspectionSummary {
    pub storage_kind: StorageKind,
    pub project_name: String,
    pub format_version: u32,
    pub ir_schema_version: u32,
    pub active_branch: String,
    pub node_count: usize,
    pub parameter_count: usize,
    pub blob_count: usize,
    pub external_ref_count: usize,
    pub pmi_count: usize,
    pub appearance_count: usize,
    pub bom_count: usize,
    pub drawing_count: usize,
    pub has_cadpack: bool,
    pub node_brep_cache_count: usize,
    pub bundled_ref_count: usize,
    pub cache_status: CacheStatus,
}

enum LazyBackend {
    Directory {
        dir: PathBuf,
    },
    Sqlite {
        container: SqliteContainer,
        cadpack_sha256: Option<String>,
    },
}

/// Metadata-first project reader that defers loading binary blobs, B-Rep caches, and `.cadpack` payloads.
pub struct LazyProjectReader {
    path: PathBuf,
    storage_kind: StorageKind,
    active_branch: String,
    manifest: ContainerManifest,
    graph: CanonicalGraph,
    kcl_view: Option<String>,
    engineering: EngineeringMetadata,
    drawings: BTreeMap<String, DrawingSheet>,
    verification_report: Option<VerificationReport>,
    cache_meta: Option<(KernelFingerprint, String)>,
    has_cadpack: bool,
    node_brep_cache_count: usize,
    bundled_ref_count: usize,
    backend: LazyBackend,
}

impl LazyProjectReader {
    /// Open any `.aicad` container path lazily, loading only metadata, canonical graph, engineering, and drawings.
    pub fn open(path: &Path) -> Result<Self, ContainerError> {
        let storage_kind = detect_storage_kind(path)?;
        match storage_kind {
            StorageKind::UnpackedSingleGraphDir => Self::open_directory(path, false),
            StorageKind::UnpackedShardedDir => Self::open_directory(path, true),
            StorageKind::SqliteContainer => Self::open_sqlite(path),
        }
    }

    fn open_directory(dir: &Path, sharded: bool) -> Result<Self, ContainerError> {
        let _guard = commit::DIRECTORY_IO
            .read()
            .map_err(|_| commit::poisoned())?;
        let mut read = |rel: &str| -> Result<Option<Vec<u8>>, ContainerError> {
            let p = directory_path(dir, rel, false)?;
            optional_io(fs::read(p))
        };

        let mut manifest =
            manifest::parse_manifest(&String::from_utf8(required(&mut read, MANIFEST)?)?)?;
        let graph = if sharded {
            let is_sharded_entry = manifest
                .entry_graph
                .eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH);
            validate_manifest_paths(&manifest, is_sharded_entry)?;
            let shard_paths = directory_shards(dir)?;
            let header_json = String::from_utf8(required(&mut read, SHARDED_ENTRY_GRAPH_PATH)?)?;
            let mut shard_docs = Vec::new();
            for shard_path in shard_paths {
                if shard_path.eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH) {
                    continue;
                }
                let shard_json = String::from_utf8(required(&mut read, &shard_path)?)?;
                shard_docs.push((shard_path, shard_json));
            }
            let merged = sharded::merge_sharded_graph(&header_json, &shard_docs)?;
            if is_sharded_entry {
                manifest.entry_graph = DEFAULT_ENTRY_GRAPH.to_owned();
            }
            merged
        } else {
            validate_manifest_paths(&manifest, false)?;
            let graph_json = String::from_utf8(required(&mut read, &manifest.entry_graph)?)?;
            manifest::parse_graph(&graph_json)?
        };

        if manifest.kcl_projection_policy == KclProjectionPolicy::OnDemandDynamic {
            manifest.kcl_view = None;
        }
        let kcl_view = manifest
            .kcl_view
            .as_deref()
            .map(|rel| String::from_utf8(required(&mut read, rel)?).map_err(ContainerError::from))
            .transpose()?;

        let engineering = match read(ENGINEERING)? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => EngineeringMetadata::default(),
        };

        let drawing_paths = directory_drawings(dir)?;
        validate_file_tree(drawing_paths.iter().map(String::as_str))?;
        let mut drawings = BTreeMap::new();
        for rel in drawing_paths {
            let sheet: DrawingSheet = serde_json::from_slice(&required(&mut read, &rel)?)?;
            if drawing_path(&sheet.sheet_id)? != rel {
                return Err(ContainerError::DrawingIdMismatch {
                    path: rel,
                    sheet_id: sheet.sheet_id,
                });
            }
            drawings.insert(sheet.sheet_id.clone(), sheet);
        }

        validate_blob_manifest_paths(&graph)?;

        let brep_exists = directory_path(dir, CACHE_BREP, false)?.is_file();
        let cache_meta = if brep_exists && let Some(meta_bytes) = read(CACHE_METADATA)? {
            let decoded: CacheMetadata<KernelFingerprint, String> =
                serde_json::from_slice(&meta_bytes)?;
            Some((decoded.kernel, decoded.graph_sha256))
        } else {
            None
        };
        let has_cadpack = directory_path(dir, CADPACK, false)?.is_file();
        let verification_report = match read(VERIFICATION_REPORT)? {
            Some(bytes) => Some(serde_json::from_slice(&bytes)?),
            None => None,
        };

        Ok(Self {
            path: dir.to_path_buf(),
            storage_kind: if sharded {
                StorageKind::UnpackedShardedDir
            } else {
                StorageKind::UnpackedSingleGraphDir
            },
            active_branch: "main".to_owned(),
            manifest,
            graph,
            kcl_view,
            engineering,
            drawings,
            verification_report,
            cache_meta,
            has_cadpack,
            node_brep_cache_count: 0,
            bundled_ref_count: 0,
            backend: LazyBackend::Directory {
                dir: dir.to_path_buf(),
            },
        })
    }

    fn open_sqlite(path: &Path) -> Result<Self, ContainerError> {
        let container = SqliteContainer::open(path)?;
        let conn = container.connection();
        let meta = read_meta_row(conn)?;
        let graph = load_branch_graph_from_conn(conn, &meta.active_branch)?;
        let engineering = load_engineering_from_conn(conn, &meta.active_branch)?;
        let drawings = load_drawings_from_conn(conn, &meta.active_branch)?;

        validate_blob_manifest_paths(&graph)?;

        let mut manifest = ContainerManifest {
            format_version: meta.format_version,
            ir_schema_version: meta.ir_schema_version,
            project_name: meta.project_name,
            created_with_kernel: meta.kernel,
            entry_graph: meta.entry_graph,
            kcl_view: meta.kcl_view_path,
            kcl_projection_policy: meta.kcl_projection_policy,
        };
        let kcl_view = if manifest.kcl_projection_policy == KclProjectionPolicy::OnDemandDynamic {
            manifest.kcl_view = None;
            None
        } else {
            meta.kcl_view_text
        };

        let cache_meta = load_whole_graph_cache_metadata_from_conn(conn, &meta.active_branch)?;
        let verification_report = load_verification_report_from_conn(conn, &meta.active_branch)?;
        let node_brep_cache_count = container.count_node_brep_cache()?;
        let bundled_ref_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM bundled_refs", [], |r| r.get(0))?;
        let has_cadpack = meta.cadpack_sha256.is_some();

        Ok(Self {
            path: path.to_path_buf(),
            storage_kind: StorageKind::SqliteContainer,
            active_branch: meta.active_branch,
            manifest,
            graph,
            kcl_view,
            engineering,
            drawings,
            verification_report,
            cache_meta,
            has_cadpack,
            node_brep_cache_count,
            bundled_ref_count: bundled_ref_count as usize,
            backend: LazyBackend::Sqlite {
                container,
                cadpack_sha256: meta.cadpack_sha256,
            },
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn storage_kind(&self) -> StorageKind {
        self.storage_kind
    }

    pub fn active_branch(&self) -> &str {
        &self.active_branch
    }

    pub const fn manifest(&self) -> &ContainerManifest {
        &self.manifest
    }

    pub const fn graph(&self) -> &CanonicalGraph {
        &self.graph
    }

    pub fn kcl_view(&self) -> Option<&str> {
        self.kcl_view.as_deref()
    }

    pub const fn engineering(&self) -> &EngineeringMetadata {
        &self.engineering
    }

    pub const fn drawings(&self) -> &BTreeMap<String, DrawingSheet> {
        &self.drawings
    }

    pub const fn verification_report(&self) -> Option<&VerificationReport> {
        self.verification_report.as_ref()
    }

    pub const fn has_cadpack(&self) -> bool {
        self.has_cadpack
    }

    /// Evaluate whole-graph cache freshness using metadata alone without reading `last_known_good.brep`.
    pub fn check_cache_status(&self, current_kernel: &KernelFingerprint) -> CacheStatus {
        evaluate_cache_status(
            &self.graph,
            self.cache_meta.as_ref().map(|(k, sha)| (k, sha.as_str())),
            current_kernel,
        )
    }

    /// Produce a zero-blob-allocation inspection summary of the project.
    pub fn inspect_summary(
        &self,
        current_kernel: &KernelFingerprint,
    ) -> ProjectInspectionSummary {
        ProjectInspectionSummary {
            storage_kind: self.storage_kind,
            project_name: self.manifest.project_name.clone(),
            format_version: self.manifest.format_version,
            ir_schema_version: self.manifest.ir_schema_version,
            active_branch: self.active_branch.clone(),
            node_count: self.graph.nodes.len(),
            parameter_count: self.graph.parameters.len(),
            blob_count: self.graph.blobs.len(),
            external_ref_count: self.graph.external_refs.len(),
            pmi_count: self.engineering.pmi.len(),
            appearance_count: self.engineering.appearances.len(),
            bom_count: self.engineering.bom.len(),
            drawing_count: self.drawings.len(),
            has_cadpack: self.has_cadpack,
            node_brep_cache_count: self.node_brep_cache_count,
            bundled_ref_count: self.bundled_ref_count,
            cache_status: self.check_cache_status(current_kernel),
        }
    }

    /// Read and SHA-256 verify a single blob on demand by `BlobHandle` name or `relative_path`.
    pub fn read_blob(&self, handle_or_path: &str) -> Result<Vec<u8>, ContainerError> {
        let entry = self.resolve_blob_entry(handle_or_path)?;
        let mut out = Vec::with_capacity(entry.byte_len as usize);
        self.stream_blob_entry(entry, &mut out)?;
        Ok(out)
    }

    /// Stream a single blob in 64 KiB chunks into `writer` while verifying `byte_len` and SHA-256 on the fly.
    pub fn stream_blob<W: Write>(
        &self,
        handle_or_path: &str,
        writer: &mut W,
    ) -> Result<u64, ContainerError> {
        let entry = self.resolve_blob_entry(handle_or_path)?;
        self.stream_blob_entry(entry, writer)
    }

    /// Read `cache/scene.cadpack` on demand if present.
    pub fn read_cadpack(&self) -> Result<Option<Vec<u8>>, ContainerError> {
        if !self.has_cadpack {
            return Ok(None);
        }
        match &self.backend {
            LazyBackend::Directory { dir } => {
                let _guard = commit::DIRECTORY_IO
                    .read()
                    .map_err(|_| commit::poisoned())?;
                let p = directory_path(dir, CADPACK, false)?;
                optional_io(fs::read(p))
            }
            LazyBackend::Sqlite {
                container,
                cadpack_sha256,
            } => match cadpack_sha256 {
                Some(sha) => Ok(Some(read_raw_blob_by_sha256(container.connection(), sha)?)),
                None => Ok(None),
            },
        }
    }

    /// Read `LastKnownGoodCache` (`last_known_good.brep`) on demand if present.
    pub fn read_last_known_good(&self) -> Result<Option<LastKnownGoodCache>, ContainerError> {
        let Some((kernel, graph_sha256)) = &self.cache_meta else {
            return Ok(None);
        };
        match &self.backend {
            LazyBackend::Directory { dir } => {
                let _guard = commit::DIRECTORY_IO
                    .read()
                    .map_err(|_| commit::poisoned())?;
                let p = directory_path(dir, CACHE_BREP, false)?;
                let Some(brep_bytes) = optional_io(fs::read(p))? else {
                    return Ok(None);
                };
                Ok(Some(LastKnownGoodCache {
                    kernel: kernel.clone(),
                    graph_sha256: graph_sha256.clone(),
                    brep_bytes,
                }))
            }
            LazyBackend::Sqlite { container, .. } => {
                load_whole_graph_cache_from_conn(container.connection(), &self.active_branch)
            }
        }
    }

    fn resolve_blob_entry(&self, handle_or_path: &str) -> Result<&BlobManifestEntry, ContainerError> {
        if let Some(entry) = self.graph.blobs.get(handle_or_path) {
            return Ok(entry);
        }
        if let Some(entry) = self
            .graph
            .blobs
            .values()
            .find(|entry| entry.relative_path == handle_or_path)
        {
            return Ok(entry);
        }
        if handle_or_path.contains('/') || handle_or_path.contains('\\') || handle_or_path.contains(':') {
            checked_blob_path(handle_or_path)?;
        }
        Err(ContainerError::MissingBlob(handle_or_path.to_owned()))
    }

    fn stream_blob_entry<W: Write>(
        &self,
        entry: &BlobManifestEntry,
        writer: &mut W,
    ) -> Result<u64, ContainerError> {
        let rel_path = checked_blob_path(&entry.relative_path)?;
        match &self.backend {
            LazyBackend::Directory { dir } => {
                let _guard = commit::DIRECTORY_IO
                    .read()
                    .map_err(|_| commit::poisoned())?;
                let target = directory_path(dir, &rel_path, false)?;
                let file = match File::open(&target) {
                    Ok(f) => f,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        return Err(ContainerError::MissingBlob(rel_path));
                    }
                    Err(e) => return Err(e.into()),
                };
                stream_reader_with_sha256(file, entry, &rel_path, writer)
            }
            LazyBackend::Sqlite { container, .. } => stream_blob_by_sha256(
                container.connection(),
                &entry.sha256,
                entry.byte_len,
                &rel_path,
                writer,
            ),
        }
    }
}

fn validate_blob_manifest_paths(graph: &CanonicalGraph) -> Result<(), ContainerError> {
    let mut unique_paths = BTreeMap::new();
    for blob in graph.blobs.values() {
        let path = checked_blob_path(&blob.relative_path)?;
        unique_paths.insert(path, ());
    }
    validate_file_tree(unique_paths.keys().map(String::as_str))
}

fn stream_reader_with_sha256<R: Read, W: Write>(
    mut reader: R,
    entry: &BlobManifestEntry,
    display_path: &str,
    writer: &mut W,
) -> Result<u64, ContainerError> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buf = [0_u8; STREAM_BUF_SIZE];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
        total += n as u64;
    }

    let actual_sha = format!("{:x}", hasher.finalize());
    if total != entry.byte_len || actual_sha != entry.sha256 {
        return Err(ContainerError::BlobHashMismatch {
            path: display_path.to_owned(),
        });
    }
    Ok(total)
}

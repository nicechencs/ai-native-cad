use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fs,
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    AicadProject, ContainerError, ContainerManifest, DrawingSheet, KclProjectionPolicy,
    KernelFingerprint, LastKnownGoodCache, StorageKind,
    blobs::{checked_blob_path, normalize_relative_path},
    detect::{detect_storage_kind, is_reparse_or_symlink},
    manifest::{self, DEFAULT_KCL_VIEW_PATH, SHARDED_ENTRY_GRAPH_PATH},
    sharded,
};

pub(crate) mod commit;

pub(crate) const MANIFEST: &str = "manifest.json";
pub(crate) const ENGINEERING: &str = "engineering.json";
pub(crate) const VERIFICATION_REPORT: &str = "verification.report.json";
pub(crate) const DRAWINGS: &str = "drawings";
pub(crate) const DRAWING_SUFFIX: &str = ".drawing.json";
pub(crate) const GRAPHS_DIR: &str = "graphs";
pub(crate) const GRAPH_SHARD_SUFFIX: &str = ".ir.json";
pub(crate) const CACHE_BREP: &str = "cache/last_known_good.brep";
pub(crate) const CACHE_METADATA: &str = "cache/last_known_good.json";
pub(crate) const CADPACK: &str = "cache/scene.cadpack";
pub(crate) const DEFAULT_ENTRY_GRAPH: &str = "graph.ir.json";

type Entries<'a> = BTreeMap<String, Cow<'a, [u8]>>;

#[derive(Serialize, Deserialize)]
pub(crate) struct CacheMetadata<K, H> {
    pub(crate) kernel: K,
    pub(crate) graph_sha256: H,
}

impl AicadProject {
    /// Automatically detect the underlying container format (`SqliteContainer`, `UnpackedShardedDir`,
    /// or `UnpackedSingleGraphDir`) and load the full project.
    pub fn load_any(path: &Path) -> Result<Self, ContainerError> {
        match detect_storage_kind(path)? {
            StorageKind::UnpackedSingleGraphDir => Self::load_directory(path),
            StorageKind::UnpackedShardedDir => Self::load_sharded_directory(path),
            StorageKind::SqliteContainer => Self::load_sqlite(path),
        }
    }

    /// Save the project as an unpacked single-graph directory, creating it if needed.
    pub fn save_directory(&self, dir: &Path) -> Result<(), ContainerError> {
        let mut entries = self.entries_single_graph()?;
        let _guard = commit::DIRECTORY_IO
            .write()
            .map_err(|_| commit::poisoned())?;
        let old_drawings = directory_drawings(dir)?;
        let old_shards = directory_shards(dir)?;
        let mut stale = Vec::new();
        for path in old_drawings
            .iter()
            .map(String::as_str)
            .chain(old_shards.iter().map(String::as_str))
            .chain([
                CACHE_BREP,
                CACHE_METADATA,
                CADPACK,
                VERIFICATION_REPORT,
                DEFAULT_KCL_VIEW_PATH,
                DEFAULT_ENTRY_GRAPH,
            ])
        {
            let will_be_written = entries.contains_key(path)
                || (cfg!(windows) && entries.keys().any(|entry| entry.eq_ignore_ascii_case(path)));
            if !will_be_written {
                let target = directory_path(dir, path, false)?;
                if target.is_file() {
                    stale.push((target, None));
                }
            }
        }
        let manifest = entries
            .remove(MANIFEST)
            .expect("entries include the manifest");
        let manifest_path = directory_path(dir, MANIFEST, false)?;
        let targets = entries
            .into_iter()
            .map(|(path, bytes)| Ok((directory_path(dir, &path, false)?, Some(bytes))))
            .collect::<Result<Vec<_>, ContainerError>>()?;
        commit::commit(
            dir,
            targets
                .into_iter()
                .chain(stale)
                .chain([(manifest_path, Some(manifest))]),
        )?;
        let graphs_dir = dir.join(GRAPHS_DIR);
        if graphs_dir.is_dir() {
            let _ = fs::remove_dir(&graphs_dir);
        }
        Ok(())
    }

    /// Save the project as a Git-friendly sharded directory (`graphs/00_header.ir.json` + `graphs/<shard>.ir.json`).
    pub fn save_sharded_directory(&self, dir: &Path) -> Result<(), ContainerError> {
        let mut entries = self.entries_sharded_graph()?;
        let _guard = commit::DIRECTORY_IO
            .write()
            .map_err(|_| commit::poisoned())?;
        let old_drawings = directory_drawings(dir)?;
        let old_shards = directory_shards(dir)?;
        let mut candidates: Vec<&str> = old_drawings
            .iter()
            .map(String::as_str)
            .chain(old_shards.iter().map(String::as_str))
            .chain([
                CACHE_BREP,
                CACHE_METADATA,
                CADPACK,
                VERIFICATION_REPORT,
                DEFAULT_KCL_VIEW_PATH,
                DEFAULT_ENTRY_GRAPH,
            ])
            .collect();
        if !self
            .manifest
            .entry_graph
            .eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH)
        {
            candidates.push(self.manifest.entry_graph.as_str());
        }
        let mut seen_candidates = BTreeSet::new();
        let mut stale = Vec::new();
        for path in candidates {
            if !seen_candidates.insert(path.to_lowercase()) {
                continue;
            }
            let normalized = normalize_relative_path(path)?;
            let will_be_written = entries.contains_key(&normalized)
                || (cfg!(windows)
                    && entries
                        .keys()
                        .any(|entry| entry.eq_ignore_ascii_case(&normalized)));
            if !will_be_written {
                let target = directory_path(dir, &normalized, false)?;
                if target.is_file() {
                    stale.push((target, None));
                }
            }
        }
        let manifest = entries
            .remove(MANIFEST)
            .expect("entries include the manifest");
        let manifest_path = directory_path(dir, MANIFEST, false)?;
        let targets = entries
            .into_iter()
            .map(|(path, bytes)| Ok((directory_path(dir, &path, false)?, Some(bytes))))
            .collect::<Result<Vec<_>, ContainerError>>()?;
        commit::commit(
            dir,
            targets
                .into_iter()
                .chain(stale)
                .chain([(manifest_path, Some(manifest))]),
        )
    }

    /// Load an unpacked project directory (automatically supporting both single-graph and sharded directories)
    /// and verify all referenced blob payloads.
    pub fn load_directory(dir: &Path) -> Result<Self, ContainerError> {
        let shards = {
            let _guard = commit::DIRECTORY_IO
                .read()
                .map_err(|_| commit::poisoned())?;
            directory_shards(dir)?
        };
        if !shards.is_empty() {
            return Self::load_sharded_directory(dir);
        }
        let _guard = commit::DIRECTORY_IO
            .read()
            .map_err(|_| commit::poisoned())?;
        load_project_single(directory_drawings(dir)?, |path| {
            let path = directory_path(dir, path, false)?;
            optional_io(fs::read(path))
        })
    }

    /// Explicitly load a sharded Git-collaborative project directory (`graphs/*.ir.json`).
    pub fn load_sharded_directory(dir: &Path) -> Result<Self, ContainerError> {
        let _guard = commit::DIRECTORY_IO
            .read()
            .map_err(|_| commit::poisoned())?;
        let shard_paths = directory_shards(dir)?;
        if shard_paths.is_empty() {
            return Err(ContainerError::MissingEntry(
                SHARDED_ENTRY_GRAPH_PATH.to_owned(),
            ));
        }
        load_project_sharded(directory_drawings(dir)?, shard_paths, |path| {
            let path = directory_path(dir, path, false)?;
            optional_io(fs::read(path))
        })
    }

    fn entries_single_graph(&self) -> Result<Entries<'_>, ContainerError> {
        let manifest = self.prepared_manifest(false)?;
        validate_manifest_paths(&manifest, false)?;
        let entry_graph = manifest.entry_graph.clone();

        let mut entries = BTreeMap::from([
            (MANIFEST.to_owned(), json_entry(&manifest)?),
            (ENGINEERING.to_owned(), json_entry(&self.engineering)?),
            (
                entry_graph,
                Cow::Owned(self.graph.to_canonical_json()?.into_bytes()),
            ),
        ]);
        self.append_common_entries(&manifest, &mut entries)?;
        validate_file_tree(entries.keys().map(String::as_str))?;
        Ok(entries)
    }

    fn entries_sharded_graph(&self) -> Result<Entries<'_>, ContainerError> {
        let manifest = self.prepared_manifest(true)?;
        validate_manifest_paths(&manifest, true)?;

        let shard_files = sharded::split_graph_into_shards(&self.graph)?;
        let mut entries = BTreeMap::from([
            (MANIFEST.to_owned(), json_entry(&manifest)?),
            (ENGINEERING.to_owned(), json_entry(&self.engineering)?),
        ]);
        for (shard_path, bytes) in shard_files {
            entries.insert(shard_path, Cow::Owned(bytes));
        }
        self.append_common_entries(&manifest, &mut entries)?;
        validate_file_tree(entries.keys().map(String::as_str))?;
        Ok(entries)
    }

    fn prepared_manifest(&self, sharded: bool) -> Result<ContainerManifest, ContainerError> {
        manifest::validate_versions(&self.manifest, self.graph.ir_schema_version)?;
        cad_ir::validate_graph(&self.graph)?;
        self.verify_all_blobs()?;

        let mut manifest = self.manifest.clone();
        if sharded {
            manifest.entry_graph = SHARDED_ENTRY_GRAPH_PATH.to_owned();
        } else if manifest
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
                if self.kcl_view.is_some() {
                    manifest
                        .kcl_view
                        .get_or_insert_with(|| DEFAULT_KCL_VIEW_PATH.to_owned());
                } else {
                    manifest.kcl_view = None;
                }
            }
        }
        Ok(manifest)
    }

    fn append_common_entries<'a>(
        &'a self,
        manifest: &ContainerManifest,
        entries: &mut Entries<'a>,
    ) -> Result<(), ContainerError> {
        if manifest.kcl_projection_policy == KclProjectionPolicy::MaterializedSynced
            && let (Some(path), Some(view)) = (manifest.kcl_view.as_ref(), &self.kcl_view)
        {
            entries.insert(path.clone(), Cow::Borrowed(view.as_bytes()));
        }
        for (id, sheet) in &self.drawings {
            let path = drawing_path(id)?;
            if sheet.sheet_id != *id {
                return Err(ContainerError::DrawingIdMismatch {
                    path,
                    sheet_id: sheet.sheet_id.clone(),
                });
            }
            entries.insert(path, json_entry(sheet)?);
        }
        for blob in self.graph.blobs.values() {
            let path = &blob.relative_path;
            entries.insert(path.clone(), Cow::Borrowed(&self.blobs[path]));
        }
        if let Some(cache) = &self.last_known_good {
            entries.insert(CACHE_BREP.to_owned(), Cow::Borrowed(&cache.brep_bytes));
            entries.insert(
                CACHE_METADATA.to_owned(),
                json_entry(&CacheMetadata {
                    kernel: &cache.kernel,
                    graph_sha256: &cache.graph_sha256,
                })?,
            );
        }
        if let Some(cadpack) = &self.cadpack_bytes {
            entries.insert(CADPACK.to_owned(), Cow::Borrowed(cadpack));
        }
        if let Some(report) = &self.verification_report {
            entries.insert(VERIFICATION_REPORT.to_owned(), json_entry(report)?);
        }
        Ok(())
    }
}

fn load_project_single(
    drawing_paths: Vec<String>,
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, ContainerError>,
) -> Result<AicadProject, ContainerError> {
    let manifest = manifest::parse_manifest(&String::from_utf8(required(&mut read, MANIFEST)?)?)?;
    validate_manifest_paths(&manifest, false)?;
    let graph_json = String::from_utf8(required(&mut read, &manifest.entry_graph)?)?;
    let graph = manifest::parse_graph(&graph_json)?;
    finish_loading_project(manifest, graph, drawing_paths, read)
}

fn load_project_sharded(
    drawing_paths: Vec<String>,
    shard_paths: Vec<String>,
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, ContainerError>,
) -> Result<AicadProject, ContainerError> {
    let mut manifest =
        manifest::parse_manifest(&String::from_utf8(required(&mut read, MANIFEST)?)?)?;
    let is_sharded_manifest = manifest
        .entry_graph
        .eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH);
    validate_manifest_paths(&manifest, is_sharded_manifest)?;

    let header_json = String::from_utf8(required(&mut read, SHARDED_ENTRY_GRAPH_PATH)?)?;
    let mut shard_docs = Vec::new();
    for shard_path in shard_paths {
        if shard_path.eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH) {
            continue;
        }
        let shard_json = String::from_utf8(required(&mut read, &shard_path)?)?;
        shard_docs.push((shard_path, shard_json));
    }
    let graph = sharded::merge_sharded_graph(&header_json, &shard_docs)?;
    if is_sharded_manifest {
        manifest.entry_graph = DEFAULT_ENTRY_GRAPH.to_owned();
    }
    finish_loading_project(manifest, graph, drawing_paths, read)
}

fn finish_loading_project(
    mut manifest: ContainerManifest,
    graph: cad_ir::CanonicalGraph,
    drawing_paths: Vec<String>,
    mut read: impl FnMut(&str) -> Result<Option<Vec<u8>>, ContainerError>,
) -> Result<AicadProject, ContainerError> {
    if manifest.kcl_projection_policy == KclProjectionPolicy::OnDemandDynamic {
        manifest.kcl_view = None;
    }
    let mut project = AicadProject::new(manifest, graph);
    project.kcl_view = project
        .manifest
        .kcl_view
        .as_deref()
        .map(|path| String::from_utf8(required(&mut read, path)?).map_err(ContainerError::from))
        .transpose()?;
    if let Some(bytes) = read(ENGINEERING)? {
        project.engineering = serde_json::from_slice(&bytes)?;
    }
    validate_file_tree(drawing_paths.iter().map(String::as_str))?;
    for path in drawing_paths {
        let sheet: DrawingSheet = serde_json::from_slice(&required(&mut read, &path)?)?;
        if drawing_path(&sheet.sheet_id)? != path {
            return Err(ContainerError::DrawingIdMismatch {
                path,
                sheet_id: sheet.sheet_id,
            });
        }
        project.drawings.insert(sheet.sheet_id.clone(), sheet);
    }
    for blob in project.graph.blobs.values() {
        let path = checked_blob_path(&blob.relative_path)?;
        if let Entry::Vacant(entry) = project.blobs.entry(path) {
            let bytes = read(entry.key())?
                .ok_or_else(|| ContainerError::MissingBlob(entry.key().clone()))?;
            entry.insert(bytes);
        }
    }
    validate_file_tree(project.blobs.keys().map(String::as_str))?;
    project.verify_all_blobs()?;

    if let Some(brep_bytes) = read(CACHE_BREP)?
        && let Some(metadata) = read(CACHE_METADATA)?
    {
        let metadata: CacheMetadata<KernelFingerprint, String> = serde_json::from_slice(&metadata)?;
        project.last_known_good = Some(LastKnownGoodCache {
            kernel: metadata.kernel,
            graph_sha256: metadata.graph_sha256,
            brep_bytes,
        });
    }
    project.cadpack_bytes = read(CADPACK)?;
    if let Some(bytes) = read(VERIFICATION_REPORT)? {
        project.verification_report = Some(serde_json::from_slice(&bytes)?);
    }
    Ok(project)
}

fn json_entry<'a>(value: &impl Serialize) -> Result<Cow<'a, [u8]>, ContainerError> {
    Ok(Cow::Owned(serde_json::to_vec_pretty(value)?))
}

pub(crate) fn required(
    read: &mut impl FnMut(&str) -> Result<Option<Vec<u8>>, ContainerError>,
    path: &str,
) -> Result<Vec<u8>, ContainerError> {
    read(path)?.ok_or_else(|| ContainerError::MissingEntry(path.to_owned()))
}

pub(crate) fn validate_manifest_paths(
    manifest: &ContainerManifest,
    allow_sharded_entry: bool,
) -> Result<(), ContainerError> {
    let graph = &manifest.entry_graph;
    let view = manifest.kcl_view.as_deref();
    let normalized_graph = normalize_relative_path(graph)?;
    let graph_reserved: &[&str] = if allow_sharded_entry
        && normalized_graph.eq_ignore_ascii_case(SHARDED_ENTRY_GRAPH_PATH)
    {
        &[
            MANIFEST,
            ENGINEERING,
            VERIFICATION_REPORT,
            "blobs",
            DRAWINGS,
            "cache",
        ]
    } else {
        &[
            MANIFEST,
            ENGINEERING,
            VERIFICATION_REPORT,
            "blobs",
            DRAWINGS,
            "cache",
            GRAPHS_DIR,
        ]
    };
    if graph_reserved
        .iter()
        .any(|reserved| paths_overlap(&normalized_graph, reserved))
    {
        return Err(ContainerError::UnsafePath(graph.clone()));
    }

    if let Some(view_path) = view {
        let normalized_view = normalize_relative_path(view_path)?;
        if [
            MANIFEST,
            ENGINEERING,
            VERIFICATION_REPORT,
            "blobs",
            DRAWINGS,
            "cache",
            GRAPHS_DIR,
        ]
        .into_iter()
        .any(|reserved| paths_overlap(&normalized_view, reserved))
        {
            return Err(ContainerError::UnsafePath(view_path.to_owned()));
        }
    }

    if paths_overlap(graph, DEFAULT_KCL_VIEW_PATH)
        || view.is_some_and(|path| paths_overlap(graph, path))
    {
        return Err(ContainerError::UnsafePath(graph.clone()));
    }
    if let Some(path) = view
        && !path.eq_ignore_ascii_case(DEFAULT_KCL_VIEW_PATH)
        && paths_overlap(path, DEFAULT_KCL_VIEW_PATH)
    {
        return Err(ContainerError::UnsafePath(path.to_owned()));
    }
    Ok(())
}

fn paths_overlap(a: &str, b: &str) -> bool {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    a == b
        || a.strip_prefix(&b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(&a).is_some_and(|rest| rest.starts_with('/'))
}

pub(crate) fn validate_file_tree<'a>(
    paths: impl IntoIterator<Item = &'a str>,
) -> Result<(), ContainerError> {
    let mut files = BTreeSet::new();
    for path in paths {
        if !files.insert(path.to_lowercase()) {
            return Err(ContainerError::UnsafePath(path.to_owned()));
        }
    }
    for path in &files {
        let has_file_ancestor = path
            .match_indices('/')
            .any(|(index, _)| files.contains(&path[..index]));
        if has_file_ancestor {
            return Err(ContainerError::UnsafePath(path.clone()));
        }
    }
    Ok(())
}

pub(crate) fn drawing_path(id: &str) -> Result<String, ContainerError> {
    let id = normalize_relative_path(id)?;
    if id.contains('/') {
        return Err(ContainerError::UnsafePath(id));
    }
    Ok(format!("{DRAWINGS}/{id}{DRAWING_SUFFIX}"))
}

pub(crate) fn directory_drawings(dir: &Path) -> Result<Vec<String>, ContainerError> {
    let path = directory_path(dir, DRAWINGS, true)?;
    let Some(children) = optional_io(fs::read_dir(path))? else {
        return Ok(Vec::new());
    };
    let mut drawings = Vec::new();
    for child in children {
        let child = child?;
        let name = child.file_name();
        if let Some(name) = name.to_str().filter(|name| name.ends_with(DRAWING_SUFFIX)) {
            let path = normalize_relative_path(&format!("{DRAWINGS}/{name}"))?;
            directory_path(dir, &path, false)?;
            drawings.push(path);
        }
    }
    drawings.sort_unstable();
    Ok(drawings)
}

pub(crate) fn directory_shards(dir: &Path) -> Result<Vec<String>, ContainerError> {
    let path = directory_path(dir, GRAPHS_DIR, true)?;
    let Some(children) = optional_io(fs::read_dir(path))? else {
        return Ok(Vec::new());
    };
    let mut shards = Vec::new();
    for child in children {
        let child = child?;
        let name = child.file_name();
        if let Some(name) = name
            .to_str()
            .filter(|name| name.ends_with(GRAPH_SHARD_SUFFIX))
        {
            let path = normalize_relative_path(&format!("{GRAPHS_DIR}/{name}"))?;
            directory_path(dir, &path, false)?;
            shards.push(path);
        }
    }
    shards.sort_unstable();
    Ok(shards)
}

pub(crate) fn directory_path(
    dir: &Path,
    entry: &str,
    directory: bool,
) -> Result<PathBuf, ContainerError> {
    check_path_type(dir, true, entry)?;
    let mut path = dir.to_path_buf();
    let mut parts = entry.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        check_path_type(&path, directory || parts.peek().is_some(), entry)?;
    }
    Ok(path)
}

fn check_path_type(path: &Path, directory: bool, entry: &str) -> Result<(), ContainerError> {
    if let Some(metadata) = optional_io(fs::symlink_metadata(path))? {
        let expected_type = if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if !expected_type || is_reparse_or_symlink(&metadata) {
            return Err(ContainerError::UnsafePath(entry.to_owned()));
        }
    }
    Ok(())
}

pub(crate) fn optional_io<T>(result: io::Result<T>) -> Result<Option<T>, ContainerError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

//! OpenUSD-style cross-container `ExternalRef` resolution, parameter override injection,
//! cycle detection, and `DeferredPayload` lightweight proxy handling for `GraphEvaluator`.

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use cad_container::{
    AicadProject, BundledRefEntry, LazyProjectReader, SqliteContainer, StorageKind,
    compute_sha256_hex, detect_storage_kind, is_reparse_or_symlink,
};
use cad_ir::{
    CanonicalGraph, ExternalRefDef, ExternalRefId, IrType, IrValue, NodeId, NodeKind, NodeState,
    PayloadPolicy,
};
use cad_kernel_occt::{BBox3, GeometryKernel, Mat4, SolidShape};

use crate::{
    graph_eval::{GraphEvalCache, GraphEvaluator},
    merkle_cache::MerkleEvalCache,
};

const DEFAULT_DEFERRED_PROXY_EXTENT_MM: [f64; 3] = [10.0, 10.0, 10.0];

/// Lightweight proxy summary produced for an `AssemblyComponent` referencing an
/// `ExternalRef` configured with `PayloadPolicy::DeferredPayload`.
#[derive(Debug, Clone, PartialEq)]
pub struct DeferredComponentSummary {
    pub node_id: NodeId,
    pub ref_id: ExternalRefId,
    pub container_path: String,
    pub project_name: Option<String>,
    /// Axis-aligned bounding box in assembly world millimetres.
    pub proxy_bbox_mm: BBox3,
    pub bom_count: usize,
    pub has_cadpack: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedExtRefPayload {
    EagerSolid(Box<SolidShape>),
    DeferredProxy(Box<DeferredComponentSummary>),
}

#[derive(Debug, Clone)]
pub(crate) struct BundledRefSnapshot {
    pub(crate) entry: BundledRefEntry,
    pub(crate) project: Option<AicadProject>,
}

/// Resolution environment for external `.aicad` references during graph evaluation.
#[derive(Debug, Clone, Default)]
pub(crate) struct ExternalRefContext {
    pub(crate) parent_container: Option<PathBuf>,
    pub(crate) workspace_root: Option<PathBuf>,
    pub(crate) registered_projects: BTreeMap<String, AicadProject>,
    pub(crate) bundled_refs: BTreeMap<String, BundledRefSnapshot>,
    pub(crate) active_chain: Vec<String>,
}

impl ExternalRefContext {
    pub(crate) fn preload_from_sqlite(
        &mut self,
        container: &SqliteContainer,
        eager_needed: bool,
    ) -> Result<(), cad_container::ContainerError> {
        self.parent_container = Some(container.path().to_path_buf());
        if let Ok(canon) = fs::canonicalize(container.path()) {
            let root_key = format!("path:{}", canon.display());
            if !self.active_chain.contains(&root_key) {
                self.active_chain.push(root_key);
            }
        }
        for entry in container.list_bundled_refs()? {
            let project = if eager_needed {
                container.load_bundled_ref_project(&entry.ref_id)?
            } else {
                None
            };
            self.bundled_refs.insert(
                entry.ref_id.clone(),
                BundledRefSnapshot {
                    entry: entry.clone(),
                    project,
                },
            );
        }
        Ok(())
    }

    fn find_bundled(&self, def: &ExternalRefDef) -> Option<&BundledRefSnapshot> {
        self.bundled_refs.get(&def.ref_id.0).or_else(|| {
            self.bundled_refs
                .values()
                .find(|item| item.entry.original_container_path == def.container_path)
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_external_ref_component(
    parent_graph: &CanonicalGraph,
    node_id: &NodeId,
    node_metadata: &BTreeMap<String, String>,
    parent_cache: &GraphEvalCache,
    def: &ExternalRefDef,
    ext_ctx: &ExternalRefContext,
    merkle_cache: &mut MerkleEvalCache,
    kernel: &dyn GeometryKernel,
) -> Result<ResolvedExtRefPayload, String> {
    validate_relative_container_path(&def.container_path)?;

    match def.payload_policy {
        PayloadPolicy::DeferredPayload => {
            evaluate_deferred_ref(node_id, node_metadata, def, ext_ctx)
        }
        PayloadPolicy::Eager => evaluate_eager_ref(
            parent_graph,
            parent_cache,
            def,
            ext_ctx,
            merkle_cache,
            kernel,
        ),
    }
}

fn evaluate_deferred_ref(
    node_id: &NodeId,
    node_metadata: &BTreeMap<String, String>,
    def: &ExternalRefDef,
    ext_ctx: &ExternalRefContext,
) -> Result<ResolvedExtRefPayload, String> {
    let local_proxy_bbox = parse_metadata_bbox(node_metadata).unwrap_or(BBox3 {
        min: [0.0, 0.0, 0.0],
        max: DEFAULT_DEFERRED_PROXY_EXTENT_MM,
    });

    // 1. Check in-memory registered projects without running child B-Rep evaluation.
    if let Some(proj) = ext_ctx
        .registered_projects
        .get(&def.ref_id.0)
        .or_else(|| ext_ctx.registered_projects.get(&def.container_path))
    {
        check_target_graph_id(def, &proj.graph.graph_id)?;
        return Ok(ResolvedExtRefPayload::DeferredProxy(Box::new(
            DeferredComponentSummary {
                node_id: node_id.clone(),
                ref_id: def.ref_id.clone(),
                container_path: def.container_path.clone(),
                project_name: Some(proj.manifest.project_name.clone()),
                proxy_bbox_mm: local_proxy_bbox,
                bom_count: proj.engineering.bom.len(),
                has_cadpack: proj.cadpack_bytes.is_some(),
            },
        )));
    }

    // 2. Check inline `bundled_refs` metadata (zero B-Rep evaluation).
    if let Some(bundled) = ext_ctx.find_bundled(def) {
        verify_expected_sha256(def, &bundled.entry.container_sha256)?;
        let (project_name, bom_count, has_cadpack) = match &bundled.project {
            Some(proj) => {
                check_target_graph_id(def, &proj.graph.graph_id)?;
                (
                    Some(proj.manifest.project_name.clone()),
                    proj.engineering.bom.len(),
                    proj.cadpack_bytes.is_some(),
                )
            }
            None => (None, 0, false),
        };
        return Ok(ResolvedExtRefPayload::DeferredProxy(Box::new(
            DeferredComponentSummary {
                node_id: node_id.clone(),
                ref_id: def.ref_id.clone(),
                container_path: def.container_path.clone(),
                project_name,
                proxy_bbox_mm: local_proxy_bbox,
                bom_count,
                has_cadpack,
            },
        )));
    }

    // 3. If filesystem roots are configured and target container exists on disk, read metadata lazily
    // via `LazyProjectReader` (zero B-Rep evaluation, zero heavy blob allocation).
    // If unmounted or no filesystem context is configured, return lightweight proxy cleanly.
    if let Some(resolved_path) = try_resolve_disk_path_optional(ext_ctx, &def.container_path)? {
        let cycle_key = format!("path:{}", resolved_path.display());
        if ext_ctx.active_chain.contains(&cycle_key) {
            return Err(format!(
                "external reference cycle detected involving `{}`",
                def.container_path
            ));
        }
        if let Some(expected_sha) = &def.expected_sha256 {
            let actual_sha = compute_container_sha256(&resolved_path)?;
            if !expected_sha.eq_ignore_ascii_case(&actual_sha) {
                return Err(format!(
                    "external reference `{}` (`{}`) SHA-256 mismatch: expected {expected_sha}, got {actual_sha}",
                    def.ref_id, def.container_path
                ));
            }
        }
        let reader = LazyProjectReader::open(&resolved_path).map_err(|err| err.to_string())?;
        check_target_graph_id(def, &reader.graph().graph_id)?;
        return Ok(ResolvedExtRefPayload::DeferredProxy(Box::new(
            DeferredComponentSummary {
                node_id: node_id.clone(),
                ref_id: def.ref_id.clone(),
                container_path: def.container_path.clone(),
                project_name: Some(reader.manifest().project_name.clone()),
                proxy_bbox_mm: local_proxy_bbox,
                bom_count: reader.engineering().bom.len(),
                has_cadpack: reader.has_cadpack(),
            },
        )));
    }

    Ok(ResolvedExtRefPayload::DeferredProxy(Box::new(
        DeferredComponentSummary {
            node_id: node_id.clone(),
            ref_id: def.ref_id.clone(),
            container_path: def.container_path.clone(),
            project_name: None,
            proxy_bbox_mm: local_proxy_bbox,
            bom_count: 0,
            has_cadpack: false,
        },
    )))
}

fn evaluate_eager_ref(
    parent_graph: &CanonicalGraph,
    parent_cache: &GraphEvalCache,
    def: &ExternalRefDef,
    ext_ctx: &ExternalRefContext,
    merkle_cache: &mut MerkleEvalCache,
    kernel: &dyn GeometryKernel,
) -> Result<ResolvedExtRefPayload, String> {
    let (child_project, cycle_key, next_parent_path) = resolve_eager_project(def, ext_ctx)?;

    if ext_ctx.active_chain.contains(&cycle_key) {
        let mut chain = ext_ctx.active_chain.clone();
        chain.push(cycle_key);
        return Err(format!(
            "external reference cycle detected: {}",
            chain.join(" -> ")
        ));
    }

    check_target_graph_id(def, &child_project.graph.graph_id)?;

    let mut child_graph = child_project.graph.clone();
    apply_overrides(parent_graph, parent_cache, def, &mut child_graph)?;

    let mut child_ctx = ext_ctx.clone();
    child_ctx.active_chain.push(cycle_key);
    if let Some(path) = next_parent_path {
        child_ctx.parent_container = Some(path);
    }

    let mut child_eval = GraphEvaluator::with_merkle_cache(kernel, std::mem::take(merkle_cache));
    child_eval.set_external_ref_context(child_ctx);
    let eval_result =
        child_eval.evaluate_graph_with_blobs(&child_graph, &child_project.blobs, kernel);
    *merkle_cache = child_eval.take_merkle_cache();

    let child_report = eval_result.map_err(|err| err.to_string())?;
    let solid = extract_child_primary_solid(&child_graph, &child_report, kernel, &def.ref_id.0)?;
    Ok(ResolvedExtRefPayload::EagerSolid(Box::new(solid)))
}

fn resolve_eager_project(
    def: &ExternalRefDef,
    ext_ctx: &ExternalRefContext,
) -> Result<(AicadProject, String, Option<PathBuf>), String> {
    // 1. Pre-registered in-memory projects.
    if let Some(proj) = ext_ctx
        .registered_projects
        .get(&def.ref_id.0)
        .or_else(|| ext_ctx.registered_projects.get(&def.container_path))
    {
        return Ok((proj.clone(), format!("mem:{}", def.ref_id.0), None));
    }

    // 2. Self-contained `bundled_refs` from parent SQLite container.
    if let Some(bundled) = ext_ctx.find_bundled(def) {
        verify_expected_sha256(def, &bundled.entry.container_sha256)?;
        if let Some(proj) = &bundled.project {
            let key = format!(
                "bundled:{}:{}",
                bundled.entry.ref_id, bundled.entry.container_sha256
            );
            return Ok((proj.clone(), key, None));
        }
    }

    // 3. Safe relative path resolution on disk.
    let resolved_path = try_resolve_disk_path_optional(ext_ctx, &def.container_path)?
        .ok_or_else(|| {
            format!(
                "external reference `{}` container `{}` was not found in bundled_refs or workspace",
                def.ref_id, def.container_path
            )
        })?;

    if let Some(expected_sha) = &def.expected_sha256 {
        let actual_sha = compute_container_sha256(&resolved_path)?;
        if !expected_sha.eq_ignore_ascii_case(&actual_sha) {
            return Err(format!(
                "external reference `{}` (`{}`) SHA-256 mismatch: expected {expected_sha}, got {actual_sha}",
                def.ref_id, def.container_path
            ));
        }
    }

    let project = AicadProject::load_any(&resolved_path).map_err(|err| {
        format!(
            "cannot load external reference `{}` (`{}`): {err}",
            def.ref_id, def.container_path
        )
    })?;
    let key = format!("path:{}", resolved_path.display());
    Ok((project, key, Some(resolved_path)))
}

fn extract_child_primary_solid(
    child_graph: &CanonicalGraph,
    child_report: &crate::graph_eval::GraphEvalReport,
    kernel: &dyn GeometryKernel,
    ref_id: &str,
) -> Result<SolidShape, String> {
    let mut parts = Vec::new();
    for out_id in &child_graph.outputs {
        if let Some(node_report) = child_report.node_reports.get(out_id)
            && node_report.status != NodeState::Valid
            && node_report.status != NodeState::Suppressed
        {
            let detail = node_report
                .error
                .as_deref()
                .unwrap_or("child output node failed");
            return Err(format!(
                "external reference `{ref_id}` output `{out_id}` failed: {detail}"
            ));
        }
        if let Some(solid) = child_report.output_solids.get(out_id) {
            parts.push(solid.clone());
        }
    }
    if parts.is_empty() {
        for solid in child_report.component_solids.values() {
            parts.push(solid.clone());
        }
    }
    match parts.len() {
        0 => Err(format!(
            "external reference `{ref_id}` produced no valid solid geometry"
        )),
        1 => Ok(parts.remove(0)),
        _ => kernel
            .try_boolean_union(parts, ref_id)
            .map_err(|err| err.to_string()),
    }
}

fn apply_overrides(
    parent_graph: &CanonicalGraph,
    parent_cache: &GraphEvalCache,
    def: &ExternalRefDef,
    child_graph: &mut CanonicalGraph,
) -> Result<(), String> {
    for (param_name, raw_value) in &def.overrides {
        let resolved = resolve_override_value(parent_graph, parent_cache, raw_value)?;
        let param_def = child_graph
            .parameters
            .get_mut(param_name)
            .ok_or_else(|| {
                format!(
                    "external reference `{}` overrides unknown child parameter `{param_name}`",
                    def.ref_id
                )
            })?;
        validate_override_compatibility(param_name, param_def.ir_type, &resolved)?;
        param_def.value = resolved;
    }
    Ok(())
}

fn resolve_override_value(
    parent_graph: &CanonicalGraph,
    parent_cache: &GraphEvalCache,
    value: &IrValue,
) -> Result<IrValue, String> {
    match value {
        IrValue::NodeRef { node_id, .. } => {
            let node = parent_graph.nodes.get(&node_id.0).ok_or_else(|| {
                format!("override references missing parent node `{node_id}`")
            })?;
            let NodeKind::ParameterRef { param_id } = &node.kind else {
                return Err(format!(
                    "override node reference `{node_id}` must target a ParameterRef node"
                ));
            };
            if parent_cache
                .get(node_id)
                .is_some_and(|snap| snap.state != NodeState::Valid)
            {
                return Err(format!(
                    "override parameter node `{node_id}` is not in a valid state"
                ));
            }
            let param = parent_graph.parameters.get(&param_id.0).ok_or_else(|| {
                format!("override references missing parent parameter `{param_id}`")
            })?;
            Ok(param.value.clone())
        }
        IrValue::List(items) => Ok(IrValue::List(
            items
                .iter()
                .map(|item| resolve_override_value(parent_graph, parent_cache, item))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        IrValue::Map(entries) => {
            let mut resolved_map = BTreeMap::new();
            for (k, v) in entries {
                resolved_map.insert(
                    k.clone(),
                    resolve_override_value(parent_graph, parent_cache, v)?,
                );
            }
            Ok(IrValue::Map(resolved_map))
        }
        literal => Ok(literal.clone()),
    }
}

fn validate_override_compatibility(
    param_name: &str,
    expected: IrType,
    value: &IrValue,
) -> Result<(), String> {
    let matches = match value {
        IrValue::Quantity(q) => q.ir_type() == expected,
        IrValue::Scalar(_) => expected == IrType::Scalar,
        IrValue::Int(_) => expected == IrType::Int,
        IrValue::Bool(_) => expected == IrType::Bool,
        IrValue::String(_) => expected == IrType::String,
        IrValue::Vec3(_) => matches!(expected, IrType::Vec3Dir | IrType::Vec3Length),
        _ => false,
    };
    if !matches {
        return Err(format!(
            "override for parameter `{param_name}` does not match expected type `{expected:?}`"
        ));
    }
    Ok(())
}

fn check_target_graph_id(def: &ExternalRefDef, actual_graph_id: &str) -> Result<(), String> {
    if let Some(expected_id) = &def.target_graph_id
        && expected_id != actual_graph_id
    {
        return Err(format!(
            "external reference `{}` expected target_graph_id `{expected_id}`, got `{actual_graph_id}`",
            def.ref_id
        ));
    }
    Ok(())
}

fn verify_expected_sha256(def: &ExternalRefDef, actual_sha256: &str) -> Result<(), String> {
    if let Some(expected) = &def.expected_sha256
        && !expected.eq_ignore_ascii_case(actual_sha256)
    {
        return Err(format!(
            "external reference `{}` (`{}`) SHA-256 mismatch: expected {expected}, got {actual_sha256}",
            def.ref_id, def.container_path
        ));
    }
    Ok(())
}

fn validate_relative_container_path(raw: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains('\\')
        || trimmed.contains(':')
        || trimmed.chars().any(|c| c.is_control())
    {
        return Err(format!("unsafe external container path `{raw}`"));
    }
    Ok(())
}

fn try_resolve_disk_path_optional(
    ext_ctx: &ExternalRefContext,
    raw_rel: &str,
) -> Result<Option<PathBuf>, String> {
    let canon_workspace = match &ext_ctx.workspace_root {
        Some(ws) => {
            let meta = fs::symlink_metadata(ws)
                .map_err(|err| format!("invalid workspace root `{}`: {err}", ws.display()))?;
            if is_reparse_or_symlink(&meta) || !meta.is_dir() {
                return Err(format!("unsafe workspace root `{}`", ws.display()));
            }
            Some(
                fs::canonicalize(ws)
                    .map_err(|err| format!("cannot canonicalize workspace root: {err}"))?,
            )
        }
        None => None,
    };

    let primary_base = if let Some(parent) = &ext_ctx.parent_container {
        if let Ok(canon_parent) = fs::canonicalize(parent) {
            if Some(&canon_parent) == canon_workspace.as_ref() {
                Some(canon_parent)
            } else {
                canon_parent.parent().map(Path::to_path_buf)
            }
        } else {
            parent.parent().map(Path::to_path_buf)
        }
    } else {
        canon_workspace.clone()
    };

    let Some(base_dir) = primary_base else {
        return Ok(None);
    };

    if let Some(found) = walk_safe_path(&base_dir, canon_workspace.as_deref(), raw_rel)? {
        return Ok(Some(found));
    }
    if let Some(ws) = &canon_workspace
        && &base_dir != ws
        && let Some(found) = walk_safe_path(ws, Some(ws.as_path()), raw_rel)?
    {
        return Ok(Some(found));
    }

    Ok(None)
}

fn walk_safe_path(
    base_dir: &Path,
    canon_workspace: Option<&Path>,
    rel: &str,
) -> Result<Option<PathBuf>, String> {
    let mut cursor = base_dir.to_path_buf();
    if let Some(ws) = canon_workspace
        && !cursor.starts_with(ws)
    {
        return Err(format!("external path `{rel}` escapes workspace root"));
    }

    for component in Path::new(rel).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !cursor.pop() {
                    return Err(format!("unsafe external container path `{rel}`"));
                }
                if let Some(ws) = canon_workspace
                    && !cursor.starts_with(ws)
                {
                    return Err(format!("external path `{rel}` escapes workspace root"));
                }
            }
            Component::Normal(part) => {
                let Some(part_str) = part.to_str() else {
                    return Err(format!("unsafe external container path `{rel}`"));
                };
                if part_str.is_empty() || part_str.ends_with(['.', ' ']) {
                    return Err(format!("unsafe external container path `{rel}`"));
                }
                cursor.push(part);
                match fs::symlink_metadata(&cursor) {
                    Ok(meta) => {
                        if is_reparse_or_symlink(&meta) {
                            return Err(format!(
                                "external container path `{rel}` traverses a symbolic link or reparse point"
                            ));
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(err) => return Err(err.to_string()),
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(format!("unsafe external container path `{rel}`"));
            }
        }
    }

    let canon = fs::canonicalize(&cursor).map_err(|err| err.to_string())?;
    if let Some(ws) = canon_workspace
        && !canon.starts_with(ws)
    {
        return Err(format!("external path `{rel}` escapes workspace root"));
    }
    Ok(Some(canon))
}

fn compute_container_sha256(path: &Path) -> Result<String, String> {
    let kind = detect_storage_kind(path).map_err(|err| err.to_string())?;
    match kind {
        StorageKind::SqliteContainer => {
            let bytes = fs::read(path).map_err(|err| err.to_string())?;
            Ok(compute_sha256_hex(&bytes))
        }
        StorageKind::UnpackedSingleGraphDir | StorageKind::UnpackedShardedDir => {
            let project = AicadProject::load_any(path).map_err(|err| err.to_string())?;
            let bytes = project.to_sqlite_bytes().map_err(|err| err.to_string())?;
            Ok(compute_sha256_hex(&bytes))
        }
    }
}

fn parse_metadata_bbox(metadata: &BTreeMap<String, String>) -> Option<BBox3> {
    let min = parse_csv_vec3(metadata.get("bbox_min")?)?;
    let max = parse_csv_vec3(metadata.get("bbox_max")?)?;
    let bbox = BBox3 { min, max };
    bbox.is_valid().then_some(bbox)
}

fn parse_csv_vec3(raw: &str) -> Option<[f64; 3]> {
    let mut nums = raw
        .split(',')
        .map(|part| part.trim().parse::<f64>().ok());
    let x = nums.next()??;
    let y = nums.next()??;
    let z = nums.next()??;
    if nums.next().is_some() || !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return None;
    }
    Some([x, y, z])
}

pub(crate) fn transform_bbox(bbox: BBox3, mat: Mat4) -> BBox3 {
    if !bbox.is_valid() {
        return bbox;
    }
    let mut out = BBox3::empty();
    for x in [bbox.min[0], bbox.max[0]] {
        for y in [bbox.min[1], bbox.max[1]] {
            for z in [bbox.min[2], bbox.max[2]] {
                out.expand_point(mat.transform_point([x, y, z]));
            }
        }
    }
    out
}

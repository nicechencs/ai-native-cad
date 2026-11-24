//! Sandboxed Tauri commands for hybrid `.aicad` project containers.
//!
//! Project paths are relative to the directory from `DesktopWorkspace::root_for`.
//! Untrusted paths are checked lexically and with symlink metadata. They are not
//! canonicalized, so a hostile path cannot reach a remote share during the check.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use cad_container::manifest::DEFAULT_KCL_VIEW_PATH;
use cad_container::{
    AicadProject, CacheStatus, ContainerManifest, KclProjectionPolicy, LazyProjectReader,
    SqliteContainer, StorageKind, apply_patch_in_place, detect_storage_kind, pack_and_go,
    vacuum_container,
};

pub use cad_container::{
    BranchInfo, BranchPurpose, BranchStatus, BundledRefEntry, KernelFingerprint, PackAndGoReport,
    VacuumReport,
};
use cad_ir::{
    CanonicalGraph, ContextSliceOptions, GraphPatch, IrNode, IrType, IrValue, LengthUnit, NodeId,
    NodeKind, NodeState, OUTPUT_PORT, ParamId, ParameterDef, Quantity, extract_context_slice,
};
use cad_kcl_bridge::{diff_graphs, emit_kcl, parse_any_kcl_to_graph};
use cad_kernel_occt::GeometryKernel;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::DesktopWorkspace;

const MAX_PROJECT_PATH_LEN: usize = 512;
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_PROJECT_NAME_LEN: usize = 200;
const MAX_BRANCH_NAME_LEN: usize = 128;
const MAX_FOCUS_NODES: usize = 256;
const MAX_NODE_ID_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AicadProjectCreated {
    pub status: &'static str,
    pub project_name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AicadProjectSummary {
    pub status: &'static str,
    pub project_name: String,
    pub format_version: u32,
    pub ir_schema_version: u32,
    pub active_branch: String,
    pub node_count: usize,
    pub parameter_count: usize,
    pub blob_count: usize,
    pub external_ref_count: usize,
    pub pmi_count: usize,
    pub bom_count: usize,
    pub drawing_count: usize,
    pub node_brep_cache_count: usize,
    pub bundled_ref_count: usize,
    pub cache_status: &'static str,
    pub storage: &'static str,
    pub blobs_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AicadPatchApplied {
    pub status: &'static str,
    pub patch_id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oplog_seq: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AicadBranchResponse {
    pub status: &'static str,
    pub action: String,
    pub active_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<BranchInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches: Option<Vec<BranchInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AicadContextSliceResponse {
    pub status: &'static str,
    pub focus_nodes: Vec<String>,
    pub kept_node_count: usize,
    pub total_node_count: usize,
    pub graph: CanonicalGraph,
    pub kcl: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AicadPackAndGoResponse {
    pub status: &'static str,
    pub output_path: String,
    pub bundled_count: usize,
    pub locked_hashes: BTreeMap<String, String>,
    pub bundled_refs: Vec<BundledRefEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InitAicadProjectRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub project_name: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InspectAicadProjectRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyAicadPatchRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub patch: GraphPatch,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmitAicadKclRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BranchAicadProjectRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub action: String,
    #[serde(default)]
    pub branch_name: Option<String>,
    #[serde(default)]
    pub from_branch: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub to_seq: Option<i64>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

const fn default_slice_depth() -> usize {
    1
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct SliceAicadContextRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub focus_nodes: Vec<String>,
    #[serde(default = "default_slice_depth")]
    pub upstream_depth: usize,
    #[serde(default = "default_slice_depth")]
    pub downstream_depth: usize,
    #[serde(default = "default_true")]
    pub ensure_valid_closure: bool,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VacuumAicadProjectRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    #[serde(default)]
    pub keep_recent_oplog: Option<usize>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackAndGoAicadProjectRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub output_path: String,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

#[tauri::command]
pub fn init_aicad_project(
    request: InitAicadProjectRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<AicadProjectCreated, String> {
    crate::observe::traced("init_aicad_project", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        init_aicad_project_with_format(
            root,
            &request.project_path,
            &request.project_name,
            request.format.as_deref(),
            &current_kernel(&workspace),
        )
    })
}

#[tauri::command]
pub fn inspect_aicad_project(
    request: InspectAicadProjectRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<AicadProjectSummary, String> {
    crate::observe::traced("inspect_aicad_project", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        inspect_aicad_project_in(root, &request.project_path, &current_kernel(&workspace))
    })
}

#[tauri::command]
pub fn apply_aicad_patch(
    request: ApplyAicadPatchRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<AicadPatchApplied, String> {
    crate::observe::traced("apply_aicad_patch", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        apply_aicad_patch_with(
            root,
            &request.project_path,
            &request.patch,
            request.branch.as_deref(),
        )
    })
}

#[tauri::command]
pub fn emit_aicad_kcl(
    request: EmitAicadKclRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<String, String> {
    crate::observe::traced("emit_aicad_kcl", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        emit_aicad_kcl_in(root, &request.project_path)
    })
}

#[tauri::command]
pub fn branch_aicad_project(
    request: BranchAicadProjectRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<AicadBranchResponse, String> {
    crate::observe::traced("branch_aicad_project", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        branch_aicad_project_in(root, &request)
    })
}

#[tauri::command]
pub fn slice_aicad_context(
    request: SliceAicadContextRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<AicadContextSliceResponse, String> {
    crate::observe::traced("slice_aicad_context", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        slice_aicad_context_in(root, &request)
    })
}

#[tauri::command]
pub fn vacuum_aicad_project(
    request: VacuumAicadProjectRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<VacuumReport, String> {
    crate::observe::traced("vacuum_aicad_project", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        vacuum_aicad_project_in(root, &request.project_path, request.keep_recent_oplog)
    })
}

#[tauri::command]
pub fn pack_and_go_aicad_project(
    request: PackAndGoAicadProjectRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<AicadPackAndGoResponse, String> {
    crate::observe::traced(
        "pack_and_go_aicad_project",
        request.trace_id.as_deref(),
        |_| {
            let root = workspace.root_for(request.workspace_root.as_deref())?;
            pack_and_go_aicad_project_in(root, &request.project_path, &request.output_path)
        },
    )
}

pub fn init_aicad_project_in(
    workspace_root: &Path,
    project_path: &str,
    project_name: &str,
    kernel: &KernelFingerprint,
) -> Result<AicadProjectCreated, String> {
    init_aicad_project_with_format(workspace_root, project_path, project_name, None, kernel)
}

pub fn init_aicad_project_with_format(
    workspace_root: &Path,
    project_path: &str,
    project_name: &str,
    format: Option<&str>,
    kernel: &KernelFingerprint,
) -> Result<AicadProjectCreated, String> {
    let storage = ProjectStorage::parse_format(format)?;
    let path = resolve_project_path(workspace_root, project_path)?;
    let project_name = validated_project_name(project_name)?;
    let relative = display_project_path(project_path)?;
    storage.ensure_init_target_available(&path)?;

    let graph = starter_graph()?;
    let mut project = AicadProject::new(
        ContainerManifest::new(project_name.clone(), kernel.clone()),
        graph,
    );
    project.kcl_view = Some(emit_kcl(&project.graph).map_err(|err| err.to_string())?);
    storage
        .save(&project, &path)
        .map_err(|err| format!("cannot create project: {err}"))?;
    Ok(AicadProjectCreated {
        status: "ok",
        project_name,
        path: relative,
    })
}

pub fn inspect_aicad_project_in(
    workspace_root: &Path,
    project_path: &str,
    kernel: &KernelFingerprint,
) -> Result<AicadProjectSummary, String> {
    let path = resolve_project_path(workspace_root, project_path)?;
    let reader =
        LazyProjectReader::open(&path).map_err(|err| format!("cannot load project: {err}"))?;
    for blob in reader.graph().blobs.values() {
        reader
            .stream_blob(&blob.handle.0, &mut io::sink())
            .map_err(|err| format!("cannot verify project blobs: {err}"))?;
    }
    let summary = reader.inspect_summary(kernel);
    Ok(AicadProjectSummary {
        status: "ok",
        project_name: summary.project_name,
        format_version: summary.format_version,
        ir_schema_version: summary.ir_schema_version,
        active_branch: summary.active_branch,
        node_count: summary.node_count,
        parameter_count: summary.parameter_count,
        blob_count: summary.blob_count,
        external_ref_count: summary.external_ref_count,
        pmi_count: summary.pmi_count,
        bom_count: summary.bom_count,
        drawing_count: summary.drawing_count,
        node_brep_cache_count: summary.node_brep_cache_count,
        bundled_ref_count: summary.bundled_ref_count,
        cache_status: cache_status_name(&summary.cache_status),
        storage: storage_kind_name(summary.storage_kind),
        blobs_verified: true,
    })
}

pub fn apply_aicad_patch_in(
    workspace_root: &Path,
    project_path: &str,
    patch_json: &str,
) -> Result<AicadPatchApplied, String> {
    apply_aicad_patch_on_branch_in(workspace_root, project_path, patch_json, None)
}

pub fn apply_aicad_patch_on_branch_in(
    workspace_root: &Path,
    project_path: &str,
    patch_json: &str,
    branch: Option<&str>,
) -> Result<AicadPatchApplied, String> {
    if patch_json.len() > MAX_PATCH_BYTES {
        return Err("Patch must be at most 1 MiB".into());
    }
    let patch: GraphPatch =
        serde_json::from_str(patch_json).map_err(|err| format!("cannot parse patch: {err}"))?;
    apply_aicad_patch_with(workspace_root, project_path, &patch, branch)
}

pub fn emit_aicad_kcl_in(workspace_root: &Path, project_path: &str) -> Result<String, String> {
    let path = resolve_project_path(workspace_root, project_path)?;
    let reader =
        LazyProjectReader::open(&path).map_err(|err| format!("cannot load project: {err}"))?;
    emit_kcl(reader.graph()).map_err(|err| format!("cannot emit KCL: {err}"))
}

pub fn branch_aicad_project_in(
    workspace_root: &Path,
    request: &BranchAicadProjectRequest,
) -> Result<AicadBranchResponse, String> {
    let path = resolve_project_path(workspace_root, &request.project_path)?;
    let storage = ProjectStorage::detect(&path)?;
    if storage != ProjectStorage::Sqlite {
        return Err("Branch operations require a SQLite .aicad container".into());
    }
    let action = request.action.trim();
    if matches!(action, "diff" | "commit_preview" | "discard_preview") {
        let raw_name = request
            .branch_name
            .as_deref()
            .ok_or_else(|| format!("{action} requires `branch_name`"))?;
        let branch_name = validated_branch_name(raw_name)?;
        let outcome =
            crate::live_attach::preview_branch_action(&path, workspace_root, action, &branch_name)?;
        return Ok(AicadBranchResponse {
            status: "ok",
            action: action.to_owned(),
            active_branch: outcome.active_branch,
            branch: None,
            branches: None,
            head_seq: None,
            node_count: None,
            diff: outcome.diff,
            revision: Some(outcome.revision),
        });
    }

    let mut container =
        SqliteContainer::open(&path).map_err(|err| format!("cannot open SQLite project: {err}"))?;

    match action {
        "list" => {
            let active_branch = container
                .active_branch()
                .map_err(|err| format!("cannot read active branch: {err}"))?;
            let branches = container
                .list_branches()
                .map_err(|err| format!("cannot list branches: {err}"))?;
            Ok(AicadBranchResponse {
                status: "ok",
                action: "list".to_owned(),
                active_branch,
                branch: None,
                branches: Some(branches),
                head_seq: None,
                node_count: None,
                diff: None,
                revision: None,
            })
        }
        "create" => {
            let raw_name = request
                .branch_name
                .as_deref()
                .ok_or_else(|| "create requires `branch_name`".to_string())?;
            let branch_name = validated_branch_name(raw_name)?;
            let from_branch = request
                .from_branch
                .as_deref()
                .map(validated_branch_name)
                .transpose()?;
            let info = match request.purpose.as_deref() {
                None => container.create_branch(&branch_name, from_branch.as_deref()),
                Some(raw_purpose) => {
                    let purpose = parse_branch_purpose(raw_purpose)?;
                    container.create_branch_with_purpose(
                        &branch_name,
                        from_branch.as_deref(),
                        purpose,
                    )
                }
            }
            .map_err(|err| format!("cannot create branch `{branch_name}`: {err}"))?;
            let active_branch = container
                .active_branch()
                .map_err(|err| format!("cannot read active branch: {err}"))?;
            let head_seq = info.head_seq;
            Ok(AicadBranchResponse {
                status: "ok",
                action: "create".to_owned(),
                active_branch,
                branch: Some(info),
                branches: None,
                head_seq: Some(head_seq),
                node_count: None,
                diff: None,
                revision: None,
            })
        }
        "checkout" => {
            let raw_name = request
                .branch_name
                .as_deref()
                .ok_or_else(|| "checkout requires `branch_name`".to_string())?;
            let branch_name = validated_branch_name(raw_name)?;
            let graph = container
                .checkout_branch(&branch_name)
                .map_err(|err| format!("cannot checkout branch `{branch_name}`: {err}"))?;
            Ok(AicadBranchResponse {
                status: "ok",
                action: "checkout".to_owned(),
                active_branch: branch_name,
                branch: None,
                branches: None,
                head_seq: None,
                node_count: Some(graph.nodes.len()),
                diff: None,
                revision: None,
            })
        }
        "rollback" => {
            let target_seq = request
                .to_seq
                .ok_or_else(|| "rollback requires `to_seq`".to_string())?;
            if target_seq < 0 {
                return Err("`to_seq` must be a non-negative sequence number".into());
            }
            let target_branch = match request.branch_name.as_deref() {
                Some(raw) => validated_branch_name(raw)?,
                None => container
                    .active_branch()
                    .map_err(|err| format!("cannot read active branch: {err}"))?,
            };
            let graph = container
                .rollback_branch_to_seq(&target_branch, target_seq)
                .map_err(|err| {
                    format!("cannot rollback branch `{target_branch}` to seq {target_seq}: {err}")
                })?;
            let active_branch = container
                .active_branch()
                .map_err(|err| format!("cannot read active branch: {err}"))?;
            Ok(AicadBranchResponse {
                status: "ok",
                action: "rollback".to_owned(),
                active_branch,
                branch: None,
                branches: None,
                head_seq: Some(target_seq),
                node_count: Some(graph.nodes.len()),
                diff: None,
                revision: None,
            })
        }
        other => Err(format!(
            "Unsupported branch action `{other}`; expected list, create, checkout, rollback, diff, commit_preview, or discard_preview"
        )),
    }
}

pub fn slice_aicad_context_in(
    workspace_root: &Path,
    request: &SliceAicadContextRequest,
) -> Result<AicadContextSliceResponse, String> {
    let path = resolve_project_path(workspace_root, &request.project_path)?;
    if request.focus_nodes.is_empty() || request.focus_nodes.len() > MAX_FOCUS_NODES {
        return Err(format!(
            "`focus_nodes` must contain between 1 and {MAX_FOCUS_NODES} node IDs"
        ));
    }
    let mut focus_ids = Vec::with_capacity(request.focus_nodes.len());
    let mut focus_names = Vec::with_capacity(request.focus_nodes.len());
    for raw_id in &request.focus_nodes {
        let trimmed = raw_id.trim();
        if trimmed.is_empty()
            || trimmed.len() > MAX_NODE_ID_LEN
            || trimmed.chars().any(|ch| ch.is_control())
        {
            return Err("Invalid node identifier in `focus_nodes`".into());
        }
        focus_names.push(trimmed.to_owned());
        focus_ids.push(NodeId::from(trimmed));
    }

    let reader =
        LazyProjectReader::open(&path).map_err(|err| format!("cannot load project: {err}"))?;
    let full_graph = reader.graph();
    let total_node_count = full_graph.nodes.len();
    let options = ContextSliceOptions {
        focus_nodes: focus_ids,
        upstream_depth: request.upstream_depth,
        downstream_depth: request.downstream_depth,
        ensure_valid_closure: request.ensure_valid_closure,
    };
    let sliced = extract_context_slice(full_graph, &options)
        .map_err(|err| format!("cannot slice context: {err}"))?;
    let kcl = if options.ensure_valid_closure {
        emit_kcl(&sliced).map_err(|err| format!("cannot emit sliced KCL: {err}"))?
    } else {
        emit_kcl(&sliced).unwrap_or_default()
    };
    let kept_node_count = sliced.nodes.len();

    Ok(AicadContextSliceResponse {
        status: "ok",
        focus_nodes: focus_names,
        kept_node_count,
        total_node_count,
        graph: sliced,
        kcl,
    })
}

pub fn vacuum_aicad_project_in(
    workspace_root: &Path,
    project_path: &str,
    keep_recent_oplog: Option<usize>,
) -> Result<VacuumReport, String> {
    let path = resolve_project_path(workspace_root, project_path)?;
    vacuum_container(&path, keep_recent_oplog)
        .map_err(|err| format!("cannot vacuum project: {err}"))
}

pub fn pack_and_go_aicad_project_in(
    workspace_root: &Path,
    project_path: &str,
    output_path: &str,
) -> Result<AicadPackAndGoResponse, String> {
    let root = real_workspace_dir(workspace_root)?;
    let src_path = resolve_project_path(&root, project_path)?;
    let out_path = resolve_project_path(&root, output_path)?;
    if src_path == out_path {
        return Err("Pack-and-Go `output_path` must differ from `project_path`".into());
    }
    ensure_init_file_available(&out_path)?;
    let relative_out = display_project_path(output_path)?;
    let report = pack_and_go(&src_path, &root, &out_path)
        .map_err(|err| format!("cannot pack-and-go project: {err}"))?;
    Ok(AicadPackAndGoResponse {
        status: "ok",
        output_path: relative_out,
        bundled_count: report.bundled_count,
        locked_hashes: report.locked_hashes,
        bundled_refs: report.bundled_refs,
    })
}

fn apply_aicad_patch_with(
    workspace_root: &Path,
    project_path: &str,
    patch: &GraphPatch,
    branch: Option<&str>,
) -> Result<AicadPatchApplied, String> {
    let path = resolve_project_path(workspace_root, project_path)?;
    let relative = display_project_path(project_path)?;
    let branch_name = branch.map(validated_branch_name).transpose()?;
    let storage = ProjectStorage::detect(&path)?;

    if storage == ProjectStorage::Sqlite {
        let entry = apply_patch_in_place(&path, patch, branch_name.as_deref())
            .map_err(|err| format!("cannot apply patch `{}`: {err}", patch.patch_id))?;
        return Ok(AicadPatchApplied {
            status: "ok",
            patch_id: patch.patch_id.clone(),
            path: relative,
            branch_name: Some(entry.branch_name),
            oplog_seq: Some(entry.seq),
        });
    }

    if branch_name.as_deref().is_some_and(|name| name != "main") {
        return Err("Named branches require a SQLite .aicad container".into());
    }

    let mut project = storage.load(&path)?;
    project
        .graph
        .apply_patch(patch)
        .map_err(|err| format!("cannot apply patch `{}`: {err}", patch.patch_id))?;
    match project.manifest.kcl_projection_policy {
        KclProjectionPolicy::MaterializedSynced => {
            project.manifest.kcl_view = Some(DEFAULT_KCL_VIEW_PATH.to_owned());
            project.kcl_view = Some(
                emit_kcl(&project.graph)
                    .map_err(|err| format!("cannot refresh KCL view: {err}"))?,
            );
        }
        KclProjectionPolicy::OnDemandDynamic => {
            project.manifest.kcl_view = None;
            project.kcl_view = None;
        }
    }
    storage
        .save(&project, &path)
        .map_err(|err| format!("cannot save project: {err}"))?;
    Ok(AicadPatchApplied {
        status: "ok",
        patch_id: patch.patch_id.clone(),
        path: relative,
        branch_name: None,
        oplog_seq: None,
    })
}

fn current_kernel(workspace: &DesktopWorkspace) -> KernelFingerprint {
    KernelFingerprint::new(
        GeometryKernel::backend_name(workspace.kernel.as_ref()),
        env!("CARGO_PKG_VERSION"),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectStorage {
    Directory,
    ShardedDirectory,
    Sqlite,
}

impl ProjectStorage {
    fn parse_format(format: Option<&str>) -> Result<Self, String> {
        match format.map(str::trim) {
            None | Some("directory") => Ok(Self::Directory),
            Some("sharded_directory" | "sharded-dir") => Ok(Self::ShardedDirectory),
            Some("sqlite") => Ok(Self::Sqlite),
            Some(other) => Err(format!(
                "Unsupported project format `{other}`; expected directory, sharded_directory, or sqlite"
            )),
        }
    }

    fn detect(path: &Path) -> Result<Self, String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|_| format!("cannot access project `{}`", display_leaf(path)))?;
        if is_reparse_point(&metadata) {
            return Err(outside_workspace());
        }
        match detect_storage_kind(path) {
            Ok(StorageKind::UnpackedSingleGraphDir) => Ok(Self::Directory),
            Ok(StorageKind::UnpackedShardedDir) => Ok(Self::ShardedDirectory),
            Ok(StorageKind::SqliteContainer) => Ok(Self::Sqlite),
            Err(err) => Err(format!(
                "cannot detect project container format at `{}`: {err}",
                display_leaf(path)
            )),
        }
    }

    fn ensure_init_target_available(self, path: &Path) -> Result<(), String> {
        match self {
            Self::Directory | Self::ShardedDirectory => ensure_init_directory_available(path),
            Self::Sqlite => ensure_init_file_available(path),
        }
    }

    fn load(self, path: &Path) -> Result<AicadProject, String> {
        match self {
            Self::Directory => AicadProject::load_directory(path),
            Self::ShardedDirectory => AicadProject::load_sharded_directory(path),
            Self::Sqlite => AicadProject::load_sqlite(path),
        }
        .map_err(|err| format!("cannot load project: {err}"))
    }

    fn save(self, project: &AicadProject, path: &Path) -> Result<(), String> {
        match self {
            Self::Directory => project.save_directory(path),
            Self::ShardedDirectory => project.save_sharded_directory(path),
            Self::Sqlite => project.save_sqlite(path),
        }
        .map_err(|err| err.to_string())
    }
}

const fn storage_kind_name(kind: StorageKind) -> &'static str {
    match kind {
        StorageKind::UnpackedSingleGraphDir => "directory",
        StorageKind::UnpackedShardedDir => "sharded_directory",
        StorageKind::SqliteContainer => "sqlite",
    }
}

fn cache_status_name(status: &CacheStatus) -> &'static str {
    match status {
        CacheStatus::FreshExactMatch => "fresh_exact_match",
        CacheStatus::KernelVersionDrift { .. } => "kernel_version_drift",
        CacheStatus::GraphModified => "graph_modified",
        CacheStatus::Missing => "missing",
    }
}

fn starter_graph() -> Result<CanonicalGraph, String> {
    let mut graph = CanonicalGraph::new("main");
    let mut inputs_for_box = BTreeMap::new();
    for (name, value) in [("width", 20.0), ("height", 10.0), ("depth", 15.0)] {
        let value = Quantity::length(value, LengthUnit::Mm).map_err(|err| err.to_string())?;
        graph.parameters.insert(
            name.to_owned(),
            ParameterDef {
                name: ParamId::from(name),
                ir_type: IrType::Length,
                value: IrValue::Quantity(value),
                formula: None,
            },
        );
        let id = format!("{name}_param");
        graph.nodes.insert(
            id.clone(),
            IrNode {
                id: NodeId::from(id.clone()),
                name: id.clone(),
                kind: NodeKind::ParameterRef {
                    param_id: ParamId::from(name),
                },
                inputs: BTreeMap::new(),
                output_type: IrType::Length,
                state: NodeState::Valid,
                kernel_op_version: None,
                metadata: BTreeMap::new(),
            },
        );
        inputs_for_box.insert(
            name.to_owned(),
            IrValue::NodeRef {
                node_id: NodeId::from(id),
                port: OUTPUT_PORT.to_owned(),
            },
        );
    }
    inputs_for_box.insert("origin".to_owned(), IrValue::Vec3([0.0, 0.0, 0.0]));
    graph.nodes.insert(
        "box".to_owned(),
        IrNode {
            id: NodeId::from("box"),
            name: "Starter box".to_owned(),
            kind: NodeKind::PrimitiveBox,
            inputs: inputs_for_box,
            output_type: IrType::Solid,
            state: NodeState::Valid,
            kernel_op_version: None,
            metadata: BTreeMap::new(),
        },
    );
    graph.outputs.push(NodeId::from("box"));
    Ok(graph)
}

fn validated_project_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > MAX_PROJECT_NAME_LEN
        || name.chars().any(|ch| ch.is_control())
        || name.contains(['/', '\\', ':'])
    {
        return Err("Project name must be a short label without path separators".into());
    }
    Ok(name.to_owned())
}

fn validated_branch_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > MAX_BRANCH_NAME_LEN
        || name.starts_with('-')
        || name.contains("..")
        || name.contains('\\')
        || name.chars().any(|ch| ch.is_control())
    {
        return Err("Invalid branch name".into());
    }
    Ok(name.to_owned())
}

fn parse_branch_purpose(raw: &str) -> Result<BranchPurpose, String> {
    match raw.trim() {
        "main" => Ok(BranchPurpose::Main),
        "user" => Ok(BranchPurpose::User),
        "ai_candidate" => Ok(BranchPurpose::AiCandidate),
        other => Err(format!(
            "Unsupported branch purpose `{other}`; expected user or ai_candidate"
        )),
    }
}

fn ensure_init_directory_available(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_reparse_point(&metadata) {
                return Err(outside_workspace());
            }
            if !metadata.is_dir() {
                return Err("Project path already exists and is not a directory".into());
            }
            let mut entries = fs::read_dir(path)
                .map_err(|err| format!("cannot read project directory: {err}"))?;
            if entries.next().is_some() {
                return Err("Project directory is not empty".into());
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot access project path: {error}")),
    }
}

fn ensure_init_file_available(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_reparse_point(&metadata) {
                return Err(outside_workspace());
            }
            Err("Project target path already exists".into())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)
                    .map_err(|err| format!("cannot create parent directory: {err}"))?;
            }
            Ok(())
        }
        Err(error) => Err(format!("cannot access project path: {error}")),
    }
}

fn display_project_path(project_path: &str) -> Result<String, String> {
    Ok(normalized_relative(project_path)?.join("/"))
}

fn display_leaf(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_owned()
}

/// Join `project_path` onto an authorized workspace root.
///
/// `..`, absolute paths, Windows drive prefixes, null bytes, and symlink or
/// junction escapes are rejected before any canonicalization of the untrusted path.
pub fn resolve_project_path(workspace_root: &Path, project_path: &str) -> Result<PathBuf, String> {
    let relative = normalized_relative(project_path)?;
    let root = real_workspace_dir(workspace_root)?;
    let mut resolved = root.clone();
    for (index, component) in relative.iter().enumerate() {
        resolved.push(component);
        let is_last = index + 1 == relative.len();
        match fs::symlink_metadata(&resolved) {
            Ok(metadata) => {
                if is_reparse_point(&metadata) {
                    return Err(outside_workspace());
                }
                if metadata.is_dir() {
                    continue;
                }
                if metadata.is_file() && is_last {
                    continue;
                }
                return Err(outside_workspace());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                for rest in &relative[index + 1..] {
                    resolved.push(rest);
                }
                break;
            }
            Err(_) => {
                return Err(
                    "Could not access the project path inside the desktop workspace".into(),
                );
            }
        }
    }
    if resolved == root || !resolved.starts_with(&root) {
        return Err(outside_workspace());
    }
    Ok(resolved)
}

fn normalized_relative(project_path: &str) -> Result<Vec<String>, String> {
    if project_path.is_empty()
        || project_path.len() > MAX_PROJECT_PATH_LEN
        || project_path.chars().any(|ch| ch.is_control())
        || project_path.contains([':', '\\'])
        || project_path.starts_with(['/', '\\'])
    {
        return Err(outside_workspace());
    }
    let path = Path::new(project_path);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(outside_workspace());
    }
    let mut parts = Vec::new();
    for component in project_path.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > 255
            || component.ends_with([' ', '.'])
            || component
                .chars()
                .any(|ch| matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
            || is_windows_device(component)
        {
            return Err(outside_workspace());
        }
        parts.push(component.to_owned());
    }
    if parts.is_empty() {
        return Err(outside_workspace());
    }
    Ok(parts)
}

fn is_windows_device(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let stem = stem.to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|number| matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"))
}

fn real_workspace_dir(workspace_root: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(workspace_root)
        .map_err(|_| "The desktop workspace is no longer available".to_string())?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err("The desktop workspace is no longer available".into());
    }
    let canonical = fs::canonicalize(workspace_root)
        .map_err(|_| "The desktop workspace is no longer available".to_string())?;
    let canonical_metadata = fs::symlink_metadata(&canonical)
        .map_err(|_| "The desktop workspace is no longer available".to_string())?;
    if !canonical_metadata.is_dir() || is_reparse_point(&canonical_metadata) {
        return Err("The desktop workspace is no longer available".into());
    }
    Ok(canonical)
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn outside_workspace() -> String {
    "Project path is outside the desktop workspace".into()
}


#[derive(Debug, Clone, Deserialize)]
pub struct OpenAicadDocumentRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenAicadDocumentResult {
    pub status: &'static str,
    pub project_path: String,
    pub project_name: String,
    pub source: String,
    #[serde(default)]
    pub history: AicadContainerHistory,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportAicadDocumentRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub file_name: String,
    pub contents_base64: String,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveAicadDocumentRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub project_name: String,
    pub source: String,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SaveAicadDocumentResult {
    pub status: &'static str,
    pub project_path: String,
    pub project_name: String,
    #[serde(default)]
    pub history: AicadContainerHistory,
}

/// TES-1-P0.5: whether the open SQLite container can roll modeling history back.
/// Frontend Undo must use this, not only in-memory session undo (which undoes "Open").
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AicadContainerHistory {
    pub can_rollback: bool,
    pub oplog_len: u32,
    pub head_seq: Option<i64>,
    pub previous_seq: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UndoAicadContainerRequest {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub project_path: String,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UndoAicadContainerResult {
    pub status: &'static str,
    pub project_path: String,
    pub project_name: String,
    pub source: String,
    pub history: AicadContainerHistory,
}

#[tauri::command]
pub fn open_aicad_document(
    request: OpenAicadDocumentRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<OpenAicadDocumentResult, String> {
    crate::observe::traced("open_aicad_document", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        open_aicad_document_in(root, &request.project_path)
    })
}

#[tauri::command]
pub fn import_aicad_document(
    request: ImportAicadDocumentRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<OpenAicadDocumentResult, String> {
    crate::observe::traced("import_aicad_document", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        import_aicad_document_in(root, &request.file_name, &request.contents_base64)
    })
}

#[tauri::command]
pub fn save_aicad_document(
    request: SaveAicadDocumentRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<SaveAicadDocumentResult, String> {
    crate::observe::traced("save_aicad_document", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        save_aicad_document_in(
            root,
            &request.project_path,
            &request.project_name,
            &request.source,
            &current_kernel(&workspace),
        )
    })
}

#[tauri::command]
pub fn undo_aicad_container(
    request: UndoAicadContainerRequest,
    workspace: State<'_, DesktopWorkspace>,
) -> Result<UndoAicadContainerResult, String> {
    crate::observe::traced("undo_aicad_container", request.trace_id.as_deref(), |_| {
        let root = workspace.root_for(request.workspace_root.as_deref())?;
        undo_aicad_container_in(root, &request.project_path)
    })
}

pub fn open_aicad_document_in(
    workspace_root: &Path,
    project_path: &str,
) -> Result<OpenAicadDocumentResult, String> {
    let path = resolve_project_path(workspace_root, project_path)?;
    let storage = ProjectStorage::detect(&path)?;
    let project = storage.load(&path)?;
    // CanonicalGraph is the source of truth. Prefer a fresh emit over a stored
    // kcl_view, which can drift after graph patches. Only fall back to kcl_view
    // when the graph has no nodes (or emit fails on an empty graph).
    let source = if project.graph.nodes.is_empty() {
        project
            .kcl_view
            .as_deref()
            .filter(|source| !source.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "无法从该 .aicad 工程还原 KCL 源码".to_string())?
    } else {
        emit_kcl(&project.graph).map_err(|err| {
            format!("无法从工程图还原 KCL 源码: {err}")
        })?
    };
    Ok(OpenAicadDocumentResult {
        status: "ok",
        project_path: display_project_path(project_path)?,
        project_name: project.manifest.project_name,
        source,
        history: sqlite_container_history(&path)?,
    })
}

const MAX_AICAD_IMPORT_BYTES: usize = 32 * 1024 * 1024;

pub fn import_aicad_document_in(
    workspace_root: &Path,
    file_name: &str,
    contents_base64: &str,
) -> Result<OpenAicadDocumentResult, String> {
    let leaf = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "无效的 .aicad 文件名".to_string())?;
    if !leaf.to_ascii_lowercase().ends_with(".aicad") {
        return Err("请选择 .aicad 工程文件".to_string());
    }
    let bytes = decode_base64(contents_base64, MAX_AICAD_IMPORT_BYTES)?;
    let relative = format!("imports/{leaf}");
    let path = resolve_project_path(workspace_root, &relative)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("无法创建工程目录: {err}"))?;
    }
    // Stage to a temporary sibling file and validate before replacing the target
    // path so a corrupt import never overwrites or leaves a broken .aicad behind.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let staging_rel = format!("imports/.import-staging-{}-{nonce}-{leaf}", std::process::id());
    let staging_path = resolve_project_path(workspace_root, &staging_rel)?;
    fs::write(&staging_path, &bytes).map_err(|err| format!("无法写入工程文件: {err}"))?;
    let validation = open_aicad_document_in(workspace_root, &staging_rel);
    if let Err(err) = validation {
        let _ = fs::remove_file(&staging_path);
        return Err(err);
    }
    if path.exists() {
        fs::remove_file(&path).map_err(|err| {
            let _ = fs::remove_file(&staging_path);
            format!("无法替换已有工程文件: {err}")
        })?;
    }
    fs::rename(&staging_path, &path).map_err(|err| {
        let _ = fs::remove_file(&staging_path);
        format!("无法保存导入工程文件: {err}")
    })?;
    open_aicad_document_in(workspace_root, &relative)
}

pub fn save_aicad_document_in(
    workspace_root: &Path,
    project_path: &str,
    project_name: &str,
    source: &str,
    kernel: &KernelFingerprint,
) -> Result<SaveAicadDocumentResult, String> {
    let project_name = validated_project_name(project_name)?;
    let path = resolve_project_path(workspace_root, project_path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("无法创建工程目录: {err}"))?;
    }
    let graph = parse_any_kcl_to_graph(source).map_err(|err| format!("无法将当前 KCL 编译为工程图: {err}"))?;
    if path.exists() {
        return save_existing_aicad_document(&path, project_path, &project_name, source, &graph);
    }
    write_new_sqlite_aicad(&path, project_path, project_name, source, kernel, graph)
}

/// TES-1-P0.5: overwriting an existing SQLite `.aicad` must not rebuild the
/// container from a fresh `ContainerManifest`. That dropped `oplog`, branches,
/// and modeling history, so Undo after save/reopen could only undo "Open".
fn save_existing_aicad_document(
    path: &Path,
    project_path: &str,
    _project_name: &str,
    source: &str,
    graph: &cad_ir::CanonicalGraph,
) -> Result<SaveAicadDocumentResult, String> {
    let storage = ProjectStorage::detect(path)?;
    match storage {
        ProjectStorage::Sqlite => {
            let existing = storage
                .load(path)
                .map_err(|err| format!("无法读取已有 .aicad 工程: {err}"))?;
            let patch_id = format!(
                "save-kcl-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis())
                    .unwrap_or(0)
            );
            let mut patch = diff_graphs(&existing.graph, graph, &patch_id);
            patch.description = "save .aicad from current KCL".to_owned();
            if !patch.ops.is_empty() {
                apply_patch_in_place(path, &patch, None)
                    .map_err(|err| format!("无法保存 .aicad 工程: {err}"))?;
            }
            Ok(SaveAicadDocumentResult {
                status: "ok",
                project_path: display_project_path(project_path)?,
                project_name: existing.manifest.project_name,
                history: sqlite_container_history(path)?,
            })
        }
        ProjectStorage::Directory | ProjectStorage::ShardedDirectory => {
            let mut project = storage
                .load(path)
                .map_err(|err| format!("无法读取已有工程: {err}"))?;
            project.graph = graph.clone();
            project.kcl_view = Some(source.to_owned());
            storage
                .save(&project, path)
                .map_err(|err| format!("无法保存工程: {err}"))?;
            Ok(SaveAicadDocumentResult {
                status: "ok",
                project_path: display_project_path(project_path)?,
                project_name: project.manifest.project_name,
                history: sqlite_container_history(path)?,
            })
        }
    }
}

fn write_new_sqlite_aicad(
    path: &Path,
    project_path: &str,
    project_name: String,
    source: &str,
    kernel: &KernelFingerprint,
    graph: cad_ir::CanonicalGraph,
) -> Result<SaveAicadDocumentResult, String> {
    let mut project = AicadProject::new(
        ContainerManifest::new(project_name.clone(), kernel.clone()),
        graph,
    );
    project.kcl_view = Some(source.to_owned());
    ProjectStorage::Sqlite
        .save(&project, path)
        .map_err(|err| format!("无法保存 .aicad 工程: {err}"))?;
    Ok(SaveAicadDocumentResult {
        status: "ok",
        project_path: display_project_path(project_path)?,
        project_name,
        history: sqlite_container_history(&path)?,
    })
}

fn sqlite_container_history(path: &Path) -> Result<AicadContainerHistory, String> {
    match ProjectStorage::detect(path) {
        Ok(ProjectStorage::Sqlite) if path.exists() => {}
        _ => return Ok(AicadContainerHistory::default()),
    }
    let container = SqliteContainer::open(path)
        .map_err(|err| format!("无法读取工程历史: {err}"))?;
    let branch = container
        .active_branch()
        .unwrap_or_else(|_| "main".to_owned());
    let entries = container
        .oplog_entries(Some(&branch))
        .map_err(|err| format!("无法读取建模历史: {err}"))?;
    let oplog_len = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let head_seq = entries.last().map(|entry| entry.seq);
    let previous_seq = entries
        .len()
        .checked_sub(2)
        .and_then(|index| entries.get(index))
        .map(|entry| entry.seq);
    Ok(AicadContainerHistory {
        can_rollback: previous_seq.is_some(),
        oplog_len,
        head_seq,
        previous_seq,
    })
}

/// TES-1-P0.5: roll the open SQLite `.aicad` back by one main-branch oplog
/// commit and return the restored emit_kcl source. This is the command the UI
/// Undo path must call; in-memory session undo after "打开" is not enough.
pub fn undo_aicad_container_in(
    workspace_root: &Path,
    project_path: &str,
) -> Result<UndoAicadContainerResult, String> {
    let path = resolve_project_path(workspace_root, project_path)?;
    let history = sqlite_container_history(&path)?;
    let Some(target) = history.previous_seq else {
        return Err("没有可撤销的容器建模历史".to_string());
    };
    {
        let mut container = SqliteContainer::open(&path)
            .map_err(|err| format!("无法打开工程以撤销历史: {err}"))?;
        container
            .rollback_to_seq(target)
            .map_err(|err| format!("无法撤销建模历史: {err}"))?;
    }
    let opened = open_aicad_document_in(workspace_root, project_path)?;
    Ok(UndoAicadContainerResult {
        status: "ok",
        project_path: opened.project_path,
        project_name: opened.project_name,
        source: opened.source,
        history: opened.history,
    })
}


fn decode_base64(input: &str, max_decoded_bytes: usize) -> Result<Vec<u8>, String> {
    fn value(byte: u8) -> Result<u8, String> {
        match byte {
            b'A'..=b'Z' => Ok(byte - b'A'),
            b'a'..=b'z' => Ok(byte - b'a' + 26),
            b'0'..=b'9' => Ok(byte - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err("无效的 Base64 内容".to_string()),
        }
    }
    let filtered: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if filtered.len() % 4 != 0 {
        return Err("无效的 Base64 内容".to_string());
    }
    let max_encoded = max_decoded_bytes.div_ceil(3).saturating_mul(4);
    if filtered.len() > max_encoded {
        return Err("aicad 文件不能超过 32 MiB".to_string());
    }
    let mut out = Vec::with_capacity(filtered.len() / 4 * 3);
    let chunk_count = filtered.len() / 4;
    for (idx, chunk) in filtered.chunks_exact(4).enumerate() {
        let is_last = idx + 1 == chunk_count;
        let pad2 = chunk[2] == b'=';
        let pad3 = chunk[3] == b'=';
        if (pad2 && !pad3) || ((pad2 || pad3) && !is_last) {
            return Err("无效的 Base64 内容".to_string());
        }
        let pad = usize::from(pad2) + usize::from(pad3);
        let a = value(chunk[0])?;
        let b = value(chunk[1])?;
        let c = if pad2 { 0 } else { value(chunk[2])? };
        let d = if pad3 { 0 } else { value(chunk[3])? };
        let n = u32::from(a) << 18 | u32::from(b) << 12 | u32::from(c) << 6 | u32::from(d);
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    if out.len() > max_decoded_bytes {
        return Err("aicad 文件不能超过 32 MiB".to_string());
    }
    Ok(out)
}
